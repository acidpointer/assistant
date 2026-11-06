//! Durable extraction leases and compute accounting. Reservations survive process
//! death; stale fences cannot publish even if native inference finishes late.
//! Graph changes and the terminal attempt record share one writer transaction.

use super::super::{
    Claim, ExtractionAccounting, ExtractionBudget, ExtractionChanges, ExtractionLease,
    ExtractionOutcome, MemoryError, MemoryId, MemoryResult, MemoryText, Source,
};
use super::{
    database::{Database, WriteBudget, check_budget, decode, json, revision},
    graph::insert_claim,
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};

impl Database {
    pub(super) fn claim_extraction_job(
        &mut self,
        now: i64,
        budget: ExtractionBudget,
    ) -> MemoryResult<Option<ExtractionLease>> {
        check_budget(
            &self.connection,
            self.directory.used_bytes()?,
            self.quota,
            WriteBudget::Ordinary,
        )?;
        let (reserve_passes, reserve_input, reserve_output, reserve_millis) = budget.limits();
        let principal = self.owner.principal().to_string();
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (clock, generation): (i64, i64) = tx.query_row(
            "SELECT max(budget_now,?1),deletion_generation FROM meta",
            [now],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        tx.execute("UPDATE meta SET budget_now=?1", [clock])?;
        let (inputs,outputs,millis):(i64,i64,i64)=tx.query_row("SELECT coalesce(sum(input_tokens),0),coalesce(sum(output_tokens),0),coalesce(sum(native_millis),0) FROM job_attempts WHERE principal=?1 AND charged_at>=?2",params![principal,clock.saturating_sub(86_400_000)],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        // Charge the conservative maximum before dispatch. An interrupted or
        // crashed attempt keeps it until a matching fenced completion reconciles.
        let mut stmt=tx.prepare("SELECT j.id,j.fence,s.id,s.revision,s.text,s.preserved,coalesce(m.operation_id,s.command_id) FROM jobs j JOIN sources s ON s.principal=j.principal AND s.id=j.source_id AND s.revision=j.source_revision JOIN source_metadata m ON m.principal=s.principal AND m.source_id=s.id AND m.source_revision=s.revision WHERE j.principal=?1 AND s.suppressed=0 AND (j.state='ready' OR (j.state='leased' AND j.lease_until<=?2)) ORDER BY j.id LIMIT 32")?;
        let candidates = stmt
            .query_map(params![principal, clock], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, bool>(5)?,
                    r.get::<_, String>(6)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        if candidates.is_empty() {
            tx.commit()?;
            return Ok(None);
        }
        if inputs + reserve_input > 60_000
            || outputs + reserve_output > 10_000
            || millis + reserve_millis > 300_000
        {
            return Err(MemoryError::Capacity);
        }
        for (id, fence, source_id, source_revision, text, preserved, root) in candidates {
            let (passes,inputs,outputs):(i64,i64,i64)=tx.query_row("SELECT coalesce(sum(passes),0),coalesce(sum(input_tokens),0),coalesce(sum(output_tokens),0) FROM job_attempts WHERE principal=?1 AND root_cause=?2",params![principal,root],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
            if passes + reserve_passes > 4
                || inputs + reserve_input > 4096
                || outputs + reserve_output > 1024
            {
                tx.execute("UPDATE jobs SET state='unresolved',lease_instance=NULL,lease_until=NULL WHERE principal=?1 AND id=?2",params![principal,id])?;
                tx.execute("UPDATE source_metadata SET formation='unresolved' WHERE principal=?1 AND source_id=?2 AND source_revision=?3",params![principal,source_id,source_revision])?;
                continue;
            }
            let next = fence.checked_add(1).ok_or(MemoryError::Capacity)?;
            let instance = MemoryId::new()?;
            tx.execute("UPDATE jobs SET state='leased',fence=?1,lease_instance=?2,lease_until=?3 WHERE principal=?4 AND id=?5",params![next,instance.to_string(),clock.checked_add(60_000).ok_or(MemoryError::Capacity)?,principal,id])?;
            tx.execute("INSERT INTO job_attempts(principal,job_id,fence,root_cause,charged_at,input_tokens,output_tokens,native_millis,passes,reserved_passes,reserved_input,reserved_output,reserved_millis,hard_until) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?9,?6,?7,?8,?10)",params![principal,id,next,root,clock,reserve_input,reserve_output,reserve_millis,reserve_passes,clock.checked_add(120_000).ok_or(MemoryError::Capacity)?])?;
            let lease = ExtractionLease {
                job: MemoryId::parse(&id)?,
                fence: revision(next)?,
                instance,
                generation,
                source: Source {
                    id: MemoryId::parse(&source_id)?,
                    revision: revision(source_revision)?,
                    text: MemoryText::new(text).map_err(|_| MemoryError::CorruptStore)?,
                    preserved,
                },
            };
            tx.commit()?;
            return Ok(Some(lease));
        }
        tx.commit()?;
        Ok(None)
    }

    pub(super) fn finish_extraction_job(
        &mut self,
        lease: &ExtractionLease,
        changes: &ExtractionChanges,
        outcome: ExtractionOutcome,
        accounting: ExtractionAccounting,
    ) -> MemoryResult<Vec<Claim>> {
        let principal = self.owner.principal().to_string();
        let usage = accounting.usage;
        // Preserve completion replay across upgrades of the single-pass format.
        let encoded = if accounting.passes == 1 {
            serde_json::to_vec(&(&changes, outcome, usage))?
        } else {
            serde_json::to_vec(&(&changes, outcome, accounting))?
        };
        let fingerprint = Sha256::digest(encoded).to_vec();
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let attempt:Option<(Option<Vec<u8>>,Option<String>)>=tx.query_row("SELECT completion_digest,result_json FROM job_attempts WHERE principal=?1 AND job_id=?2 AND fence=?3",params![principal,lease.job.to_string(),lease.fence.get()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        if let Some((Some(old), Some(result))) = &attempt {
            let eligible:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM sources s,meta m WHERE s.principal=?1 AND s.id=?2 AND s.revision=?3 AND s.suppressed=0 AND m.deletion_generation=?4)",params![principal,lease.source.id.to_string(),lease.source.revision.get(),lease.generation],|r|r.get(0))?;
            if !eligible {
                return Err(MemoryError::Conflict);
            }
            if old == &fingerprint {
                return decode(result);
            }
            return Err(MemoryError::Conflict);
        }
        let clock: i64 = tx.query_row(
            "SELECT max(budget_now,?1) FROM meta",
            [crate::memory::session::utc_millis()?],
            |r| r.get(0),
        )?;
        let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM jobs j JOIN sources s ON s.principal=j.principal AND s.id=j.source_id AND s.revision=j.source_revision JOIN job_attempts a ON a.principal=j.principal AND a.job_id=j.id AND a.fence=j.fence,meta m WHERE j.principal=?1 AND j.id=?2 AND j.state='leased' AND j.fence=?3 AND j.lease_instance=?4 AND s.id=?5 AND s.revision=?6 AND s.suppressed=0 AND m.deletion_generation=?7 AND j.lease_until>?8 AND a.hard_until>?8)",params![principal,lease.job.to_string(),lease.fence.get(),lease.instance.to_string(),lease.source.id.to_string(),lease.source.revision.get(),lease.generation,clock],|r|r.get(0))?;
        if !valid {
            return Err(MemoryError::Conflict);
        }
        tx.execute("UPDATE meta SET budget_now=?1", [clock])?;
        let (passes,input,output,millis):(i64,i64,i64,i64)=tx.query_row("SELECT reserved_passes,reserved_input,reserved_output,reserved_millis FROM job_attempts WHERE principal=?1 AND job_id=?2 AND fence=?3",params![principal,lease.job.to_string(),lease.fence.get()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
        if i64::from(accounting.passes) > passes
            || i64::from(usage.input_tokens) > input
            || i64::from(usage.output_tokens) > output
            || i64::from(usage.native_millis) > millis
        {
            return Err(MemoryError::InvalidInput("extraction reservation exceeded"));
        }
        if !matches!(outcome, ExtractionOutcome::Complete)
            && (!changes.entities.is_empty() || !changes.claims.is_empty())
        {
            return Err(MemoryError::InvalidInput("nonterminal extraction changes"));
        }
        if !changes.entities.is_empty() || !changes.claims.is_empty() {
            check_budget(
                &tx,
                self.directory.used_bytes()?,
                self.quota,
                WriteBudget::Ordinary,
            )?;
        }
        let sequence: i64 = tx.query_row("SELECT commit_seq FROM meta", [], |r| r.get(0))?;
        let sequence = revision(sequence.checked_add(1).ok_or(MemoryError::Capacity)?)?;
        for entity in &changes.entities {
            tx.execute(
                "INSERT INTO nodes(principal,id,kind,label) VALUES(?1,?2,?3,?4)",
                params![
                    principal,
                    entity.id.to_string(),
                    json(&entity.kind)?,
                    entity.label.as_str()
                ],
            )?;
            tx.execute("INSERT INTO node_sources(principal,node_id,source_id,source_revision) VALUES(?1,?2,?3,?4)",params![principal,entity.id.to_string(),lease.source.id.to_string(),lease.source.revision.get()])?;
        }
        let mut result = Vec::new();
        for claim in &changes.claims {
            if claim.evidence.iter().any(|e| {
                e.source_id != lease.source.id || e.source_revision != lease.source.revision
            }) {
                return Err(MemoryError::Forbidden);
            }
            result.push(insert_claim(
                &tx,
                &principal,
                MemoryId::new()?,
                revision(1)?,
                sequence,
                claim,
            )?);
        }
        let state = match outcome {
            ExtractionOutcome::Complete => "succeeded",
            ExtractionOutcome::Unresolved => "unresolved",
            ExtractionOutcome::Deferred => "ready",
        };
        tx.execute("UPDATE jobs SET state=?1,lease_instance=NULL,lease_until=NULL WHERE principal=?2 AND id=?3 AND fence=?4",params![state,principal,lease.job.to_string(),lease.fence.get()])?;
        tx.execute("UPDATE job_attempts SET input_tokens=?1,output_tokens=?2,native_millis=?3,completion_digest=?4,result_json=?5,passes=?9 WHERE principal=?6 AND job_id=?7 AND fence=?8",params![i64::from(usage.input_tokens),i64::from(usage.output_tokens),i64::from(usage.native_millis),fingerprint,json(&result)?,principal,lease.job.to_string(),lease.fence.get(),i64::from(accounting.passes)])?;
        tx.execute("UPDATE source_metadata SET formation=?1 WHERE principal=?2 AND source_id=?3 AND source_revision=?4", params![match outcome {ExtractionOutcome::Complete=>"complete", ExtractionOutcome::Unresolved=>"unresolved", ExtractionOutcome::Deferred=>"queued"},principal,lease.source.id.to_string(),lease.source.revision.get()])?;
        tx.execute("UPDATE meta SET commit_seq=?1", [sequence.get()])?;
        tx.execute("INSERT INTO change_events(principal,seq,kind,root_cause) SELECT ?1,?2,'extraction',coalesce(m.operation_id,s.command_id) FROM sources s JOIN source_metadata m ON m.principal=s.principal AND m.source_id=s.id AND m.source_revision=s.revision WHERE s.principal=?1 AND s.id=?3 AND s.revision=?4",params![principal,sequence.get(),lease.source.id.to_string(),lease.source.revision.get()])?;
        tx.commit()?;
        Ok(result)
    }

    pub(super) fn heartbeat_extraction_job(
        &mut self,
        lease: &ExtractionLease,
        now: i64,
    ) -> MemoryResult<()> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let clock: i64 =
            tx.query_row("SELECT max(budget_now,?1) FROM meta", [now], |r| r.get(0))?;
        let updated=tx.execute("UPDATE jobs SET lease_until=min(?1+60000,(SELECT hard_until FROM job_attempts a WHERE a.principal=jobs.principal AND a.job_id=jobs.id AND a.fence=jobs.fence)) WHERE principal=?2 AND id=?3 AND state='leased' AND fence=?4 AND lease_instance=?5 AND lease_until>?1 AND EXISTS(SELECT 1 FROM job_attempts a WHERE a.principal=jobs.principal AND a.job_id=jobs.id AND a.fence=jobs.fence AND a.hard_until>?1) AND EXISTS(SELECT 1 FROM sources s,meta m WHERE s.principal=jobs.principal AND s.id=jobs.source_id AND s.revision=jobs.source_revision AND s.suppressed=0 AND m.deletion_generation=?6)",params![clock,self.owner.principal().to_string(),lease.job.to_string(),lease.fence.get(),lease.instance.to_string(),lease.generation])?;
        if updated != 1 {
            return Err(MemoryError::Conflict);
        }
        tx.execute("UPDATE meta SET budget_now=?1", [clock])?;
        tx.commit()?;
        Ok(())
    }
}

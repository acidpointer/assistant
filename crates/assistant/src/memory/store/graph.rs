//! Qualified graph mutations and bounded adjacency reads. Evidence spans are
//! resolved in immutable UTF-8 sources. Type checks do not prove entailment;
//! callers must validate model semantics before using the host mutation API.

use super::super::{
    Claim, ClaimInput, ClaimModality, ClaimObject, Entity, EntityKind, GraphBundle, MemoryError,
    MemoryId, MemoryReceipt, MemoryResult, MemoryText, OperationKey, Predicate, Revision,
};
use super::database::{Database, decode, json, revision};
use rusqlite::{OptionalExtension, Transaction, params};
use std::collections::{BTreeSet, VecDeque};

pub(super) fn validate_bounds(input: &ClaimInput) -> MemoryResult<()> {
    if input.evidence.is_empty()
        || input.evidence.len() > 4
        || input.conditions.len() > 4
        || input.conditions.iter().any(|s| s.as_str().len() > 1024)
        || input.evidence.iter().any(|e| e.quote.as_str().len() > 2048)
        || matches!(&input.object,ClaimObject::Text(s) if s.as_str().len()>2048)
        || matches!((input.valid_from,input.valid_to),(Some(a),Some(b)) if a>=b)
    {
        return Err(MemoryError::InvalidInput("claim bounds"));
    }
    Ok(())
}

fn kind(tx: &Transaction<'_>, principal: &str, id: MemoryId) -> MemoryResult<EntityKind> {
    let value: String = tx.query_row(
        "SELECT kind FROM nodes WHERE principal=?1 AND id=?2",
        params![principal, id.to_string()],
        |r| r.get(0),
    )?;
    decode(&value)
}

fn validate_types(tx: &Transaction<'_>, principal: &str, input: &ClaimInput) -> MemoryResult<()> {
    use EntityKind::{
        Agent, Commitment, Goal, Occupation, Organization, Person, Place, Project, Question,
    };
    let subject = kind(tx, principal, input.subject)?;
    let object = match input.object {
        ClaimObject::Reference(id) => Some(kind(tx, principal, id)?),
        _ => None,
    };
    let valid = match input.predicate {
        Predicate::SiblingOf | Predicate::ParentOf | Predicate::PartnerOf => {
            subject == Person && object == Some(Person)
        }
        Predicate::HasOccupation => subject == Person && object == Some(Occupation),
        Predicate::LivesIn => subject == Person && object == Some(Place),
        Predicate::WorksOn => subject == Person && object == Some(Project),
        Predicate::MemberOf => subject == Person && object == Some(Organization),
        Predicate::HasGoal => matches!(subject, Person | Agent) && object == Some(Goal),
        Predicate::Asks => matches!(subject, Person | Agent) && object == Some(Question),
        Predicate::Prefers | Predicate::InterestedIn => {
            matches!(subject, Person | Agent) && !matches!(input.object, ClaimObject::Timestamp(_))
        }
        Predicate::Owns | Predicate::Uses => {
            matches!(subject, Person | Agent | Organization) && object.is_some()
        }
        Predicate::BlockedBy | Predicate::DependsOn | Predicate::ReasonFor => {
            matches!(subject, Goal | Project | Commitment) && object.is_some()
        }
        Predicate::DueAt => {
            subject == Commitment && matches!(input.object, ClaimObject::Timestamp(_))
        }
        Predicate::Status => match &input.object {
            ClaimObject::Text(text) => match subject {
                Goal => ["active", "completed", "abandoned"].contains(&text.as_str()),
                Commitment => ["pending", "fulfilled", "cancelled"].contains(&text.as_str()),
                Project => ["planned", "active", "blocked", "completed"].contains(&text.as_str()),
                _ => false,
            },
            _ => false,
        },
        Predicate::AssociatedWith => object.is_some(),
    };
    if !valid {
        return Err(MemoryError::InvalidInput("predicate types"));
    }
    Ok(())
}

pub(super) fn insert_claim(
    tx: &Transaction<'_>,
    principal: &str,
    id: MemoryId,
    version: Revision,
    sequence: Revision,
    input: &ClaimInput,
) -> MemoryResult<Claim> {
    validate_bounds(input)?;
    validate_types(tx, principal, input)?;
    if matches!(input.predicate, Predicate::Status | Predicate::DueAt)
        && input.modality == ClaimModality::Asserted
        && !input.negative
    {
        let mut statement=tx.prepare("SELECT payload FROM claim_versions WHERE principal=?1 AND subject=?2 AND predicate=?3 AND recorded_to IS NULL LIMIT 257")?;
        let candidates = statement.query_map(
            params![
                principal,
                input.subject.to_string(),
                json(&input.predicate)?
            ],
            |r| r.get::<_, String>(0),
        )?;
        for (count, payload) in candidates.enumerate() {
            if count >= 256 {
                return Err(MemoryError::Capacity);
            }
            let existing: ClaimInput = decode(&payload?)?;
            let disjoint = input
                .valid_to
                .zip(existing.valid_from)
                .is_some_and(|(end, start)| end <= start)
                || existing
                    .valid_to
                    .zip(input.valid_from)
                    .is_some_and(|(end, start)| end <= start);
            if existing.modality == ClaimModality::Asserted && !existing.negative && !disjoint {
                return Err(MemoryError::Conflict);
            }
        }
    }
    let mut spans = Vec::new();
    for evidence in &input.evidence {
        let source:String=tx.query_row("SELECT text FROM sources WHERE principal=?1 AND id=?2 AND revision=?3 AND suppressed=0",params![principal,evidence.source_id.to_string(),evidence.source_revision.get()],|r|r.get(0))?;
        let mut matches = source
            .char_indices()
            .filter(|(index, _)| source[*index..].starts_with(evidence.quote.as_str()));
        let (start, _) = matches
            .next()
            .ok_or(MemoryError::InvalidInput("evidence quote absent"))?;
        if matches.next().is_some() {
            return Err(MemoryError::InvalidInput("ambiguous evidence quote"));
        }
        spans.push((evidence, start, start + evidence.quote.as_str().len()));
    }
    let object = match input.object {
        ClaimObject::Reference(id) => Some(id.to_string()),
        _ => None,
    };
    tx.execute("INSERT INTO claim_versions(principal,id,revision,subject,predicate,object_node,payload,recorded_from) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![principal,id.to_string(),version.get(),input.subject.to_string(),json(&input.predicate)?,object,json(input)?,sequence.get()])?;
    for (evidence, start, end) in spans {
        tx.execute("INSERT INTO evidence(principal,claim_id,claim_revision,source_id,source_revision,byte_start,byte_end) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![principal,id.to_string(),version.get(),evidence.source_id.to_string(),evidence.source_revision.get(),i64::try_from(start).map_err(|_|MemoryError::Capacity)?,i64::try_from(end).map_err(|_|MemoryError::Capacity)?])?;
    }
    tx.execute("INSERT INTO predicate_scopes(principal,subject,predicate,revision) VALUES(?1,?2,?3,1) ON CONFLICT(principal,subject,predicate) DO UPDATE SET revision=revision+1",params![principal,input.subject.to_string(),json(&input.predicate)?])?;
    tx.execute("UPDATE meta SET derived_epoch=derived_epoch+1", [])?;
    Ok(Claim {
        id,
        revision: version,
        input: input.clone(),
        recorded_from: sequence,
        recorded_to: None,
    })
}

impl Database {
    pub(super) fn create_entity(
        &mut self,
        key: &OperationKey,
        kind: EntityKind,
        label: MemoryText,
    ) -> MemoryResult<MemoryReceipt<Entity>> {
        let principal = self.owner.principal().to_string();
        let payload = label.clone();
        self.mutate(key, &(kind, label), "entity", move |tx, _, _| {
            let entity = Entity {
                id: MemoryId::new()?,
                kind,
                label: payload,
            };
            tx.execute(
                "INSERT INTO nodes(principal,id,kind,label) VALUES(?1,?2,?3,?4)",
                params![
                    principal,
                    entity.id.to_string(),
                    json(&kind)?,
                    entity.label.as_str()
                ],
            )?;
            Ok(entity)
        })
    }
    pub(super) fn add_claim(
        &mut self,
        key: &OperationKey,
        input: &ClaimInput,
    ) -> MemoryResult<MemoryReceipt<Claim>> {
        let principal = self.owner.principal().to_string();
        let payload = input.clone();
        self.mutate(key, input, "claim", move |tx, _, seq| {
            insert_claim(
                tx,
                &principal,
                MemoryId::new()?,
                revision(1)?,
                seq,
                &payload,
            )
        })
    }
    pub(super) fn correct_claim(
        &mut self,
        key: &OperationKey,
        id: MemoryId,
        expected: Revision,
        input: &ClaimInput,
    ) -> MemoryResult<MemoryReceipt<Claim>> {
        let principal = self.owner.principal().to_string();
        let payload = input.clone();
        self.mutate(key,&(id,expected,input),"correct",move|tx,_,seq|{
            let current:Option<(i64,String)>=tx.query_row("SELECT revision,payload FROM claim_versions c WHERE principal=?1 AND id=?2 AND recorded_to IS NULL AND NOT EXISTS(SELECT 1 FROM evidence e JOIN sources s ON s.principal=e.principal AND s.id=e.source_id AND s.revision=e.source_revision WHERE e.principal=c.principal AND e.claim_id=c.id AND e.claim_revision=c.revision AND s.suppressed=1)",params![principal,id.to_string()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            let Some((current,original))=current else{return Err(MemoryError::NotFound)};
            if current!=expected.get(){return Err(MemoryError::Conflict)}
            let original:ClaimInput=decode(&original)?;
            if original.subject!=payload.subject||original.predicate!=payload.predicate{return Err(MemoryError::InvalidInput("correction target"))}
            tx.execute("UPDATE claim_versions SET recorded_to=?1 WHERE principal=?2 AND id=?3 AND revision=?4",params![seq.get(),principal,id.to_string(),current])?;
            insert_claim(tx,&principal,id,revision(current.checked_add(1).ok_or(MemoryError::Capacity)?)?,seq,&payload)
        })
    }
    pub(super) fn claim_history(&self, id: MemoryId) -> MemoryResult<Vec<Claim>> {
        let page = self.claim_history_page(id, None, 256)?;
        if page.next_after.is_some() {
            return Err(MemoryError::Capacity);
        }
        Ok(page.claims)
    }
    pub(super) fn claim_history_page(
        &self,
        id: MemoryId,
        after: Option<Revision>,
        limit: usize,
    ) -> MemoryResult<crate::memory::ClaimHistoryPage> {
        let mut stmt=self.connection.prepare("SELECT c.revision,c.payload,c.recorded_from,c.recorded_to FROM claim_versions c WHERE c.principal=?1 AND c.id=?2 AND NOT EXISTS(SELECT 1 FROM evidence e JOIN sources s ON s.principal=e.principal AND s.id=e.source_id AND s.revision=e.source_revision WHERE e.principal=c.principal AND e.claim_id=c.id AND e.claim_revision=c.revision AND s.suppressed=1) AND c.revision>?3 ORDER BY c.revision LIMIT ?4")?;
        let rows = stmt.query_map(
            params![
                self.owner.principal().to_string(),
                id.to_string(),
                after.map_or(0, Revision::get),
                i64::try_from(limit + 1).map_err(|_| MemoryError::InvalidInput("history limit"))?
            ],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                ))
            },
        )?;
        let mut claims = Vec::new();
        for row in rows {
            let (rev, payload, from, to) = row?;
            claims.push(Claim {
                id,
                revision: revision(rev)?,
                input: decode(&payload)?,
                recorded_from: revision(from)?,
                recorded_to: to.map(revision).transpose()?,
            });
        }
        if claims.is_empty() && after.is_none() {
            return Err(MemoryError::NotFound);
        }
        let more = claims.len() > limit;
        claims.truncate(limit);
        let next_after = if more {
            claims.last().map(|claim| claim.revision)
        } else {
            None
        };
        Ok(crate::memory::ClaimHistoryPage { claims, next_after })
    }
    pub(super) fn follow(
        &mut self,
        start: MemoryId,
        depth: usize,
        at: i64,
    ) -> MemoryResult<GraphBundle> {
        let tx = self.connection.transaction()?;
        let seq: i64 = tx.query_row("SELECT commit_seq FROM meta", [], |r| r.get(0))?;
        let principal = self.owner.principal().to_string();
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM nodes WHERE principal=?1 AND id=?2)",
            params![principal, start.to_string()],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(MemoryError::NotFound);
        }
        let mut pending = VecDeque::from([(start, 0)]);
        let mut visited = BTreeSet::new();
        let mut result = Vec::new();
        let mut emitted = BTreeSet::new();
        let mut examined = 0;
        let mut truncated = false;
        while let Some((node, step)) = pending.pop_front() {
            if step >= depth || !visited.insert((node, step)) {
                continue;
            }
            if visited.len() > 2048 || examined >= 8192 || result.len() >= 256 {
                truncated = true;
                break;
            }
            let mut stmt=tx.prepare("SELECT c.id,c.revision,c.payload,c.recorded_from FROM claim_versions c WHERE c.principal=?1 AND c.subject=?2 AND c.recorded_to IS NULL AND NOT EXISTS(SELECT 1 FROM evidence e JOIN sources s ON s.principal=e.principal AND s.id=e.source_id AND s.revision=e.source_revision WHERE e.principal=c.principal AND e.claim_id=c.id AND e.claim_revision=c.revision AND s.suppressed=1) ORDER BY c.id LIMIT 257")?;
            let rows = stmt.query_map(params![principal, node.to_string()], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?;
            let mut degree = 0;
            for row in rows {
                examined += 1;
                degree += 1;
                if examined > 8192 || degree > 256 || result.len() >= 256 {
                    truncated = true;
                    break;
                }
                let (id, rev, payload, from) = row?;
                let id = MemoryId::parse(&id)?;
                let input: ClaimInput = decode(&payload)?;
                if !matches!(
                    input.modality,
                    ClaimModality::Asserted | ClaimModality::Reported
                ) || input.predicate == Predicate::AssociatedWith
                    || input.valid_from.is_some_and(|start| at < start)
                    || input.valid_to.is_some_and(|end| at >= end)
                {
                    continue;
                }
                if !input.negative
                    && input.modality == ClaimModality::Asserted
                    && let ClaimObject::Reference(target) = input.object
                {
                    pending.push_back((target, step + 1));
                }
                if emitted.insert(id) {
                    result.push(Claim {
                        id,
                        revision: revision(rev)?,
                        input,
                        recorded_from: revision(from)?,
                        recorded_to: None,
                    });
                }
            }
        }
        Ok(GraphBundle {
            sequence: if seq == 0 { None } else { Some(revision(seq)?) },
            claims: result,
            truncated,
        })
    }
}

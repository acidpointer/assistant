//! Universal source graph, freeform profile and durable initiative mailbox.
//! One writer owns mutations and timer claims. Reads use bounded snapshots;
//! notes retain transitive source dependencies so forgetting their evidence also
//! suppresses interpretations. No topic-specific extraction runs here.

use super::{
    MemoryHandle, OwnerCapability,
    database::{Database, WriteBudget, check_budget, decode, json, revision},
    operations::{SourceOrigin, insert_source},
};
use crate::memory::{
    AssistantNotice, AssistantProfile, AssistantSettings, MemoryError, MemoryId, MemoryLink,
    MemoryReceipt, MemoryResult, MemoryText, OperationKey, Revision, Source, SourceGraph,
    SourceRef,
};
use rusqlite::{OptionalExtension, params};
use std::collections::{BTreeSet, VecDeque};

fn validate_settings(settings: &AssistantSettings) -> MemoryResult<()> {
    if settings.instructions.as_str().len() > 4096
        || settings
            .initiative_interval_seconds
            .is_some_and(|value| !(60..=86400).contains(&value))
    {
        return Err(MemoryError::InvalidInput("assistant settings bounds"));
    }
    Ok(())
}
impl MemoryHandle {
    /// Runtime source graphs do not depend on the experimental typed extractor.
    pub(crate) async fn use_source_graph(&self, owner: &OwnerCapability) -> MemoryResult<()> {
        self.authorize(owner)?;
        self.0.writer.call(|db| {
            let tx=db.connection.transaction()?;
            tx.execute("UPDATE jobs SET state='cancelled',fence=fence+1,lease_instance=NULL,lease_until=NULL WHERE state IN('ready','leased')",[])?;
            tx.execute("UPDATE source_metadata SET formation='not_applicable' WHERE formation IN('queued','deferred')",[])?;
            tx.commit()?;
            db.pending_job_limit=0;
            Ok(())
        }).await
    }

    /// Record a live owner's note with host observation time. Historical imports
    /// keep the original API's unknown time; retry preserves the first observation.
    pub(crate) async fn remember_observed(
        &self,
        owner: &OwnerCapability,
        key: OperationKey,
        text: MemoryText,
        now: i64,
    ) -> MemoryResult<MemoryReceipt<Source>> {
        self.authorize(owner)?;
        if now < 0 || text.as_str().len() > 16_384 {
            return Err(MemoryError::InvalidInput("owner note bounds"));
        }
        self.0
            .writer
            .call(move |db| db.remember_observed(&key, &text, now))
            .await
    }
    /// Read durable freeform personality and initiative status.
    /// # Errors
    /// Reports foreign authority or unavailable storage.
    pub async fn assistant_profile(
        &self,
        owner: &OwnerCapability,
    ) -> MemoryResult<AssistantProfile> {
        self.authorize(owner)?;
        self.reader().call(|db| db.assistant_profile()).await
    }
    /// Atomically replace settings if the observed revision is still current.
    /// # Errors
    /// Reports invalid bounds, stale edits, authority or storage failure.
    pub async fn update_assistant(
        &self,
        owner: &OwnerCapability,
        expected: Revision,
        settings: AssistantSettings,
        now: i64,
    ) -> MemoryResult<AssistantProfile> {
        self.authorize(owner)?;
        validate_settings(&settings)?;
        if now < 0 {
            return Err(MemoryError::InvalidInput("host clock"));
        }
        self.0
            .writer
            .call(move |db| db.update_assistant(expected, &settings, now))
            .await
    }
    /// Save an assistant interpretation and its navigation/evidence links.
    /// # Errors
    /// Rejects missing/suppressed references, invalid bounds and key conflicts.
    pub async fn remember_note(
        &self,
        owner: &OwnerCapability,
        key: OperationKey,
        text: MemoryText,
        links: Vec<(SourceRef, MemoryText)>,
        now: i64,
    ) -> MemoryResult<MemoryReceipt<Source>> {
        self.authorize(owner)?;
        if text.as_str().len() > 4096
            || links.is_empty()
            || links.len() > 8
            || links.iter().any(|(_, label)| label.as_str().len() > 128)
            || now < 0
        {
            return Err(MemoryError::InvalidInput("note bounds"));
        }
        self.0
            .writer
            .call(move |db| db.remember_note(&key, &text, &links, now, false))
            .await
    }
    /// Walk source links in both directions, preserving original labels/direction.
    /// # Errors
    /// Rejects absent entries, invalid depth, authority or unavailable storage.
    pub async fn follow_sources(
        &self,
        owner: &OwnerCapability,
        start: MemoryId,
        depth: usize,
    ) -> MemoryResult<SourceGraph> {
        self.authorize(owner)?;
        if !(1..=4).contains(&depth) {
            return Err(MemoryError::InvalidInput("source graph depth"));
        }
        self.reader()
            .call(move |db| db.follow_sources(start, depth))
            .await
    }
    /// Read one adjacency page. Repeating with its cursor reaches high-degree
    /// nodes without increasing the model's context or silently dropping edges.
    /// # Errors
    /// Reports invalid cursors/entries, foreign authority or storage failure.
    pub async fn source_page(
        &self,
        owner: &OwnerCapability,
        start: MemoryId,
        cursor: Option<String>,
    ) -> MemoryResult<SourceGraph> {
        self.authorize(owner)?;
        if cursor.as_ref().is_some_and(|cursor| cursor.len() > 1024) {
            return Err(MemoryError::InvalidInput("graph cursor"));
        }
        let after = cursor
            .map(|cursor| {
                serde_json::from_str::<(MemoryId, MemoryId, MemoryText)>(&cursor)
                    .map_err(|_| MemoryError::InvalidInput("graph cursor"))
            })
            .transpose()?;
        if after
            .as_ref()
            .is_some_and(|(_, _, label)| label.as_str().len() > 128)
        {
            return Err(MemoryError::InvalidInput("graph cursor"));
        }
        self.reader()
            .call(move |db| db.source_page(start, after))
            .await
    }
    /// Newest-first durable assistant-initiated messages; suppressed items vanish.
    /// # Errors
    /// Reports foreign authority or unavailable storage.
    pub async fn assistant_notices(
        &self,
        owner: &OwnerCapability,
    ) -> MemoryResult<Vec<AssistantNotice>> {
        self.authorize(owner)?;
        self.reader().call(|db| db.assistant_notices()).await
    }
    /// Dismiss a mailbox item without erasing its source.
    /// # Errors
    /// Reports absent items, authority or storage failure.
    pub async fn dismiss_notice(&self, owner: &OwnerCapability, id: MemoryId) -> MemoryResult<()> {
        self.authorize(owner)?;
        self.0
            .writer
            .call(move |db| {
                if db.connection.execute(
                    "UPDATE assistant_notices SET dismissed=1 WHERE principal=?1 AND source_id=?2",
                    params![db.owner.principal().to_string(), id.to_string()],
                )? == 0
                {
                    return Err(MemoryError::NotFound);
                }
                Ok(())
            })
            .await
    }
    /// Claim one due decision by advancing the durable schedule before inference.
    pub(crate) async fn claim_attention(
        &self,
        owner: &OwnerCapability,
        now: i64,
    ) -> MemoryResult<Option<AssistantProfile>> {
        self.authorize(owner)?;
        self.0.writer.call(move |db| {
            let profile=db.assistant_profile()?;
            let Some(interval)=profile.settings.initiative_interval_seconds else { return Ok(None); };
            if profile.next_check_at.is_none_or(|due| due>now) { return Ok(None); }
            db.connection.execute("UPDATE assistant_profile SET next_check_at=?1,last_check='running' WHERE principal=?2",params![now.saturating_add(i64::from(interval)*1000),db.owner.principal().to_string()])?;
            Ok(Some(profile))
        }).await
    }
    pub(crate) async fn settle_attention(
        &self,
        owner: &OwnerCapability,
        profile: Revision,
        text: Option<MemoryText>,
        evidence: Vec<SourceRef>,
        now: i64,
        status: &'static str,
    ) -> MemoryResult<()> {
        self.authorize(owner)?;
        self.0
            .writer
            .call(move |db| {
                let current = db.assistant_profile()?;
                if current.revision != profile
                    || current.settings.initiative_interval_seconds.is_none()
                {
                    return Err(MemoryError::Conflict);
                }
                if let Some(text) = text {
                    let links = evidence
                        .into_iter()
                        .map(|source| Ok((source, MemoryText::new("based_on")?)))
                        .collect::<MemoryResult<Vec<_>>>()?;
                    let key = OperationKey::new(
                        "assistant-initiative",
                        format!("{}-{now}", profile.get()),
                    )?;
                    db.remember_note(&key, &text, &links, now, true)?;
                }
                let next=current.settings.initiative_interval_seconds.map(|interval|now.saturating_add(i64::from(interval)*1000));
                db.connection.execute(
                    "UPDATE assistant_profile SET last_check=?1,next_check_at=?2 WHERE principal=?3",
                    params![status,next,db.owner.principal().to_string()],
                )?;
                Ok(())
            })
            .await
    }
}
impl Database {
    fn assistant_profile(&self) -> MemoryResult<AssistantProfile> {
        let row:Option<(i64,String,Option<i64>,Option<String>)>=self.connection.query_row("SELECT revision,settings,next_check_at,last_check FROM assistant_profile WHERE principal=?1",[self.owner.principal().to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
        match row {
            Some((version, settings, next_check_at, last_check)) => Ok(AssistantProfile {
                revision: revision(version)?,
                settings: decode(&settings)?,
                next_check_at,
                last_check,
            }),
            None => Ok(AssistantProfile {
                revision: revision(1)?,
                settings: AssistantSettings {
                    instructions: MemoryText::new(
                        "You are a personal assistant. Be thoughtful, direct and attentive to the user's preferences.",
                    )?,
                    initiative_interval_seconds: None,
                },
                next_check_at: None,
                last_check: None,
            }),
        }
    }
    fn update_assistant(
        &mut self,
        expected: Revision,
        settings: &AssistantSettings,
        now: i64,
    ) -> MemoryResult<AssistantProfile> {
        let current = self.assistant_profile()?;
        if current.revision != expected {
            return Err(MemoryError::Conflict);
        }
        let budget = if settings.instructions == current.settings.instructions
            && settings.initiative_interval_seconds.is_none()
        {
            WriteBudget::Revocation
        } else {
            WriteBudget::Ordinary
        };
        check_budget(
            &self.connection,
            self.directory.used_bytes()?,
            self.quota,
            budget,
        )?;
        let next = expected.get().checked_add(1).ok_or(MemoryError::Capacity)?;
        let due = settings
            .initiative_interval_seconds
            .map(|interval| now.saturating_add(i64::from(interval) * 1000));
        self.connection.execute("INSERT INTO assistant_profile(principal,revision,settings,next_check_at,last_check) VALUES(?1,?2,?3,?4,NULL) ON CONFLICT(principal) DO UPDATE SET revision=excluded.revision,settings=excluded.settings,next_check_at=excluded.next_check_at,last_check=NULL",params![self.owner.principal().to_string(),next,json(settings)?,due])?;
        self.assistant_profile()
    }
    fn remember_note(
        &mut self,
        key: &OperationKey,
        text: &MemoryText,
        links: &[(SourceRef, MemoryText)],
        now: i64,
        notice: bool,
    ) -> MemoryResult<MemoryReceipt<Source>> {
        let principal = self.owner.principal().to_string();
        let text = text.clone();
        let links = links.to_vec();
        self.mutate(key,&(text.clone(),links.clone()),"assistant_note",move |tx,command,_| {
            let source=Source{id:MemoryId::new()?,revision:revision(1)?,text,preserved:true};
            // Check references inside the writer transaction: a concurrent forget
            // cannot leave a fresh note derived from already suppressed evidence.
            for (target,_) in &links {
                let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM sources WHERE principal=?1 AND id=?2 AND revision=?3 AND suppressed=0)",params![principal,target.id.to_string(),target.revision.get()],|r|r.get(0))?;
                if !valid { return Err(MemoryError::NotFound); }
            }
            insert_source(tx,&principal,command,&source,SourceOrigin::Generated{observed_at:now},0)?;
            for (target,label) in &links {
                connect(tx,&principal,SourceRef{id:source.id,revision:source.revision},*target,label.as_str())?;
                tx.execute("INSERT OR IGNORE INTO note_dependencies(principal,note_id,source_id) VALUES(?1,?2,?3)",params![principal,source.id.to_string(),target.id.to_string()])?;
                tx.execute("INSERT OR IGNORE INTO note_dependencies(principal,note_id,source_id) SELECT principal,?2,source_id FROM note_dependencies WHERE principal=?1 AND note_id=?3",params![principal,source.id.to_string(),target.id.to_string()])?;
            }
            if notice {
                tx.execute("INSERT INTO assistant_notices(principal,source_id,created_at) VALUES(?1,?2,?3)",params![principal,source.id.to_string(),now])?;
            }
            Ok(source)
        })
    }
    fn follow_sources(&mut self, start: MemoryId, depth: usize) -> MemoryResult<SourceGraph> {
        let tx = self.connection.transaction()?;
        let principal = self.owner.principal().to_string();
        let mut queue = VecDeque::from([(start, 0)]);
        let mut seen = BTreeSet::new();
        let mut links = Vec::new();
        let mut ids = Vec::new();
        let mut bytes = 0;
        let mut truncated = false;
        while let Some((id, level)) = queue.pop_front() {
            if seen.contains(&id) {
                continue;
            }
            let size:Option<i64>=tx.query_row("SELECT length(CAST(text AS BLOB)) FROM sources WHERE principal=?1 AND id=?2 AND suppressed=0",params![principal,id.to_string()],|r|r.get(0)).optional()?;
            let Some(size) = size else {
                if id == start {
                    return Err(MemoryError::NotFound);
                }
                continue;
            };
            let size = usize::try_from(size).map_err(|_| MemoryError::CorruptStore)?;
            if seen.len() == 64 || bytes + size > 262_144 {
                truncated = true;
                continue;
            }
            seen.insert(id);
            ids.push(id);
            bytes += size;
            if level == depth {
                continue;
            }
            let mut statement=tx.prepare("SELECT l.from_id,l.from_revision,l.to_id,l.to_revision,l.relation FROM memory_links l JOIN sources a ON a.principal=l.principal AND a.id=l.from_id AND a.revision=l.from_revision JOIN sources b ON b.principal=l.principal AND b.id=l.to_id AND b.revision=l.to_revision WHERE l.principal=?1 AND (l.from_id=?2 OR l.to_id=?2) AND a.suppressed=0 AND b.suppressed=0 ORDER BY l.from_id,l.to_id,l.relation LIMIT 129")?;
            let rows = statement
                .query_map(params![principal, id.to_string()], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, String>(4)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            for (from, from_rev, to, to_rev, label) in rows {
                let link = MemoryLink {
                    from: SourceRef {
                        id: MemoryId::parse(&from)?,
                        revision: revision(from_rev)?,
                    },
                    to: SourceRef {
                        id: MemoryId::parse(&to)?,
                        revision: revision(to_rev)?,
                    },
                    relation: MemoryText::new(label)?,
                };
                if links.iter().any(|old: &MemoryLink| {
                    old.from.id == link.from.id
                        && old.to.id == link.to.id
                        && old.relation == link.relation
                }) {
                    continue;
                }
                if links.len() == 128 {
                    truncated = true;
                    break;
                }
                queue.push_back((
                    if link.from.id == id {
                        link.to.id
                    } else {
                        link.from.id
                    },
                    level + 1,
                ));
                links.push(link);
            }
        }
        // Keep source hydration in the same read transaction.
        let mut sources = Vec::new();
        for id in ids {
            sources.push(super::operations::source_record(&tx, &principal, id)?);
        }
        links.retain(|link| seen.contains(&link.from.id) && seen.contains(&link.to.id));
        tx.commit()?;
        Ok(SourceGraph {
            sources,
            links,
            truncated,
            next_cursor: None,
        })
    }
    fn source_page(
        &mut self,
        start: MemoryId,
        after: Option<(MemoryId, MemoryId, MemoryText)>,
    ) -> MemoryResult<SourceGraph> {
        let principal = self.owner.principal().to_string();
        let tx = self.connection.transaction()?;
        let root = super::operations::source_record(&tx, &principal, start)?;
        let mut bytes = root.source.text.as_str().len();
        let mut sources = vec![root];
        let mut links = Vec::new();
        let (from, to, label) = after.map_or_else(
            || (String::new(), String::new(), String::new()),
            |(from, to, label)| (from.to_string(), to.to_string(), label.as_str().to_owned()),
        );
        let mut statement=tx.prepare("SELECT l.from_id,l.from_revision,l.to_id,l.to_revision,l.relation FROM memory_links l JOIN sources a ON a.principal=l.principal AND a.id=l.from_id AND a.revision=l.from_revision JOIN sources b ON b.principal=l.principal AND b.id=l.to_id AND b.revision=l.to_revision WHERE l.principal=?1 AND (l.from_id=?2 OR l.to_id=?2) AND (l.from_id,l.to_id,l.relation)>(?3,?4,?5) AND a.suppressed=0 AND b.suppressed=0 ORDER BY l.from_id,l.to_id,l.relation LIMIT 7")?;
        let rows = statement
            .query_map(
                params![principal, start.to_string(), from, to, label],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, String>(4)?,
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        let mut more = false;
        for (from, from_revision, to, to_revision, relation) in rows {
            if links.len() == 6 {
                more = true;
                break;
            }
            let from = MemoryId::parse(&from)?;
            let to = MemoryId::parse(&to)?;
            let neighbor = if from == start { to } else { from };
            if !sources.iter().any(|record| record.source.id == neighbor) {
                let record = super::operations::source_record(&tx, &principal, neighbor)?;
                if bytes + record.source.text.as_str().len() > 262_144 {
                    more = true;
                    break;
                }
                bytes += record.source.text.as_str().len();
                sources.push(record);
            }
            links.push(MemoryLink {
                from: SourceRef {
                    id: from,
                    revision: revision(from_revision)?,
                },
                to: SourceRef {
                    id: to,
                    revision: revision(to_revision)?,
                },
                relation: MemoryText::new(relation)?,
            });
        }
        let next_cursor = if more {
            links
                .last()
                .map(|link| json(&(link.from.id, link.to.id, &link.relation)))
                .transpose()?
        } else {
            None
        };
        tx.commit()?;
        Ok(SourceGraph {
            sources,
            links,
            truncated: more,
            next_cursor,
        })
    }
    fn assistant_notices(&self) -> MemoryResult<Vec<AssistantNotice>> {
        let mut statement=self.connection.prepare("SELECT n.source_id,s.text,n.created_at,n.dismissed FROM assistant_notices n JOIN sources s ON s.principal=n.principal AND s.id=n.source_id WHERE n.principal=?1 AND s.suppressed=0 ORDER BY n.created_at DESC,n.source_id DESC LIMIT 32")?;
        statement
            .query_map([self.owner.principal().to_string()], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, bool>(3)?,
                ))
            })?
            .map(|row| {
                let (id, text, created_at, dismissed) = row?;
                Ok(AssistantNotice {
                    id: MemoryId::parse(&id)?,
                    text: MemoryText::new(text)?,
                    created_at,
                    dismissed,
                })
            })
            .collect()
    }
}
/// Connection direction and labels are host/model navigation, not inferred facts.
pub(super) fn connect(
    connection: &rusqlite::Connection,
    principal: &str,
    from: SourceRef,
    to: SourceRef,
    relation: &str,
) -> MemoryResult<()> {
    connection.execute("INSERT OR IGNORE INTO memory_links(principal,from_id,from_revision,to_id,to_revision,relation) VALUES(?1,?2,?3,?4,?5,?6)",params![principal,from.id.to_string(),from.revision.get(),to.id.to_string(),to.revision.get(),relation])?;
    Ok(())
}

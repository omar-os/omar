//! Durable budgets for host-owned research adapters. This does not meter the
//! native designer's own tools, tokens or subscription usage.
use super::*;
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    os::{fd::AsRawFd, unix::fs::PermissionsExt},
    path::{Path, PathBuf},
};

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum Category {
    Catalog,
    Source,
}

pub struct Data {
    pub value: Value,
    pub location: String,
    pub offset: usize,
}

#[derive(Debug, Serialize)]
pub struct Reply {
    pub data: Option<Value>,
    pub capture: Option<ResearchCaptureSummary>,
    pub error: Option<String>,
    pub replayed: bool,
    pub research: ResearchUsage,
}

#[derive(Serialize, Deserialize)]
enum EventStatus {
    Pending,
    Succeeded,
    Failed,
}

#[derive(Serialize, Deserialize)]
struct Event {
    fingerprint: String,
    category: Category,
    status: EventStatus,
    data: Option<Value>,
    capture: Option<ResearchCaptureSummary>,
    error: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Session {
    version: u32,
    conversation: String,
    id: String,
    operator_sequence: u64,
    message_digest: String,
    limits: ResearchLimits,
    started_at_ms: u64,
    last_seen_ms: u64,
    finished_at_ms: Option<u64>,
    stop: Option<ResearchStop>,
    events: BTreeMap<String, Event>,
}

impl Session {
    fn usage(&self) -> ResearchUsage {
        let captures: Vec<_> = self
            .events
            .values()
            .filter_map(|e| e.capture.clone())
            .collect();
        ResearchUsage {
            session_id: self.id.clone(),
            operator_sequence: self.operator_sequence,
            limits: self.limits.clone(),
            started_at_ms: self.started_at_ms,
            finished_at_ms: self.finished_at_ms,
            catalog_calls: self
                .events
                .values()
                .filter(|e| e.category == Category::Catalog)
                .count() as u32,
            source_calls: self
                .events
                .values()
                .filter(|e| e.category == Category::Source)
                .count() as u32,
            failed_calls: self
                .events
                .values()
                .filter(|e| matches!(e.status, EventStatus::Failed))
                .count() as u32,
            evidence_bytes: captures.iter().map(|c| c.evidence_bytes).sum(),
            captures,
            errors: self
                .events
                .iter()
                .filter_map(|(id, e)| e.error.as_ref().map(|message| format!("{id}: {message}")))
                .collect(),
            stop_reason: self.stop,
            adapter_cost_usd: 0.0,
            designer_cost_usd: None,
            external_retrieval_supported: false,
        }
    }
    fn close(&mut self, reason: ResearchStop, now: u64) {
        if self.stop.is_none() {
            self.stop = Some(reason);
            self.finished_at_ms = Some(now);
        }
    }
    fn observe_clock(&mut self, now: u64) {
        if now < self.last_seen_ms {
            self.close(ResearchStop::ClockChanged, now);
        }
        if now.saturating_sub(self.started_at_ms) >= self.limits.wall_time_ms {
            self.close(ResearchStop::TimeLimit, now);
        }
        self.last_seen_ms = self.last_seen_ms.max(now);
    }
    fn recover(&mut self, now: u64) {
        let mut interrupted = false;
        for event in self.events.values_mut() {
            if matches!(event.status, EventStatus::Pending) {
                event.status = EventStatus::Failed;
                event.error = Some(
                    "Lookup was interrupted after budget reservation; it was not repeated.".into(),
                );
                interrupted = true;
            }
        }
        if interrupted {
            self.close(ResearchStop::Interrupted, now);
        }
    }
}

pub struct Store {
    root: PathBuf,
    conversation: String,
}

struct ResearchLock(fs::File);
impl Drop for ResearchLock {
    fn drop(&mut self) {
        // Release explicitly: a concurrent fork may briefly inherit the open
        // file description until exec, even though the descriptor is CLOEXEC.
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

impl Store {
    pub fn new(ea_root: &Path, conversation: &str) -> Result<Self> {
        ensure!(
            super::store::identifier(conversation),
            "invalid research owner"
        );
        let root = ea_root.join("workflow-research").join(conversation);
        fs::create_dir_all(&root)?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        Ok(Self {
            root,
            conversation: conversation.into(),
        })
    }
    pub fn session_id(sequence: u64) -> String {
        format!("request-{sequence}")
    }
    fn path(&self, id: &str) -> Result<PathBuf> {
        ensure!(super::store::identifier(id), "invalid research session ID");
        Ok(self.root.join(format!("{id}.json")))
    }
    fn lock(&self, id: &str) -> Result<ResearchLock> {
        self.path(id)?;
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.root.join(format!("{id}.lock")))?;
        ensure!(
            unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
            "research conflict: another lookup is in progress"
        );
        Ok(ResearchLock(file))
    }
    fn load(&self, id: &str) -> Result<Session> {
        let session: Session = serde_json::from_slice(&fs::read(self.path(id)?)?)?;
        ensure!(
            session.version == 1 && session.conversation == self.conversation && session.id == id,
            "research ownership or version mismatch"
        );
        Ok(session)
    }
    fn save(&self, session: &Session) -> Result<()> {
        let path = self.path(&session.id)?;
        crate::topology::write_json_atomic(&path, session)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        fs::File::open(&path)?.sync_all()?;
        fs::File::open(&self.root)?.sync_all()?;
        Ok(())
    }
    /// One session per real operator message, including across restarts.
    /// A caller cannot acquire fresh limits by inventing another request ID.
    pub fn begin(
        &self,
        sequence: u64,
        message_digest: &str,
        limits: ResearchLimits,
        now: u64,
    ) -> Result<ResearchUsage> {
        ensure!(
            sequence > 0 && message_digest.len() == 64,
            "invalid operator research scope"
        );
        let ceiling = ResearchLimits::default();
        ensure!(
            limits.catalog_calls <= ceiling.catalog_calls
                && limits.source_calls <= ceiling.source_calls
                && limits.evidence_bytes <= ceiling.evidence_bytes
                && limits.per_source_bytes <= ceiling.per_source_bytes
                && limits.wall_time_ms <= ceiling.wall_time_ms
                && limits.adapter_spend_usd == 0.0,
            "unsupported research budget"
        );
        let id = Self::session_id(sequence);
        let _lock = self.lock(&id)?;
        let session = if self.path(&id)?.exists() {
            let session = self.load(&id)?;
            ensure!(
                session.operator_sequence == sequence && session.message_digest == message_digest,
                "research scope changed"
            );
            session
        } else {
            Session {
                version: 1,
                conversation: self.conversation.clone(),
                id,
                operator_sequence: sequence,
                message_digest: message_digest.into(),
                limits,
                started_at_ms: now,
                last_seen_ms: now,
                finished_at_ms: None,
                stop: None,
                events: BTreeMap::new(),
            }
        };
        self.save(&session)?;
        Ok(session.usage())
    }
    pub fn summary(&self, id: &str) -> Result<ResearchUsage> {
        Ok(self.load(id)?.usage())
    }
    pub fn exists(&self, id: &str) -> Result<bool> {
        Ok(self.path(id)?.exists())
    }
    pub fn finish(&self, id: &str, reason: ResearchStop, now: u64) -> Result<ResearchUsage> {
        let _lock = self.lock(id)?;
        let mut session = self.load(id)?;
        session.recover(now);
        session.observe_clock(now);
        session.close(reason, now);
        self.save(&session)?;
        Ok(session.usage())
    }
    pub fn capture(
        &self,
        reference: &ResearchCaptureRef,
    ) -> Result<(Value, ResearchCaptureSummary)> {
        let session = self.load(&reference.session_id)?;
        let event = session
            .events
            .get(&reference.request_id)
            .context("unknown research capture")?;
        ensure!(
            matches!(event.status, EventStatus::Succeeded),
            "research capture did not succeed"
        );
        let data = event
            .data
            .clone()
            .context("research capture data missing")?;
        let capture = event
            .capture
            .clone()
            .context("research capture receipt missing")?;
        let content = content(&data)?;
        ensure!(
            super::digest(content.as_bytes()) == capture.content_digest
                && content.len() == capture.evidence_bytes,
            "research capture digest mismatch"
        );
        Ok((data, capture))
    }
    pub fn lookup(
        &self,
        id: &str,
        request_id: &str,
        fingerprint: &str,
        category: Category,
        clock: impl Fn() -> u64,
        read: impl FnOnce() -> Result<Data>,
    ) -> Result<Reply> {
        ensure!(
            super::store::identifier(request_id),
            "invalid research request ID"
        );
        let _lock = self.lock(id)?;
        let mut session = self.load(id)?;
        let now = clock();
        session.recover(now);
        session.observe_clock(now);
        if let Some(event) = session.events.get(request_id) {
            ensure!(
                event.fingerprint == fingerprint && event.category == category,
                "research request ID reused for different content"
            );
            let reply = Reply {
                data: event.data.clone(),
                capture: event.capture.clone(),
                error: event.error.clone(),
                replayed: true,
                research: session.usage(),
            };
            self.save(&session)?;
            return Ok(reply);
        }
        let usage = session.usage();
        if (category == Category::Catalog && usage.catalog_calls >= session.limits.catalog_calls)
            || (category == Category::Source && usage.source_calls >= session.limits.source_calls)
        {
            session.close(ResearchStop::CallLimit, now);
        }
        if usage.evidence_bytes >= session.limits.evidence_bytes {
            session.close(ResearchStop::ContextLimit, now);
        }
        if session.stop.is_some() {
            self.save(&session)?;
            return Ok(Reply {
                data: None,
                capture: None,
                error: Some(
                    "Research budget is closed; continue with captured evidence and explicit gaps."
                        .into(),
                ),
                replayed: false,
                research: session.usage(),
            });
        }
        session.events.insert(
            request_id.into(),
            Event {
                fingerprint: fingerprint.into(),
                category,
                status: EventStatus::Pending,
                data: None,
                capture: None,
                error: None,
            },
        );
        self.save(&session)?; // Count before any adapter can run.
        let result = read();
        let finished = clock();
        session.observe_clock(finished);
        let result = if session.stop.is_some() {
            Err(anyhow::anyhow!(
                "Research deadline elapsed; late evidence was discarded."
            ))
        } else {
            result
        };
        let remaining = session.limits.evidence_bytes - usage.evidence_bytes;
        let result=result.and_then(|data| {
            let full=content(&data.value)?;
            let value=if category==Category::Source {
                ensure!(data.value.is_string(),"source adapter must return text");
                ensure!(data.offset<=full.len() && full.is_char_boundary(data.offset),"invalid source offset");
                let mut end=(data.offset.saturating_add(remaining.min(session.limits.per_source_bytes))).min(full.len());
                while !full.is_char_boundary(end) { end-=1; }
                if end==data.offset && data.offset<full.len() { session.close(ResearchStop::ContextLimit,finished);anyhow::bail!("No evidence budget remains for the next character"); }
                Value::String(full[data.offset..end].into())
            } else {
                if full.len()>remaining { session.close(ResearchStop::ContextLimit,finished);anyhow::bail!("Catalog exceeds the remaining evidence budget; no partial catalog was returned"); }
                data.value
            };
            let returned=content(&value)?;
            let capture=ResearchCaptureSummary {request_id:request_id.into(),location:data.location,captured_at_ms:finished,
                content_digest:super::digest(returned.as_bytes()),full_content_digest:super::digest(full.as_bytes()),
                evidence_bytes:returned.len(),full_content_bytes:full.len(),offset:data.offset,
                truncated:data.offset>0 || returned.len()<full.len()};
            Ok((value,capture))
        });
        let event = session.events.get_mut(request_id).unwrap();
        match result {
            Ok((value, capture)) => {
                event.status = EventStatus::Succeeded;
                event.data = Some(value);
                event.capture = Some(capture);
            }
            Err(error) => {
                event.status = EventStatus::Failed;
                event.error = Some(format!("{error:#}").chars().take(1000).collect());
            }
        }
        let reply = Reply {
            data: event.data.clone(),
            capture: event.capture.clone(),
            error: event.error.clone(),
            replayed: false,
            research: session.usage(),
        };
        self.save(&session)?;
        Ok(reply)
    }
}

pub fn content(value: &Value) -> Result<String> {
    Ok(match value.as_str() {
        Some(text) => text.into(),
        None => serde_json::to_string(value)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::Cell;

    fn setup(limits: ResearchLimits) -> (tempfile::TempDir, Store, String) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path(), "chat-one").unwrap();
        let usage = store.begin(1, &digest(b"operator"), limits, 100).unwrap();
        (dir, store, usage.session_id)
    }
    fn data(text: &str, offset: usize) -> Result<Data> {
        Ok(Data {
            value: json!(text),
            location: "test:source".into(),
            offset,
        })
    }

    #[test]
    fn replay_and_restart_keep_one_budget_and_original_capture() {
        let (dir, store, id) = setup(ResearchLimits::default());
        let first = store
            .lookup(
                &id,
                "one",
                "same",
                Category::Source,
                || 101,
                || data("first", 0),
            )
            .unwrap();
        let restarted = Store::new(dir.path(), "chat-one").unwrap();
        restarted
            .begin(
                1,
                &digest(b"operator"),
                ResearchLimits {
                    source_calls: 1,
                    ..ResearchLimits::default()
                },
                102,
            )
            .unwrap();
        let replay = restarted
            .lookup(
                &id,
                "one",
                "same",
                Category::Source,
                || 103,
                || panic!("must not dispatch"),
            )
            .unwrap();
        assert!(replay.replayed);
        assert_eq!(replay.data, first.data);
        assert_eq!(replay.research.source_calls, 1);
        assert_eq!(replay.research.limits.source_calls, 3);
        assert!(restarted
            .lookup(
                &id,
                "one",
                "different",
                Category::Source,
                || 104,
                || data("bad", 0)
            )
            .is_err());
        assert!(restarted
            .begin(1, &digest(b"changed"), ResearchLimits::default(), 104)
            .is_err());
        assert_eq!(replay.research.adapter_cost_usd, 0.0);
        assert_eq!(replay.research.designer_cost_usd, None);
    }

    #[test]
    fn failed_reads_count_and_call_limit_prevents_dispatch() {
        let (_dir, store, id) = setup(ResearchLimits {
            source_calls: 1,
            ..ResearchLimits::default()
        });
        let first = store
            .lookup(
                &id,
                "missing",
                "one",
                Category::Source,
                || 101,
                || anyhow::bail!("missing input"),
            )
            .unwrap();
        assert_eq!(first.research.failed_calls, 1);
        assert_eq!(first.research.source_calls, 1);
        let exhausted = store
            .lookup(
                &id,
                "second",
                "two",
                Category::Source,
                || 102,
                || panic!("budget must gate dispatch"),
            )
            .unwrap();
        assert_eq!(
            exhausted.research.stop_reason,
            Some(ResearchStop::CallLimit)
        );
        assert!(exhausted.data.is_none());
        assert_eq!(exhausted.research.source_calls, 1);
    }

    #[test]
    fn context_limit_clips_at_utf8_boundaries_and_records_full_digest() {
        let (_dir, store, id) = setup(ResearchLimits {
            evidence_bytes: 5,
            per_source_bytes: 4,
            ..ResearchLimits::default()
        });
        let first = store
            .lookup(
                &id,
                "first",
                "a",
                Category::Source,
                || 101,
                || data("a🙂z", 0),
            )
            .unwrap();
        assert_eq!(first.data, Some(json!("a")));
        let capture = first.capture.unwrap();
        assert!(capture.truncated);
        assert_eq!(capture.full_content_bytes, 6);
        assert_eq!(capture.full_content_digest, digest("a🙂z".as_bytes()));
        let second = store
            .lookup(
                &id,
                "next",
                "b",
                Category::Source,
                || 102,
                || data("a🙂z", 1),
            )
            .unwrap();
        assert_eq!(second.data, Some(json!("🙂")));
        assert_eq!(second.research.evidence_bytes, 5);
        let blocked = store
            .lookup(
                &id,
                "last",
                "c",
                Category::Source,
                || 103,
                || panic!("context exhausted"),
            )
            .unwrap();
        assert_eq!(
            blocked.research.stop_reason,
            Some(ResearchStop::ContextLimit)
        );
    }

    #[test]
    fn catalogs_are_complete_or_rejected_and_invalid_offsets_are_counted() {
        let (_dir, store, id) = setup(ResearchLimits {
            evidence_bytes: 4,
            ..ResearchLimits::default()
        });
        let reply = store
            .lookup(
                &id,
                "catalog",
                "a",
                Category::Catalog,
                || 101,
                || {
                    Ok(Data {
                        value: json!({"long":"catalog"}),
                        location: "test".into(),
                        offset: 0,
                    })
                },
            )
            .unwrap();
        assert_eq!(reply.research.stop_reason, Some(ResearchStop::ContextLimit));
        assert_eq!(reply.research.evidence_bytes, 0);
        assert!(reply.data.is_none());
        let (_dir, store, id) = setup(ResearchLimits::default());
        let bad = store
            .lookup(
                &id,
                "invalid",
                "b",
                Category::Source,
                || 101,
                || data("🙂", 1),
            )
            .unwrap();
        assert_eq!(bad.research.failed_calls, 1);
        assert!(bad.error.unwrap().contains("offset"));
    }

    #[test]
    fn deadlines_and_clock_rollback_discard_late_evidence() {
        for (before, after, reason) in [
            (110, 110, ResearchStop::TimeLimit),
            (101, 110, ResearchStop::TimeLimit),
            (101, 99, ResearchStop::ClockChanged),
        ] {
            let (_dir, store, id) = setup(ResearchLimits {
                wall_time_ms: 10,
                ..ResearchLimits::default()
            });
            let calls = Cell::new(0);
            let clock = Cell::new(before);
            let reply = store
                .lookup(
                    &id,
                    "read",
                    "same",
                    Category::Source,
                    || clock.get(),
                    || {
                        calls.set(calls.get() + 1);
                        clock.set(after);
                        data("late", 0)
                    },
                )
                .unwrap();
            assert_eq!(reply.research.stop_reason, Some(reason));
            assert!(reply.data.is_none());
            assert_eq!(calls.get(), if before == 110 { 0 } else { 1 });
        }
    }

    #[test]
    fn interrupted_reservation_closes_budget_and_competing_lookup_cannot_read() {
        let (_dir, store, id) = setup(ResearchLimits::default());
        let lock = store.lock(&id).unwrap();
        assert!(store
            .lookup(
                &id,
                "r",
                "a",
                Category::Source,
                || 101,
                || panic!("conflicting read")
            )
            .unwrap_err()
            .to_string()
            .contains("conflict"));
        let mut pending = store.load(&id).unwrap();
        pending.events.insert(
            "r".into(),
            Event {
                fingerprint: "a".into(),
                category: Category::Source,
                status: EventStatus::Pending,
                data: None,
                capture: None,
                error: None,
            },
        );
        store.save(&pending).unwrap();
        drop(lock);
        let reply = store
            .lookup(
                &id,
                "r",
                "a",
                Category::Source,
                || 102,
                || panic!("interrupted read must not retry"),
            )
            .unwrap();
        assert!(reply.replayed);
        assert_eq!(reply.research.failed_calls, 1);
        assert_eq!(reply.research.stop_reason, Some(ResearchStop::Interrupted));
    }

    #[test]
    fn captures_are_owned_integrity_checked_and_closed_summaries_are_stable() {
        let (dir, store, id) = setup(ResearchLimits::default());
        store
            .lookup(
                &id,
                "read",
                "a",
                Category::Source,
                || 101,
                || data("evidence", 0),
            )
            .unwrap();
        let reference = ResearchCaptureRef {
            session_id: id.clone(),
            request_id: "read".into(),
        };
        assert!(store.capture(&reference).is_ok());
        let other = Store::new(dir.path(), "chat-two").unwrap();
        assert!(other.capture(&reference).is_err());
        let finished = store
            .finish(&id, ResearchStop::SufficientEvidence, 102)
            .unwrap();
        assert_eq!(
            store.finish(&id, ResearchStop::Superseded, 999999).unwrap(),
            finished
        );
        let mut tampered = store.load(&id).unwrap();
        tampered.events.get_mut("read").unwrap().data = Some(json!("forged"));
        store.save(&tampered).unwrap();
        assert!(store.capture(&reference).is_err());
        assert!(store.summary("../escape").is_err());
    }
}

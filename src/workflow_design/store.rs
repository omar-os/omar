use super::WorkflowPlanRevision;
use anyhow::{ensure, Context, Result};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

pub fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub struct Store {
    root: PathBuf,
    conversation: String,
}

impl Store {
    pub fn new(ea_root: &Path, conversation: &str) -> Result<Self> {
        ensure!(identifier(conversation), "invalid conversation ID");
        Ok(Self {
            root: ea_root.join("workflow-plans").join(conversation),
            conversation: conversation.into(),
        })
    }

    fn directory(&self, id: &str) -> Result<PathBuf> {
        ensure!(identifier(id), "invalid plan ID");
        Ok(self.root.join(id))
    }

    pub fn latest(&self, id: &str) -> Result<Option<WorkflowPlanRevision>> {
        let dir = self.directory(id)?;
        if !dir.exists() {
            return Ok(None);
        }
        let revision = fs::read_dir(&dir)?
            .filter_map(|entry| {
                let name = entry.ok()?.file_name().to_str()?.to_string();
                name.strip_suffix(".json")?.parse::<u32>().ok()
            })
            .max();
        revision.map(|rev| self.read(id, rev)).transpose()
    }

    pub fn read(&self, id: &str, revision: u32) -> Result<WorkflowPlanRevision> {
        let plan: WorkflowPlanRevision = serde_json::from_slice(&fs::read(
            self.directory(id)?.join(format!("{revision}.json")),
        )?)?;
        ensure!(
            plan.schema_version == 1
                && plan.id == id
                && plan.revision == revision
                && plan.conversation_id == self.conversation,
            "plan ownership or schema mismatch"
        );
        ensure!(
            plan.source_digest == super::digest(plan.program.as_bytes()),
            "plan source digest mismatch"
        );
        Ok(plan)
    }

    /// A lock serializes writers, while readers only see complete immutable
    /// files. The directory is the index: a crash cannot leave a stale HEAD.
    pub fn save(
        &self,
        mut plan: WorkflowPlanRevision,
        expected: u32,
    ) -> Result<WorkflowPlanRevision> {
        ensure!(
            plan.conversation_id == self.conversation && identifier(&plan.request_id),
            "invalid plan ownership/request ID"
        );
        let dir = self.directory(&plan.id)?;
        fs::create_dir_all(&dir)?;
        let lock_path = dir.join("write.lock");
        let mut lock = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;
        // Advisory locks release on process death, unlike create_new lock files.
        use std::os::fd::AsRawFd;
        ensure!(
            unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
            "revision conflict: another writer is saving this plan"
        );
        writeln!(lock, "{}", std::process::id())?;
        let latest = self.latest(&plan.id)?;
        for revision in 1..=latest.as_ref().map_or(0, |p| p.revision) {
            let existing = self.read(&plan.id, revision)?;
            if existing.request_id == plan.request_id {
                ensure!(
                    existing.program == plan.program
                        && existing.inputs == plan.inputs
                        && existing.draft == plan.draft
                        && existing.profiles == plan.profiles,
                    "idempotency key reused with different content"
                );
                return Ok(existing);
            }
        }
        let current = latest.as_ref().map_or(0, |p| p.revision);
        ensure!(
            current == expected,
            "revision conflict: expected {expected}, current {current}"
        );
        plan.revision = current.checked_add(1).context("revision overflow")?;
        plan.schema_version = 1;
        plan.source_digest = super::digest(plan.program.as_bytes());
        plan.created_at = super::now();
        crate::topology::write_json_atomic(&dir.join(format!("{}.json", plan.revision)), &plan)?;
        fs::File::open(dir.join(format!("{}.json", plan.revision)))?.sync_all()?;
        fs::File::open(&dir)?.sync_all()?;
        Ok(plan)
    }
}

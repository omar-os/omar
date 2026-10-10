//! Durable runtime checkpoints: what a run needs to continue from a completed
//! tag in a fresh process, and the store that publishes them atomically.
//!
//! A checkpoint is the execution state at a tag boundary plus references to
//! the file version every team instance had at that moment. File versions
//! stay in each workspace's Git history; a checkpoint only names them.
//! Publication stages everything, validates it, fsyncs, and renames into
//! place. A crash leaves the previous complete checkpoint or the new one
//! discoverable, never a partial directory advertised as safe.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const FORMAT: u32 = 1;
pub const DEFAULT_PERIOD: Duration = Duration::from_secs(60 * 60);

/// One queued tag: its moment and everything present at it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedTag {
    pub timestamp: u64,
    pub microstep: u64,
    pub events: BTreeMap<String, Value>,
}

/// Everything the event loop keeps between tags, in a shape independent of
/// its local variables. Timers need no entry of their own: a timer's next
/// firing is a queued tag, and a one-shot timer already consumed has none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionState {
    pub version: u32,
    /// The last tag that ran to completion. `None` before the first.
    pub completed_tag: Option<(u64, u64)>,
    /// Every future tag in order, payloads included. Nothing is in flight at
    /// a boundary, so this is the whole of the run's remaining work.
    pub queue: Vec<QueuedTag>,
    pub outputs: BTreeMap<String, Value>,
    pub state_vars: BTreeMap<String, Value>,
    /// Logical time elapsed on the run's clock, which excludes time spent
    /// paused: on resume the clock continues from here, so a tag due later
    /// waits exactly the remainder, never a catch-up burst.
    pub elapsed_ns: u64,
}

impl ExecutionState {
    pub fn next_tag(&self) -> Option<(u64, u64)> {
        self.queue.first().map(|tag| (tag.timestamp, tag.microstep))
    }
}

/// Why a capture happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    Automatic,
    Manual,
    Pause,
}

/// How often a run checkpoints on its own: physical time, so a run paced by
/// its logical clock and one run `--fast` both capture hourly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    pub period_secs: u64,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            period_secs: DEFAULT_PERIOD.as_secs(),
        }
    }
}

impl Policy {
    pub fn period(self) -> Duration {
        Duration::from_secs(self.period_secs)
    }
}

/// Parse `30m`, `1h`, `90s`, `1h30m`. Zero, missing units, and overflow are
/// refused before anything is launched or changed.
pub fn parse_period(text: &str) -> Result<Duration> {
    let text = text.trim();
    anyhow::ensure!(!text.is_empty(), "checkpoint period is empty");
    let mut total: u64 = 0;
    let mut digits = String::new();
    for c in text.chars() {
        if c.is_ascii_digit() {
            digits.push(c);
            continue;
        }
        let unit: u64 = match c {
            's' => 1,
            'm' => 60,
            'h' => 3600,
            'd' => 86_400,
            other => bail!("checkpoint period '{text}': unknown unit '{other}' (use s, m, h, d)"),
        };
        anyhow::ensure!(
            !digits.is_empty(),
            "checkpoint period '{text}': unit '{c}' has no number"
        );
        let count: u64 = digits
            .parse()
            .with_context(|| format!("checkpoint period '{text}' overflows"))?;
        digits.clear();
        total = count
            .checked_mul(unit)
            .and_then(|part| total.checked_add(part))
            .with_context(|| format!("checkpoint period '{text}' overflows"))?;
    }
    anyhow::ensure!(
        digits.is_empty(),
        "checkpoint period '{text}' needs a unit (s, m, h, d)"
    );
    anyhow::ensure!(total > 0, "checkpoint period '{text}' must be positive");
    Ok(Duration::from_secs(total))
}

/// The file version one instance had when the checkpoint was taken.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub workspace_id: String,
    pub snapshot_id: String,
    pub commit: String,
}

/// What can be restored for an agent, stated rather than assumed. No backend
/// offers an immutable fork of a conversation at a recorded turn yet, so a
/// resumed agent starts a fresh conversation and is told it continues a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentContext {
    pub backend: String,
    pub restoration: String,
}

pub const FRESH_CONVERSATION: &str = "fresh_conversation";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub id: String,
    pub sequence: u64,
    /// The checkpoint this one continues from; `None` for a run's first.
    pub parent: Option<String>,
    pub deployment_id: String,
    pub team: String,
    pub ea_id: u32,
    pub created_at: u64,
    pub completed_tag: Option<(u64, u64)>,
    pub next_tag: Option<(u64, u64)>,
    pub trigger: Trigger,
    pub policy: Policy,
    pub pace: String,
    pub program_sha256: String,
    pub program_path: Option<String>,
    pub state_sha256: String,
    pub elapsed_ns: u64,
    pub workspaces: BTreeMap<String, ArtifactRef>,
    pub agents: BTreeMap<String, AgentContext>,
}

/// Where a run's checkpoints live, beneath its deployment directory and so
/// outside every agent-accessible mount.
pub struct Store {
    dir: PathBuf,
}

const LATEST: &str = "latest";
const HEAD: &str = "head.json";
const STAGING_PREFIX: &str = ".staging-";

/// Which checkpoint a resume continues from. Normally the latest; a rollback
/// moves it to an older one and records what it left behind.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Head {
    pub checkpoint_id: String,
    /// The checkpoint that was head before a rollback, kept so the abandoned
    /// branch stays discoverable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abandoned: Option<String>,
    pub updated_at: u64,
    /// The highest sequence that existed when this head was written. A
    /// complete checkpoint above it whose parent is the head was published
    /// after it: its own pointer never landed, and it is the resume point.
    #[serde(default)]
    pub sequence: u64,
}

pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn valid_id(id: &str) -> Result<()> {
    anyhow::ensure!(
        Uuid::parse_str(id)
            .map(|u| u.to_string() == id)
            .unwrap_or(false),
        "invalid checkpoint id '{id}'"
    );
    Ok(())
}

fn fsync_dir(path: &Path) -> Result<()> {
    fs::File::open(path)?.sync_all()?;
    Ok(())
}

fn write_synced(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = fs::File::create(path)?;
    use std::io::Write;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

/// Replace a pointer file durably: synced temporary, rename, synced
/// directory. A crash leaves the old pointer or the new one, never a torn
/// file that a loader cannot read.
fn write_pointer(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().context("pointer has no directory")?;
    let staged = dir.join(format!(
        ".{}.{}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        Uuid::new_v4().simple()
    ));
    write_synced(&staged, bytes)?;
    fs::rename(&staged, path)?;
    fsync_dir(dir)
}

impl Store {
    pub fn new(deployment_dir: &Path) -> Self {
        Self {
            dir: deployment_dir.join("checkpoints"),
        }
    }

    #[cfg(test)]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn entry_dir(&self, manifest: &Manifest) -> PathBuf {
        self.dir
            .join(format!("{:06}-{}", manifest.sequence, manifest.id))
    }

    /// Stage, validate, fsync, and rename into place; then move `latest`.
    /// Returns the published directory.
    pub fn publish(
        &self,
        manifest: &Manifest,
        state: &ExecutionState,
        program: &Value,
    ) -> Result<PathBuf> {
        valid_id(&manifest.id)?;
        anyhow::ensure!(manifest.version == FORMAT, "unsupported checkpoint format");
        let state_bytes = serde_json::to_vec_pretty(state)?;
        let program_bytes = serde_json::to_vec(program)?;
        anyhow::ensure!(
            sha256(&state_bytes) == manifest.state_sha256,
            "execution state does not match its manifest hash"
        );
        anyhow::ensure!(
            sha256(&program_bytes) == manifest.program_sha256,
            "program does not match its manifest hash"
        );
        fs::create_dir_all(&self.dir)?;
        let staging = self.dir.join(format!("{STAGING_PREFIX}{}", manifest.id));
        let _ = fs::remove_dir_all(&staging);
        fs::create_dir(&staging)?;
        let result = (|| -> Result<PathBuf> {
            write_synced(&staging.join("state.json"), &state_bytes)?;
            write_synced(&staging.join("program.json"), &program_bytes)?;
            write_synced(
                &staging.join("manifest.json"),
                &serde_json::to_vec_pretty(manifest)?,
            )?;
            fsync_dir(&staging)?;
            // Reading back what was staged is the validation that matters:
            // a loader must accept exactly these bytes later.
            Self::load_dir(&staging)?;
            let published = self.entry_dir(manifest);
            anyhow::ensure!(
                !published.exists(),
                "checkpoint {} already published",
                manifest.id
            );
            fs::rename(&staging, &published)?;
            fsync_dir(&self.dir)?;
            write_pointer(&self.dir.join(LATEST), manifest.id.as_bytes())?;
            // A new checkpoint is the run's head: a resume continues from
            // it, until a rollback moves the head to an older one.
            write_pointer(
                &self.dir.join(HEAD),
                &serde_json::to_vec(&Head {
                    checkpoint_id: manifest.id.clone(),
                    abandoned: None,
                    updated_at: crate::deploy::now_unix(),
                    sequence: manifest.sequence,
                })?,
            )?;
            Ok(published)
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(&staging);
        }
        result
    }

    fn load_dir(dir: &Path) -> Result<(Manifest, ExecutionState, Value)> {
        let manifest_bytes = fs::read(dir.join("manifest.json"))
            .with_context(|| format!("read {}", dir.join("manifest.json").display()))?;
        anyhow::ensure!(
            manifest_bytes.len() <= 4 << 20,
            "checkpoint manifest too large"
        );
        let manifest: Manifest = serde_json::from_slice(&manifest_bytes)?;
        anyhow::ensure!(
            manifest.version == FORMAT,
            "checkpoint {} uses format {}, this runtime reads {FORMAT}",
            manifest.id,
            manifest.version
        );
        valid_id(&manifest.id)?;
        let state_bytes = fs::read(dir.join("state.json"))?;
        anyhow::ensure!(state_bytes.len() <= 256 << 20, "checkpoint state too large");
        anyhow::ensure!(
            sha256(&state_bytes) == manifest.state_sha256,
            "checkpoint {} state hash mismatch",
            manifest.id
        );
        let state: ExecutionState = serde_json::from_slice(&state_bytes)?;
        anyhow::ensure!(
            state.version == FORMAT,
            "unsupported execution state format"
        );
        anyhow::ensure!(
            state.elapsed_ns == manifest.elapsed_ns
                && state.completed_tag == manifest.completed_tag
                && state.next_tag() == manifest.next_tag,
            "checkpoint {} state disagrees with its manifest",
            manifest.id
        );
        let program_bytes = fs::read(dir.join("program.json"))?;
        anyhow::ensure!(
            sha256(&program_bytes) == manifest.program_sha256,
            "checkpoint {} program hash mismatch",
            manifest.id
        );
        let program: Value = serde_json::from_slice(&program_bytes)?;
        for (instance, artifact) in &manifest.workspaces {
            anyhow::ensure!(
                artifact.commit.len() == 40
                    && artifact.commit.bytes().all(|b| b.is_ascii_hexdigit()),
                "checkpoint {} names an invalid commit for instance '{instance}'",
                manifest.id
            );
        }
        Ok((manifest, state, program))
    }

    /// Every complete checkpoint, oldest first. Staging directories and
    /// anything a loader refuses are left out, with the reason reported.
    pub fn list(&self) -> Result<(Vec<Manifest>, Vec<String>)> {
        let mut manifests = Vec::new();
        let mut problems = Vec::new();
        if !self.dir.exists() {
            return Ok((manifests, problems));
        }
        for entry in fs::read_dir(&self.dir)? {
            let path = entry?.path();
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if !path.is_dir() || name.starts_with(STAGING_PREFIX) {
                continue;
            }
            match Self::load_dir(&path) {
                Ok((manifest, _, _)) => manifests.push(manifest),
                Err(error) => problems.push(format!("{name}: {error:#}")),
            }
        }
        manifests.sort_by_key(|m| m.sequence);
        Ok((manifests, problems))
    }

    pub fn find(&self, id: &str) -> Result<PathBuf> {
        valid_id(id)?;
        for entry in fs::read_dir(&self.dir)
            .with_context(|| format!("no checkpoints under {}", self.dir.display()))?
        {
            let path = entry?.path();
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            if path.is_dir() && !name.starts_with(STAGING_PREFIX) && name.ends_with(id) {
                return Ok(path);
            }
        }
        bail!("no checkpoint '{id}'")
    }

    pub fn load(&self, id: &str) -> Result<(Manifest, ExecutionState, Value)> {
        let dir = self.find(id)?;
        let loaded = Self::load_dir(&dir)?;
        anyhow::ensure!(
            loaded.0.id == id,
            "checkpoint directory {} holds {}",
            dir.display(),
            loaded.0.id
        );
        Ok(loaded)
    }

    /// The most recent complete checkpoint. `latest` is a hint; a complete
    /// checkpoint whose pointer never landed is still found by sequence.
    pub fn latest(&self) -> Result<Option<Manifest>> {
        let (manifests, _) = self.list()?;
        if let Ok(id) = fs::read_to_string(self.dir.join(LATEST)) {
            let id = id.trim();
            if let Some(found) = manifests.iter().find(|m| m.id == id) {
                if manifests.iter().all(|m| m.sequence <= found.sequence) {
                    return Ok(Some(found.clone()));
                }
            }
        }
        Ok(manifests.last().cloned())
    }

    pub fn head(&self) -> Result<Option<Head>> {
        let path = self.dir.join(HEAD);
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(serde_json::from_slice(&fs::read(path)?)?))
    }

    /// Which checkpoint a resume continues from: the head a rollback chose,
    /// else the latest.
    pub fn resume_point(&self) -> Result<Option<Manifest>> {
        let Some(head) = self.head()? else {
            return self.latest();
        };
        let (current, _, _) = self.load(&head.checkpoint_id)?;
        // Publication renames the checkpoint before it moves the head, so a
        // crash between the two leaves a complete child of the head above
        // everything the head knew of. A rollback's abandoned children sit
        // at or below that mark, so they are not mistaken for one.
        let (manifests, _) = self.list()?;
        Ok(Some(
            manifests
                .into_iter()
                .filter(|m| m.parent.as_deref() == Some(current.id.as_str()))
                .filter(|m| m.sequence > head.sequence)
                .max_by_key(|m| m.sequence)
                .unwrap_or(current),
        ))
    }

    /// Set an earlier run's checkpoints aside before a fresh run of the team
    /// starts its own lineage, so nothing from one run can be rolled back
    /// into another. Nothing is deleted; the directory keeps its contents
    /// under a dated name beside the live one.
    pub fn archive(&self) -> Result<Option<PathBuf>> {
        if !self.dir.exists() || fs::read_dir(&self.dir)?.next().is_none() {
            return Ok(None);
        }
        let stamp = crate::deploy::now_unix();
        let mut target = self.dir.with_file_name(format!("checkpoints-{stamp}"));
        let mut n = 1;
        while target.exists() {
            target = self.dir.with_file_name(format!("checkpoints-{stamp}-{n}"));
            n += 1;
        }
        fs::rename(&self.dir, &target)?;
        Ok(Some(target))
    }

    /// Roll the head back to `id`. The current head is recorded as abandoned,
    /// its checkpoints untouched, so a later resume branches from `id`.
    pub fn set_head(&self, id: &str) -> Result<Head> {
        let (manifest, _, _) = self.load(id)?;
        let previous = self
            .resume_point()?
            .filter(|current| current.id != manifest.id)
            .map(|current| current.id);
        let head = Head {
            checkpoint_id: manifest.id,
            abandoned: previous,
            updated_at: crate::deploy::now_unix(),
            sequence: self.list()?.0.iter().map(|m| m.sequence).max().unwrap_or(0),
        };
        write_pointer(&self.dir.join(HEAD), &serde_json::to_vec(&head)?)?;
        Ok(head)
    }

    #[cfg(test)]
    pub fn clear_head(&self) -> Result<()> {
        let path = self.dir.join(HEAD);
        if path.exists() {
            fs::remove_file(path)?;
        }
        Ok(())
    }

    /// Check everything a resume would need, without launching anything.
    pub fn verify(&self, omar_dir: &Path, id: &str) -> Result<Manifest> {
        let (manifest, state, program) = self.load(id)?;
        let bytecode: crate::topology::Bytecode = serde_json::from_value(program)?;
        let verified = crate::topology::verify(&bytecode)?;
        anyhow::ensure!(
            verified.team == manifest.team,
            "checkpoint {} belongs to team {}, program says {}",
            id,
            manifest.team,
            verified.team
        );
        for name in verified.state_vars.keys() {
            anyhow::ensure!(
                state.state_vars.contains_key(name),
                "checkpoint {id} has no value for state variable '{name}'"
            );
        }
        for (instance, artifact) in &manifest.workspaces {
            let workspace = crate::workspace::Workspace::load(omar_dir, &artifact.workspace_id)
                .with_context(|| format!("instance '{instance}' workspace"))?;
            let snapshot = workspace
                .snapshots(omar_dir)?
                .into_iter()
                .find(|s| s.id == artifact.snapshot_id)
                .with_context(|| {
                    format!(
                        "instance '{instance}' snapshot {} is missing from workspace {}",
                        artifact.snapshot_id, artifact.workspace_id
                    )
                })?;
            anyhow::ensure!(
                snapshot.commit == artifact.commit,
                "instance '{instance}' snapshot {} commit changed",
                artifact.snapshot_id
            );
        }
        Ok(manifest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn state(elapsed: u64) -> ExecutionState {
        ExecutionState {
            version: FORMAT,
            completed_tag: Some((5, 0)),
            queue: vec![QueuedTag {
                timestamp: 9,
                microstep: 1,
                events: BTreeMap::from([("t".to_string(), json!(9))]),
            }],
            outputs: BTreeMap::from([("out".to_string(), json!("v"))]),
            state_vars: BTreeMap::from([("count".to_string(), json!(2))]),
            elapsed_ns: elapsed,
        }
    }

    fn manifest(sequence: u64, state: &ExecutionState, program: &Value) -> Manifest {
        Manifest {
            version: FORMAT,
            id: Uuid::new_v4().to_string(),
            sequence,
            parent: None,
            deployment_id: "d".into(),
            team: "T".into(),
            ea_id: 0,
            created_at: 1,
            completed_tag: state.completed_tag,
            next_tag: state.next_tag(),
            trigger: Trigger::Manual,
            policy: Policy::default(),
            pace: "fast".into(),
            program_sha256: sha256(&serde_json::to_vec(program).unwrap()),
            program_path: None,
            state_sha256: sha256(&serde_json::to_vec_pretty(state).unwrap()),
            elapsed_ns: state.elapsed_ns,
            workspaces: BTreeMap::new(),
            agents: BTreeMap::new(),
        }
    }

    #[test]
    fn periods_parse_with_units_and_refuse_nonsense() {
        assert_eq!(parse_period("1h").unwrap(), Duration::from_secs(3600));
        assert_eq!(parse_period("30m").unwrap(), Duration::from_secs(1800));
        assert_eq!(parse_period("1h30m").unwrap(), Duration::from_secs(5400));
        assert_eq!(parse_period(" 90s ").unwrap(), Duration::from_secs(90));
        for bad in ["", "0m", "10", "m", "5x", "99999999999999999999h", "-1h"] {
            assert!(parse_period(bad).is_err(), "{bad:?} should be refused");
        }
        assert_eq!(Policy::default().period(), DEFAULT_PERIOD);
    }

    #[test]
    fn publication_is_atomic_and_latest_follows_sequence() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let program = json!({"version": 1});
        assert!(store.latest().unwrap().is_none());
        let first = manifest(1, &state(10), &program);
        store.publish(&first, &state(10), &program).unwrap();
        let second = manifest(2, &state(20), &program);
        store.publish(&second, &state(20), &program).unwrap();
        assert_eq!(store.latest().unwrap().unwrap().id, second.id);
        let (loaded, loaded_state, _) = store.load(&first.id).unwrap();
        assert_eq!(loaded, first);
        assert_eq!(loaded_state, state(10));

        // A staging directory a crash left behind is never a checkpoint.
        let staging = store.dir().join(format!("{STAGING_PREFIX}abandoned"));
        fs::create_dir(&staging).unwrap();
        fs::write(staging.join("manifest.json"), b"{").unwrap();
        // A complete checkpoint whose `latest` pointer never landed still wins.
        let third = manifest(3, &state(30), &program);
        store.publish(&third, &state(30), &program).unwrap();
        fs::write(store.dir().join(LATEST), second.id.as_bytes()).unwrap();
        assert_eq!(store.latest().unwrap().unwrap().id, third.id);
        let (listed, problems) = store.list().unwrap();
        assert_eq!(listed.len(), 3);
        assert!(problems.is_empty(), "{problems:?}");

        // A hash mismatch or wrong format is refused, and the rest survive.
        let mut forged = manifest(4, &state(40), &program);
        forged.state_sha256 = sha256(b"other");
        assert!(store.publish(&forged, &state(40), &program).is_err());
        let tampered = store.find(&third.id).unwrap().join("state.json");
        fs::write(&tampered, serde_json::to_vec_pretty(&state(31)).unwrap()).unwrap();
        let (listed, problems) = store.list().unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("hash mismatch"), "{problems:?}");
        assert_eq!(store.latest().unwrap().unwrap().id, second.id);
        assert!(!store
            .dir()
            .join(format!("{STAGING_PREFIX}{}", forged.id))
            .exists());
    }

    #[test]
    fn rollback_moves_the_head_and_keeps_the_abandoned_branch() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let program = json!({"version": 1});
        let first = manifest(1, &state(1), &program);
        let second = manifest(2, &state(2), &program);
        store.publish(&first, &state(1), &program).unwrap();
        store.publish(&second, &state(2), &program).unwrap();
        assert_eq!(store.resume_point().unwrap().unwrap().id, second.id);
        let head = store.set_head(&first.id).unwrap();
        assert_eq!(head.abandoned.as_deref(), Some(second.id.as_str()));
        assert_eq!(store.resume_point().unwrap().unwrap().id, first.id);
        assert_eq!(store.list().unwrap().0.len(), 2, "rollback deletes nothing");
        assert!(store.set_head("not-an-id").is_err());
        store.clear_head().unwrap();
        assert_eq!(store.resume_point().unwrap().unwrap().id, second.id);
    }

    /// A checkpoint whose head pointer never landed is still the resume
    /// point, while a rollback's abandoned children are not.
    #[test]
    fn a_published_child_of_the_head_outranks_a_stale_head_pointer() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let program = json!({"version": 1});
        let first = manifest(1, &state(1), &program);
        let mut second = manifest(2, &state(2), &program);
        second.parent = Some(first.id.clone());
        store.publish(&first, &state(1), &program).unwrap();
        store.publish(&second, &state(2), &program).unwrap();
        // The crash: the second checkpoint is complete, the head still names the first.
        let stale = Head {
            checkpoint_id: first.id.clone(),
            abandoned: None,
            updated_at: 0,
            sequence: 1,
        };
        write_pointer(
            &dir.path().join("checkpoints").join(HEAD),
            &serde_json::to_vec(&stale).unwrap(),
        )
        .unwrap();
        assert_eq!(store.resume_point().unwrap().unwrap().id, second.id);
        // A rollback to the first leaves the second behind for good.
        store.set_head(&first.id).unwrap();
        assert_eq!(store.resume_point().unwrap().unwrap().id, first.id);
    }

    #[test]
    fn archiving_sets_a_lineage_aside_without_deleting_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        assert!(store.archive().unwrap().is_none(), "nothing to archive");
        let program = json!({"version": 1});
        store
            .publish(&manifest(1, &state(1), &program), &state(1), &program)
            .unwrap();
        let archived = store.archive().unwrap().unwrap();
        assert!(
            archived.join("manifest.json").exists()
                || std::fs::read_dir(&archived).unwrap().count() > 0
        );
        assert!(store.list().unwrap().0.is_empty() && store.resume_point().unwrap().is_none());
        assert!(Store::new(dir.path()).archive().unwrap().is_none());
    }
}

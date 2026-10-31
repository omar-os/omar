//! Cross-process EA admission and restartable daemon-owned deletion.
use std::fs::{self, File};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::ea::EaId;

fn directory(root: &Path) -> PathBuf {
    root.join("ea-lifecycle")
}
fn marker(root: &Path, id: EaId) -> PathBuf {
    directory(root).join(format!("{id}.json"))
}

pub(crate) fn fenced(root: &Path, id: EaId) -> bool {
    marker(root, id).exists()
}

/// Shared lock held while a request runs; deletion takes it exclusively.
/// Dropping the file releases it.
pub(crate) struct Admission {
    _lock: File,
}
impl Admission {
    pub(crate) fn acquire(root: &Path, id: EaId) -> Result<Self> {
        anyhow::ensure!(
            !fenced(root, id),
            "EA {id} is deleting or deleted; inspect the session runtime log"
        );
        let lock = lock(root, id)?;
        anyhow::ensure!(
            try_flock(&lock, libc::LOCK_SH),
            "EA {id} lifecycle operation in progress; retry after it completes"
        );
        anyhow::ensure!(!fenced(root, id), "EA {id} is deleting or deleted");
        Ok(Self { _lock: lock })
    }
}
fn try_flock(file: &File, operation: i32) -> bool {
    unsafe { libc::flock(file.as_raw_fd(), operation | libc::LOCK_NB) == 0 }
}
fn lock(root: &Path, id: EaId) -> Result<File> {
    fs::create_dir_all(directory(root))?;
    Ok(File::create(directory(root).join(format!("{id}.lock")))?)
}

/// Poll `ready` until it holds or `seconds` pass.
pub(crate) fn wait_until(
    seconds: u64,
    timeout: &str,
    mut ready: impl FnMut() -> bool,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while !ready() {
        anyhow::ensure!(Instant::now() < deadline, "{timeout}");
        std::thread::sleep(Duration::from_millis(25));
    }
    Ok(())
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct DeleteResult {
    pub(crate) deleted_ea: EaId,
    pub(crate) agents_killed: usize,
    pub(crate) events_cancelled: usize,
}
#[derive(Default, Serialize, Deserialize)]
struct Deletion {
    result: DeleteResult,
    paths: Vec<PathBuf>,
    complete: bool,
    processes: Vec<(u32, String)>,
    agents: Vec<String>,
}

/// The caller serializes registry selection and drains its live run contexts.
/// On failure the durable fence stays in place; retry or daemon restart resumes
/// the same deletion. Retired IDs stay fenced, even after their files are gone.
pub(crate) fn delete(
    root: &Path,
    id: EaId,
    prefix: &str,
    scheduler: &crate::scheduler::Scheduler,
    quiesce: impl FnOnce() -> Result<()>,
    retire_history: impl FnOnce(EaId) -> Result<()>,
) -> Result<DeleteResult> {
    fs::create_dir_all(directory(root))?;
    let path = marker(root, id);
    let registry = crate::ea::load_registry(root);
    let mut deletion = if path.exists() {
        let saved: Deletion = serde_json::from_slice(&fs::read(&path)?)?;
        anyhow::ensure!(!saved.complete, "EA {id} was already deleted");
        saved
    } else {
        anyhow::ensure!(registry.iter().any(|ea| ea.id == id), "EA {id} not found");
        anyhow::ensure!(
            registry.len() > 1,
            "Cannot delete the only EA; at least one EA must remain"
        );
        let saved = Deletion {
            result: DeleteResult {
                deleted_ea: id,
                ..Default::default()
            },
            paths: vec![
                format!("ea/{id}").into(),
                format!("mcp/ea-{id}").into(),
                format!("manager_notes_ea{id}.md").into(),
            ],
            ..Default::default()
        };
        crate::topology::write_json_atomic(&path, &saved)?;
        saved
    };
    let file = lock(root, id)?;
    wait_until(
        40,
        &format!("EA {id} deletion is fenced but requests did not drain; retry deletion"),
        || try_flock(&file, libc::LOCK_EX),
    )?;
    let client = crate::tmux::TmuxClient::new("");
    let manager = crate::ea::ea_manager_session(id, prefix);
    let workers = crate::ea::ea_prefix(id, prefix);
    // Journal the EA's live sessions and their process trees.
    let capture = |deletion: &mut Deletion| -> Result<Vec<String>> {
        let sessions = client
            .list_all_sessions()?
            .into_iter()
            .filter(|s| s.name == manager || s.name.starts_with(&workers))
            .collect::<Vec<_>>();
        anyhow::ensure!(
            !sessions.iter().any(|s| s.attached),
            "Cannot delete attached session: EA {id} deletion is pending; detach its agent terminals, then retry deletion"
        );
        for session in &sessions {
            if !deletion.agents.contains(&session.name) {
                deletion.agents.push(session.name.clone());
            }
            deletion
                .processes
                .extend(client.session_process_tree(&session.name)?);
        }
        deletion.processes.sort();
        deletion.processes.dedup();
        deletion.result.agents_killed = deletion.agents.len();
        crate::topology::write_json_atomic(&path, &*deletion)?;
        Ok(sessions.into_iter().map(|s| s.name).collect())
    };
    capture(&mut deletion)?;
    quiesce()?;
    // Run teardown may already have closed some of the captured panes.
    let sessions = capture(&mut deletion)?;
    let closed = sessions
        .iter()
        .map(|name| client.kill_session(name))
        .collect::<Result<Vec<_>>>();
    crate::process::signal_tree(&deletion.processes, "-TERM");
    std::thread::sleep(Duration::from_millis(100));
    crate::process::signal_tree(&deletion.processes, "-KILL");
    closed?;
    wait_until(
        5,
        &format!("EA {id} child processes survived termination; retry deletion"),
        || !crate::process::tree_alive(&deletion.processes),
    )?;
    for name in &sessions {
        anyhow::ensure!(
            !client.has_session(name)?,
            "EA process session survived teardown"
        );
    }
    // Admitted requests and run teardown may have allocated a workspace after
    // the fence was written. Inventory only once both have fully quiesced.
    // Keep existing journal entries and indices when resuming staged cleanup.
    for workspace in crate::workspace::list(root, id)? {
        for directory in ["workspaces", "workspace-history", "editors"] {
            let relative = PathBuf::from(directory).join(&workspace.id);
            if !deletion.paths.contains(&relative) {
                deletion.paths.push(relative);
            }
        }
    }
    deletion.result.events_cancelled += scheduler.cancel_by_ea_checked(id)?;
    crate::topology::write_json_atomic(&path, &deletion)?;
    // Never remove another session's global backend registration.
    crate::backend::antigravity::remove_session_mcp_config(root, id)?;
    let survivor = registry
        .iter()
        .filter(|ea| ea.id != id)
        .map(|ea| ea.id)
        .min()
        .context("no surviving EA in deletion journal")?;
    retire_history(survivor)?;
    let staging = directory(root).join(format!("deleted-{id}"));
    fs::create_dir_all(&staging)?;
    for (index, relative) in deletion.paths.iter().enumerate() {
        let src = root.join(relative);
        let dst = staging.join(index.to_string());
        match src.symlink_metadata() {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        }
        anyhow::ensure!(
            dst.symlink_metadata().is_err(),
            "deletion staging conflict at {}",
            src.display()
        );
        fs::rename(&src, dst)?;
    }
    if crate::ea::load_registry(root).iter().any(|ea| ea.id == id) {
        crate::ea::unregister_ea(root, id)?;
    }
    crate::ea::save_active_ea(root, survivor)?;
    fs::remove_dir_all(staging)?;
    deletion.complete = true;
    crate::topology::write_json_atomic(&path, &deletion)?;
    Ok(deletion.result)
}

pub(crate) fn recover(
    root: &Path,
    prefix: &str,
    scheduler: &crate::scheduler::Scheduler,
) -> Result<()> {
    let dir = directory(root);
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let saved: Deletion = serde_json::from_slice(&fs::read(&path)?)?;
        if !saved.complete {
            delete(
                root,
                saved.result.deleted_ea,
                prefix,
                scheduler,
                || Ok(()),
                |survivor| {
                    crate::chat_history::History::load_session(root)?
                        .remove_ea(saved.result.deleted_ea, survivor)
                },
            )
            .context("resume pending EA deletion before starting the runtime")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{chat_history::History, ea, scheduler::Scheduler};

    fn fixture() -> (tempfile::TempDir, Scheduler, String) {
        let dir = tempfile::tempdir().unwrap();
        ea::ensure_default_ea(dir.path()).unwrap();
        ea::register_ea(dir.path(), "review", None).unwrap();
        let scheduler = Scheduler::with_store(crate::scheduler::events_store_path(dir.path()));
        let prefix = format!("delete-test-{}-", uuid::Uuid::new_v4());
        (dir, scheduler, prefix)
    }
    fn write_state(root: &Path, id: EaId, text: &str) {
        fs::create_dir_all(ea::ea_state_dir(id, root)).unwrap();
        fs::write(ea::ea_state_dir(id, root).join("sentinel"), text).unwrap();
    }
    fn remove(root: &Path, id: EaId, prefix: &str, scheduler: &Scheduler) -> Result<DeleteResult> {
        delete(
            root,
            id,
            prefix,
            scheduler,
            || Ok(()),
            |survivor| History::load_session(root)?.remove_ea(id, survivor),
        )
    }

    #[test]
    fn only_and_unknown_eas_leave_no_fence_or_mutation() {
        let (dir, scheduler, prefix) = fixture();
        let root = dir.path();
        let before = fs::read(root.join("eas.json")).unwrap();
        assert!(remove(root, 99, &prefix, &scheduler).is_err());
        assert!(!fenced(root, 99));
        assert_eq!(before, fs::read(root.join("eas.json")).unwrap());
        ea::unregister_ea(root, 1).unwrap();
        assert!(remove(root, 0, &prefix, &scheduler).is_err());
        assert!(!fenced(root, 0));
        assert!(ea::ea_state_dir(0, root).exists());
    }

    #[test]
    fn fence_rejects_new_requests_and_drains_existing_ones() {
        let (dir, scheduler, prefix) = fixture();
        let root = dir.path().to_path_buf();
        let admission = Admission::acquire(&root, 1).unwrap();
        let worker_root = root.clone();
        let (send, recv) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            delete(
                &worker_root,
                1,
                &prefix,
                &scheduler,
                || {
                    send.send(()).unwrap();
                    anyhow::bail!("fault after drain")
                },
                |_| Ok(()),
            )
        });
        wait_until(5, "fence not written", || fenced(&root, 1)).unwrap();
        assert!(Admission::acquire(&root, 1).is_err());
        assert!(Admission::acquire(&root, 0).is_ok());
        assert!(recv.try_recv().is_err());
        drop(admission);
        recv.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(worker.join().unwrap().is_err());
        assert!(fenced(&root, 1));
        assert_eq!(ea::load_registry(&root).len(), 2);
    }

    #[test]
    fn cleanup_includes_workspaces_created_while_admitted_work_drains() {
        let (dir, scheduler, prefix) = fixture();
        let root = dir.path();
        let mut created = None;
        delete(
            root,
            1,
            &prefix,
            &scheduler,
            || {
                let workspace =
                    crate::workspace::Workspace::create(root, 1, "draining", "worker", None)?;
                fs::write(workspace.worktree(root).join("sentinel"), "late state")?;
                let editor = root.join("editors").join(&workspace.id);
                fs::create_dir_all(&editor)?;
                fs::write(editor.join("sentinel"), "late editor state")?;
                created = Some(workspace.id);
                Ok(())
            },
            |survivor| History::load_session(root)?.remove_ea(1, survivor),
        )
        .unwrap();
        let id = created.unwrap();
        for directory in ["workspaces", "workspace-history", "editors"] {
            assert!(
                !root.join(directory).join(&id).exists(),
                "left {directory}/{id} after deletion"
            );
        }
    }

    #[test]
    fn cancellation_failure_keeps_state_and_retries_on_restart() {
        let (dir, scheduler, prefix) = fixture();
        let root = dir.path();
        write_state(root, 1, "keep");
        fs::write(crate::scheduler::events_store_path(root), "invalid events").unwrap();
        assert!(remove(root, 1, &prefix, &scheduler).is_err());
        assert_eq!(ea::load_registry(root).len(), 2);
        assert!(ea::ea_state_dir(1, root).join("sentinel").exists());
        fs::write(crate::scheduler::events_store_path(root), "[]").unwrap();
        recover(root, &prefix, &scheduler).unwrap();
        assert_eq!(ea::load_registry(root).len(), 1);
        assert!(!ea::ea_state_dir(1, root).exists());
        assert!(remove(root, 1, &prefix, &scheduler).is_err());
        assert!(Admission::acquire(root, 1).is_err());
    }

    #[test]
    fn inaccessible_state_is_an_error_and_can_be_retried() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, scheduler, prefix) = fixture();
        let root = dir.path();
        write_state(root, 1, "keep");
        fs::set_permissions(root.join("ea"), fs::Permissions::from_mode(0o000)).unwrap();
        let result = remove(root, 1, &prefix, &scheduler);
        fs::set_permissions(root.join("ea"), fs::Permissions::from_mode(0o700)).unwrap();
        assert!(result.is_err());
        assert_eq!(ea::load_registry(root).len(), 2);
        assert_eq!(
            fs::read_to_string(ea::ea_state_dir(1, root).join("sentinel")).unwrap(),
            "keep"
        );
        remove(root, 1, &prefix, &scheduler).unwrap();
        assert_eq!(ea::load_registry(root).len(), 1);
    }

    #[test]
    fn staging_collision_is_explicit_and_keeps_both_copies() {
        let (dir, scheduler, prefix) = fixture();
        let root = dir.path();
        write_state(root, 1, "source");
        let staged = directory(root).join("deleted-1/0");
        fs::create_dir_all(&staged).unwrap();
        fs::write(staged.join("sentinel"), "conflicting state").unwrap();
        assert!(remove(root, 1, &prefix, &scheduler).is_err());
        assert_eq!(
            fs::read_to_string(ea::ea_state_dir(1, root).join("sentinel")).unwrap(),
            "source"
        );
        assert_eq!(
            fs::read_to_string(staged.join("sentinel")).unwrap(),
            "conflicting state"
        );
        assert_eq!(ea::load_registry(root).len(), 2);
        assert!(fenced(root, 1));
    }

    #[test]
    fn registry_failure_after_staging_recovers_without_touching_survivor() {
        let (dir, scheduler, prefix) = fixture();
        let root = dir.path();
        write_state(root, 1, "target");
        write_state(root, 0, "survivor");
        fs::remove_file(root.join("eas.lock")).unwrap();
        fs::create_dir(root.join("eas.lock")).unwrap();
        assert!(remove(root, 1, &prefix, &scheduler).is_err());
        assert_eq!(ea::load_registry(root).len(), 2);
        assert!(directory(root).join("deleted-1/0/sentinel").exists());
        fs::remove_dir(root.join("eas.lock")).unwrap();
        recover(root, &prefix, &scheduler).unwrap();
        assert_eq!(ea::load_registry(root).len(), 1);
        assert_eq!(
            fs::read_to_string(ea::ea_state_dir(0, root).join("sentinel")).unwrap(),
            "survivor"
        );
        recover(root, &prefix, &scheduler).unwrap();
        assert!(ea::register_ea(root, "next", None).unwrap() > 1);
    }
}

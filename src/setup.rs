//! A team's `setup` command: runs once in an instance's workspace, before any
//! agent of the run launches, so what it writes (an agent's hooks, skills,
//! config) is there when the agent starts. Outside the reactor: it precedes
//! the first tag, reads no port and writes none.
use anyhow::{bail, Context, Result};
use serde_json::json;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How much of the output's end is quoted in an error and in the record.
const TAIL: u64 = 4096;

/// Run `command` with `sh -c` in `worktree`, in its own process group, with the
/// environment a worker gets. Success is exit 0. The full combined output is
/// `<record_dir>/<instance>.log` and a summary `<instance>.json`; a failure or
/// a run past `timeout` is an error that carries the output's tail.
pub(crate) fn run(
    instance: &str,
    command: &str,
    worktree: &Path,
    temp: &Path,
    timeout: Duration,
    record_dir: &Path,
) -> Result<()> {
    fs::create_dir_all(record_dir)?;
    let log_path = record_dir.join(format!("{instance}.log"));
    let log = fs::File::create(&log_path)?;
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(worktree)
        .env("OMAR_WORKTREE", worktree)
        .env("OMAR_TEMP", temp)
        .env("TMPDIR", temp)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()
        .with_context(|| format!("setup for instance '{instance}': cannot run sh"))?;
    let started = Instant::now();
    // Observed without reaping, so the group id cannot be recycled before the
    // group is killed below.
    let timed_out = loop {
        if crate::process::child_exited(&child)?.is_some() {
            break false;
        }
        if started.elapsed() >= timeout {
            break true;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    // The group, not just `sh`: a setup that left a descendant running must not
    // outlive its step. On a timeout this is also what stops it.
    unsafe {
        libc::killpg(child.id() as libc::pid_t, libc::SIGKILL);
    }
    let exit = if timed_out {
        child.wait()?;
        None
    } else {
        child.wait()?.code()
    };
    let tail = tail(&log_path);
    let summary = json!({
        "instance": instance,
        "command": command,
        "exit_code": exit,
        "timed_out": timed_out,
        "output_tail": tail,
    });
    fs::write(
        record_dir.join(format!("{instance}.json")),
        serde_json::to_vec_pretty(&summary)?,
    )?;
    if timed_out {
        bail!(
            "setup for instance '{instance}' ran past the {}s timeout and was killed: {command}\n{tail}",
            timeout.as_secs()
        );
    }
    if exit != Some(0) {
        let how = exit.map_or("was killed by a signal".to_string(), |code| {
            format!("exited {code}")
        });
        bail!("setup for instance '{instance}' {how}: {command}\n{tail}");
    }
    Ok(())
}

fn tail(path: &Path) -> String {
    let read = || -> std::io::Result<String> {
        let mut file = fs::File::open(path)?;
        let len = file.metadata()?.len();
        file.seek(SeekFrom::Start(len.saturating_sub(TAIL)))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok(String::from_utf8_lossy(&bytes).trim().to_string())
    };
    read().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let worktree = root.path().join("worktree");
        let temp = root.path().join("temp");
        fs::create_dir_all(&worktree).unwrap();
        fs::create_dir_all(&temp).unwrap();
        (root, worktree, temp)
    }

    #[test]
    fn setup_runs_in_the_worktree_with_a_workers_environment_and_is_recorded() {
        let (root, worktree, temp) = dirs();
        let record = root.path().join("setup");
        run(
            "n1",
            "pwd > where; echo \"$OMAR_WORKTREE $OMAR_TEMP $TMPDIR\" > env; echo said",
            &worktree,
            &temp,
            Duration::from_secs(10),
            &record,
        )
        .unwrap();
        let canonical = |p: &Path| p.canonicalize().unwrap();
        assert_eq!(
            canonical(Path::new(
                fs::read_to_string(worktree.join("where")).unwrap().trim()
            )),
            canonical(&worktree)
        );
        assert_eq!(
            fs::read_to_string(worktree.join("env")).unwrap().trim(),
            format!("{0} {1} {1}", worktree.display(), temp.display())
        );
        let summary: serde_json::Value =
            serde_json::from_slice(&fs::read(record.join("n1.json")).unwrap()).unwrap();
        assert_eq!(summary["exit_code"], 0);
        assert_eq!(summary["output_tail"], "said");
        assert_eq!(fs::read_to_string(record.join("n1.log")).unwrap(), "said\n");
    }

    #[test]
    fn a_failing_setup_is_an_error_with_its_output_and_exit_code() {
        let (root, worktree, temp) = dirs();
        let record = root.path().join("setup");
        let error = run(
            "n1",
            "echo out; echo boom >&2; exit 7",
            &worktree,
            &temp,
            Duration::from_secs(10),
            &record,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("exited 7"), "{error}");
        assert!(error.contains("out") && error.contains("boom"), "{error}");
        let summary: serde_json::Value =
            serde_json::from_slice(&fs::read(record.join("n1.json")).unwrap()).unwrap();
        assert_eq!(summary["exit_code"], 7);
    }

    #[test]
    fn a_setup_past_the_timeout_is_killed_with_its_descendants() {
        let (root, worktree, temp) = dirs();
        let record = root.path().join("setup");
        let started = Instant::now();
        let error = run(
            "n1",
            "(sleep 30; touch survived) & sleep 30",
            &worktree,
            &temp,
            Duration::from_millis(300),
            &record,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("timeout"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(!worktree.join("survived").exists());
    }
}

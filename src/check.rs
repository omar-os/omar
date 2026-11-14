//! A task's completion check: a shell command `omar_complete` runs before it
//! accepts a completion. Exit 0 accepts; anything else refuses, and the agent
//! is told why so it can keep working in the same conversation.
//!
//! The command and everything it starts live in one process group, which is
//! always killed before `run` returns: on exit, at the deadline, or when the
//! invocation is cancelled.

use std::io::Read;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// How much of a check's combined stdout and stderr is kept, from the end:
/// what a failing command says last is what the agent needs.
pub const OUTPUT_LIMIT: usize = 16 * 1024;

/// How long to wait for the output reader once the command is gone. A
/// descendant can hold the pipe open, and `run` may not wait on it forever.
const READER_GRACE: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckOutcome {
    Passed,
    Failed,
    /// Still running at the invocation's deadline, and killed.
    TimedOut,
    /// Killed because the invocation ended before the check did.
    Cancelled,
}

/// One run of a check, as it is recorded.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckRun {
    pub started_ms: u64,
    pub ended_ms: u64,
    pub outcome: CheckOutcome,
    /// `None` when the command was killed or died on a signal.
    pub exit_code: Option<i32>,
    /// The last `OUTPUT_LIMIT` bytes of stdout and stderr together.
    pub output: String,
    /// Whether earlier output was dropped to stay within the limit.
    pub truncated: bool,
}

impl CheckRun {
    pub fn passed(&self) -> bool {
        self.outcome == CheckOutcome::Passed
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// The end of a stream, bounded.
#[derive(Default)]
struct Tail {
    bytes: Vec<u8>,
    truncated: bool,
}

impl Tail {
    fn push(&mut self, data: &[u8]) {
        self.bytes.extend_from_slice(data);
        if self.bytes.len() > OUTPUT_LIMIT {
            let excess = self.bytes.len() - OUTPUT_LIMIT;
            self.bytes.drain(..excess);
            self.truncated = true;
        }
    }
}

/// Run `command` with `sh -c` in `dir` until it exits, `deadline` passes or
/// `cancel` is set. `env` is added to this process's environment; stdin is
/// empty.
pub fn run(
    command: &str,
    dir: &Path,
    env: &[(String, String)],
    deadline: Instant,
    cancel: &AtomicBool,
) -> CheckRun {
    let started_ms = now_ms();
    let finish = |outcome, exit_code, output: String, truncated| CheckRun {
        started_ms,
        ended_ms: now_ms(),
        outcome,
        exit_code,
        output,
        truncated,
    };
    let failed = |message: String| finish(CheckOutcome::Failed, None, message, false);

    // One pipe for both streams, so the output keeps the order it was written in.
    let (mut reader, writer) = match std::io::pipe() {
        Ok(pipe) => pipe,
        Err(error) => return failed(format!("failed to run check: {error}")),
    };
    let mut process = Command::new("sh");
    process
        .arg("-c")
        .arg(command)
        .current_dir(dir)
        .envs(env.iter().map(|(name, value)| (name, value)))
        .process_group(0)
        .stdin(Stdio::null());
    match writer.try_clone() {
        Ok(copy) => process.stdout(copy).stderr(writer),
        Err(error) => return failed(format!("failed to run check: {error}")),
    };
    let spawned = process.spawn();
    // The parent's copies of the write end must go, or the reader never sees
    // the end of the stream.
    drop(process);
    let child = match spawned {
        Ok(child) => child,
        Err(error) => return failed(format!("failed to run check: {error}")),
    };
    let mut process = crate::reaction::ReactionProcess::new(child);

    // Read from a thread so the output so far is available whenever the check
    // ends, whether or not a descendant still holds the pipe.
    let tail = Arc::new(Mutex::new(Tail::default()));
    let (ended, reader_done) = mpsc::channel();
    let reading = tail.clone();
    std::thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        while let Ok(n) = reader.read(&mut chunk) {
            if n == 0 {
                break;
            }
            if let Ok(mut tail) = reading.lock() {
                tail.push(&chunk[..n]);
            }
        }
        ended.send(()).ok();
    });

    let exited = loop {
        match crate::process::child_exited(&process.child) {
            Ok(Some(_)) => break Ok(CheckOutcome::Failed),
            Ok(None) if Instant::now() >= deadline => break Ok(CheckOutcome::TimedOut),
            Ok(None) if cancel.load(Ordering::Acquire) => break Ok(CheckOutcome::Cancelled),
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(error) => break Err(error),
        }
    };
    // What it said before it stopped, if it stopped by itself.
    if matches!(exited, Ok(CheckOutcome::Failed)) {
        reader_done.recv_timeout(READER_GRACE).ok();
    }
    let cleanup = process.cleanup();
    reader_done.recv_timeout(READER_GRACE).ok();

    let (mut output, truncated) = match tail.lock() {
        Ok(tail) => (
            String::from_utf8_lossy(&tail.bytes).into_owned(),
            tail.truncated,
        ),
        Err(_) => (String::new(), false),
    };
    let status = process.child.wait().ok();
    let mut outcome = match exited {
        Ok(outcome) => outcome,
        Err(error) => {
            output.push_str(&format!("\n[cannot observe the check: {error:#}]"));
            CheckOutcome::Failed
        }
    };
    if outcome == CheckOutcome::Failed && status.is_some_and(|status| status.success()) {
        outcome = CheckOutcome::Passed;
    }
    let mut exit_code = status.and_then(|status| status.code());
    if outcome != CheckOutcome::Passed && exit_code.is_none() {
        if let Some(signal) = status.and_then(|status| status.signal()) {
            if matches!(outcome, CheckOutcome::Failed) {
                output.push_str(&format!("\n[check died on signal {signal}]"));
            }
        }
    }
    if matches!(outcome, CheckOutcome::TimedOut | CheckOutcome::Cancelled) {
        exit_code = None;
    }
    if let Err(error) = cleanup {
        // Nothing may be left running, so a check that cannot be confirmed
        // gone does not pass.
        output.push_str(&format!("\n[check cleanup unconfirmed: {error:#}]"));
        outcome = CheckOutcome::Failed;
    }
    finish(outcome, exit_code, output, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_in(command: &str, dir: &Path, within: Duration) -> CheckRun {
        run(
            command,
            dir,
            &[("CHECK_GREETING".into(), "hello".into())],
            Instant::now() + within,
            &AtomicBool::new(false),
        )
    }

    #[test]
    fn exit_zero_passes_and_anything_else_fails_with_its_output() {
        let dir = tempfile::tempdir().unwrap();
        let passed = run_in("echo fine", dir.path(), Duration::from_secs(10));
        assert!(passed.passed(), "{passed:?}");
        assert_eq!(passed.exit_code, Some(0));
        assert_eq!(passed.output, "fine\n");

        let failed = run_in(
            "echo out; echo err >&2; exit 3",
            dir.path(),
            Duration::from_secs(10),
        );
        assert_eq!(failed.outcome, CheckOutcome::Failed);
        assert_eq!(failed.exit_code, Some(3));
        // Both streams, in the order they were written.
        assert_eq!(failed.output, "out\nerr\n");
        assert!(failed.ended_ms >= failed.started_ms);
    }

    #[test]
    fn it_runs_in_the_workspace_with_the_given_environment() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("marker"), "here").unwrap();
        let run = run_in(
            "cat marker; printf ' %s' \"$CHECK_GREETING\"",
            dir.path(),
            Duration::from_secs(10),
        );
        assert_eq!(run.output, "here hello");
    }

    #[test]
    fn only_the_end_of_long_output_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let run = run_in(
            "yes 0123456789 | head -c 100000; echo; echo LAST; exit 1",
            dir.path(),
            Duration::from_secs(10),
        );
        assert!(run.truncated);
        assert_eq!(run.output.len(), OUTPUT_LIMIT);
        assert!(run.output.ends_with("LAST\n"), "{:?}", run.output);
    }

    /// Whether any process still belongs to the group the test started, found
    /// through the file the check wrote its descendant's pid to.
    fn alive(pid: &str) -> bool {
        Command::new("kill")
            .args(["-0", pid])
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    #[test]
    fn a_check_still_running_at_the_deadline_is_killed_with_its_children() {
        let dir = tempfile::tempdir().unwrap();
        let started = Instant::now();
        let run = run_in(
            "sh -c 'trap \"\" TERM; echo $$ > child.pid; sleep 60' & echo started; wait",
            dir.path(),
            Duration::from_millis(600),
        );
        assert_eq!(run.outcome, CheckOutcome::TimedOut, "{run:?}");
        assert_eq!(run.exit_code, None);
        assert!(run.output.contains("started"), "{:?}", run.output);
        assert!(started.elapsed() < Duration::from_secs(10));
        let pid = std::fs::read_to_string(dir.path().join("child.pid")).unwrap();
        assert!(!alive(pid.trim()), "descendant {pid} outlived the check");
    }

    #[test]
    fn a_descendant_left_behind_by_a_finished_check_is_killed_too() {
        let dir = tempfile::tempdir().unwrap();
        let run = run_in(
            "sh -c 'echo $$ > child.pid; sleep 60' >/dev/null 2>&1 & while [ ! -s child.pid ]; do sleep 0.01; done",
            dir.path(),
            Duration::from_secs(10),
        );
        assert!(run.passed(), "{run:?}");
        let pid = std::fs::read_to_string(dir.path().join("child.pid")).unwrap();
        assert!(!alive(pid.trim()), "descendant {pid} outlived the check");
    }

    #[test]
    fn cancelling_kills_the_check() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(true);
        let run = run(
            "sleep 60",
            dir.path(),
            &[],
            Instant::now() + Duration::from_secs(60),
            &cancel,
        );
        assert_eq!(run.outcome, CheckOutcome::Cancelled);
    }
}

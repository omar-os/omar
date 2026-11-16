//! Unit tests start real tmux servers. Before any test runs, this points the
//! whole test binary at a tmux socket directory of its own, so no session lands
//! on the developer's tmux, and every server in it is killed when the binary
//! exits. A run that is killed outright (no exit handlers) is swept by the next.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Set for the binary and everything it starts; a child that is itself a test
/// binary shares its parent's servers rather than making its own.
const OWNER: &str = "OMAR_TEST_TMUX_OWNER";
const PREFIX: &str = "omar-test-tmux-";

/// `/tmp` rather than `TMPDIR`: a socket path is capped near 104 bytes, and
/// macOS's `TMPDIR` plus a `-L` name with a UUID would exceed it.
fn directory(pid: u32) -> PathBuf {
    Path::new("/tmp").join(format!("{PREFIX}{pid}"))
}

#[ctor::ctor(unsafe)]
fn isolate() {
    if std::env::var_os(OWNER).is_some() {
        return;
    }
    sweep();
    let pid = std::process::id();
    let dir = directory(pid);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    // Before main, nothing else is running: changing the environment is safe.
    std::env::set_var("TMUX_TMPDIR", &dir);
    // Inside tmux, `TMUX` names the developer's server and wins over the
    // socket directory for commands without `-L`.
    std::env::remove_var("TMUX");
    std::env::remove_var("TMUX_PANE");
    std::env::set_var(OWNER, pid.to_string());
    unsafe { libc::atexit(clean_up) };
}

extern "C" fn clean_up() {
    remove(&directory(std::process::id()));
}

/// Removes the directories of runs that are gone.
fn sweep() {
    let Ok(entries) = std::fs::read_dir("/tmp") else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|name| name.strip_prefix(PREFIX))
            .and_then(|pid| pid.parse::<i32>().ok())
        else {
            continue;
        };
        if !alive(pid) {
            remove(&entry.path());
        }
    }
}

fn alive(pid: i32) -> bool {
    let sent = unsafe { libc::kill(pid, 0) };
    sent == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Kills every server whose socket is in `dir` (tmux keeps them in
/// `tmux-<uid>/`), then removes it.
fn remove(dir: &Path) {
    let sockets = dir.join(format!("tmux-{}", unsafe { libc::getuid() }));
    for socket in std::fs::read_dir(sockets).into_iter().flatten().flatten() {
        let _ = Command::new("tmux")
            .arg("-S")
            .arg(socket.path())
            .arg("kill-server")
            .env_remove("TMUX")
            .output();
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tests_start_tmux_in_a_directory_of_their_own() {
        let dir = directory(std::process::id());
        assert_eq!(std::env::var_os("TMUX_TMPDIR"), Some(dir.clone().into()));
        assert!(std::env::var_os("TMUX").is_none());
        if Command::new("tmux").arg("-V").output().is_err() {
            return;
        }
        let server = format!("omar-test-isolation-{}", uuid::Uuid::new_v4());
        let started = Command::new("tmux")
            .args([
                "-L",
                &server,
                "new-session",
                "-d",
                "-s",
                "probe",
                "sleep 30",
            ])
            .status()
            .unwrap();
        assert!(started.success());
        let socket = dir
            .join(format!("tmux-{}", unsafe { libc::getuid() }))
            .join(&server);
        assert!(socket.exists(), "{} is missing", socket.display());
        let _ = Command::new("tmux")
            .args(["-L", &server, "kill-server"])
            .status();
    }

    #[test]
    fn a_finished_runs_directory_is_swept_and_its_servers_killed() {
        if Command::new("tmux").arg("-V").output().is_err() {
            return;
        }
        // A pid that cannot be alive: one past the kernel's ceiling.
        let dead = std::fs::read_to_string("/proc/sys/kernel/pid_max")
            .ok()
            .and_then(|max| max.trim().parse::<u32>().ok())
            .unwrap_or(4_194_304)
            + 1;
        let dir = directory(dead);
        std::fs::create_dir_all(&dir).unwrap();
        let started = Command::new("tmux")
            .args([
                "-L",
                "leftover",
                "new-session",
                "-d",
                "-s",
                "probe",
                "sleep 30",
            ])
            .env("TMUX_TMPDIR", &dir)
            .status()
            .unwrap();
        assert!(started.success());
        let alive_before = Command::new("tmux")
            .args(["-L", "leftover", "has-session"])
            .env("TMUX_TMPDIR", &dir)
            .status()
            .unwrap();
        assert!(alive_before.success());
        sweep();
        assert!(!dir.exists());
        let alive_after = Command::new("tmux")
            .args(["-L", "leftover", "has-session"])
            .env("TMUX_TMPDIR", &dir)
            .output()
            .unwrap();
        assert!(!alive_after.status.success());
    }
}

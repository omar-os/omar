use std::fs;
use std::path::Path;

/// Return true if a process with the given PID currently exists. Uses
/// `kill -0 <pid>`, the standard POSIX no-op signal check. On non-Unix
/// platforms, conservatively assume the process is still alive rather than
/// reclaiming a lock we shouldn't.
pub(crate) fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        if pid > i32::MAX as u32 {
            return false;
        }
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(true)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

/// Observe exit without reaping: retaining the child PID prevents its process
/// group ID from being recycled before descendant cleanup has finished.
pub(crate) fn child_exited(child: &std::process::Child) -> anyhow::Result<Option<bool>> {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    loop {
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                child.id() as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result == 0 {
            break;
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error.into());
        }
    }
    if unsafe { info.si_pid() } == 0 {
        return Ok(None);
    }
    Ok(Some(
        info.si_code == libc::CLD_EXITED && unsafe { info.si_status() } == 0,
    ))
}

pub(crate) fn pid_file_is_stale(path: &Path) -> bool {
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| {
            let text = raw.trim();
            if text.is_empty() {
                return None;
            }
            text.parse::<u32>().ok()
        })
        .is_none_or(|pid| pid == 0 || !pid_alive(pid))
}

/// Snapshot a pane's descendants before tmux closes its terminal, while their
/// parentage is still available. Include start times to avoid signaling a PID
/// that has since been recycled.
pub(crate) fn process_tree(root: u32) -> Vec<(u32, String)> {
    let all = process_snapshot();
    let mut pending = vec![root];
    let mut found = Vec::new();
    while let Some(pid) = pending.pop() {
        if found.iter().any(|(known, _)| *known == pid) {
            continue;
        }
        if let Some((_, _, started)) = all.iter().find(|(id, _, _)| *id == pid) {
            found.push((pid, started.clone()));
            pending.extend(
                all.iter()
                    .filter(|(_, parent, _)| *parent == pid)
                    .map(|(id, _, _)| *id),
            );
        }
    }
    found
}

fn process_snapshot() -> Vec<(u32, u32, String)> {
    std::process::Command::new("ps")
        .args(["-axo", "pid=,ppid=,lstart="])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter_map(|line| {
                    let mut words = line.split_whitespace();
                    Some((
                        words.next()?.parse().ok()?,
                        words.next()?.parse().ok()?,
                        words.collect::<Vec<_>>().join(" "),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn signal_tree(tree: &[(u32, String)], signal: &str) {
    let live = process_snapshot();
    for (pid, started) in tree {
        if *pid > 1
            && *pid != std::process::id()
            && live
                .iter()
                .any(|(id, _, time)| id == pid && time == started)
        {
            let _ = std::process::Command::new("kill")
                .args([signal, &pid.to_string()])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::pid_file_is_stale;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn exit_observation_keeps_child_waitable_until_cleanup() {
        let mut child = std::process::Command::new("sh")
            .args(["-c", "exit 7"])
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while super::child_exited(&child).unwrap().is_none() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(super::child_exited(&child).unwrap(), Some(false));
        assert_eq!(super::child_exited(&child).unwrap(), Some(false));
        assert_eq!(child.wait().unwrap().code(), Some(7));
    }

    #[test]
    fn pid_file_with_invalid_content_is_treated_as_stale() {
        let mut file = NamedTempFile::new().expect("temp lock file");
        file.write_all(b"not-a-pid").expect("write lock payload");
        assert!(pid_file_is_stale(file.path()));
    }

    #[test]
    fn pid_file_with_pid_zero_is_treated_as_stale() {
        let mut file = NamedTempFile::new().expect("temp lock file");
        file.write_all(b"0").expect("write lock payload");
        assert!(pid_file_is_stale(file.path()));
    }
}

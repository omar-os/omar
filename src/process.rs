use std::collections::BTreeSet;
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

/// Check captured identities rather than a potentially recycled PID.
pub(crate) fn tree_alive(tree: &[(u32, String)]) -> bool {
    let live = process_snapshot();
    tree.iter().any(|(pid, started)| {
        live.iter()
            .any(|(id, _, time)| id == pid && time == started)
            && std::process::Command::new("ps")
                .args(["-p", &pid.to_string(), "-o", "stat="])
                .output()
                .map(|output| {
                    let state = String::from_utf8_lossy(&output.stdout);
                    !state.trim().is_empty() && !state.trim().starts_with('Z')
                })
                .unwrap_or(true)
    })
}

/// The sessions `tree`'s live processes belong to, other than this one's.
pub(crate) fn sessions(tree: &[(u32, String)]) -> BTreeSet<u32> {
    tree.iter()
        .filter_map(|(pid, _)| session_of(*pid))
        .filter(|sid| Some(*sid) != session_of(0))
        .collect()
}

fn session_of(pid: u32) -> Option<u32> {
    let pid = libc::pid_t::try_from(pid).ok()?;
    let sid = unsafe { libc::getsid(pid) };
    u32::try_from(sid).ok().filter(|sid| *sid > 1)
}

/// What is left of a stopped tree that no ppid walk finds: every process,
/// other than this one, in one of `sessions`, or whose environment holds
/// `marker`. A process whose parent exits is reparented out of every tree,
/// but keeps its session unless it starts one of its own, and its
/// environment unless it clears it. One that does both is not found.
pub(crate) fn leftovers(sessions: &BTreeSet<u32>, marker: &str) -> Vec<(u32, String)> {
    let me = std::process::id();
    process_snapshot()
        .into_iter()
        .filter(|(pid, _, _)| *pid > 1 && *pid != me)
        .filter(|(pid, _, _)| {
            session_of(*pid).is_some_and(|sid| sessions.contains(&sid)) || carries(*pid, marker)
        })
        .map(|(pid, _, started)| (pid, started))
        .collect()
}

#[cfg(target_os = "linux")]
fn carries(pid: u32, marker: &str) -> bool {
    fs::read(format!("/proc/{pid}/environ")).is_ok_and(|environ| {
        environ
            .split(|byte| *byte == 0)
            .any(|var| var == marker.as_bytes())
    })
}

/// The environment a process was started with, as KERN_PROCARGS2 gives it
/// (argc, executable path, arguments, then the environment, NUL-separated),
/// which macOS shows for this user's own processes, whatever their session.
#[cfg(target_os = "macos")]
fn carries(pid: u32, marker: &str) -> bool {
    let Ok(pid) = libc::c_int::try_from(pid) else {
        return false;
    };
    let none = std::ptr::null_mut();
    // The most it can be: the kernel's limit on arguments and environment.
    let mut argmax: libc::c_int = 0;
    let mut size = std::mem::size_of::<libc::c_int>();
    let mut mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
    // SAFETY: each call writes at most `size` bytes into a buffer of that
    // length.
    if unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            2,
            (&mut argmax as *mut libc::c_int).cast(),
            &mut size,
            none,
            0,
        )
    } != 0
    {
        return false;
    }
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut size = usize::try_from(argmax).unwrap_or(0);
    let mut args = vec![0u8; size];
    if unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            args.as_mut_ptr().cast(),
            &mut size,
            none,
            0,
        )
    } != 0
    {
        return false;
    }
    args.truncate(size);
    args.split(|byte| *byte == 0)
        .any(|var| var == marker.as_bytes())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn carries(_pid: u32, _marker: &str) -> bool {
    false
}

/// Every process descended from one of `roots`, the roots included.
#[cfg(target_os = "macos")]
pub(crate) fn descendants(roots: impl IntoIterator<Item = u32>) -> BTreeSet<u32> {
    let all = process_snapshot();
    let mut found = BTreeSet::new();
    let mut pending: Vec<u32> = roots.into_iter().collect();
    while let Some(pid) = pending.pop() {
        if found.insert(pid) {
            pending.extend(
                all.iter()
                    .filter(|(_, parent, _)| *parent == pid)
                    .map(|(id, _, _)| *id),
            );
        }
    }
    found
}

/// What a worker may have left running that neither its session nor its
/// environment shows (macOS hides the environment of its own binaries, the
/// shells among them): every process started since `since`, to the second,
/// whose working directory, or a file it holds open, is under one of `dirs`.
/// Never this process, one in `spared`, or one in a session a process of
/// either is in. An error is a listing that could not be made, never an
/// empty one.
#[cfg(target_os = "macos")]
pub(crate) fn touching(
    dirs: &[std::path::PathBuf],
    since: std::time::SystemTime,
    spared: &BTreeSet<u32>,
) -> anyhow::Result<Vec<(u32, String)>> {
    let since = i64::try_from(since.duration_since(std::time::UNIX_EPOCH)?.as_secs())?;
    let me = std::process::id();
    let spared_sessions: BTreeSet<u32> = spared
        .iter()
        .copied()
        .chain([0])
        .filter_map(session_of)
        .collect();
    // A start time that cannot be read could be a recent one.
    let candidates: Vec<(u32, String)> = process_snapshot()
        .into_iter()
        .filter(|(pid, _, _)| *pid > 1 && *pid != me && !spared.contains(pid))
        .filter(|(_, _, started)| started_at(started).is_none_or(|at| at >= since))
        .filter(|(pid, _, _)| session_of(*pid).is_none_or(|sid| !spared_sessions.contains(&sid)))
        .map(|(pid, _, started)| (pid, started))
        .collect();
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let pids = candidates
        .iter()
        .map(|(pid, _)| pid.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let output = std::process::Command::new("lsof")
        .args(["-nP", "-w", "-F", "pn", "-p", &pids])
        .output()?;
    // lsof exits 1, saying nothing, when a process it was named has exited.
    anyhow::ensure!(
        output.status.success() || output.stderr.is_empty(),
        "lsof failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let dirs: Vec<_> = dirs
        .iter()
        .map(|dir| fs::canonicalize(dir).unwrap_or_else(|_| dir.clone()))
        .collect();
    let mut found = BTreeSet::new();
    let mut pid = None;
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if let Some(id) = line.strip_prefix('p') {
            pid = id.parse::<u32>().ok();
        } else if let (Some(name), Some(id)) = (line.strip_prefix('n'), pid) {
            if dirs.iter().any(|dir| Path::new(name).starts_with(dir)) {
                found.insert(id);
            }
        }
    }
    Ok(candidates
        .into_iter()
        .filter(|(pid, _)| found.contains(pid))
        .collect())
}

/// `ps -o lstart` as seconds since the epoch, in local time.
#[cfg(target_os = "macos")]
fn started_at(lstart: &str) -> Option<i64> {
    use chrono::TimeZone;
    let at = chrono::NaiveDateTime::parse_from_str(lstart, "%a %b %e %H:%M:%S %Y").ok()?;
    Some(
        chrono::Local
            .from_local_datetime(&at)
            .earliest()?
            .timestamp(),
    )
}

/// The processes listening on TCP `port`, any address. An error is a listing
/// that could not be made, never an empty one.
pub(crate) fn tcp_listeners(port: u16) -> anyhow::Result<BTreeSet<u32>> {
    #[cfg(target_os = "linux")]
    {
        use anyhow::Context;
        // Socket inodes in state 0A (LISTEN) on the port, then whoever holds one.
        let mut inodes = BTreeSet::new();
        for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
            let text = fs::read_to_string(table).with_context(|| format!("read {table}"))?;
            for line in text.lines().skip(1) {
                let fields: Vec<&str> = line.split_whitespace().collect();
                let listening = fields.get(3) == Some(&"0A");
                let on_port = fields
                    .get(1)
                    .and_then(|local| local.rsplit_once(':'))
                    .and_then(|(_, hex)| u16::from_str_radix(hex, 16).ok())
                    == Some(port);
                if listening && on_port {
                    inodes.extend(fields.get(9).map(|inode| format!("socket:[{inode}]")));
                }
            }
        }
        let mut pids = BTreeSet::new();
        for entry in fs::read_dir("/proc")?.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<u32>().ok())
            else {
                continue;
            };
            let Ok(fds) = fs::read_dir(entry.path().join("fd")) else {
                continue;
            };
            if fds.flatten().any(|fd| {
                fs::read_link(fd.path())
                    .is_ok_and(|link| inodes.contains(link.to_string_lossy().as_ref()))
            }) {
                pids.insert(pid);
            }
        }
        Ok(pids)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let output = std::process::Command::new("lsof")
            .args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-t"])
            .output()?;
        // lsof exits 1, saying nothing, when nothing matches.
        anyhow::ensure!(
            output.status.success() || output.stdout.is_empty() && output.stderr.is_empty(),
            "lsof failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .filter_map(|pid| pid.parse().ok())
            .collect())
    }
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

    /// A background process whose parent exits is in no tree, but is still
    /// found by the session it was started in, and (Linux) by its marker.
    #[test]
    fn a_reparented_process_is_found_by_its_session() {
        use std::os::unix::process::CommandExt;
        let mut command = std::process::Command::new("sh");
        command
            .args(["-c", "sh -c 'sleep 30 >/dev/null 2>&1 &'; exec sleep 30"])
            .env("OMAR_ATTEMPT", "leftover-test");
        // A session of its own, as a tmux pane has.
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let mut leader = command.spawn().unwrap();
        let mut sessions = super::BTreeSet::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let orphan = loop {
            let tree = super::process_tree(leader.id());
            sessions.extend(super::sessions(&tree));
            let found = super::leftovers(&sessions, "OMAR_ATTEMPT=leftover-test");
            if let Some(orphan) = found
                .iter()
                .find(|(pid, _)| !tree.iter().any(|(known, _)| known == pid))
            {
                break orphan.clone();
            }
            assert!(std::time::Instant::now() < deadline, "{found:?}");
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        assert!(!sessions.is_empty());
        leader.kill().unwrap();
        leader.wait().unwrap();
        let left = super::leftovers(&sessions, "OMAR_ATTEMPT=leftover-test");
        assert!(left.contains(&orphan), "{left:?}");
        super::signal_tree(&left, "-KILL");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while super::tree_alive(std::slice::from_ref(&orphan)) {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        // This process's own session is never one of them.
        assert!(!super::sessions(&super::process_tree(std::process::id()))
            .contains(&super::session_of(0).unwrap()));
    }

    /// What `a_process_in_a_session_of_its_own_is_found_by_its_marker`
    /// starts: this test binary, which macOS shows the environment of, as it
    /// does not its own binaries' (`sleep`, `sh`).
    #[test]
    #[ignore = "started by a_process_in_a_session_of_its_own_is_found_by_its_marker"]
    fn marked_sleeper() {
        std::thread::sleep(std::time::Duration::from_secs(30));
    }

    /// A process in a session of its own, which no session finds, is still
    /// found by the marker in its environment.
    #[test]
    fn a_process_in_a_session_of_its_own_is_found_by_its_marker() {
        use std::os::unix::process::CommandExt;
        let marker = format!("OMAR_ATTEMPT=marker-test-{}", std::process::id());
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["process::tests::marked_sleeper", "--exact", "--ignored"])
            .stdout(std::process::Stdio::null())
            .env("OMAR_ATTEMPT", marker.split_once('=').unwrap().1);
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let mut escaped = command.spawn().unwrap();
        // Once it has exec'd: until then it shows its parent's environment.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let found = loop {
            let found = super::leftovers(&super::BTreeSet::new(), &marker);
            if !found.is_empty() || std::time::Instant::now() > deadline {
                break found;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        let unmarked = super::leftovers(&super::BTreeSet::new(), "OMAR_ATTEMPT=nobody");
        escaped.kill().unwrap();
        escaped.wait().unwrap();
        assert_eq!(
            found.iter().map(|(pid, _)| *pid).collect::<Vec<_>>(),
            vec![escaped.id()]
        );
        assert!(!unmarked.iter().any(|(pid, _)| *pid == escaped.id()));
    }

    /// macOS: a process in a session of its own, whose environment a shell
    /// would hide, is found by its working directory under the workspace,
    /// unless it is spared; one elsewhere, or started earlier, is not.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_process_in_the_workspace_is_found_by_its_working_directory() {
        use std::os::unix::process::CommandExt;
        let earlier = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let since = std::time::SystemTime::now();
        let workspace = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let spawn = |dir: &std::path::Path| {
            let mut command = std::process::Command::new("sh");
            command.args(["-c", "sleep 30"]).current_dir(dir);
            unsafe {
                command.pre_exec(|| {
                    libc::setsid();
                    Ok(())
                });
            }
            command.spawn().unwrap()
        };
        let mut inside = spawn(workspace.path());
        let mut outside = spawn(elsewhere.path());
        let dirs = [workspace.path().to_path_buf()];
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let found = loop {
            let found = super::touching(&dirs, since, &super::BTreeSet::new()).unwrap();
            if !found.is_empty() || std::time::Instant::now() > deadline {
                break found;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        let spared = super::touching(&dirs, since, &super::BTreeSet::from([inside.id()])).unwrap();
        let mut earlier = earlier;
        for child in [&mut inside, &mut outside, &mut earlier] {
            child.kill().unwrap();
            child.wait().unwrap();
        }
        assert_eq!(
            found.iter().map(|(pid, _)| *pid).collect::<Vec<_>>(),
            vec![inside.id()]
        );
        assert!(spared.is_empty(), "{spared:?}");
        // Read from `ps -o lstart`.
        assert!(super::started_at("Fri Nov 6 00:41:45 2026").is_some());
    }

    /// A listener is found by its port, as this process's; a closed one is
    /// not found at all.
    #[test]
    fn a_tcp_listener_is_found_by_its_port() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert_eq!(
            super::tcp_listeners(port).unwrap(),
            super::BTreeSet::from([std::process::id()])
        );
        drop(listener);
        assert!(super::tcp_listeners(port).unwrap().is_empty());
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

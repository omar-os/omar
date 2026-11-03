use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

pub(crate) const BACKEND_VERSION_PROBE_TIMEOUT: Duration = Duration::from_millis(1500);

pub(crate) fn backend_version_probe_succeeds(binary: &str) -> bool {
    command_succeeds_with_timeout(binary, &["--version"], BACKEND_VERSION_PROBE_TIMEOUT)
}

/// Capture only the local CLI version, with the same bounded probe used by
/// backend discovery. This says nothing about authentication or model access.
pub(crate) fn backend_version(binary: &str) -> Option<String> {
    use std::io::Read;
    let path = std::env::temp_dir().join(format!("omar-version-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .ok()?;
        let mut child = Command::new(binary)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(file)
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let start = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => break,
                Ok(None) if start.elapsed() < BACKEND_VERSION_PROBE_TIMEOUT => {
                    thread::sleep(Duration::from_millis(10))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
            }
        }
        let mut output = String::new();
        std::fs::File::open(&path)
            .ok()?
            .take(512)
            .read_to_string(&mut output)
            .ok()?;
        let version = output.lines().next()?.trim();
        (!version.is_empty()).then(|| version.to_owned())
    })();
    let _ = std::fs::remove_file(path);
    result
}

pub(crate) fn command_succeeds_with_timeout(
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> bool {
    let mut child = match Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return false,
    };

    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }

        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::Instant;

    #[cfg(unix)]
    fn executable_script(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let path = dir.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn command_timeout_kills_hanging_probe_quickly() {
        let temp = tempfile::tempdir().unwrap();
        let sleeper = executable_script(temp.path(), "slow-version", "sleep 5");

        let start = Instant::now();
        let ok = command_succeeds_with_timeout(
            sleeper.to_str().unwrap(),
            &["--version"],
            Duration::from_millis(100),
        );

        assert!(!ok);
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "probe should return soon after timeout"
        );
    }

    #[cfg(unix)]
    #[test]
    fn command_timeout_reports_successful_probe() {
        let temp = tempfile::tempdir().unwrap();
        let fast = executable_script(temp.path(), "fast-version", "exit 0");

        assert!(command_succeeds_with_timeout(
            fast.to_str().unwrap(),
            &["--version"],
            Duration::from_secs(2),
        ));
    }

    #[cfg(unix)]
    #[test]
    fn version_capture_records_output_and_keeps_unavailability_explicit() {
        let temp = tempfile::tempdir().unwrap();
        let fast = executable_script(temp.path(), "version", "echo test-cli-1.0");
        assert_eq!(
            backend_version(fast.to_str().unwrap()).as_deref(),
            Some("test-cli-1.0")
        );
        assert_eq!(backend_version("/nonexistent/omar-test-backend"), None);
    }
}

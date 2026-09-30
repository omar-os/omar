//! Local Docker Sandboxes execution. Only explicit workspace directories cross
//! the VM boundary; control messages use sbx exec's pipes, not a host listener.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Mutex};
use std::time::{Duration, Instant};

pub const FRAME_LIMIT: usize = 8 * 1024 * 1024;
pub const PROTOCOL: u32 = 1;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    /// A registry image pinned by SHA-256 digest.
    pub template: Option<String>,
}

pub fn validate_template(template: &str) -> Result<()> {
    let hash = template
        .rsplit_once("@sha256:")
        .filter(|(name, _)| !name.is_empty())
        .map(|(_, hash)| hash);
    anyhow::ensure!(
        hash.is_some_and(|h| h.len() == 64 && h.bytes().all(|c| c.is_ascii_hexdigit())),
        "sandbox template must be pinned by IMAGE@sha256:<digest>"
    );
    Ok(())
}

/// Source path inputs refer to the seeded copy, never to another host mount.
pub fn map_input_paths(
    value: &mut Value,
    ty: &str,
    workspace: &crate::workspace::Workspace,
    root: &Path,
) -> Result<()> {
    if let Some(inner) = ty.strip_prefix("option<").and_then(|s| s.strip_suffix('>')) {
        if !value.is_null() {
            map_input_paths(value, inner, workspace, root)?;
        }
    } else if let Some(inner) = ty.strip_prefix("list<").and_then(|s| s.strip_suffix('>')) {
        for value in value.as_array_mut().context("expected path list")? {
            map_input_paths(value, inner, workspace, root)?;
        }
    } else if ty == "path" {
        let path = PathBuf::from(value.as_str().context("expected path")?);
        anyhow::ensure!(
            !path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir)),
            "sandbox path inputs must not contain parent traversal"
        );
        let worktree = workspace.worktree(root);
        if path.is_relative() {
            *value = Value::String(worktree.join(path).to_string_lossy().into_owned());
            return Ok(());
        }
        if !path.starts_with(&worktree) && !path.starts_with(workspace.temp(root)) {
            let relative = path.strip_prefix(&workspace.source)
                .context("sandbox path input is outside its workspace and seeded source; explicit artifact transfer is required")?;
            *value = Value::String(worktree.join(relative).to_string_lossy().into_owned());
        }
    }
    Ok(())
}

pub fn name(workspace: &crate::workspace::Workspace) -> String {
    format!("omar-team-{}", workspace.id)
}

fn command() -> Command {
    let mut command =
        Command::new(std::env::var_os("OMAR_SBX_BIN").unwrap_or_else(|| "sbx".into()));
    command.env_remove("SSH_AUTH_SOCK").stdin(Stdio::null());
    command
}

pub fn read_frame(reader: &mut impl BufRead) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take((FRAME_LIMIT + 1) as u64)
        .read_until(b'\n', &mut bytes)?;
    anyhow::ensure!(
        bytes.len() <= FRAME_LIMIT,
        "sandbox frame exceeds size limit"
    );
    anyhow::ensure!(
        bytes.last() == Some(&b'\n'),
        "sandbox control stream closed or truncated"
    );
    Ok(bytes)
}

pub fn write_frame(writer: &mut impl Write, value: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    anyhow::ensure!(
        bytes.len() < FRAME_LIMIT,
        "sandbox frame exceeds size limit"
    );
    writer.write_all(&bytes)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

/// Bound control commands too: a stopped daemon must not hang deployment cleanup.
fn checked(mut command: Command, timeout: Duration) -> Result<Vec<u8>> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("start sbx; install Docker Sandboxes and run sbx login first")?;
    let stdout = child.stdout.take().context("sbx stdout")?;
    let stderr = child.stderr.take().context("sbx stderr")?;
    let collect = |stream: Box<dyn Read + Send>| {
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = stream.take(1024 * 1024 + 1).read_to_end(&mut bytes);
            let _ = sender.send(result.map(|_| bytes));
        });
        receiver
    };
    let output = collect(Box::new(stdout));
    let errors = collect(Box::new(stderr));
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            let bytes = errors.recv_timeout(timeout.saturating_sub(started.elapsed()))??;
            anyhow::ensure!(
                status.success(),
                "sbx failed: {}",
                String::from_utf8_lossy(&bytes)
            );
            let bytes = output.recv_timeout(timeout.saturating_sub(started.elapsed()))??;
            anyhow::ensure!(bytes.len() <= 1024 * 1024, "sbx output too large");
            return Ok(bytes);
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            bail!("sbx command timed out");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

pub fn create(name: &str, template: &str, worktree: &Path, temp: &Path) -> Result<()> {
    validate_template(template)?;
    anyhow::ensure!(
        worktree.is_absolute() && temp.is_absolute(),
        "sandbox mounts must be absolute"
    );
    let mut cmd = command();
    cmd.args([
        "create",
        "--name",
        name,
        "--template",
        template,
        "--skills",
        "off",
        "--pull",
        "missing",
        "--cpus",
        "2",
        "--memory",
        "2g",
        "shell",
    ])
    .arg(worktree)
    .arg(temp);
    checked(cmd, Duration::from_secs(180)).map(|_| ())
}

pub fn stop_all(names: &std::collections::BTreeMap<String, String>) -> Vec<String> {
    cleanup(names, Duration::from_secs(60), |args, timeout| {
        let mut cmd = command();
        cmd.args(args);
        checked(cmd, timeout)
    })
}

/// One deployment-wide budget and, on failure, one inventory check. A stalled
/// daemon must not make cleanup take a fresh minute for every team instance.
fn cleanup(
    names: &std::collections::BTreeMap<String, String>,
    budget: Duration,
    run: impl Fn(&[&str], Duration) -> Result<Vec<u8>>,
) -> Vec<String> {
    let started = Instant::now();
    let mut failures = std::collections::BTreeMap::new();
    for name in names.values() {
        let result = (|| -> Result<()> {
            // Persisted identities must never address arbitrary user sandboxes.
            let id = name
                .strip_prefix("omar-team-")
                .context("invalid sandbox name")?;
            anyhow::ensure!(
                uuid::Uuid::parse_str(id)?.to_string() == id,
                "invalid sandbox ID"
            );
            let left = budget.saturating_sub(started.elapsed());
            anyhow::ensure!(!left.is_zero(), "deployment sandbox cleanup timed out");
            run(&["stop", name], left)?;
            Ok(())
        })();
        if let Err(error) = result {
            failures.insert(name.clone(), format!("{name}: {error:#}"));
        }
    }
    let left = budget.saturating_sub(started.elapsed());
    if !failures.is_empty() && !left.is_zero() {
        // Creation may fail before allocation, or an operator may remove a VM.
        // Only a successful inventory proves absence, never a daemon/auth error.
        if let Ok(output) = run(&["ls", "--quiet"], left) {
            if let Ok(list) = std::str::from_utf8(&output) {
                failures.retain(|name, _| list.lines().any(|line| line.trim() == name));
            }
        }
    }
    failures.into_values().collect()
}

type Request = (Value, mpsc::Sender<Result<Value>>);

pub struct Worker {
    process: Mutex<Process>,
}
struct Process {
    child: Child,
    requests: mpsc::Sender<Request>,
    failed: bool,
}
impl Worker {
    pub fn start(name: &str, worktree: &Path, log: &Path) -> Result<Self> {
        if let Some(parent) = log.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut child = command()
            .args(["exec", "-i", "--workdir"])
            .arg(worktree)
            .args([name, "/usr/local/bin/omar", "sandbox-worker"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(std::fs::File::create(log)?)
            .spawn()
            .context("start sandbox worker")?;
        let mut input = child.stdin.take().context("sandbox stdin")?;
        let mut output = BufReader::new(child.stdout.take().context("sandbox stdout")?);
        let (send, receive) = mpsc::channel::<Request>();
        std::thread::spawn(move || {
            while let Ok((request, response)) = receive.recv() {
                let result = (|| {
                    write_frame(&mut input, &request)?;
                    Ok(serde_json::from_slice(&read_frame(&mut output)?)?)
                })();
                let failed = result.is_err();
                if response.send(result).is_err() || failed {
                    break;
                }
            }
        });
        Ok(Self {
            process: Mutex::new(Process {
                child,
                requests: send,
                failed: false,
            }),
        })
    }
    pub fn request(&self, request: Value, timeout: Duration) -> Result<Value> {
        let mut process = self
            .process
            .lock()
            .map_err(|_| anyhow::anyhow!("sandbox worker lock poisoned"))?;
        anyhow::ensure!(!process.failed, "sandbox worker is unavailable");
        let (send, receive) = mpsc::channel();
        process
            .requests
            .send((request, send))
            .context("sandbox worker stopped")?;
        let result = receive
            .recv_timeout(timeout)
            .context("sandbox worker timed out or disconnected")
            .and_then(|r| r);
        if result.is_err() {
            process.failed = true;
            let _ = process.child.kill();
        }
        let response = result?;
        if let Some(error) = response.get("error") {
            bail!("sandbox worker: {error}");
        }
        anyhow::ensure!(
            response.get("protocol").and_then(Value::as_u64) == Some(PROTOCOL as u64),
            "sandbox worker protocol mismatch; rebuild the template with this OMAR version"
        );
        Ok(response)
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Serialize, Deserialize)]
pub struct Init {
    pub protocol: u32,
    pub state: crate::topology::VmState,
    pub workspace: crate::workspace::Workspace,
    pub root: PathBuf,
    pub timeout: Duration,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_unpinned_templates_and_truncated_or_oversized_frames() {
        assert!(validate_template("omar:latest").is_err());
        assert!(validate_template(&format!("omar@sha256:{}", "a".repeat(64))).is_ok());
        assert!(read_frame(&mut &b"{}"[..]).is_err());
        let data = vec![b'x'; FRAME_LIMIT + 1];
        assert!(read_frame(&mut data.as_slice()).is_err());
        assert_eq!(read_frame(&mut &b"{}\n"[..]).unwrap(), b"{}\n");
    }
    #[test]
    fn paths_are_remapped_only_to_the_owning_workspace() {
        let root = Path::new("/omar");
        let workspace = crate::workspace::Workspace {
            version: 1,
            id: uuid::Uuid::new_v4().to_string(),
            ea_id: 0,
            deployment_id: "test".into(),
            instance: "parent.child".into(),
            parent_instance: Some("parent".into()),
            source: "/source".into(),
            restored_from: None,
        };
        let own = workspace.temp(root).join("scratch");
        let mut paths = serde_json::json!(["/source/file", null, own]);
        map_input_paths(&mut paths, "list<option<path>>", &workspace, root).unwrap();
        assert_eq!(
            paths[0],
            workspace.worktree(root).join("file").to_str().unwrap()
        );
        assert!(paths[1].is_null());
        assert_eq!(paths[2], own.to_str().unwrap());
        let mut relative = serde_json::json!("artifact");
        map_input_paths(&mut relative, "path", &workspace, root).unwrap();
        assert_eq!(
            relative,
            workspace.worktree(root).join("artifact").to_str().unwrap()
        );
        for path in [
            "/source/../secret",
            "/source-other/file",
            "/omar/workspace-history/key",
            "/omar/workspaces/another/worktree/file",
        ] {
            assert!(
                map_input_paths(&mut serde_json::json!(path), "path", &workspace, root).is_err(),
                "{path}"
            );
        }
    }

    #[test]
    fn worker_timeout_disables_further_requests() {
        let child = Command::new("sleep").arg("30").spawn().unwrap();
        let (send, _receive) = mpsc::channel();
        let worker = Worker {
            process: Mutex::new(Process {
                child,
                requests: send,
                failed: false,
            }),
        };
        let error = worker
            .request(Value::Null, Duration::from_millis(10))
            .unwrap_err();
        assert!(error.to_string().contains("timed out"));
        assert!(worker
            .request(Value::Null, Duration::from_secs(1))
            .unwrap_err()
            .to_string()
            .contains("unavailable"));
    }
    #[test]
    fn cleanup_uses_one_budget_and_one_inventory() {
        let names: std::collections::BTreeMap<_, _> = (0..3)
            .map(|n| (n.to_string(), format!("omar-team-{}", uuid::Uuid::new_v4())))
            .collect();
        let calls = std::cell::RefCell::new(Vec::new());
        let failed = cleanup(&names, Duration::from_secs(1), |args, _| {
            calls.borrow_mut().push(args[0].to_string());
            if args[0] == "stop" {
                bail!("not found or daemon unavailable");
            }
            Ok(format!("{}\n", names["1"]).into_bytes())
        });
        assert_eq!(failed.len(), 1);
        assert!(failed[0].contains(&names["1"]));
        assert_eq!(*calls.borrow(), ["stop", "stop", "stop", "ls"]);

        let count = std::cell::Cell::new(0);
        let started = Instant::now();
        let failed = cleanup(&names, Duration::from_millis(10), |_, _| {
            count.set(count.get() + 1);
            std::thread::sleep(Duration::from_millis(15));
            bail!("daemon stalled")
        });
        assert_eq!(count.get(), 1);
        assert_eq!(failed.len(), 3);
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}

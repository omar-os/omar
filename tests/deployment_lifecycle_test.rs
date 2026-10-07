//! Deployment lifecycle integration tests: the real binary, a stub-backed
//! never-ending program, a runtime session (and its tmux server) per test.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const TEAM: &str = "Pulse";

/// Bytecode the fake compiler passes through. A 300ms timer keeps the run
/// alive forever; the stub answers each tag.
const PROGRAM: &str = r#"{
  "version": 1,
  "team": "Pulse",
  "instructions": [
    {"op":"begin_plan","team":"Pulse"},
    {"op":"spawn_agent","name":"w","backend":"stub"},
    {"op":"declare_timer","name":"t","offset":0,"period":300000000},
    {"op":"define_port","kind":"output","name":"beat","type":"string"},
    {"op":"install_reaction","id":"reaction.0","agent":"w","triggers":["t"],"effects":["beat"],"contract":"beat","prompt":"Beat"},
    {"op":"commit_plan"}
  ]
}"#;

struct Harness {
    home: tempfile::TempDir,
    program: PathBuf,
    omarc: PathBuf,
    session: Value,
}

impl Harness {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("tempdir");
        let program = home.path().join("Pulse.omar");
        std::fs::write(&program, PROGRAM).expect("write program");
        // omarc is the Lean compiler, which a cargo test run does not build.
        // The program above is already bytecode, so a copy stands in.
        let omarc = home.path().join("omarc");
        std::fs::write(&omarc, "#!/bin/sh\ncp \"$1\" \"$2\"\n").expect("write omarc");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&omarc).expect("stat omarc").permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&omarc, permissions).expect("chmod omarc");
        }
        // One runtime session per harness, on its own tmux server.
        let mut up = Command::new(env!("CARGO_BIN_EXE_omar"));
        for key in [
            "TMUX",
            "TMUX_PANE",
            "OMAR_SESSION_ID",
            "OMAR_STATE_DIR",
            "OMAR_TMUX_SERVER",
            "OMAR_HOME",
        ] {
            up.env_remove(key);
        }
        let output = up
            .args(["up", "--no-ea", "--json"])
            .env("HOME", home.path())
            .env("OMARC_BIN", &omarc)
            .current_dir(home.path())
            .output()
            .expect("run omar up");
        assert!(
            output.status.success(),
            "omar up failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let session: Value = serde_json::from_slice(&output.stdout).expect("session record");
        Self {
            home,
            program,
            omarc,
            session,
        }
    }

    fn id(&self) -> &str {
        self.session["name"].as_str().expect("session name")
    }

    fn tmux_server(&self) -> &str {
        self.session["tmux_server"].as_str().expect("tmux server")
    }

    fn omar(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_omar"));
        for key in [
            "TMUX",
            "TMUX_PANE",
            "OMAR_SESSION_ID",
            "OMAR_STATE_DIR",
            "OMAR_TMUX_SERVER",
            "OMAR_HOME",
        ] {
            cmd.env_remove(key);
        }
        cmd.args(["-s", self.id()])
            .env("HOME", self.home.path())
            .env("OMARC_BIN", &self.omarc);
        cmd
    }

    /// Admit the program; the runtime runs it in the background.
    fn start_run(&self) -> Value {
        let output = self
            .omar()
            .args(["--json", "run", self.program.to_str().unwrap()])
            .output()
            .expect("run omar run");
        assert!(
            output.status.success(),
            "run failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).expect("run record")
    }

    /// The run record of `omar status <TEAM>`, or null before it exists.
    fn status(&self) -> Value {
        let output = self
            .omar()
            .args(["status", TEAM])
            .output()
            .expect("run omar status");
        serde_json::from_slice(&output.stdout).unwrap_or(Value::Null)
    }

    fn wait_for_status(&self, status: &str, timeout: Duration) {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if self.status()["status"] == status {
                return;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        panic!(
            "run never reached {status}; last status:\n{}",
            self.status()
        );
    }

    fn sessions(&self) -> Vec<String> {
        let output = Command::new("tmux")
            .args([
                "-L",
                self.tmux_server(),
                "list-sessions",
                "-F",
                "#{session_name}",
            ])
            .output()
            .expect("run tmux");
        // No server left is the cleanest possible answer.
        if !output.status.success() {
            return Vec::new();
        }
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn deployment_dir(&self) -> PathBuf {
        // The default EA is created on first use with id 0.
        PathBuf::from(self.session["directory"].as_str().expect("directory"))
            .join("ea")
            .join("0")
            .join("topologies")
            .join(TEAM)
    }

    fn record(&self) -> String {
        std::fs::read_to_string(self.deployment_dir().join("deployment.json"))
            .expect("read deployment record")
    }

    fn down(&self, force: bool) -> std::process::Output {
        let mut cmd = self.omar();
        cmd.args(["down", "--timeout", "30"]);
        if force {
            cmd.arg("--force");
        }
        cmd.output().expect("run omar down")
    }

    fn kill_server(&self) {
        let _ = Command::new("tmux")
            .args(["-L", self.tmux_server(), "kill-server"])
            .output();
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.down(true);
        self.kill_server();
    }
}

fn agent_sessions(harness: &Harness) -> Vec<String> {
    harness
        .sessions()
        .into_iter()
        .filter(|name| name.contains("-w"))
        .collect()
}

/// A run is admitted before its agent panes exist; wait for them.
fn wait_for_agents(harness: &Harness) {
    let start = Instant::now();
    while agent_sessions(harness).is_empty() {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "agent session never appeared"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn assert_dir_has(dir: &Path, name: &str) {
    assert!(
        dir.join(name).exists(),
        "{name} missing in {}",
        dir.display()
    );
}

#[test]
fn stop_terminates_gracefully_and_leaves_no_orphans() {
    if Command::new("tmux").arg("-V").output().is_err() {
        eprintln!("tmux not installed; skipping");
        return;
    }
    let harness = Harness::new();
    harness.start_run();
    harness.wait_for_status("running", Duration::from_secs(30));
    wait_for_agents(&harness);

    // A second run of the same team is refused while the first is alive.
    let duplicate = harness
        .omar()
        .args(["run", harness.program.to_str().unwrap()])
        .output()
        .expect("run duplicate");
    assert!(!duplicate.status.success());
    assert!(String::from_utf8_lossy(&duplicate.stderr).contains("already has an active run"));

    let stop = harness
        .omar()
        .args(["stop", TEAM])
        .output()
        .expect("run omar stop");
    assert!(
        stop.status.success(),
        "stop failed: {}",
        String::from_utf8_lossy(&stop.stderr)
    );
    harness.wait_for_status("stopped", Duration::from_secs(30));
    assert!(harness.record().contains("TERMINATED"));
    assert!(agent_sessions(&harness).is_empty(), "orphan agent session");

    let dir = harness.deployment_dir();
    assert_dir_has(&dir, "outputs.json");
    assert_dir_has(&dir, "state.json");
    assert_dir_has(&dir.join("logs"), "w.txt");
    let down = harness.down(false);
    assert!(
        down.status.success(),
        "{}",
        String::from_utf8_lossy(&down.stderr)
    );
}

#[test]
fn forced_shutdown_sweeps_a_running_topology() {
    if Command::new("tmux").arg("-V").output().is_err() {
        eprintln!("tmux not installed; skipping");
        return;
    }
    let harness = Harness::new();
    harness.start_run();
    harness.wait_for_status("running", Duration::from_secs(30));
    wait_for_agents(&harness);

    // `kill` is for standalone agents; a runtime-owned topology ends with
    // `stop`, or with the whole runtime.
    let kill = harness
        .omar()
        .args(["kill", TEAM])
        .output()
        .expect("run omar kill");
    assert!(!kill.status.success());
    assert!(String::from_utf8_lossy(&kill.stderr).contains("use stop"));

    let down = harness.down(true);
    assert!(
        down.status.success(),
        "{}",
        String::from_utf8_lossy(&down.stderr)
    );
    assert!(agent_sessions(&harness).is_empty(), "orphan agent session");
    assert!(
        harness.sessions().is_empty(),
        "the runtime's tmux server outlived it"
    );
}

#[test]
fn a_dead_runtime_is_reported_and_never_signalled_blindly() {
    if Command::new("tmux").arg("-V").output().is_err() {
        eprintln!("tmux not installed; skipping");
        return;
    }
    let harness = Harness::new();
    harness.start_run();
    harness.wait_for_status("running", Duration::from_secs(30));
    wait_for_agents(&harness);

    // A hard kill of the runtime is a crash: nothing writes an ending.
    let pid = harness.session["pid"].as_u64().expect("pid");
    let _ = Command::new("kill").args(["-9", &pid.to_string()]).output();
    let start = Instant::now();
    let state = loop {
        let output = harness
            .omar()
            .args(["ls", "--json"])
            .output()
            .expect("run omar ls");
        let listed: Value = serde_json::from_slice(&output.stdout).unwrap_or(Value::Null);
        let state = listed
            .as_array()
            .into_iter()
            .flatten()
            .find(|s| s["name"] == harness.session["name"])
            .map(|s| s["state"].as_str().unwrap_or_default().to_string())
            .unwrap_or_default();
        if state == "stale" || start.elapsed() > Duration::from_secs(10) {
            break state;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    assert_eq!(state, "stale");
    assert!(
        !agent_sessions(&harness).is_empty(),
        "a crash leaves sessions behind"
    );
    // A stale record is never permission to signal a pid.
    let down = harness.down(false);
    assert!(!down.status.success());
    harness.kill_server();
    assert!(agent_sessions(&harness).is_empty(), "orphan agent session");
}

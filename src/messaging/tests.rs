use super::*;
use runtime::{OmarRuntime, Run};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::sync::Notify;

pub(super) fn fixture() -> Config {
    let mut c: Config = serde_json::from_value(json!({
        "runtimeUrl":"http://127.0.0.1:8000/chats/test", "platform":"mock", "accountId":"account",
        "allowedUsers":["user"], "allowedConversations":["room"], "command":"demo", "outputPort":"run.result",
        "programPath":"demo.omar"
    })).unwrap();
    c.request = json!({"program":"fixed source","raw_inputs":["run.request=fixed"]});
    c
}
pub(super) fn incoming() -> Incoming {
    serde_json::from_value(json!({"platform":"mock","accountId":"account","senderId":"user","eventId":"event",
        "command":"run demo","destination":{"platform":"mock","accountId":"account","conversationId":"room"}})).unwrap()
}
pub(super) struct FakeRuntime {
    pub starts: AtomicUsize,
    pub mode: &'static str,
    pub gate: Option<Arc<Notify>>,
}
impl Default for FakeRuntime {
    fn default() -> Self {
        Self {
            starts: AtomicUsize::new(0),
            mode: "completed",
            gate: None,
        }
    }
}
impl Runtime for FakeRuntime {
    async fn start(&self, _: &Value) -> Result<Run> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        if self.mode == "uncertain" {
            anyhow::bail!("lost response");
        }
        if self.mode == "500" || self.mode == "409" || self.mode == "400" || self.mode == "503-id" {
            return Err(RuntimeError {
                status: if self.mode == "503-id" {
                    503
                } else {
                    self.mode.parse().unwrap()
                },
                run_id: (self.mode == "503-id").then(|| "id".into()),
            }
            .into());
        }
        Ok(Run {
            run_id: "id".into(),
            status: "running".into(),
            outputs: Default::default(),
        })
    }
    async fn wait(&self, mut run: Run, _: u64) -> Result<Run> {
        if let Some(gate) = &self.gate {
            gate.notified().await;
        }
        if self.mode == "monitor" {
            anyhow::bail!("lost monitor");
        }
        run.status = match self.mode {
            "failed" | "paused" | "stopped" => self.mode,
            _ => "completed",
        }
        .into();
        if self.mode != "missing" {
            run.outputs.insert(
                "run.result".into(),
                match self.mode {
                    "long" => json!("<🌍>".repeat(4000)),
                    "object" => json!({"ok":true}),
                    _ => json!("selected result"),
                },
            );
        }
        run.outputs
            .insert("private".into(), json!("secret other port"));
        Ok(run)
    }
}
#[derive(Default)]
pub(super) struct FakeMessenger {
    pub replies: Mutex<Vec<(Destination, Reply)>>,
    pub fail: bool,
}
impl Messenger for FakeMessenger {
    fn usage(&self, command: &str) -> String {
        format!("Mock: run {command}")
    }
    async fn send(&self, d: &Destination, r: Reply) -> Result<()> {
        self.replies.lock().unwrap().push((d.clone(), r));
        anyhow::ensure!(!self.fail, "send failed");
        Ok(())
    }
}
fn gateway(mode: &'static str) -> Gateway<FakeRuntime, FakeMessenger> {
    Gateway::new(
        fixture(),
        FakeRuntime {
            mode,
            ..Default::default()
        },
        FakeMessenger::default(),
    )
}

#[tokio::test]
async fn duplicate_event_runs_once_and_selects_only_configured_output() {
    let g = gateway("completed");
    g.handle(incoming()).await;
    g.handle(incoming()).await;
    assert_eq!(g.runtime.starts.load(Ordering::SeqCst), 1);
    let replies = g.messenger.replies.lock().unwrap();
    assert_eq!(replies.len(), 2);
    assert_eq!(
        replies[1].1.text,
        "OMAR run id: completed.\nselected result"
    );
    assert_eq!(replies[1].1.run_id.as_deref(), Some("id"));
    assert_eq!(replies[1].0.thread_id, None);
}
#[tokio::test]
async fn platform_account_sender_and_destination_are_authorized_before_dedup() {
    for index in 0..7 {
        let g = gateway("completed");
        let mut m = incoming();
        match index {
            0 => m.platform = "other".into(),
            1 => m.account_id = "other".into(),
            2 => m.sender_id = "other".into(),
            3 => m.destination.platform = "other".into(),
            4 => m.destination.account_id = "other".into(),
            5 => m.destination.conversation_id = "other".into(),
            _ => m.event_id.clear(),
        }
        g.handle(m).await;
        assert_eq!(g.runtime.starts.load(Ordering::SeqCst), 0);
        assert!(g.messenger.replies.lock().unwrap().is_empty());
        g.handle(incoming()).await;
        assert_eq!(g.runtime.starts.load(Ordering::SeqCst), 1);
    }
}
#[tokio::test]
async fn distinct_mock_platforms_and_optional_threads_use_same_core() {
    for platform in ["mock-a", "mock-b"] {
        let mut c = fixture();
        c.platform = platform.into();
        let g = Gateway::new(c, FakeRuntime::default(), FakeMessenger::default());
        let mut m = incoming();
        m.platform = platform.into();
        m.destination.platform = platform.into();
        m.destination.thread_id = Some("parent".into());
        g.handle(m.clone()).await;
        assert_eq!(g.messenger.replies.lock().unwrap()[1].0, m.destination);
    }
}
#[tokio::test]
async fn adapter_owns_usage_and_receives_full_output() {
    let g = gateway("long");
    let mut m = incoming();
    m.command = "help".into();
    g.handle(m).await;
    assert_eq!(g.runtime.starts.load(Ordering::SeqCst), 0);
    assert_eq!(
        g.messenger.replies.lock().unwrap()[0].1.text,
        "Mock: run demo"
    );
    let mut m = incoming();
    m.event_id = "second".into();
    g.handle(m).await;
    assert!(
        g.messenger
            .replies
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .1
            .text
            .len()
            > 10000
    );
}
#[tokio::test]
async fn distinct_event_while_running_receives_busy_without_queuing() {
    let notify = Arc::new(Notify::new());
    let g = Arc::new(Gateway::new(
        fixture(),
        FakeRuntime {
            gate: Some(notify.clone()),
            ..Default::default()
        },
        FakeMessenger::default(),
    ));
    let cloned = g.clone();
    let task = tokio::spawn(async move { cloned.handle(incoming()).await });
    while g.messenger.replies.lock().unwrap().is_empty() {
        tokio::task::yield_now().await;
    }
    let mut m = incoming();
    m.event_id = "second".into();
    g.handle(m).await;
    assert!(g.messenger.replies.lock().unwrap()[1]
        .1
        .text
        .contains("busy"));
    notify.notify_one();
    task.await.unwrap();
    assert_eq!(g.runtime.starts.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn dedup_capacity_retains_unexpired_events_and_allows_expiry() {
    let mut g = gateway("completed");
    g.dedup_limit = 1;
    g.handle(incoming()).await;
    let mut m = incoming();
    m.event_id = "second".into();
    g.handle(m.clone()).await;
    g.handle(incoming()).await;
    assert_eq!(g.runtime.starts.load(Ordering::SeqCst), 1);
    for expires in g.state.lock().unwrap().seen.values_mut() {
        *expires = Instant::now();
    }
    g.handle(m).await;
    assert_eq!(g.runtime.starts.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn delivery_failure_never_repeats_execution() {
    let g = Gateway::new(
        fixture(),
        FakeRuntime::default(),
        FakeMessenger {
            fail: true,
            ..Default::default()
        },
    );
    g.handle(incoming()).await;
    g.handle(incoming()).await;
    assert_eq!(g.runtime.starts.load(Ordering::SeqCst), 1);
    assert_eq!(g.messenger.replies.lock().unwrap().len(), 2);
}
#[tokio::test]
async fn uncertain_admission_and_monitoring_block_later_starts() {
    for mode in ["uncertain", "500", "503-id", "monitor"] {
        let g = gateway(mode);
        g.handle(incoming()).await;
        let mut m = incoming();
        m.event_id = "second".into();
        g.handle(m).await;
        assert_eq!(g.runtime.starts.load(Ordering::SeqCst), 1, "{mode}");
        assert!(g
            .messenger
            .replies
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .1
            .text
            .contains("operator checking"));
    }
}
#[tokio::test]
async fn known_rejection_does_not_latch_uncertainty() {
    for mode in ["400", "409"] {
        let g = gateway(mode);
        g.handle(incoming()).await;
        let mut m = incoming();
        m.event_id = "second".into();
        g.handle(m).await;
        assert_eq!(g.runtime.starts.load(Ordering::SeqCst), 2);
    }
}
#[tokio::test]
async fn terminal_failure_states_are_not_reported_as_success() {
    for mode in ["failed", "paused", "stopped"] {
        let g = gateway(mode);
        g.handle(incoming()).await;
        let replies = g.messenger.replies.lock().unwrap();
        let text = &replies.last().unwrap().1.text;
        assert!(text.contains(mode));
        assert!(!text.contains("selected result"));
    }
}
#[tokio::test]
async fn missing_and_structured_outputs_are_reported_without_other_ports() {
    for mode in ["missing", "object"] {
        let g = gateway(mode);
        g.handle(incoming()).await;
        let replies = g.messenger.replies.lock().unwrap();
        let text = &replies.last().unwrap().1.text;
        assert!(text.contains(if mode == "missing" {
            "is missing"
        } else {
            "{\"ok\":true}"
        }));
        assert!(!text.contains("secret other port"));
    }
}
#[test]
fn config_rejects_unscoped_remote_and_credential_urls() {
    for url in [
        "https://localhost/chats/x",
        "http://example.com/chats/x",
        "http://localhost/",
        "http://localhost/chats/a/b",
        "http://user@localhost/chats/x",
        "http://localhost/chats/x?x=1",
        "http://localhost/chats/x#frag",
        "http://localhost/chats/x//",
    ] {
        let mut c = fixture();
        c.runtime_url = url.into();
        assert!(c.validate().is_err(), "{url}");
    }
    for url in [
        "http://localhost/chats/x",
        "http://127.0.0.1:9000/chats/x/",
        "http://[::1]/chats/x",
    ] {
        let mut c = fixture();
        c.runtime_url = url.into();
        c.validate().unwrap();
    }
}
#[test]
fn config_rejects_empty_policy_invalid_command_timeout_and_raw_inputs() {
    for index in 0..5 {
        let mut c = fixture();
        match index {
            0 => c.allowed_users.clear(),
            1 => c.allowed_conversations = vec!["".into()],
            2 => c.timeout_seconds = 0,
            3 => c.command = "../path".into(),
            _ => c.output_port.clear(),
        }
        assert!(c.validate().is_err());
    }
}
#[test]
fn legacy_config_resolves_bundle_relative_to_config_without_changing_inputs() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("demo.omar"), "main {}").unwrap();
    std::fs::write(dir.path().join("schema.json"), "{}").unwrap();
    let path = dir.path().join("config.json");
    let value = json!({"runtimeUrl":"http://localhost/chats/x","workspaceId":"T1","allowedUsers":["U1"],"allowedChannels":["C1"],"command":"demo","outputPort":"out","programPath":"demo.omar","rawInputs":["in=hello"],"files":{"schemas/type.json":"schema.json"}});
    std::fs::write(&path, value.to_string()).unwrap();
    let c = Config::load(&path).unwrap();
    assert_eq!(c.account_id, "T1");
    assert_eq!(c.allowed_conversations, ["C1"]);
    assert_eq!(c.request["files"]["schemas/type.json"], "{}");
    assert_eq!(c.request["raw_inputs"][0], "in=hello");
    assert_eq!(c.request["replace"], false);
    assert_eq!(
        serde_json::from_slice::<Value>(&std::fs::read(path).unwrap()).unwrap(),
        value
    );
}

// Invoked by the Python CI harness against an isolated, already running OMAR.
#[tokio::test]
#[ignore = "requires test runtime; run through tests/ci/slack_workflow.py"]
async fn real_runtime() {
    let path = std::env::var("OMAR_MESSAGING_TEST_CONFIG").unwrap();
    let c = Config::load(std::path::Path::new(&path)).unwrap();
    let runtime = OmarRuntime::new(c.runtime_url.clone()).unwrap();
    let g = Gateway::new(c.clone(), runtime, FakeMessenger::default());
    let m = Incoming {
        platform: c.platform.clone(),
        account_id: c.account_id.clone(),
        sender_id: c.allowed_users[0].clone(),
        event_id: "fixture-event".into(),
        command: format!("run {}", c.command),
        destination: Destination {
            platform: c.platform.clone(),
            account_id: c.account_id.clone(),
            conversation_id: c.allowed_conversations[0].clone(),
            thread_id: (c.platform == "slack").then(|| "parent".into()),
        },
    };
    let m = if c.platform == "slack" {
        slack::normalize(&json!({"type":"event_callback","team_id":c.account_id,"event_id":"fixture-event","event":{"type":"app_mention","user":m.sender_id,"channel":m.destination.conversation_id,"ts":"message","thread_ts":"parent","text":format!("<@BOT> run {}",c.command)}}),"BOT").unwrap()
    } else {
        m
    };
    g.handle(m.clone()).await;
    g.handle(m.clone()).await;
    let id = {
        let replies = g.messenger.replies.lock().unwrap();
        assert_eq!(replies.len(), 2);
        assert_eq!(replies[1].0, m.destination);
        assert!(
            replies[1].1.text.contains(": completed.\n"),
            "{}",
            replies[1].1.text
        );
        replies[1].1.run_id.clone().unwrap()
    };
    let runs = g.runtime.call("/v1/runs", None).await.unwrap();
    assert_eq!(
        runs["runs"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["run_id"] == id)
            .count(),
        1
    );
    let expected = std::env::var("OMAR_MESSAGING_TEST_OUTPUT").unwrap();
    assert!(g.messenger.replies.lock().unwrap()[1]
        .1
        .text
        .ends_with(&expected));
}

#[test]
fn research_launcher_forwards_arguments_without_global_configuration_changes() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let fake = dir.path().join("fake-codex");
    std::fs::write(&fake, "#!/bin/sh\nprintf '%s\\n' \"$@\"\n").unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();
    let output = std::process::Command::new("sh")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/integrations/slack/research-bin/codex"
        ))
        .args(["--model", "fixture"])
        .env("SLACK_RESEARCH_CODEX_BIN", fake)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "-c\ncheck_for_update_on_startup=false\n--model\nfixture\n"
    );
}

#[test]
fn cli_preserves_config_argument_and_supports_login_logout() {
    use clap::Parser;
    for args in [
        vec!["omar", "messaging", "slack", "config.json"],
        vec!["omar", "messaging", "slack", "login"],
        vec!["omar", "messaging", "slack", "logout"],
    ] {
        assert!(crate::Cli::try_parse_from(args).is_ok());
    }
    assert!(crate::Cli::try_parse_from(["omar", "messaging", "slack"]).is_err());
    assert!(
        crate::Cli::try_parse_from(["omar", "messaging", "slack", "config.json", "login"]).is_err()
    );
}

fn registry_config() -> Config {
    let mut config: Config = serde_json::from_value(json!({
        "runtimeUrl":"http://localhost/chats/test", "platform":"mock", "accountId":"account",
        "allowedUsers":["user"],"allowedConversations":["room"],"workflows":{}
    }))
    .unwrap();
    config.workflows = serde_json::from_value(json!({
        "first": {"description":"First approved workflow", "programPath":"first.omar", "outputPort":"first.result", "timeoutSeconds":10},
        "second": {"description":"Second approved workflow", "programPath":"second.omar", "outputPort":"second.result", "timeoutSeconds":20}
    })).unwrap();
    config.workflows.get_mut("first").unwrap().request =
        json!({"program":"FIRST", "raw_inputs":["in=one"], "files":{"schema.json":"first schema"}});
    config.workflows.get_mut("second").unwrap().request =
        json!({"program":"SECOND", "inputs":{"count":2}, "files":{"schema.json":"second schema"}});
    config
}
#[derive(Default)]
struct RegistryRuntime {
    requests: Mutex<Vec<Value>>,
    timeouts: Mutex<Vec<u64>>,
}
impl Runtime for RegistryRuntime {
    async fn start(&self, request: &Value) -> Result<Run> {
        self.requests.lock().unwrap().push(request.clone());
        Ok(Run {
            run_id: request["program"].as_str().unwrap().into(),
            status: "running".into(),
            outputs: Default::default(),
        })
    }
    async fn wait(&self, mut run: Run, timeout: u64) -> Result<Run> {
        self.timeouts.lock().unwrap().push(timeout);
        run.status = "completed".into();
        run.outputs
            .insert("first.result".into(), json!("first output"));
        run.outputs
            .insert("second.result".into(), json!("second output"));
        Ok(run)
    }
}
#[tokio::test]
async fn registry_runs_different_workflows_sequentially_with_own_inputs_outputs_and_timeouts() {
    let config = registry_config();
    let g = Gateway::new(
        config.clone(),
        RegistryRuntime::default(),
        FakeMessenger::default(),
    );
    for (index, name) in ["first", "second"].iter().enumerate() {
        let mut m = incoming();
        m.event_id = format!("event-{index}");
        m.command = format!("run {name}");
        m.destination.thread_id = Some(format!("thread-{name}"));
        g.handle(m.clone()).await;
        g.handle(m.clone()).await;
        let replies = g.messenger.replies.lock().unwrap();
        let (destination, reply) = replies.last().unwrap();
        assert_eq!(destination, &m.destination);
        assert!(reply.text.ends_with(&format!("{name} output")));
        assert!(!reply.text.contains(if *name == "first" {
            "second output"
        } else {
            "first output"
        }));
    }
    assert_eq!(
        *g.runtime.requests.lock().unwrap(),
        vec![
            config.workflows["first"].request.clone(),
            config.workflows["second"].request.clone()
        ]
    );
    assert_eq!(*g.runtime.timeouts.lock().unwrap(), vec![10, 20]);
}
#[tokio::test]
async fn registry_list_is_authorized_deterministic_and_does_not_start_work() {
    let g = Gateway::new(
        registry_config(),
        FakeRuntime::default(),
        FakeMessenger::default(),
    );
    let mut m = incoming();
    m.command = "list".into();
    let mut unauthorized = m.clone();
    unauthorized.sender_id = "other".into();
    g.handle(unauthorized).await;
    assert!(g.messenger.replies.lock().unwrap().is_empty());
    g.handle(m.clone()).await;
    g.handle(m).await;
    assert_eq!(g.runtime.starts.load(Ordering::SeqCst), 0);
    let replies = g.messenger.replies.lock().unwrap();
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].1.text,"Available workflows:\n- first — First approved workflow\n- second — Second approved workflow");
    assert!(!replies[0].1.text.contains(".omar"));
}
#[tokio::test]
async fn registry_unknown_names_and_chat_supplied_arguments_never_start() {
    let g = Gateway::new(
        registry_config(),
        FakeRuntime::default(),
        FakeMessenger::default(),
    );
    for (index, command) in [
        "run missing",
        "run ../first.omar",
        "run first extra-input",
        "run",
        "help",
    ]
    .iter()
    .enumerate()
    {
        let mut m = incoming();
        m.command = command.to_string();
        m.event_id = index.to_string();
        g.handle(m).await;
    }
    assert_eq!(g.runtime.starts.load(Ordering::SeqCst), 0);
    for (_, reply) in g.messenger.replies.lock().unwrap().iter() {
        assert!(reply.text.contains("list"));
    }
}
#[tokio::test]
async fn registry_has_one_busy_state_across_names_but_list_still_works() {
    let notify = Arc::new(Notify::new());
    let g = Arc::new(Gateway::new(
        registry_config(),
        FakeRuntime {
            gate: Some(notify.clone()),
            ..Default::default()
        },
        FakeMessenger::default(),
    ));
    let clone = g.clone();
    let task = tokio::spawn(async move {
        let mut m = incoming();
        m.command = "run first".into();
        clone.handle(m).await;
    });
    while g.messenger.replies.lock().unwrap().is_empty() {
        tokio::task::yield_now().await;
    }
    let mut m = incoming();
    m.command = "run second".into();
    m.event_id = "second".into();
    g.handle(m.clone()).await;
    assert!(g
        .messenger
        .replies
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .1
        .text
        .contains("busy"));
    let mut list = m.clone();
    list.event_id = "list".into();
    list.command = "list".into();
    g.handle(list).await;
    assert!(g
        .messenger
        .replies
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .1
        .text
        .contains("Available workflows"));
    assert_eq!(g.runtime.starts.load(Ordering::SeqCst), 1);
    notify.notify_one();
    task.await.unwrap();
    // Busy requests are not queued or automatically replayed after completion.
    g.handle(m.clone()).await;
    assert_eq!(g.runtime.starts.load(Ordering::SeqCst), 1);
    m.event_id = "new-second-request".into();
    notify.notify_one();
    g.handle(m).await;
    assert_eq!(g.runtime.starts.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn registry_uncertainty_blocks_all_names_not_just_the_first() {
    let g = Gateway::new(
        registry_config(),
        FakeRuntime {
            mode: "uncertain",
            ..Default::default()
        },
        FakeMessenger::default(),
    );
    let mut m = incoming();
    m.command = "run first".into();
    g.handle(m.clone()).await;
    m.command = "run second".into();
    m.event_id = "second".into();
    g.handle(m).await;
    assert_eq!(g.runtime.starts.load(Ordering::SeqCst), 1);
    assert!(g
        .messenger
        .replies
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .1
        .text
        .contains("operator checking"));
}
#[test]
fn registry_loads_each_source_bundle_and_rejects_ambiguous_or_invalid_entries() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("one.omar"), "FIRST").unwrap();
    std::fs::write(dir.path().join("two.omar"), "SECOND").unwrap();
    std::fs::write(dir.path().join("schema.json"), "{schema}").unwrap();
    let path = dir.path().join("registry.json");
    let base = json!({"runtimeUrl":"http://localhost/chats/x","platform":"mock","accountId":"account","allowedUsers":["user"],"allowedConversations":["room"],"workflows":{
        "first":{"programPath":"one.omar","outputPort":"one.out","rawInputs":["in=one"]},
        "second":{"programPath":"two.omar","outputPort":"two.out","files":{"schemas/type.json":"schema.json"},"timeoutSeconds":25}
    }});
    std::fs::write(&path, base.to_string()).unwrap();
    let config = Config::load(&path).unwrap();
    assert_eq!(config.workflows["first"].request["program"], "FIRST");
    assert_eq!(
        config.workflows["second"].request["files"]["schemas/type.json"],
        "{schema}"
    );
    assert_eq!(config.select("second").unwrap().timeout_seconds, 25);
    for mutation in 0..7 {
        let mut value = base.clone();
        match mutation {
            0 => value["command"] = json!("legacy"),
            1 => value["workflows"]["../escape"] = value["workflows"]["first"].clone(),
            2 => value["workflows"]["first"]["programPath"] = json!("missing.omar"),
            3 => value["workflows"]["first"]["timeoutSeconds"] = json!(0),
            4 => value["workflows"]["first"]["outputPort"] = json!(""),
            5 => value["workflows"]["first"]["rawInputs"] = json!(["not-an-assignment"]),
            _ => value["workflows"] = json!({}),
        }
        std::fs::write(&path, value.to_string()).unwrap();
        assert!(Config::load(&path).is_err(), "mutation {mutation}");
    }
}

#[tokio::test]
#[ignore = "requires test runtime; run through tests/ci/slack_workflow.py"]
async fn real_registry() {
    let path = std::env::var("OMAR_MESSAGING_TEST_CONFIG").unwrap();
    let c = Config::load(std::path::Path::new(&path)).unwrap();
    let expected: std::collections::BTreeMap<String, String> =
        serde_json::from_str(&std::env::var("OMAR_MESSAGING_REGISTRY_EXPECTED").unwrap()).unwrap();
    let runtime = OmarRuntime::new(c.runtime_url.clone()).unwrap();
    let g = Gateway::new(c.clone(), runtime, FakeMessenger::default());
    let mut m = Incoming {
        platform: c.platform.clone(),
        account_id: c.account_id.clone(),
        sender_id: c.allowed_users[0].clone(),
        event_id: "list".into(),
        command: "list".into(),
        destination: Destination {
            platform: c.platform.clone(),
            account_id: c.account_id.clone(),
            conversation_id: c.allowed_conversations[0].clone(),
            thread_id: Some("registry-thread".into()),
        },
    };
    g.handle(m.clone()).await;
    for (name, output) in expected {
        assert!(g.messenger.replies.lock().unwrap()[0]
            .1
            .text
            .contains(&name));
        m.event_id = format!("run-{name}");
        m.command = format!("run {name}");
        g.handle(m.clone()).await;
        g.handle(m.clone()).await;
        let replies = g.messenger.replies.lock().unwrap();
        let (destination, reply) = replies.last().unwrap();
        assert_eq!(destination, &m.destination);
        assert!(reply.text.contains(": completed.\n"), "{}", reply.text);
        assert!(reply.text.ends_with(&output), "{}", reply.text);
    }
    let replies = g.messenger.replies.lock().unwrap();
    assert_eq!(replies.len(), 1 + c.workflows.len() * 2);
    let ids: std::collections::HashSet<_> = replies
        .iter()
        .filter_map(|(_, r)| r.run_id.as_ref())
        .collect();
    assert_eq!(ids.len(), c.workflows.len());
}

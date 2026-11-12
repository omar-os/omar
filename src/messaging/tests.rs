use super::*;
use runtime::Run;
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::sync::Notify;

pub(super) fn fixture() -> Config {
    let c: Config = serde_json::from_value(json!({
        "runtimeUrl":"http://127.0.0.1:8000/chats/test", "platform":"mock", "accountId":"account",
        "allowedUsers":["user"], "allowedConversations":["room"]
    }))
    .unwrap();

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
    async fn approved(&self) -> Result<Vec<runtime::ApprovedWorkflow>> {
        Ok(vec![
            runtime::ApprovedWorkflow {
                name: "demo".into(),
                request: json!({"program":"fixed source","raw_inputs":["run.request=fixed"]}),
                output_port: "run.result".into(),
                timeout_seconds: 60,
            },
            runtime::ApprovedWorkflow {
                name: "first".into(),
                request: json!({"program":"FIRST","raw_inputs":["in=one"],"files":{"schema.json":"first schema"}}),
                output_port: "first.result".into(),
                timeout_seconds: 10,
            },
            runtime::ApprovedWorkflow {
                name: "second".into(),
                request: json!({"program":"SECOND","inputs":{"count":2},"files":{"schema.json":"second schema"}}),
                output_port: "second.result".into(),
                timeout_seconds: 20,
            },
        ])
    }

    async fn converse(&self, sender: &str, text: &str) -> Result<String> {
        Ok(format!("Answer to {sender}: {text}"))
    }

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
            conversation_id: None,
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
        "Mock: run <workflow>. Use list to see saved workflows."
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
fn config_rejects_empty_user_and_channel_allowlists() {
    for index in 0..2 {
        let mut c = fixture();
        match index {
            0 => c.allowed_users.clear(),
            1 => c.allowed_conversations = vec!["".into()],
            _ => unreachable!(),
        }
        assert!(c.validate().is_err());
    }
}

// Invoked by the Python CI harness against an isolated, already running OMAR.

#[test]
fn cli_preserves_config_argument_and_supports_login_logout() {
    use clap::Parser;
    for platform in ["slack", "discord"] {
        for action in ["config.json", "login", "logout", "connect"] {
            assert!(crate::Cli::try_parse_from(["omar", "messaging", platform, action]).is_ok());
        }
        assert!(crate::Cli::try_parse_from(["omar", "messaging", platform]).is_err());
        assert!(crate::Cli::try_parse_from([
            "omar",
            "messaging",
            platform,
            "config.json",
            "login"
        ])
        .is_err());
    }
}

fn registry_config() -> Config {
    fixture()
}
#[derive(Default)]
struct RegistryRuntime {
    requests: Mutex<Vec<Value>>,
    timeouts: Mutex<Vec<u64>>,
}
impl Runtime for RegistryRuntime {
    async fn approved(&self) -> Result<Vec<runtime::ApprovedWorkflow>> {
        Ok(vec![
            runtime::ApprovedWorkflow {
                name: "demo".into(),
                request: json!({"program":"fixed source","raw_inputs":["run.request=fixed"]}),
                output_port: "run.result".into(),
                timeout_seconds: 60,
            },
            runtime::ApprovedWorkflow {
                name: "first".into(),
                request: json!({"program":"FIRST","raw_inputs":["in=one"],"files":{"schema.json":"first schema"}}),
                output_port: "first.result".into(),
                timeout_seconds: 10,
            },
            runtime::ApprovedWorkflow {
                name: "second".into(),
                request: json!({"program":"SECOND","inputs":{"count":2},"files":{"schema.json":"second schema"}}),
                output_port: "second.result".into(),
                timeout_seconds: 20,
            },
        ])
    }

    async fn start(&self, request: &Value) -> Result<Run> {
        self.requests.lock().unwrap().push(request.clone());
        Ok(Run {
            run_id: request["program"].as_str().unwrap().into(),
            status: "running".into(),
            outputs: Default::default(),
            conversation_id: None,
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
            json!({"program":"FIRST","raw_inputs":["in=one"],"files":{"schema.json":"first schema"}}),
            json!({"program":"SECOND","inputs":{"count":2},"files":{"schema.json":"second schema"}})
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
    assert_eq!(
        replies[0].1.text,
        "Saved workflows:\n- demo\n- first\n- second"
    );
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
        .contains("Saved workflows"));
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

#[tokio::test]
async fn discord_plain_text_uses_assistant_with_authorization_and_dedup() {
    let mut config = fixture();
    config.platform = "discord".into();
    let g = Gateway::new(config, FakeRuntime::default(), FakeMessenger::default());
    let mut m = incoming();
    m.platform = "discord".into();
    m.destination.platform = "discord".into();
    m.command = "Help me plan some research".into();
    let mut unauthorized = m.clone();
    unauthorized.sender_id = "outsider".into();
    g.handle(unauthorized).await;
    assert!(g.messenger.replies.lock().unwrap().is_empty());
    g.handle(m.clone()).await;
    g.handle(m).await;
    assert_eq!(g.runtime.starts.load(Ordering::SeqCst), 0);
    let replies = g.messenger.replies.lock().unwrap();
    assert_eq!(replies.len(), 1);
    assert_eq!(
        replies[0].1.text,
        "Answer to user: Help me plan some research"
    );
}

#[derive(Default)]
struct ApprovedRuntime {
    entries: Mutex<Vec<runtime::ApprovedWorkflow>>,
    workflow: RegistryRuntime,
    discoveries: AtomicUsize,
}
impl Runtime for ApprovedRuntime {
    async fn approved(&self) -> Result<Vec<runtime::ApprovedWorkflow>> {
        self.discoveries.fetch_add(1, Ordering::SeqCst);
        Ok(self.entries.lock().unwrap().clone())
    }
    async fn start(&self, request: &Value) -> Result<Run> {
        self.workflow.start(request).await
    }
    async fn wait(&self, run: Run, timeout: u64) -> Result<Run> {
        self.workflow.wait(run, timeout).await
    }
}

#[tokio::test]
async fn web_approval_becomes_listable_and_runnable_without_restarting_the_gateway() {
    let g = Gateway::new(
        fixture(),
        ApprovedRuntime::default(),
        FakeMessenger::default(),
    );
    let mut m = incoming();
    m.command = "list".into();
    let mut outsider = m.clone();
    outsider.sender_id = "outsider".into();
    g.handle(outsider).await;
    assert_eq!(g.runtime.discoveries.load(Ordering::SeqCst), 0);
    g.handle(m.clone()).await;
    assert!(!g.messenger.replies.lock().unwrap()[0]
        .1
        .text
        .contains("web-News"));
    let bundle = json!({"program":"FIRST","inputs":{"run.request":"approved"},"files":{"schema.json":"allowed"},"conversation_id":"original-chat"});
    g.runtime
        .entries
        .lock()
        .unwrap()
        .push(runtime::ApprovedWorkflow {
            name: "web-News-1".into(),
            request: bundle.clone(),
            output_port: String::new(),
            timeout_seconds: 90,
        });
    m.event_id = "second".into();
    g.handle(m.clone()).await;
    assert!(g.messenger.replies.lock().unwrap()[1]
        .1
        .text
        .contains("web-News-1"));
    m.event_id = "third".into();
    m.command = "run web-News-1".into();
    g.handle(m.clone()).await;
    g.handle(m).await;
    assert_eq!(*g.runtime.workflow.requests.lock().unwrap(), vec![bundle]);
    assert_eq!(*g.runtime.workflow.timeouts.lock().unwrap(), vec![90]);
    let replies = g.messenger.replies.lock().unwrap();
    assert!(replies
        .last()
        .unwrap()
        .1
        .text
        .contains("first.result: first output"));
}

pub(super) fn review_fixture() -> runtime::Proposal {
    runtime::Proposal {
        conversation_id:uuid::Uuid::new_v4().to_string(),sequence:2,summary:"Review this workflow".into(),
        design:crate::serve::ProposedDesign {
            program:"main Review {}".into(),files:std::collections::BTreeMap::from([("schemas/decision.json".into(),r#"{"type":"string","enum":["yes","no"]}"#.into())]),
            inputs:Default::default(),lemmas:String::new(),properties:vec![],
            preview:serde_json::from_value(json!({"protocol_version":1,"team":"Review","sequence":0,"status":"ready","current_tag":null,"lag":null,"instances":[],"agents":[],"ports":[],"reactions":[],"edges":[]})).unwrap(),
        },
    }
}
struct ReviewRuntime {
    starts: Mutex<Vec<Value>>,
}
impl Runtime for ReviewRuntime {
    async fn converse(&self, _: &str, _: &str) -> Result<String> {
        Ok("draft".into())
    }
    async fn proposal(&self, _: &str) -> Result<Option<runtime::Proposal>> {
        Ok(Some(review_fixture()))
    }
    async fn start(&self, request: &Value) -> Result<Run> {
        self.starts.lock().unwrap().push(request.clone());
        Ok(Run {
            run_id: "review-run".into(),
            status: "completed".into(),
            outputs: std::collections::BTreeMap::from([(
                "verify.report".into(),
                json!("Final report"),
            )]),
            conversation_id: None,
        })
    }
    async fn wait(&self, run: Run, _: u64) -> Result<Run> {
        Ok(run)
    }
}
fn review_gateway() -> Gateway<ReviewRuntime, FakeMessenger> {
    let mut c = fixture();
    c.platform = "discord".into();
    c.allowed_users.push("other".into());
    Gateway::new(
        c,
        ReviewRuntime {
            starts: Mutex::new(vec![]),
        },
        FakeMessenger::default(),
    )
}
fn review_message(text: &str, event: &str) -> Incoming {
    let mut m = incoming();
    m.platform = "discord".into();
    m.destination.platform = "discord".into();
    m.command = text.into();
    m.event_id = event.into();
    m
}
#[tokio::test]
async fn discord_approval_is_explicit_user_scoped_once_only_and_returns_the_report() {
    let g = review_gateway();
    g.handle(review_message("Create a workflow", "create"))
        .await;
    assert!(g.runtime.starts.lock().unwrap().is_empty());
    let token = g
        .state
        .lock()
        .unwrap()
        .pending
        .keys()
        .next()
        .unwrap()
        .clone();
    let mut outsider = review_message(&format!("approve {token}"), "other");
    outsider.sender_id = "other".into();
    g.handle(outsider).await;
    assert!(g.runtime.starts.lock().unwrap().is_empty());
    assert!(g.state.lock().unwrap().pending.contains_key(&token));
    g.handle(review_message(&format!("approve {token}"), "approve-1"))
        .await;
    g.handle(review_message(&format!("approve {token}"), "approve-2"))
        .await;
    let starts = g.runtime.starts.lock().unwrap();
    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0]["proposal_sequence"], 2);
    assert!(starts[0].get("program").is_none());
    assert!(g
        .messenger
        .replies
        .lock()
        .unwrap()
        .iter()
        .any(|(_, r)| r.text.contains("verify.report: Final report")));
}
#[tokio::test]
async fn discord_cancel_revision_expiry_and_new_messages_never_execute_old_proposals() {
    for action in ["cancel", "revise", "expired", "new-message"] {
        let g = review_gateway();
        g.handle(review_message("Create a workflow", "create"))
            .await;
        let token = g
            .state
            .lock()
            .unwrap()
            .pending
            .keys()
            .next()
            .unwrap()
            .clone();
        match action {
            "expired" => {
                g.state
                    .lock()
                    .unwrap()
                    .pending
                    .get_mut(&token)
                    .unwrap()
                    .expires = Instant::now()
            }
            "new-message" => g.handle(review_message("Change the input", "new")).await,
            action => {
                g.handle(review_message(&format!("{action} {token}"), "action"))
                    .await
            }
        }
        g.handle(review_message(&format!("approve {token}"), "old-button"))
            .await;
        assert!(g.runtime.starts.lock().unwrap().is_empty(), "{action}");
    }
}

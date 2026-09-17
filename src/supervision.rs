//! Durable legacy task lifecycle. Model context is a projection of this state,
//! never the only place a child obligation or an unconsumed result exists.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub const CHECK_INTERVAL_MS: u64 = 60_000;

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Running,
    Blocked,
    Completed,
    Failed,
    Cancelled,
}
impl Status {
    pub fn terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub ea_id: u32,
    pub agent: String,
    pub session: String,
    pub parent: String,
    #[serde(default)]
    pub parent_task_id: Option<String>,
    pub project_id: usize,
    pub assignment: String,
    pub status: Status,
    pub result: Option<Value>,
    #[serde(default)]
    pub result_revision: u64,
    pub acknowledged: bool,
    #[serde(default)]
    pub retired: bool,
    pub next_check_ms: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct Ledger {
    tasks: BTreeMap<String, Task>,
}

fn transaction<T>(root: &Path, f: impl FnOnce(&mut Ledger) -> Result<T>) -> Result<T> {
    fs::create_dir_all(root)?;
    // OS releases this lock on process death. Never unlink the lock inode:
    // replacing it could give two processes independent locks on the ledger.
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join("task-lifecycle.lock"))?;
    lock.lock()?;
    let path = root.join("task-lifecycle.json");
    let mut ledger: Ledger = match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .context("invalid task lifecycle state (not overwritten)")?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ledger::default(),
        Err(e) => return Err(e.into()),
    };
    let value = f(&mut ledger)?;
    let temp = root.join(format!(".task-lifecycle-{}.tmp", uuid::Uuid::new_v4()));
    let write = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        serde_json::to_writer(&mut file, &ledger)?;
        file.sync_all()?;
        fs::rename(&temp, &path)?;
        Ok(())
    })();
    if write.is_err() {
        let _ = fs::remove_file(&temp);
    }
    write?;
    Ok(value)
}

fn read_state(root: &Path) -> Result<Ledger> {
    match fs::read(root.join("task-lifecycle.json")) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).context("invalid task lifecycle state (not overwritten)")
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Ledger::default()),
        Err(error) => Err(error.into()),
    }
}

fn read<T>(root: &Path, f: impl FnOnce(&Ledger) -> Result<T>) -> Result<T> {
    f(&read_state(root)?)
}

pub fn register(root: &Path, mut task: Task) -> Result<String> {
    transaction(root, |ledger| {
        if ledger.tasks.values().any(|t| {
            t.ea_id == task.ea_id
                && t.agent == task.agent
                && (!t.status.terminal() || !t.acknowledged || !t.retired)
        }) {
            bail!("agent '{}' already has an unfinished task", task.agent);
        }
        task.parent_task_id = ledger
            .tasks
            .values()
            .find(|t| {
                t.ea_id == task.ea_id
                    && t.agent == task.parent
                    && !t.retired
                    && !t.status.terminal()
            })
            .map(|t| t.id.clone());
        task.id = uuid::Uuid::new_v4().to_string();
        task.next_check_ms = now_ms() + CHECK_INTERVAL_MS;
        let id = task.id.clone();
        ledger.tasks.insert(id.clone(), task);
        Ok(id)
    })
}

fn authorize(task: &Task, ea: u32, actor: &str, parent: bool) -> Result<()> {
    if task.ea_id != ea
        || (actor != "ea" && actor != if parent { &task.parent } else { &task.agent })
    {
        bail!("task is outside this agent's coordination scope");
    }
    Ok(())
}

pub fn finish(
    root: &Path,
    ea: u32,
    actor: &str,
    id: &str,
    status: Status,
    result: Value,
) -> Result<Task> {
    if !matches!(status, Status::Completed | Status::Failed | Status::Blocked) || result.is_null() {
        bail!("finish_task requires completed, failed, or blocked and a non-null result");
    }
    transaction(root, |ledger| {
        let task = ledger.tasks.get(id).context("unknown task_id")?;
        authorize(task, ea, actor, false)?;
        if task.status == status && task.result.as_ref() == Some(&result) {
            return Ok(task.clone());
        }
        if task.status.terminal() {
            bail!("task already finished with a different result");
        }
        if status != Status::Blocked
            && ledger.tasks.values().any(|t| {
                t.ea_id == ea
                    && t.parent == task.agent
                    && (!t.status.terminal() || !t.acknowledged || !t.retired)
            })
        {
            bail!("finish or cancel children, acknowledge their results, and retire their sessions before finishing this task");
        }
        let task = ledger.tasks.get_mut(id).unwrap();
        task.status = status;
        task.result = Some(result);
        task.result_revision += 1;
        task.acknowledged = false;
        // Result and parent-delivery obligation commit in the same record.
        task.next_check_ms = 0;
        Ok(task.clone())
    })
}

pub fn acknowledge(root: &Path, ea: u32, actor: &str, id: &str, revision: u64) -> Result<Task> {
    transaction(root, |ledger| {
        let task = ledger.tasks.get_mut(id).context("unknown task_id")?;
        authorize(task, ea, actor, true)?;
        if task.result.is_none() {
            bail!("task has no result to acknowledge");
        }
        if task.result_revision != revision {
            bail!("result changed; read the current result before acknowledging it");
        }
        task.acknowledged = true;
        Ok(task.clone())
    })
}

pub fn cancel_agent(root: &Path, ea: u32, agent: &str) -> Result<()> {
    transaction(root, |ledger| {
        for task in ledger
            .tasks
            .values_mut()
            .filter(|t| t.ea_id == ea && t.agent == agent)
        {
            task.retired = true;
            if !task.status.terminal() {
                task.status = Status::Cancelled;
                task.result = Some(json!({"reason":"agent session was explicitly killed"}));
                task.result_revision += 1;
                task.acknowledged = false;
                task.next_check_ms = 0;
            }
        }
        Ok(())
    })
}

pub fn remove_ea(root: &Path, ea: u32) -> Result<()> {
    if !root.join("task-lifecycle.json").exists() {
        return Ok(());
    }
    transaction(root, |ledger| {
        ledger.tasks.retain(|_, task| task.ea_id != ea);
        Ok(())
    })
}

pub fn touch(root: &Path, ea: u32, actor: &str) -> Result<()> {
    if !root.join("task-lifecycle.json").exists() {
        return Ok(());
    }
    transaction(root, |ledger| {
        for task in ledger
            .tasks
            .values_mut()
            .filter(|t| t.ea_id == ea && t.agent == actor && t.status == Status::Running)
        {
            task.next_check_ms = now_ms() + CHECK_INTERVAL_MS;
        }
        Ok(())
    })
}

fn projection(ledger: &Ledger, ea: u32, actor: &str) -> Value {
    projection_page(ledger, ea, actor, 0)
}

fn projection_page(ledger: &Ledger, ea: u32, actor: &str, offset: usize) -> Value {
    let mut tasks: Vec<_> = ledger.tasks.values().filter(|t| t.ea_id == ea &&
        ((t.agent == actor && !t.status.terminal()) || (t.parent == actor && (!t.status.terminal() || !t.acknowledged || !t.retired))))
        .map(|t| json!({"task_id":t.id,"agent":t.agent,"parent":t.parent,"project_id":t.project_id,
            "task":preview(&t.assignment, 400),"status":t.status,"has_result":t.result.is_some(),"result_revision":t.result_revision,"result_preview":t.result.as_ref().map(|r|preview(&r.to_string(),400)),"acknowledged":t.acknowledged,"retired":t.retired})).collect();
    tasks.sort_by_key(|t| {
        (
            t["agent"] != actor,
            !(t["has_result"] == true && t["acknowledged"] == false),
        )
    });
    let total = tasks.len();
    let tasks: Vec<_> = tasks.into_iter().skip(offset).take(32).collect();
    let end = offset.saturating_add(tasks.len());
    json!({"agent":actor,"tasks":tasks,"total_tasks":total,"offset":offset,"next_offset":if end < total { Some(end) } else { None },"truncated":end < total})
}

fn preview(text: &str, limit: usize) -> String {
    let mut value: String = text.chars().take(limit).collect();
    if text.chars().count() > limit {
        value.push_str("… [use get_task]");
    }
    value
}

pub fn get_task(root: &Path, ea: u32, actor: &str, args: &Value) -> Result<Value> {
    let id = args["task_id"].as_str().context("task_id is required")?;
    let offset = args["offset"].as_u64().unwrap_or(0) as usize;
    read(root, |ledger| {
        let task = ledger.tasks.get(id).context("unknown task_id")?;
        if authorize(task, ea, actor, false).is_err() {
            authorize(task, ea, actor, true)?;
        }
        let text =
            serde_json::to_string(&json!({"assignment":task.assignment,"result":task.result}))?;
        let page: String = text.chars().skip(offset).take(8000).collect();
        let next = offset.saturating_add(page.chars().count());
        Ok(
            json!({"task_id":id,"status":task.status,"result_revision":task.result_revision,"content":page,"offset":offset,
            "next_offset":if next < text.chars().count() { Some(next) } else { None }}),
        )
    })
}

pub fn resume(root: &Path, ea: u32, actor: &str, id: &str) -> Result<()> {
    transaction(root, |ledger| {
        let task = ledger.tasks.get_mut(id).context("unknown task_id")?;
        authorize(task, ea, actor, true)?;
        if task.status != Status::Blocked {
            bail!("only a blocked task can be resumed");
        }
        task.status = Status::Running;
        task.result = None;
        task.acknowledged = false;
        task.next_check_ms = 0;
        Ok(())
    })
}

pub fn context(root: &Path, ea: u32, actor: &str) -> Result<Value> {
    context_page(root, ea, actor, 0)
}

pub fn context_page(root: &Path, ea: u32, actor: &str, offset: usize) -> Result<Value> {
    read(root, |ledger| {
        Ok(projection_page(ledger, ea, actor, offset))
    })
}

pub fn context_message(state: &Value) -> String {
    format!("OMAR COORDINATION STATE (authoritative runtime state, not conversation memory)\n{}\nUse finish_task(task_id, status, result) to return your work; the runtime notifies your parent. Read child results and call acknowledge_task(task_id, result_revision) after incorporating each result. If truncated, use coordination_state(offset=next_offset) to inspect remaining tasks. Running children are owned by the runtime; do not create polling timers. If blocked, finish_task with status=blocked and the concrete reason. Retire acknowledged terminal children with kill_agent. Never infer completion from a quiet terminal.", state)
}

pub fn stop_reason(root: &Path, ea: u32, actor: &str) -> Result<Option<String>> {
    if !root.join("task-lifecycle.json").exists() {
        return Ok(None);
    }
    read(root, |ledger| {
        let unconsumed = ledger.tasks.values().any(|t| {
            t.ea_id == ea
                && t.parent == actor
                && t.result.is_some()
                && (!t.acknowledged || (t.status.terminal() && !t.retired))
        });
        let owns_work = ledger
            .tasks
            .values()
            .any(|t| t.ea_id == ea && t.agent == actor && t.status == Status::Running);
        let waiting = ledger
            .tasks
            .values()
            .any(|t| t.ea_id == ea && t.parent == actor && t.status == Status::Running);
        Ok((unconsumed || (owns_work && !waiting))
            .then(|| context_message(&projection(ledger, ea, actor))))
    })
}

pub fn hook_response(context: &crate::manager::McpLaunchContext, input: &Value) -> Result<Value> {
    if context.topology.is_some() {
        return Ok(json!({}));
    }
    let actor = context.agent_name.as_deref().unwrap_or("ea");
    let event = input["hook_event_name"]
        .as_str()
        .context("hook_event_name is required")?;
    if event == "Stop" {
        // One immediate correction per turn; the persistent watchdog handles
        // repeated refusal without an unbounded native Stop-hook loop.
        if input["stop_hook_active"] == true {
            return Ok(json!({}));
        }
        return Ok(
            match stop_reason(&context.omar_dir, context.ea_id, actor)? {
                Some(reason) => json!({"decision":"block","reason":reason}),
                None => json!({}),
            },
        );
    }
    if !matches!(event, "SessionStart" | "UserPromptSubmit") {
        return Ok(json!({}));
    }
    let state = self::context(&context.omar_dir, context.ea_id, actor)?;
    Ok(
        json!({"hookSpecificOutput":{"hookEventName":event,"additionalContext":context_message(&state)}}),
    )
}

/// MCP is also used without a dashboard (including an EA launched by serve).
/// Elect one fallback consumer across these processes. The OS releases the
/// lease on exit, so another connected agent takes over automatically.
pub fn start_fallback_runtime(context: &crate::manager::McpLaunchContext) {
    if context.topology.is_some() {
        return;
    }
    let root = context.omar_dir.clone();
    let prefix = context.session_prefix.clone();
    std::thread::spawn(move || {
        let run = || -> Result<()> {
            fs::create_dir_all(&root)?;
            let lease = fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(root.join("supervisor-consumer.lock"))?;
            lease.lock()?;
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            let scheduler = std::sync::Arc::new(crate::scheduler::Scheduler::with_store(
                crate::scheduler::events_store_path(&root),
            ));
            runtime.block_on(crate::scheduler::run_event_loop(
                scheduler,
                crate::scheduler::TickerBuffer::new(),
                crate::scheduler::new_popup_receiver(),
                prefix,
            ));
            Ok(())
        };
        if let Err(error) = run() {
            eprintln!("OMAR fallback supervisor failed: {error:#}");
        }
    });
}

pub fn run_hook(path: &Path) -> Result<()> {
    use std::io::Read;
    let context = serde_json::from_slice(&fs::read(path)?)?;
    let mut input = String::new();
    std::io::stdin()
        .take(1_048_576)
        .read_to_string(&mut input)?;
    println!(
        "{}",
        hook_response(&context, &serde_json::from_str(&input)?)?
    );
    Ok(())
}

/// Parent notifications remain obligations until acknowledged, even after a
/// successful transport write or daemon restart. Running tasks always have a
/// check-in, without requiring the model to schedule one.
pub fn reconcile(
    root: &Path,
    now: u64,
    live: &BTreeSet<String>,
) -> Result<Vec<(u32, String, String)>> {
    if !root.join("task-lifecycle.json").exists() {
        return Ok(Vec::new());
    }
    transaction(root, |ledger| {
        let mut recipients = BTreeSet::new();
        for task in ledger.tasks.values_mut() {
            if !live.contains(&task.session) {
                task.retired = true;
            }
            if task.status == Status::Running && !live.contains(&task.session) {
                task.status = Status::Failed;
                task.result =
                    Some(json!({"reason":"agent session exited without returning a task result"}));
                task.result_revision += 1;
                task.acknowledged = false;
                task.next_check_ms = 0;
            }
        }
        // If a supervisor dies, its outstanding children become the nearest
        // surviving ancestor's responsibility instead of notifying a dead pane.
        let departed: BTreeMap<_, _> = ledger
            .tasks
            .values()
            .filter(|t| t.retired && t.status.terminal())
            .map(|t| (t.id.clone(), (t.parent.clone(), t.parent_task_id.clone())))
            .collect();
        for task in ledger.tasks.values_mut() {
            if task.status.terminal() && task.acknowledged && task.retired {
                continue;
            }
            let mut visited = BTreeSet::new();
            while let Some((parent, parent_id)) =
                task.parent_task_id.as_ref().and_then(|id| departed.get(id))
            {
                if !visited.insert(task.parent_task_id.clone()) {
                    break;
                }
                task.parent = parent.clone();
                task.parent_task_id = parent_id.clone();
                task.next_check_ms = 0;
            }
        }
        let waiting: BTreeSet<_> = ledger
            .tasks
            .values()
            .filter(|t| t.status == Status::Running)
            .map(|t| (t.ea_id, t.parent.clone()))
            .collect();
        for task in ledger.tasks.values_mut() {
            if task.next_check_ms > now {
                continue;
            }
            if task.result.is_some()
                && (!task.acknowledged || (task.status.terminal() && !task.retired))
            {
                recipients.insert((task.ea_id, task.parent.clone()));
            } else if task.status == Status::Running
                && !waiting.contains(&(task.ea_id, task.agent.clone()))
            {
                recipients.insert((task.ea_id, task.agent.clone()));
                recipients.insert((task.ea_id, task.parent.clone()));
            }
            task.next_check_ms = now.saturating_add(CHECK_INTERVAL_MS);
        }
        Ok(recipients
            .into_iter()
            .map(|(ea, actor)| {
                let message = context_message(&projection(ledger, ea, &actor));
                (ea, actor, message)
            })
            .collect())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn task(agent: &str, parent: &str) -> Task {
        Task {
            id: String::new(),
            ea_id: 0,
            agent: agent.into(),
            session: format!("s-{agent}"),
            parent: parent.into(),
            parent_task_id: None,
            project_id: 1,
            assignment: "work".into(),
            status: Status::Running,
            result: None,
            result_revision: 0,
            acknowledged: false,
            retired: false,
            next_check_ms: 0,
        }
    }
    #[test]
    fn completion_is_durable_idempotent_and_retried_until_consumed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let id = register(root, task("child", "pm")).unwrap();
        finish(
            root,
            0,
            "child",
            &id,
            Status::Completed,
            json!({"file":"result.txt"}),
        )
        .unwrap();
        let live = BTreeSet::from(["s-child".into()]);
        assert_eq!(reconcile(root, 10, &live).unwrap()[0].1, "pm");
        assert!(reconcile(root, 11, &live).unwrap().is_empty());
        assert_eq!(
            reconcile(root, CHECK_INTERVAL_MS + 11, &live)
                .unwrap()
                .len(),
            1
        );
        finish(
            root,
            0,
            "child",
            &id,
            Status::Completed,
            json!({"file":"result.txt"}),
        )
        .unwrap();
        assert!(acknowledge(root, 0, "stranger", &id, 1).is_err());
        acknowledge(root, 0, "pm", &id, 1).unwrap();
        assert_eq!(
            reconcile(root, CHECK_INTERVAL_MS * 3, &live).unwrap()[0].1,
            "pm",
            "retirement remains an obligation after result consumption"
        );
        cancel_agent(root, 0, "child").unwrap();
        assert!(reconcile(root, CHECK_INTERVAL_MS * 4, &live)
            .unwrap()
            .is_empty());
        assert!(finish(root, 0, "child", &id, Status::Completed, json!("changed")).is_err());
    }
    #[test]
    fn parent_cannot_finish_with_unconsumed_children_and_compaction_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let pm = register(root, task("pm", "ea")).unwrap();
        let child = register(root, task("worker", "pm")).unwrap();
        assert!(finish(root, 0, "pm", &pm, Status::Completed, json!("done")).is_err());
        finish(
            root,
            0,
            "worker",
            &child,
            Status::Completed,
            json!("artifact"),
        )
        .unwrap();
        assert!(finish(root, 0, "pm", &pm, Status::Completed, json!("done")).is_err());
        assert!(context_message(&context(root, 0, "pm").unwrap()).contains("artifact"));
        acknowledge(root, 0, "pm", &child, 1).unwrap();
        cancel_agent(root, 0, "worker").unwrap();
        finish(
            root,
            0,
            "pm",
            &pm,
            Status::Completed,
            json!("combined result"),
        )
        .unwrap();
        assert!(context_message(&context(root, 0, "ea").unwrap()).contains("combined result"));
    }
    #[test]
    fn forgotten_timer_and_crashed_worker_have_runtime_continuations() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        register(root, task("worker", "ea")).unwrap();
        let live = BTreeSet::from(["s-worker".into()]);
        let due = reconcile(root, now_ms() + CHECK_INTERVAL_MS + 1, &live).unwrap();
        assert_eq!(
            due.iter().map(|r| r.1.as_str()).collect::<Vec<_>>(),
            vec!["ea", "worker"]
        );
        let failed = reconcile(root, now_ms() + CHECK_INTERVAL_MS + 2, &BTreeSet::new()).unwrap();
        assert_eq!(failed[0].1, "ea");
        assert!(failed[0].2.contains("exited without"));
    }
    #[test]
    fn corrupt_ledger_is_never_silently_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("task-lifecycle.json");
        fs::write(&path, "broken").unwrap();
        assert!(register(dir.path(), task("a", "ea")).is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), "broken");
    }
    #[test]
    fn blocked_result_ack_cannot_consume_a_later_revision() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let id = register(root, task("child", "pm")).unwrap();
        finish(root, 0, "child", &id, Status::Blocked, json!("need input")).unwrap();
        assert!(resume(root, 0, "stranger", &id).is_err());
        acknowledge(root, 0, "pm", &id, 1).unwrap();
        resume(root, 0, "pm", &id).unwrap();
        finish(root, 0, "child", &id, Status::Completed, json!("done")).unwrap();
        assert!(acknowledge(root, 0, "pm", &id, 1).is_err());
        acknowledge(root, 0, "pm", &id, 2).unwrap();
        assert!(finish(root, 1, "ea", &id, Status::Completed, json!("done")).is_err());
    }

    #[test]
    fn hierarchy_propagates_results_without_polling_timers() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let pm = register(root, task("pm", "ea")).unwrap();
        let supervisor = register(root, task("supervisor", "pm")).unwrap();
        let worker = register(root, task("worker", "supervisor")).unwrap();
        let live = BTreeSet::from(["s-pm".into(), "s-supervisor".into(), "s-worker".into()]);
        for (actor, parent, id) in [
            ("worker", "supervisor", worker),
            ("supervisor", "pm", supervisor),
            ("pm", "ea", pm),
        ] {
            finish(
                root,
                0,
                actor,
                &id,
                Status::Completed,
                json!(format!("{actor} result")),
            )
            .unwrap();
            let wakes = reconcile(root, now_ms(), &live).unwrap();
            assert!(wakes
                .iter()
                .any(|(_, recipient, body)| recipient == parent && body.contains(&id)));
            acknowledge(root, 0, parent, &id, 1).unwrap();
            cancel_agent(root, 0, actor).unwrap();
        }
        assert!(context(root, 0, "ea").unwrap()["tasks"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn concurrent_workers_preserve_every_result_and_context_is_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let workers: Vec<_> = (0..40)
            .map(|i| {
                let root = root.to_owned();
                std::thread::spawn(move || {
                    let name = format!("worker-{i}");
                    let id = register(&root, task(&name, "ea")).unwrap();
                    finish(
                        &root,
                        0,
                        &name,
                        &id,
                        Status::Completed,
                        json!("λ".repeat(12000)),
                    )
                    .unwrap();
                    id
                })
            })
            .collect();
        let ids: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
        let state = context(root, 0, "ea").unwrap();
        assert_eq!(state["total_tasks"], 40);
        let page2 = context_page(
            root,
            0,
            "ea",
            state["next_offset"].as_u64().unwrap() as usize,
        )
        .unwrap();
        assert_eq!(page2["tasks"].as_array().unwrap().len(), 8);
        assert!(page2["next_offset"].is_null());
        assert_eq!(state["tasks"].as_array().unwrap().len(), 32);
        assert!(state.to_string().len() < 45000);
        let mut restored = String::new();
        let mut offset = 0;
        loop {
            let page = get_task(root, 0, "ea", &json!({"task_id":ids[0],"offset":offset})).unwrap();
            restored.push_str(page["content"].as_str().unwrap());
            let Some(next) = page["next_offset"].as_u64() else {
                break;
            };
            offset = next;
        }
        assert_eq!(
            serde_json::from_str::<Value>(&restored).unwrap()["result"],
            "λ".repeat(12000)
        );
        assert!(get_task(root, 0, "stranger", &json!({"task_id":ids[0]})).is_err());
        for id in ids {
            let state = get_task(root, 0, "ea", &json!({"task_id":id})).unwrap();
            assert_eq!(state["result_revision"], 1);
        }
    }

    #[test]
    fn hooks_restore_fresh_state_and_block_only_actionable_obligations() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let context: crate::manager::McpLaunchContext = serde_json::from_value(json!({
            "omar_dir":root,"ea_id":0,"agent_name":"pm","session_prefix":"s-",
            "default_command":"cat","default_workdir":".","health_idle_warning":15
        }))
        .unwrap();
        let pm = register(root, task("pm", "ea")).unwrap();
        let child = register(root, task("child", "pm")).unwrap();
        let stop = json!({"hook_event_name":"Stop","stop_hook_active":false});
        assert_eq!(
            hook_response(&context, &stop).unwrap(),
            json!({}),
            "waiting is allowed"
        );
        finish(
            root,
            0,
            "child",
            &child,
            Status::Completed,
            json!("unique result"),
        )
        .unwrap();
        for source in ["compact", "resume", "startup"] {
            let response = hook_response(
                &context,
                &json!({"hook_event_name":"SessionStart","source":source}),
            )
            .unwrap();
            let restored = response["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .unwrap();
            assert!(
                restored.contains(&pm)
                    && restored.contains(&child)
                    && restored.contains("unique result")
            );
        }
        assert_eq!(hook_response(&context, &stop).unwrap()["decision"], "block");
        assert_eq!(
            hook_response(
                &context,
                &json!({"hook_event_name":"Stop","stop_hook_active":true})
            )
            .unwrap(),
            json!({})
        );
        acknowledge(root, 0, "pm", &child, 1).unwrap();
        cancel_agent(root, 0, "child").unwrap();
        finish(root, 0, "pm", &pm, Status::Completed, json!("combined")).unwrap();
        assert_eq!(hook_response(&context, &stop).unwrap(), json!({}));
    }
    #[test]
    fn dead_supervisor_escalates_its_children_to_a_live_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        register(root, task("pm", "ea")).unwrap();
        let child = register(root, task("child", "pm")).unwrap();
        let live = BTreeSet::from(["s-child".into()]);
        let wakes = reconcile(root, now_ms(), &live).unwrap();
        assert!(wakes
            .iter()
            .any(|(_, recipient, body)| recipient == "ea" && body.contains(&child)));
        finish(
            root,
            0,
            "child",
            &child,
            Status::Completed,
            json!("survived"),
        )
        .unwrap();
        assert_eq!(reconcile(root, now_ms(), &live).unwrap()[0].1, "ea");
        assert!(acknowledge(root, 0, "pm", &child, 1).is_err());
        acknowledge(root, 0, "ea", &child, 1).unwrap();
    }
}

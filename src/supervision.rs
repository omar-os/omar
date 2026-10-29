//! Durable legacy task lifecycle. Model context is a projection of this state,
//! never the only place a child obligation or an unconsumed result exists.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

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
}

#[derive(Default, Serialize, Deserialize)]
struct Ledger {
    tasks: BTreeMap<String, Task>,
    #[serde(default)]
    pending_wakes: BTreeMap<String, (u32, String)>,
    #[serde(default)]
    idle: BTreeMap<String, IdleWatch>,
}

#[derive(Serialize, Deserialize)]
struct IdleWatch {
    reminded_at_ms: u64,
    escalated: bool,
}

pub fn running_sessions(root: &Path) -> Result<BTreeSet<String>> {
    read(root, |ledger| {
        Ok(ledger
            .tasks
            .values()
            .filter(|task| task.status == Status::Running)
            .map(|task| task.session.clone())
            .collect())
    })
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
        let task = task.clone();
        ledger.idle.remove(id);
        // Commit the result and one parent-delivery obligation together.
        ledger
            .pending_wakes
            .insert(uuid::Uuid::new_v4().to_string(), (ea, task.parent.clone()));
        Ok(task)
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
            if !task.retired && !task.acknowledged {
                ledger
                    .pending_wakes
                    .insert(uuid::Uuid::new_v4().to_string(), (ea, task.parent.clone()));
            }
            task.retired = true;
            ledger.idle.remove(&task.id);
            if !task.status.terminal() {
                task.status = Status::Cancelled;
                task.result = Some(json!({"reason":"agent session was explicitly killed"}));
                task.result_revision += 1;
                task.acknowledged = false;
                ledger
                    .pending_wakes
                    .insert(uuid::Uuid::new_v4().to_string(), (ea, task.parent.clone()));
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
        ledger.pending_wakes.retain(|_, (ea_id, _)| *ea_id != ea);
        ledger.idle.retain(|id, _| ledger.tasks.contains_key(id));
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
            "task":preview(&t.assignment, 400),"status":t.status,"has_result":t.result.is_some(),"result_revision":t.result_revision,"result_preview":t.result.as_ref().map(|r|preview(&r.to_string(),400)),"acknowledged":t.acknowledged,"retired":t.retired,
            "idle_without_result":ledger.idle.contains_key(&t.id),
            "idle_escalated":ledger.idle.get(&t.id).is_some_and(|idle| idle.escalated)})).collect();
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
        ledger.idle.remove(id);
        ledger
            .pending_wakes
            .insert(uuid::Uuid::new_v4().to_string(), (ea, task.agent.clone()));
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
    let mut reminders = String::new();
    if let Some(tasks) = state["tasks"].as_array() {
        for task in tasks {
            if task["idle_without_result"] == true {
                let agent = task["agent"].as_str().unwrap_or("");
                if task["agent"] == state["agent"] {
                    reminders.push_str(&format!("\nIdle without a result: {agent}. Report with finish_task, or status=blocked with a concrete reason."));
                } else if task["idle_escalated"] == true {
                    reminders.push_str(&format!("\nStalled child: {agent} is still idle without a result after its reminder."));
                }
            }
        }
    }
    format!("OMAR COORDINATION STATE (authoritative runtime state, not conversation memory)\n{}\nUse finish_task(task_id, status, result) to return your work; the runtime notifies your parent. Read child results and call acknowledge_task(task_id, result_revision) after incorporating each result. If truncated, use coordination_state(offset=next_offset) to inspect remaining tasks. Running children are owned by the runtime; do not create polling timers. If blocked, finish_task with status=blocked and the concrete reason. Retire acknowledged terminal children with kill_agent. Never infer completion from a quiet terminal.{}", state, reminders)
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
        // One immediate correction per turn, without an unbounded Stop-hook loop.
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
    if !matches!(event, "SessionStart" | "UserPromptSubmit" | "PostToolUse") {
        return Ok(json!({}));
    }
    let state = self::context(&context.omar_dir, context.ea_id, actor)?;
    Ok(
        json!({"hookSpecificOutput":{"hookEventName":event,"additionalContext":context_message(&state)}}),
    )
}

/// Backend wire contracts share one authoritative state and stop predicate.
/// Unsupported notification hooks never pretend that output was injected.
pub fn backend_hook_response(
    context: &crate::manager::McpLaunchContext,
    input: &Value,
    backend: &str,
) -> Result<Value> {
    if context.topology.is_some() {
        return Ok(json!({}));
    }
    let actor = context.agent_name.as_deref().unwrap_or("ea");
    let event = input["hook_event_name"].as_str().unwrap_or("");
    match backend {
        "claude" | "codex" => hook_response(context, input),
        "cursor" if event == "stop" => {
            if input["status"] != "completed" || input["loop_count"].as_u64().unwrap_or(0) > 0 {
                return Ok(json!({}));
            }
            Ok(
                match stop_reason(&context.omar_dir, context.ea_id, actor)? {
                    Some(reason) => json!({"followup_message":reason}),
                    None => json!({}),
                },
            )
        }
        "cursor" if matches!(event, "sessionStart" | "postToolUse" | "postToolUseFailure") => Ok(
            json!({"additional_context":context_message(&self::context(&context.omar_dir,context.ea_id,actor)?)}),
        ),
        "cursor" => Ok(json!({})),
        "agy" | "antigravity" => Ok(
            json!({"injectSteps":[{"ephemeralMessage":context_message(&self::context(&context.omar_dir,context.ea_id,actor)?)}]}),
        ),
        "opencode" if event == "Stop" => Ok(json!({})),
        "opencode" => Ok(
            json!({"context":context_message(&self::context(&context.omar_dir,context.ea_id,actor)?)}),
        ),
        _ => bail!("unsupported coordination hook backend: {backend}"),
    }
}

pub fn inherited_hook_response(input: &Value, backend: &str) -> Result<Value> {
    let Some(path) = std::env::var_os("OMAR_MCP_CONTEXT_FILE") else {
        return Ok(json!({}));
    };
    let context = serde_json::from_slice(&fs::read(path)?)?;
    backend_hook_response(&context, input, backend)
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
                prefix,
            ));
            Ok(())
        };
        if let Err(error) = run() {
            eprintln!("OMAR fallback supervisor failed: {error:#}");
        }
    });
}

pub fn run_hook(path: &Path, backend: &str, event: Option<&str>) -> Result<()> {
    use std::io::Read;
    let context = serde_json::from_slice(&fs::read(path)?)?;
    let mut raw = String::new();
    let mut input = if let Some(event) = event {
        json!({"hook_event_name":event})
    } else {
        std::io::stdin().take(1_048_576).read_to_string(&mut raw)?;
        serde_json::from_str(&raw)?
    };
    if let Some(event) = event {
        input["hook_event_name"] = json!(event);
    }
    println!("{}", backend_hook_response(&context, &input, backend)?);
    Ok(())
}

/// Screen idleness is only a prompt to report progress, never task failure.
pub fn check_idle(
    root: &Path,
    now: u64,
    live: &BTreeSet<String>,
    idle_ms: &BTreeMap<String, u64>,
    idle_threshold_ms: u64,
) -> Result<()> {
    transaction(root, |ledger| {
        let mut recipients = BTreeSet::new();
        ledger.idle.retain(|id, _| {
            ledger
                .tasks
                .get(id)
                .is_some_and(|task| task.status == Status::Running && live.contains(&task.session))
        });
        for task in ledger
            .tasks
            .values()
            .filter(|t| t.status == Status::Running)
        {
            // An unavailable activity observation is not evidence of a stall or recovery.
            let Some(elapsed) = idle_ms.get(&task.session) else {
                continue;
            };
            if *elapsed == 0 || *elapsed < idle_threshold_ms {
                ledger.idle.remove(&task.id);
                continue;
            }
            match ledger.idle.get_mut(&task.id) {
                None => {
                    ledger.idle.insert(
                        task.id.clone(),
                        IdleWatch {
                            reminded_at_ms: now,
                            escalated: false,
                        },
                    );
                    recipients.insert((task.ea_id, task.agent.clone()));
                }
                Some(idle)
                    if !idle.escalated
                        && now.saturating_sub(idle.reminded_at_ms) >= idle_threshold_ms =>
                {
                    idle.escalated = true;
                    recipients.insert((task.ea_id, task.parent.clone()));
                }
                _ => {}
            }
        }
        for (ea, actor) in recipients {
            ledger
                .pending_wakes
                .insert(uuid::Uuid::new_v4().to_string(), (ea, actor));
        }
        Ok(())
    })
}

/// Queue state changes once; unconsumed results remain in the ledger for hooks
/// and coordination_state. Persist the queue before clearing delivery obligations.
pub fn reconcile(
    root: &Path,
    live: &BTreeSet<String>,
    enqueue: impl FnOnce(&[(u32, String, String)]) -> Result<()>,
) -> Result<()> {
    if !root.join("task-lifecycle.json").exists() {
        return Ok(());
    }
    let (pending, wakes) = transaction(root, |ledger| {
        let mut recipients = BTreeSet::new();
        for task in ledger.tasks.values_mut() {
            if !live.contains(&task.session) && !task.retired {
                task.retired = true;
                if !task.acknowledged {
                    recipients.insert((task.ea_id, task.parent.clone()));
                }
            }
            if !task.status.terminal() && !live.contains(&task.session) {
                task.status = Status::Failed;
                let mut result =
                    json!({"reason":"agent session exited without returning a task result"});
                if let Some(previous) = task.result.take() {
                    result["previous_result"] = previous;
                }
                task.result = Some(result);
                task.result_revision += 1;
                task.acknowledged = false;
                recipients.insert((task.ea_id, task.parent.clone()));
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
                recipients.insert((task.ea_id, task.parent.clone()));
            }
        }
        let departed_agents: BTreeSet<_> = ledger
            .tasks
            .values()
            .filter(|t| {
                t.retired
                    && t.status.terminal()
                    && !ledger.tasks.values().any(|current| {
                        current.ea_id == t.ea_id && current.agent == t.agent && !current.retired
                    })
            })
            .map(|t| (t.ea_id, t.agent.clone()))
            .collect();
        recipients.retain(|recipient| !departed_agents.contains(recipient));
        for (ea, actor) in recipients {
            ledger
                .pending_wakes
                .insert(uuid::Uuid::new_v4().to_string(), (ea, actor));
        }
        ledger
            .pending_wakes
            .retain(|_, recipient| !departed_agents.contains(recipient));
        let recipients: BTreeSet<_> = ledger.pending_wakes.values().cloned().collect();
        let wakes: Vec<_> = recipients
            .into_iter()
            .map(|(ea, actor)| {
                let message = context_message(&projection(ledger, ea, &actor));
                (ea, actor, message)
            })
            .collect();
        Ok((ledger.pending_wakes.clone(), wakes))
    })?;
    // No ledger lock is held during the scheduler transaction. New changes have
    // distinct IDs, so clearing this snapshot cannot consume a concurrent wake.
    enqueue(&wakes)?;
    if pending.is_empty() {
        return Ok(());
    }
    transaction(root, |ledger| {
        ledger
            .pending_wakes
            .retain(|id, _| !pending.contains_key(id));
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn reconcile(
        root: &Path,
        now: u64,
        live: &BTreeSet<String>,
    ) -> Result<Vec<(u32, String, String)>> {
        reconcile_idle(root, now, live, &BTreeMap::new(), 300_000)
    }

    fn reconcile_idle(
        root: &Path,
        now: u64,
        live: &BTreeSet<String>,
        idle: &BTreeMap<String, u64>,
        threshold: u64,
    ) -> Result<Vec<(u32, String, String)>> {
        let mut wakes = Vec::new();
        super::check_idle(root, now, live, idle, threshold)?;
        super::reconcile(root, live, |pending| {
            wakes = pending.to_vec();
            Ok(())
        })?;
        Ok(wakes)
    }

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
        }
    }
    #[test]
    fn completion_is_durable_idempotent_and_delivered_once() {
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
        let wakes = reconcile(root, 10, &live).unwrap();
        assert_eq!(wakes.len(), 1);
        assert_eq!(wakes[0].1, "pm");
        assert!(reconcile(root, 11, &live).unwrap().is_empty());
        assert!(reconcile(root, 60_000 + 11, &live).unwrap().is_empty());
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
        assert!(reconcile(root, 60_000 * 3, &live).unwrap().is_empty());
        cancel_agent(root, 0, "child").unwrap();
        assert!(reconcile(root, 60_000 * 4, &live).unwrap().is_empty());
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
    fn running_is_quiet_and_dead_session_sends_once() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        register(root, task("worker", "ea")).unwrap();
        let live = BTreeSet::from(["s-worker".into()]);
        assert!(reconcile(root, now_ms() + 60_000, &live)
            .unwrap()
            .is_empty());
        let failed = reconcile(root, now_ms() + 60_001, &BTreeSet::new()).unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].1, "ea");
        assert!(failed[0].2.contains("exited without"));
        assert!(reconcile(root, now_ms() + 600_000, &BTreeSet::new())
            .unwrap()
            .is_empty());
    }
    #[test]
    fn idle_reminds_once_escalates_once_and_activity_rearms() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let id = register(root, task("worker", "ea")).unwrap();
        let live = BTreeSet::from(["s-worker".into()]);
        let active = BTreeMap::from([("s-worker".into(), 0)]);
        let idle = BTreeMap::from([("s-worker".into(), 10_000)]);
        assert!(reconcile_idle(root, 0, &live, &active, 10_000)
            .unwrap()
            .is_empty());
        for start in [10_000, 100_000] {
            let reminder = reconcile_idle(root, start, &live, &idle, 10_000).unwrap();
            assert_eq!(reminder.len(), 1);
            assert_eq!(reminder[0].1, "worker");
            assert!(reminder[0].2.contains("Idle without a result: worker"));
            assert!(reminder[0].2.contains("status=blocked"));
            assert!(reconcile_idle(root, start + 9_999, &live, &idle, 10_000)
                .unwrap()
                .is_empty());
            let escalation = reconcile_idle(root, start + 10_000, &live, &idle, 10_000).unwrap();
            assert_eq!(escalation.len(), 1);
            assert_eq!(escalation[0].1, "ea");
            assert!(escalation[0].2.contains("Stalled child: worker"));
            assert!(reconcile_idle(root, start + 80_000, &live, &idle, 10_000)
                .unwrap()
                .is_empty());
            // A new read of the ledger preserves the latch; only activity re-arms it.
            assert!(read_state(root).unwrap().idle[&id].escalated);
            assert!(reconcile_idle(root, start + 80_001, &live, &active, 10_000)
                .unwrap()
                .is_empty());
            assert!(read_state(root).unwrap().idle.is_empty());
        }
        finish(
            root,
            0,
            "worker",
            &id,
            Status::Blocked,
            json!("needs input"),
        )
        .unwrap();
        assert!(read_state(root).unwrap().idle.is_empty());
        assert_eq!(
            reconcile_idle(root, 200_000, &live, &idle, 10_000).unwrap()[0].1,
            "ea"
        );
        assert!(reconcile_idle(root, 300_000, &live, &idle, 10_000)
            .unwrap()
            .is_empty());
        resume(root, 0, "ea", &id).unwrap();
        let resumed = reconcile(root, 300_001, &live).unwrap();
        assert_eq!(resumed.len(), 1);
        assert_eq!(resumed[0].1, "worker");
    }

    #[test]
    fn silent_running_work_gets_bounded_notices_without_failure_or_retirement() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let id = register(root, task("worker", "ea")).unwrap();
        let live = BTreeSet::from(["s-worker".into()]);
        let idle = BTreeMap::from([("s-worker".into(), 300_000)]);
        let reminder = reconcile_idle(root, 300_000, &live, &idle, 300_000).unwrap();
        assert_eq!(reminder.len(), 1);
        assert_eq!(reminder[0].1, "worker");
        // A missing capture is neither activity nor further evidence of a stall.
        assert!(reconcile(root, 600_000, &live).unwrap().is_empty());
        let escalation = reconcile_idle(root, 600_001, &live, &idle, 300_000).unwrap();
        assert_eq!(escalation.len(), 1);
        assert_eq!(escalation[0].1, "ea");
        assert!(escalation[0].2.contains("Stalled child: worker"));
        for now in [900_000, 1_200_000, 3_600_000] {
            assert!(reconcile_idle(root, now, &live, &idle, 300_000)
                .unwrap()
                .is_empty());
        }
        let ledger = read_state(root).unwrap();
        assert_eq!(ledger.tasks[&id].status, Status::Running);
        assert!(ledger.tasks[&id].result.is_none());
        assert!(!ledger.tasks[&id].retired);
    }

    #[test]
    fn cancelled_and_changed_results_each_notify_once() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let id = register(root, task("worker", "ea")).unwrap();
        let live = BTreeSet::from(["s-worker".into()]);
        for result in ["first blocker", "second blocker"] {
            finish(root, 0, "worker", &id, Status::Blocked, json!(result)).unwrap();
            let wakes = reconcile(root, 1, &live).unwrap();
            assert_eq!(wakes.len(), 1);
            assert_eq!(wakes[0].1, "ea");
            assert!(wakes[0].2.contains(result));
            finish(root, 0, "worker", &id, Status::Blocked, json!(result)).unwrap();
            assert!(reconcile(root, 600_000, &live).unwrap().is_empty());
        }
        cancel_agent(root, 0, "worker").unwrap();
        let cancelled = reconcile(root, 600_001, &BTreeSet::new()).unwrap();
        assert_eq!(cancelled.len(), 1);
        assert_eq!(cancelled[0].1, "ea");
        assert!(cancelled[0].2.contains("explicitly killed"));
        cancel_agent(root, 0, "worker").unwrap();
        assert!(reconcile(root, 900_000, &BTreeSet::new())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn killing_an_unacknowledged_finished_session_notifies_once() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let id = register(root, task("worker", "ea")).unwrap();
        let live = BTreeSet::from(["s-worker".into()]);
        finish(root, 0, "worker", &id, Status::Completed, json!("done")).unwrap();
        assert_eq!(reconcile(root, 0, &live).unwrap().len(), 1);
        cancel_agent(root, 0, "worker").unwrap();
        let retired = reconcile(root, 1, &BTreeSet::new()).unwrap();
        assert_eq!(retired.len(), 1);
        assert_eq!(retired[0].1, "ea");
        assert_eq!(read_state(root).unwrap().tasks[&id].result_revision, 1);
        cancel_agent(root, 0, "worker").unwrap();
        assert!(reconcile(root, 600_000, &BTreeSet::new())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_dead_blocked_session_preserves_its_report_and_reparents_children() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let pm = register(root, task("pm", "ea")).unwrap();
        let child = register(root, task("child", "pm")).unwrap();
        finish(
            root,
            0,
            "pm",
            &pm,
            Status::Blocked,
            json!("need credentials"),
        )
        .unwrap();
        let live = BTreeSet::from(["s-child".into()]);
        let wakes = reconcile(root, 0, &live).unwrap();
        assert_eq!(wakes.len(), 1);
        assert_eq!(wakes[0].1, "ea");
        let state = read_state(root).unwrap();
        assert_eq!(
            state.tasks[&pm].result.as_ref().unwrap()["previous_result"],
            "need credentials"
        );
        assert_eq!(state.tasks[&child].parent, "ea");
        assert!(reconcile(root, 600_000, &live).unwrap().is_empty());
    }

    #[test]
    fn a_reused_manager_name_receives_new_child_results() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let old = register(root, task("pm", "ea")).unwrap();
        cancel_agent(root, 0, "pm").unwrap();
        acknowledge(root, 0, "ea", &old, 1).unwrap();
        register(root, task("pm", "ea")).unwrap();
        let child = register(root, task("child", "pm")).unwrap();
        finish(
            root,
            0,
            "child",
            &child,
            Status::Completed,
            json!("new work"),
        )
        .unwrap();
        let live = BTreeSet::from(["s-pm".into(), "s-child".into()]);
        let wakes = reconcile(root, 0, &live).unwrap();
        assert!(wakes.iter().any(|(_, parent, _)| parent == "pm"));
    }

    #[test]
    fn failed_queue_handoff_preserves_the_delivery_obligation() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let id = register(root, task("worker", "ea")).unwrap();
        let live = BTreeSet::from(["s-worker".into()]);
        finish(root, 0, "worker", &id, Status::Failed, json!("failure")).unwrap();
        assert!(super::reconcile(root, &live, |_| { bail!("queue unavailable") }).is_err());
        let wakes = reconcile(root, 2, &live).unwrap();
        assert_eq!(wakes.len(), 1);
        assert_eq!(wakes[0].1, "ea");
        assert!(reconcile(root, 3, &live).unwrap().is_empty());
    }

    #[test]
    fn handoff_releases_ledger_lock_and_cannot_clear_a_newer_change() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let id = register(root, task("worker", "ea")).unwrap();
        let live = BTreeSet::from(["s-worker".into()]);
        finish(root, 0, "worker", &id, Status::Blocked, json!("first")).unwrap();
        super::reconcile(root, &live, |wakes| {
            assert_eq!(wakes.len(), 1);
            // The queue handoff may overlap another process's ledger change.
            finish(root, 0, "worker", &id, Status::Blocked, json!("second"))?;
            Ok(())
        })
        .unwrap();
        let next = reconcile(root, 1, &live).unwrap();
        assert_eq!(next.len(), 1);
        assert!(next[0].2.contains("second"));
        assert!(reconcile(root, 2, &live).unwrap().is_empty());
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
        assert!(reconcile(root, now_ms() + 1, &live).unwrap().is_empty());
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
    #[test]
    fn all_backend_adapters_restore_live_state_and_preserve_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let context: crate::manager::McpLaunchContext = serde_json::from_value(json!({
            "omar_dir":root,"ea_id":0,"agent_name":"pm","session_prefix":"s-",
            "default_command":"cat","default_workdir":".","health_idle_warning":15
        }))
        .unwrap();
        let own = register(root, task("pm", "ea")).unwrap();
        let child = register(root, task("child", "pm")).unwrap();
        for (backend, event) in [
            ("claude", "SessionStart"),
            ("codex", "SessionStart"),
            ("cursor", "postToolUse"),
            ("agy", "PreInvocation"),
            ("opencode", "PreInvocation"),
        ] {
            let before = backend_hook_response(
                &context,
                &json!({"hook_event_name":event,"source":"compact"}),
                backend,
            )
            .unwrap()
            .to_string();
            assert!(
                before.contains(&own) && before.contains(&child),
                "{backend} lost active ownership"
            );
        }
        finish(
            root,
            0,
            "child",
            &child,
            Status::Completed,
            json!("fresh result after compaction"),
        )
        .unwrap();
        for (backend, event) in [
            ("claude", "UserPromptSubmit"),
            ("codex", "PostToolUse"),
            ("cursor", "sessionStart"),
            ("agy", "PreInvocation"),
            ("opencode", "PreCompact"),
        ] {
            let state = backend_hook_response(&context, &json!({"hook_event_name":event}), backend)
                .unwrap()
                .to_string();
            assert!(
                state.contains("fresh result after compaction"),
                "{backend} reused stale context"
            );
        }
        for backend in ["claude", "codex"] {
            assert_eq!(
                backend_hook_response(&context, &json!({"hook_event_name":"Stop"}), backend)
                    .unwrap()["decision"],
                "block"
            );
            assert_eq!(
                backend_hook_response(
                    &context,
                    &json!({"hook_event_name":"Stop","stop_hook_active":true}),
                    backend
                )
                .unwrap(),
                json!({})
            );
        }
        assert!(backend_hook_response(
            &context,
            &json!({"hook_event_name":"stop","status":"completed","loop_count":0}),
            "cursor"
        )
        .unwrap()["followup_message"]
            .is_string());
        for input in [
            json!({"hook_event_name":"stop","status":"aborted"}),
            json!({"hook_event_name":"stop","status":"completed","loop_count":1}),
            json!({"hook_event_name":"beforeSubmitPrompt"}),
        ] {
            assert_eq!(
                backend_hook_response(&context, &input, "cursor").unwrap(),
                json!({})
            );
        }
        backend_hook_response(&context, &json!({"hook_event_name":"Stop"}), "opencode").unwrap();
        let wakes = reconcile(
            root,
            now_ms(),
            &BTreeSet::from(["s-pm".into(), "s-child".into()]),
        )
        .unwrap();
        assert!(wakes.iter().any(|(_, actor, _)| actor == "pm"));
    }
}

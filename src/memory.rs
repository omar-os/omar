//! Persistent memory — state snapshots in per-EA state directories
//!
//! Writes a human-readable markdown snapshot of the current OMAR state
//! so a newly created manager session can resume seamlessly.
//! All functions take a `state_dir` parameter for EA-scoped isolation.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::app::AgentInfo;
use crate::ea::EaId;
use crate::projects;
use crate::scheduler::ScheduledEvent;
use crate::tmux::TmuxClient;
use uuid::Uuid;

/// Per-file-type mutexes to serialize concurrent read-modify-write operations.
/// These are process-global (not per-EA) which is sufficient since all EAs
/// run in the same process and operate on separate state_dir paths.
static WORKER_TASKS_LOCK: Mutex<()> = Mutex::new(());
static AGENT_PARENTS_LOCK: Mutex<()> = Mutex::new(());
static AGENT_PROJECTS_LOCK: Mutex<()> = Mutex::new(());

/// Generic JSON helpers
fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    fs::read_to_string(path)
        .ok()
        .and_then(|c| serde_json::from_str(&c).ok())
}

/// Write JSON atomically: write to a unique sibling temp file then rename into place.
/// This prevents partial writes from being visible to concurrent readers.
fn write_json<T: serde::Serialize>(path: &Path, data: &T) {
    if let Ok(json) = serde_json::to_string_pretty(data) {
        write_text_atomic(path, &json);
    }
}

fn write_text_atomic(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state");
    let tmp = path.with_file_name(format!(".{}.{}.tmp", file_name, Uuid::new_v4()));
    if fs::write(&tmp, text).is_ok() {
        if fs::rename(&tmp, path).is_err() {
            let _ = fs::remove_file(&tmp);
        }
    } else {
        let _ = fs::remove_file(&tmp);
    }
}

/// Save a worker's task description (upsert)
pub fn save_worker_task_in(state_dir: &Path, session: &str, task: &str) {
    let path = state_dir.join("worker_tasks.json");
    let _guard = WORKER_TASKS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut tasks = load_worker_tasks_inner(state_dir);
    tasks.insert(session.to_string(), task.to_string());
    write_json(&path, &tasks);
}

/// Load all worker task mappings for an EA
pub fn load_worker_tasks_from(state_dir: &Path) -> HashMap<String, String> {
    let _guard = WORKER_TASKS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    load_worker_tasks_inner(state_dir)
}

/// Inner (lock-free) loader — only call while holding `WORKER_TASKS_LOCK`.
fn load_worker_tasks_inner(state_dir: &Path) -> HashMap<String, String> {
    let path = state_dir.join("worker_tasks.json");
    read_json(&path).unwrap_or_default()
}

/// Save an agent->project mapping (upsert)
pub fn save_agent_project_in(state_dir: &Path, session: &str, project_id: usize) {
    let path = state_dir.join("agent_projects.json");
    let _guard = AGENT_PROJECTS_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut projects = load_agent_projects_inner(state_dir);
    projects.insert(session.to_string(), project_id);
    write_json(&path, &projects);
}

/// Load all agent->project mappings for an EA
pub fn load_agent_projects_from(state_dir: &Path) -> HashMap<String, usize> {
    let _guard = AGENT_PROJECTS_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    load_agent_projects_inner(state_dir)
}

/// Inner (lock-free) loader — only call while holding `AGENT_PROJECTS_LOCK`.
fn load_agent_projects_inner(state_dir: &Path) -> HashMap<String, usize> {
    let path = state_dir.join("agent_projects.json");
    read_json(&path).unwrap_or_default()
}

/// Remove an agent->project mapping
pub fn remove_agent_project_in(state_dir: &Path, session: &str) {
    let path = state_dir.join("agent_projects.json");
    let _guard = AGENT_PROJECTS_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut projects = load_agent_projects_inner(state_dir);
    projects.remove(session);
    write_json(&path, &projects);
}

/// Save a child->parent mapping (upsert)
pub fn save_agent_parent_in(state_dir: &Path, child: &str, parent: &str) {
    let path = state_dir.join("agent_parents.json");
    let _guard = AGENT_PARENTS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut parents = load_agent_parents_inner(state_dir);
    parents.insert(child.to_string(), parent.to_string());
    write_json(&path, &parents);
}

/// Load all child->parent mappings for an EA
pub fn load_agent_parents_from(state_dir: &Path) -> HashMap<String, String> {
    let _guard = AGENT_PARENTS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    load_agent_parents_inner(state_dir)
}

/// Inner (lock-free) loader — only call while holding `AGENT_PARENTS_LOCK`.
fn load_agent_parents_inner(state_dir: &Path) -> HashMap<String, String> {
    let path = state_dir.join("agent_parents.json");
    read_json(&path).unwrap_or_default()
}

/// Remove a child->parent mapping
pub fn remove_agent_parent_in(state_dir: &Path, child: &str) {
    let path = state_dir.join("agent_parents.json");
    let _guard = AGENT_PARENTS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut parents = load_agent_parents_inner(state_dir);
    parents.remove(child);
    write_json(&path, &parents);
}

/// Load an agent's self-reported status
pub fn load_agent_status_in(state_dir: &Path, session_name: &str) -> Option<String> {
    let path = state_dir
        .join("status")
        .join(format!("{}.md", session_name));
    fs::read_to_string(&path)
        .ok()
        .filter(|s| !s.trim().is_empty())
}

/// Save an agent's status
pub fn save_agent_status_in(state_dir: &Path, session_name: &str, status: &str) {
    let dir = state_dir.join("status");
    fs::create_dir_all(&dir).ok();
    let path = dir.join(format!("{}.md", session_name));
    write_text_atomic(&path, status);
}

/// Load the memory file contents (empty string if missing)
pub fn load_memory_from(state_dir: &Path) -> String {
    let path = state_dir.join("memory.md");
    truncate_for_prompt(fs::read_to_string(&path).unwrap_or_default())
}

/// Maximum bytes of free-form context (memory snapshot or manager notes) that
/// will be inlined into the EA system prompt. Linux exec(3) limits any single
/// argv element to roughly 128 KB (32 pages × 4 KB) regardless of `ARG_MAX`,
/// and the codex backend bakes the prompt into a single `-c
/// "developer_instructions='''...'''"` arg. Without this cap, a manager-notes
/// file that grows past ~120 KB silently blows up `manager start` with
/// `Argument list too long`, leaving the user with `can't find session`. We
/// keep the most recent bytes (tail) since recency matters most for resume
/// context and prepend a truncation marker so the agent knows.
// Reserve 8 KiB for headings, truncation markers, placeholder expansion, and
// backend arguments. Split the remaining space between the snapshot and notes
// after accounting for the embedded template and structured knowledge.
const PROMPT_CONTEXT_BYTE_CAP: usize =
    (120 * 1024 - crate::manager::PROMPT_EA.len() - crate::knowledge::CONTEXT_BYTES) / 2;

fn truncate_for_prompt(content: String) -> String {
    if content.len() <= PROMPT_CONTEXT_BYTE_CAP {
        return content;
    }
    let cut = content.len() - PROMPT_CONTEXT_BYTE_CAP;
    // Advance to the next char boundary so we never split a UTF-8 codepoint.
    let mut start = cut;
    while start < content.len() && !content.is_char_boundary(start) {
        start += 1;
    }
    // Then to the next line boundary so the surviving section starts cleanly.
    let tail = if let Some(nl) = content[start..].find('\n') {
        &content[start + nl + 1..]
    } else {
        &content[start..]
    };
    format!(
        "[... truncated {} earlier bytes; showing most recent tail to keep \
         system-prompt under the OS argv size limit ...]\n{}",
        start, tail
    )
}

/// Returns the EA-specific manager notes file path.
/// Each EA uses its own file: ~/.omar/manager_notes_ea<ID>.md
/// (e.g. ~/.omar/manager_notes_ea0.md for EA 0)
pub fn manager_notes_path(omar_dir: &Path, ea_id: EaId) -> PathBuf {
    omar_dir.join(format!("manager_notes_ea{}.md", ea_id))
}

/// Load manager notes for an EA (empty string if file is missing).
pub fn load_manager_notes(omar_dir: &Path, ea_id: EaId) -> String {
    let path = manager_notes_path(omar_dir, ea_id);
    truncate_for_prompt(fs::read_to_string(&path).unwrap_or_default())
}

/// Write a full state snapshot — SCOPED to one EA
pub fn write_memory_to(
    state_dir: &Path,
    agents: &[AgentInfo],
    manager: Option<&AgentInfo>,
    manager_session: &str,
    client: &TmuxClient,
    events: &[ScheduledEvent],
) {
    let project_list = projects::load_projects_from(state_dir);

    // Read task metadata without pruning. Session discovery can lag tmux
    // creation during large fan-outs; pruning here can erase valid tasks.
    // Explicit kill/delete paths own cleanup.
    let worker_tasks = {
        let _guard = WORKER_TASKS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        load_worker_tasks_inner(state_dir)
    };

    let mut out = String::from("# OMAR State\n\n");

    // Active projects
    if !project_list.is_empty() {
        out.push_str("## Active Projects\n");
        for p in &project_list {
            out.push_str(&format!("{}. {}\n", p.id, p.name));
        }
        out.push('\n');
    }

    // Active workers
    if !agents.is_empty() {
        out.push_str("## Active Agents\n");
        for agent in agents {
            let health = agent.health.as_str();
            let task_desc = worker_tasks
                .get(&agent.session.name)
                .map(|t| t.as_str())
                .unwrap_or("(no task assigned)");
            out.push_str(&format!(
                "- {} ({}): {}\n",
                agent.session.name, health, task_desc
            ));
        }
        out.push('\n');
    }

    // Scheduled events — include exact periods and full payloads for recovery
    if !events.is_empty() {
        out.push_str("## Scheduled Events\n");
        for ev in events {
            let type_label = match ev.recurring_ns {
                Some(ns) => {
                    let secs = ns / 1_000_000_000;
                    format!("cron every {}s (period_ns={})", secs, ns)
                }
                None => "once".to_string(),
            };
            out.push_str(&format!(
                "- id={} [{}] {} -> {}\n  payload: {}\n",
                ev.id, type_label, ev.sender, ev.receiver, ev.payload
            ));
        }
        out.push('\n');
    }

    // Manager status
    out.push_str("## Manager\n");
    match manager {
        Some(_) => out.push_str("- Status: Running\n"),
        None => out.push_str("- Status: Not running\n"),
    }
    out.push('\n');

    // Manager's recent context (last ~20 lines of pane output)
    if manager.is_some() {
        if let Ok(output) = client.capture_pane(manager_session, 20) {
            let trimmed: Vec<&str> = output
                .lines()
                .map(|l| l.trim_end())
                .filter(|l| !l.is_empty())
                .collect();
            if !trimmed.is_empty() {
                out.push_str("## Manager's Recent Context\n");
                for line in &trimmed {
                    out.push_str(&format!("> {}\n", line));
                }
                out.push('\n');
            }
        }
    }

    let path = state_dir.join("memory.md");
    fs::create_dir_all(state_dir).ok();
    write_text_atomic(&path, &out);
}

/// Clear runtime/transient EA state that should not leak across dashboard sessions.
/// Keeps durable artifacts such as projects, task metadata, hierarchy, memory,
/// scheduled events, and manager notes intact so the dashboard can resume.
#[cfg(test)]
pub fn clear_runtime_state_in(state_dir: &Path) {
    let _ = fs::remove_dir_all(state_dir.join("status"));
    let _ = fs::create_dir_all(state_dir.join("status"));
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)]
    use super::*;

    #[test]
    fn manager_notes_path_is_ea_scoped() {
        let base = std::path::PathBuf::from("/home/user/.omar");
        assert_eq!(
            manager_notes_path(&base, 0),
            std::path::PathBuf::from("/home/user/.omar/manager_notes_ea0.md")
        );
        assert_eq!(
            manager_notes_path(&base, 1),
            std::path::PathBuf::from("/home/user/.omar/manager_notes_ea1.md")
        );
        assert_eq!(
            manager_notes_path(&base, 42),
            std::path::PathBuf::from("/home/user/.omar/manager_notes_ea42.md")
        );
    }

    #[test]
    fn load_manager_notes_returns_empty_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let notes = load_manager_notes(dir.path(), 0);
        assert!(notes.is_empty());
    }

    #[test]
    fn load_manager_notes_reads_ea_specific_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = manager_notes_path(dir.path(), 3);
        std::fs::write(&path, "EA 3 notes").unwrap();

        // EA 3 gets its notes
        assert_eq!(load_manager_notes(dir.path(), 3), "EA 3 notes");
        // EA 0 does NOT see EA 3's notes
        assert!(load_manager_notes(dir.path(), 0).is_empty());
    }

    #[test]
    fn clear_runtime_state_only_removes_status_files() {
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path();
        std::fs::create_dir_all(state_dir.join("status")).unwrap();
        std::fs::write(state_dir.join("status/omar-agent-0-worker.md"), "stale").unwrap();
        std::fs::write(state_dir.join("worker_tasks.json"), r#"{"worker":"task"}"#).unwrap();
        std::fs::write(state_dir.join("agent_parents.json"), r#"{"worker":"ea"}"#).unwrap();
        std::fs::write(state_dir.join("agent_projects.json"), r#"{"worker":1}"#).unwrap();
        std::fs::write(state_dir.join("memory.md"), "resume context").unwrap();

        clear_runtime_state_in(state_dir);

        assert!(state_dir.join("status").is_dir());
        assert!(!state_dir.join("status/omar-agent-0-worker.md").exists());
        assert_eq!(
            std::fs::read_to_string(state_dir.join("worker_tasks.json")).unwrap(),
            r#"{"worker":"task"}"#
        );
        assert_eq!(
            std::fs::read_to_string(state_dir.join("agent_parents.json")).unwrap(),
            r#"{"worker":"ea"}"#
        );
        assert_eq!(
            std::fs::read_to_string(state_dir.join("agent_projects.json")).unwrap(),
            r#"{"worker":1}"#
        );
        assert_eq!(
            std::fs::read_to_string(state_dir.join("memory.md")).unwrap(),
            "resume context"
        );
    }

    #[test]
    fn scheduled_event_format_includes_period_and_payload() {
        // Verify the format string used in write_memory includes exact details
        let ns: u64 = 300_000_000_000; // 300s = 5min
        let secs = ns / 1_000_000_000;
        let label = format!("cron every {}s (period_ns={})", secs, ns);
        assert_eq!(label, "cron every 300s (period_ns=300000000000)");

        let line = format!(
            "- id={} [{}] {} -> {}\n  payload: {}\n",
            "abc-123", label, "manager", "worker-1", "check deployment status"
        );
        assert!(line.contains("period_ns=300000000000"));
        assert!(line.contains("payload: check deployment status"));
        assert!(line.contains("id=abc-123"));
    }
}

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

static METRICS_ENABLED: AtomicBool = AtomicBool::new(false);
static METRICS_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

pub fn configure(enabled: bool) {
    METRICS_ENABLED.store(enabled, Ordering::Relaxed);
}

fn enabled() -> bool {
    METRICS_ENABLED.load(Ordering::Relaxed)
}

fn sink_path() -> PathBuf {
    crate::sessions::state_root()
        .join("metrics")
        .join("spawn_metrics.jsonl")
}

fn now_unix_ns() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn write_metric(event: &str, payload: serde_json::Value) {
    if !enabled() {
        return;
    }

    let lock = METRICS_LOCK.get_or_init(|| Mutex::new(()));
    let _guard = match lock.lock() {
        Ok(g) => g,
        Err(_) => return,
    };

    let path = sink_path();
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }

    let mut entry = serde_json::Map::new();
    entry.insert(
        "timestamp_ns".to_string(),
        serde_json::Value::String(now_unix_ns().to_string()),
    );
    entry.insert(
        "pid".to_string(),
        serde_json::Value::Number((std::process::id() as u64).into()),
    );
    entry.insert(
        "event".to_string(),
        serde_json::Value::String(event.to_string()),
    );
    if let Some(obj) = payload.as_object() {
        for (k, v) in obj {
            entry.insert(k.clone(), v.clone());
        }
    }

    let line = match serde_json::to_string(&serde_json::Value::Object(entry)) {
        Ok(s) => s,
        Err(_) => return,
    };

    let mut file = match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(f) => f,
        Err(_) => return,
    };

    let _ = writeln!(file, "{}", line);
}

pub fn record_backend_bootstrap(backend: &str) {
    write_metric(
        "backend_bootstrap",
        serde_json::json!({
            "backend": backend
        }),
    );
}

pub struct AgentSpawnMetric<'a> {
    pub ea_id: u32,
    pub session: &'a str,
    pub short_name: &'a str,
    pub backend: &'a str,
    pub has_task: bool,
    pub spawn_lock_wait_ms: u64,
    pub tmux_spawn_ms: u64,
    pub total_spawn_ms: u64,
}

pub fn record_agent_spawn(metric: AgentSpawnMetric<'_>) {
    write_metric(
        "agent_spawn",
        serde_json::json!({
            "ea_id": metric.ea_id,
            "session": metric.session,
            "short_name": metric.short_name,
            "backend": metric.backend,
            "has_task": metric.has_task,
            "spawn_lock_wait_ms": metric.spawn_lock_wait_ms,
            "tmux_spawn_ms": metric.tmux_spawn_ms,
            "total_spawn_ms": metric.total_spawn_ms
        }),
    );
}

pub fn record_prompt_delivery(
    ea_id: u32,
    session: &str,
    backend: &str,
    delivery_ms: u64,
    success: bool,
) {
    write_metric(
        "initial_prompt_delivery",
        serde_json::json!({
            "ea_id": ea_id,
            "session": session,
            "backend": backend,
            "delivery_ms": delivery_ms,
            "success": success
        }),
    );
}

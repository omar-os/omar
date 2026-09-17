mod client;
#[cfg(test)]
mod delivery_tests;
mod health;
mod session;

pub use client::{flatten_agent_name, tmux_command, DeliveryOptions, TmuxClient};
pub use health::{HealthChecker, HealthState};
pub use session::Session;

/// Backend startup banners used for launch diagnostics. Message delivery waits
/// for the backend channel itself and never sends input to these widgets.
pub fn backend_readiness_markers(backend: &str) -> &'static [&'static str] {
    match backend {
        "codex" => &["OpenAI Codex"],
        "cursor" => &["Cursor Agent"],
        // `agy` is not available in CI/local discovery here, so avoid
        // overfitting to an unverified banner and let delivery use the
        // existing stable-pane fallback.
        "agy" => &[],
        "claude" => &["Claude Code", "❯"],
        "opencode" => &["tab agents", "ctrl+p commands"],
        _ => &[],
    }
}

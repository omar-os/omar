//! Conservative, read-only observers for interactive agent terminals.
//!
//! Claude Code, Cursor and Antigravity do not offer a passive approval stream
//! that can be attached to an already-running terminal session. Their adapters
//! therefore inspect only the native permission overlay currently visible in
//! the agent's own tmux pane. They neither write to the pane nor install a
//! hook that could influence the agent's permission policy.

use crate::approvals::{
    ApprovalConnection, ApprovalDetails, ApprovalObserver, ApprovalOutcome, ApprovalSink,
    ApprovalTarget,
};
use crate::tmux::TmuxClient;
use std::thread;
use std::time::Duration;

const POLL_INTERVAL: Duration = Duration::from_millis(750);
const CAPTURE_LINES: i32 = 50;
const ACTIVE_PROMPT_TAIL_LINES: usize = 18;

#[derive(Clone, Copy)]
enum Backend {
    Claude,
    Cursor,
    Antigravity,
}

pub(super) struct TerminalObserver {
    session: String,
    backend: Backend,
}

impl TerminalObserver {
    pub(super) fn claude_from_target(target: ApprovalTarget) -> Box<dyn ApprovalObserver> {
        Self::from_target(target, Backend::Claude)
    }

    pub(super) fn cursor_from_target(target: ApprovalTarget) -> Box<dyn ApprovalObserver> {
        Self::from_target(target, Backend::Cursor)
    }

    pub(super) fn antigravity_from_target(target: ApprovalTarget) -> Box<dyn ApprovalObserver> {
        Self::from_target(target, Backend::Antigravity)
    }

    fn from_target(target: ApprovalTarget, backend: Backend) -> Box<dyn ApprovalObserver> {
        Box::new(Self {
            session: target.session,
            backend,
        })
    }
}

impl ApprovalObserver for TerminalObserver {
    fn observe(self: Box<Self>, sink: ApprovalSink) {
        let client = TmuxClient::new("");
        let mut tracker = Tracker::default();
        while sink.active() {
            match client.capture_pane_plain(&self.session, CAPTURE_LINES) {
                Ok(pane) => {
                    sink.connection(ApprovalConnection::Connected);
                    tracker.reconcile(&sink, detect(self.backend, &pane));
                }
                Err(_) => sink.connection(ApprovalConnection::Disconnected),
            }
            thread::sleep(POLL_INTERVAL);
        }
    }
}

#[derive(Default)]
struct Tracker {
    pending: Option<(String, String)>, // public id, current prompt fingerprint
}

impl Tracker {
    fn reconcile(&mut self, sink: &ApprovalSink, detected: Option<DetectedApproval>) {
        match detected {
            Some(detected)
                if self
                    .pending
                    .as_ref()
                    .is_some_and(|(_, id)| id == &detected.id) => {}
            Some(detected) => {
                if let Some((request, _)) = self.pending.take() {
                    // A different native overlay superseded the previous one.
                    // We know only that it is gone, never whether it was allowed.
                    sink.resolve(&request, ApprovalOutcome::Resolved);
                }
                let id = detected.id.clone();
                if let Some(request) = sink.request(detected.details()) {
                    self.pending = Some((request.request_id, id));
                }
            }
            None => {
                if let Some((request, _)) = self.pending.take() {
                    // Closing a native overlay is an acknowledgement of neither
                    // approval nor denial. The terminal remains authoritative.
                    sink.resolve(&request, ApprovalOutcome::Resolved);
                }
            }
        }
    }
}

struct DetectedApproval {
    id: String,
    summary: String,
    tool: String,
    scope: String,
    command: Option<String>,
}

impl DetectedApproval {
    fn details(self) -> ApprovalDetails {
        ApprovalDetails {
            summary: self.summary,
            tool: self.tool,
            scope: self.scope,
            command: self.command,
            cwd: None,
            started: None,
        }
    }
}

fn detect(backend: Backend, pane: &str) -> Option<DetectedApproval> {
    match backend {
        Backend::Claude => detect_claude(pane),
        Backend::Cursor => detect_cursor(pane),
        Backend::Antigravity => detect_antigravity(pane),
    }
}

fn detect_claude(pane: &str) -> Option<DetectedApproval> {
    let lines = pane.lines().collect::<Vec<_>>();
    let prompt = recent_index(&lines, is_claude_terminator)?;
    let below = &lines[prompt + 1..];
    let below_text = below.join("\n").to_ascii_lowercase();

    // AskUserQuestion has numbered choices too, but it is not an approval.
    if below_text.contains("type something.") && below_text.contains("enter to select") {
        return None;
    }
    // Leaving plan mode uses a different approval model and must not be shown
    // as a tool permission request.
    if below_text.contains("use auto mode") || below_text.contains("/.claude/plans/") {
        return None;
    }
    if !below.iter().any(|line| is_numbered_yes(line)) {
        return None;
    }

    let terminator = lower_line(lines[prompt]);
    let (summary, tool) = if terminator.contains("make this edit") {
        ("Apply file changes", "File changes")
    } else if terminator.contains("create ") {
        ("Create a file", "File changes")
    } else {
        ("Run a command", "Command execution")
    };
    let command = block_after_header(&lines[..prompt], "bash command");
    Some(DetectedApproval {
        id: fingerprint("claude", &terminator, command.as_deref()),
        summary: summary.into(),
        tool: tool.into(),
        scope: "Review the requested scope in the Claude Code terminal".into(),
        command,
    })
}

fn detect_cursor(pane: &str) -> Option<DetectedApproval> {
    let lines = pane.lines().collect::<Vec<_>>();
    let command_prompt = recent_index(&lines, |line| {
        lower_line(line).contains("run this command?")
    });
    let fetch_prompt = recent_index(&lines, |line| {
        lower_line(line).contains("allow this web fetch?")
    });
    let (prompt, summary, tool) = match (command_prompt, fetch_prompt) {
        (Some(command), Some(fetch)) if command > fetch => {
            (command, "Run a command", "Command execution")
        }
        (Some(_), Some(fetch)) => (fetch, "Fetch a web resource", "Web fetch"),
        (Some(command), None) => (command, "Run a command", "Command execution"),
        (None, Some(fetch)) => (fetch, "Fetch a web resource", "Web fetch"),
        (None, None) => return None,
    };
    let below = &lines[prompt + 1..];
    // The native overlay presents either the one-time run or the skip action.
    if !below.iter().any(|line| {
        let line = lower_line(line);
        line.contains("run (once)") || line.starts_with("skip")
    }) {
        return None;
    }
    let marker = lower_line(lines[prompt]);
    Some(DetectedApproval {
        id: fingerprint("cursor", &marker, None),
        summary: summary.into(),
        tool: tool.into(),
        scope: "Review the requested scope in the Cursor terminal".into(),
        command: None,
    })
}

fn detect_antigravity(pane: &str) -> Option<DetectedApproval> {
    let lines = pane.lines().collect::<Vec<_>>();
    let prompt = recent_index(&lines, |line| {
        lower_line(line).contains("requesting permission for:")
    })?;
    let below = &lines[prompt + 1..];
    if !below
        .iter()
        .any(|line| lower_line(line).contains("do you want to proceed?"))
        || !below.iter().any(|line| is_numbered_yes(line))
    {
        return None;
    }
    let header = clean_line(lines[prompt]);
    let header_lower = header.to_ascii_lowercase();
    let command = header
        .split_once(':')
        .map(|(_, command)| command)
        .map(str::trim)
        .filter(|command| !command.is_empty())
        .map(display_text)
        .or_else(|| {
            below
                .iter()
                .map(|line| clean_line(line))
                .find(|line| {
                    !line.is_empty()
                        && !line
                            .to_ascii_lowercase()
                            .contains("do you want to proceed?")
                })
                .map(|line| display_text(&line))
        });
    Some(DetectedApproval {
        id: fingerprint("antigravity", &header_lower, command.as_deref()),
        summary: "Run a command".into(),
        tool: "Command execution".into(),
        scope: "Review the requested scope in the Antigravity terminal".into(),
        command,
    })
}

fn recent_index(lines: &[&str], matches: impl Fn(&str) -> bool) -> Option<usize> {
    let start = lines.len().saturating_sub(ACTIVE_PROMPT_TAIL_LINES);
    (start..lines.len())
        .rev()
        .find(|&index| matches(lines[index]))
}

fn is_claude_terminator(line: &str) -> bool {
    let line = lower_line(line);
    [
        "do you want to proceed?",
        "would you like to proceed?",
        "do you want to make this edit",
        "do you want to create ",
    ]
    .iter()
    .any(|marker| line.contains(marker))
}

fn is_numbered_yes(line: &str) -> bool {
    let line = lower_line(line);
    line.contains("1. yes") || line.contains("1) yes")
}

fn block_after_header(lines: &[&str], header: &str) -> Option<String> {
    let header = lines
        .iter()
        .rposition(|line| lower_line(line).contains(header))?;
    let mut block = Vec::new();
    for line in &lines[header + 1..] {
        let line = clean_line(line);
        if line.is_empty() {
            if !block.is_empty() {
                break;
            }
            continue;
        }
        block.push(line);
        if block.len() == 4 {
            break;
        }
    }
    (!block.is_empty()).then(|| display_text(&block.join("\n")))
}

fn clean_line(line: &str) -> String {
    line.trim()
        .trim_matches(|character| matches!(character, '│' | '|' | '╭' | '╮' | '╰' | '╯'))
        .trim()
        .into()
}

fn lower_line(line: &str) -> String {
    clean_line(line).to_ascii_lowercase()
}

/// Do not publish a raw terminal command that might contain a credential.
fn display_text(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    if [
        "password",
        "passwd",
        "secret",
        "token",
        "api_key",
        "api-key",
        "authorization",
        "bearer ",
        "private key",
        "sk-",
    ]
    .iter()
    .any(|word| lower.contains(word))
    {
        return "Sensitive details hidden — review in the agent terminal".into();
    }
    text.chars()
        .filter(|character| !character.is_control() || *character == '\n' || *character == '\t')
        .take(2000)
        .collect()
}

fn fingerprint(backend: &str, prompt: &str, command: Option<&str>) -> String {
    format!("{backend}\n{prompt}\n{}", command.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_claude_command_permission_but_not_a_question_picker() {
        let approval = r#"
╭─────────────────────────────────────────────────╮
│ Bash command                                      │
│                                                   │
│   git status                                      │
│                                                   │
│ Do you want to proceed?                           │
│ ❯ 1. Yes                                          │
│   2. No                                           │
╰─────────────────────────────────────────────────╯
"#;
        let found = detect(Backend::Claude, approval).expect("approval is detected");
        assert_eq!(found.summary, "Run a command");
        assert_eq!(found.command.as_deref(), Some("git status"));

        let question = r#"
What's your favorite color?
❯ 1. Blue
  2. Green
  3. Type something.
Enter to select · ↑/↓ to navigate · Esc to cancel
"#;
        assert!(detect(Backend::Claude, question).is_none());
    }

    #[test]
    fn detects_cursor_command_and_web_fetch_overlays() {
        let command = r#"
Run this command?
  → Run (once) (y)
    Add Shell(curl) to allowlist? (tab)
    Skip (esc or n)
"#;
        let found = detect(Backend::Cursor, command).expect("command approval is detected");
        assert_eq!(found.tool, "Command execution");

        let fetch = r#"
Allow this web fetch?
  → Run (once) (y)
    Skip (esc or n)
"#;
        let found = detect(Backend::Cursor, fetch).expect("fetch approval is detected");
        assert_eq!(found.tool, "Web fetch");
    }

    #[test]
    fn detects_antigravity_permission_and_redacts_command_secrets() {
        let permission = r#"
Requesting permission for: curl https://example.com
Do you want to proceed?
> 1. Yes
  2. No
"#;
        let found = detect(Backend::Antigravity, permission).expect("approval is detected");
        assert_eq!(found.command.as_deref(), Some("curl https://example.com"));

        let secret = r#"
Requesting permission for: curl -H 'Authorization: Bearer token' https://example.com
Do you want to proceed?
> 1. Yes
"#;
        let found = detect(Backend::Antigravity, secret).expect("approval is detected");
        assert_eq!(
            found.command.as_deref(),
            Some("Sensitive details hidden — review in the agent terminal")
        );
    }

    #[test]
    fn ignores_old_prompt_outside_the_current_pane_tail() {
        let mut pane = vec![
            "Run this command?",
            "  → Run (once) (y)",
            "    Skip (esc or n)",
        ];
        pane.extend(vec!["agent output"; ACTIVE_PROMPT_TAIL_LINES]);
        assert!(detect(Backend::Cursor, &pane.join("\n")).is_none());
    }
}

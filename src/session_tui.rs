//! A terminal client, never a scheduler or owner of agent processes.
use crate::sessions::{self, Session};
use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    widgets::{Block, Borders, Paragraph},
    Terminal,
};
use serde_json::{json, Value};
use std::io;
use std::time::{Duration, Instant};

struct TerminalGuard;
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), crossterm::cursor::Show, LeaveAlternateScreen);
    }
}
pub fn attach(mut session: Session, initial_ea: Option<&str>) -> Result<()> {
    sessions::rpc(&session, json!({"op":"hello"}), Duration::from_secs(3))?;
    enable_raw_mode()?;
    let _guard = TerminalGuard;
    execute!(io::stdout(), EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut data = Value::Null;
    let mut status = String::new();
    let mut ea = initial_ea.unwrap_or("0").to_string();
    let mut input: Option<String> = None;
    let mut agent_index = 0usize;
    let mut last_refresh = Instant::now() - Duration::from_secs(2);
    loop {
        if last_refresh.elapsed() >= Duration::from_secs(1) {
            match sessions::rpc(
                &session,
                json!({"op":"overview"}),
                Duration::from_millis(750),
            ) {
                Ok(value) => {
                    data = value;
                    if status.starts_with("Disconnected") {
                        status.clear();
                    }
                }
                Err(error) => {
                    status = format!(
                        "Disconnected: {error}. Reconnecting; q detaches, s switches sessions."
                    )
                }
            }
            last_refresh = Instant::now();
        }
        let eas = data["eas"].as_array().cloned().unwrap_or_default();
        let selected = eas
            .iter()
            .find(|item| item["id"].as_u64() == ea.parse::<u64>().ok() || item["name"] == ea)
            .or_else(|| eas.first());
        let selected_id = selected.and_then(|item| item["id"].as_u64()).unwrap_or(0);
        let agents = data["agents"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|a| a["ea_id"] == selected_id)
            .collect::<Vec<_>>();
        agent_index = agent_index.min(agents.len().saturating_sub(1));
        terminal.draw(|frame| {
            let areas = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Min(4),
                    Constraint::Length(4),
                ])
                .split(frame.area());
            let title = format!(
                "{} ({}) · {} · EA {} · {}",
                session.name, session.id, session.version, selected_id, session.url
            );
            frame.render_widget(
                Paragraph::new(title)
                    .block(Block::default().borders(Borders::ALL).title("OMAR session")),
                areas[0],
            );
            let mut lines = vec!["Executive assistants (Tab selects):".to_string()];
            for item in &eas {
                lines.push(format!(
                    "{} {}  {}",
                    if item["id"] == selected_id { ">" } else { " " },
                    item["id"],
                    item["name"].as_str().unwrap_or("")
                ));
            }
            lines.push("\nTopology runs:".into());
            for run in data["runs"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|run| run["ea_id"] == selected_id)
            {
                lines.push(format!(
                    "{}  {}  {}",
                    run["team"].as_str().unwrap_or(""),
                    run["status"].as_str().unwrap_or(""),
                    run["run_id"].as_str().unwrap_or("")
                ));
            }
            lines.push("\nAgents:".into());
            for (index, agent) in agents.iter().enumerate() {
                lines.push(format!("{} {}", if index == agent_index { ">" } else { " " }, agent["name"].as_str().unwrap_or("")));
            }
            frame.render_widget(
                Paragraph::new(lines.join("\n")).block(Block::default().borders(Borders::ALL)),
                areas[1],
            );
            let footer = if let Some(text) = &input {
                format!("Switch session: {text}\nEnter selects · Esc cancels\n{status}")
            } else {
                format!("q / Esc detach · s switch session · Tab next EA · w open web · ↑/↓ agent · Enter inspect\n{status}")
            };
            frame.render_widget(
                Paragraph::new(footer).style(Style::default().fg(Color::LightMagenta)),
                areas[2],
            );
        })?;
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if let Some(text) = input.as_mut() {
                match key.code {
                    KeyCode::Esc => input = None,
                    KeyCode::Backspace => {
                        text.pop();
                    }
                    KeyCode::Enter => {
                        let target = text.clone();
                        input = None;
                        match sessions::resolve(&target).and_then(|s| {
                            sessions::rpc(&s, json!({"op":"hello"}), Duration::from_secs(2))
                                .map(|_| s)
                        }) {
                            Ok(next) => {
                                session = next;
                                data = Value::Null;
                                status.clear();
                                last_refresh = Instant::now() - Duration::from_secs(2);
                            }
                            Err(error) => status = error.to_string(),
                        }
                    }
                    KeyCode::Char(c) => text.push(c),
                    _ => (),
                }
            } else {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => break,
                    KeyCode::Char('s') => {
                        status = sessions::discover()?
                            .iter()
                            .map(|s| format!("{} ({})", s.name, s.state))
                            .collect::<Vec<_>>()
                            .join(", ");
                        input = Some(String::new());
                    }
                    KeyCode::Char('w') => crate::open_browser(&session.url),
                    KeyCode::Up => agent_index = agent_index.saturating_sub(1),
                    KeyCode::Down => {
                        agent_index = (agent_index + 1).min(agents.len().saturating_sub(1))
                    }
                    KeyCode::Enter => {
                        if let Some(agent) =
                            agents.get(agent_index).and_then(|a| a["name"].as_str())
                        {
                            disable_raw_mode()?;
                            execute!(io::stdout(), LeaveAlternateScreen)?;
                            let result = std::process::Command::new("tmux")
                                .args([
                                    "-L",
                                    &session.tmux_server,
                                    "attach-session",
                                    "-t",
                                    &format!("={agent}"),
                                ])
                                .env_remove("TMUX")
                                .status();
                            enable_raw_mode()?;
                            execute!(io::stdout(), EnterAlternateScreen)?;
                            terminal.clear()?;
                            status = match result {
                                Ok(s) if s.success() => String::new(),
                                Ok(s) => format!("Terminal exited: {s}"),
                                Err(e) => e.to_string(),
                            };
                        }
                    }
                    KeyCode::Tab if !eas.is_empty() => {
                        let index = eas.iter().position(|e| e["id"] == selected_id).unwrap_or(0);
                        ea = eas[(index + 1) % eas.len()]["id"].to_string();
                    }
                    _ => (),
                }
            }
        }
    }
    Ok(())
}

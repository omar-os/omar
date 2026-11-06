//! `omar attach --tui`: Mission Control's workflow in a terminal. Chat with
//! the assistant, read and deploy its designs and their proofs, follow runs
//! in logical time. A client of the session's HTTP API and nothing else, so
//! it shows what Mission Control shows and works wherever that API reaches.

mod client;
mod state;
mod view;

use anyhow::Result;
use crossterm::event::{self, Event, KeyEventKind};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use serde_json::json;
use std::io::stdout;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use client::{Client, Remote};
use state::{Action, Console};

const REFRESH: Duration = Duration::from_millis(700);

enum Update {
    Remote(Box<Result<Remote, String>>),
    Done(Result<String, String>),
}

pub fn run(session: &crate::sessions::Session) -> Result<()> {
    let client = Client::new(&session.url)?;
    let mut console = Console::new(session.name.clone(), session.url.clone());
    let (updates, inbox) = mpsc::channel();
    let selected: Arc<Mutex<Option<String>>> = Arc::default();
    let (wake, woken) = mpsc::channel::<()>();
    {
        let (client, updates, selected) = (client.clone(), updates.clone(), selected.clone());
        let mut kept = std::collections::HashMap::new();
        std::thread::spawn(move || loop {
            let run = selected.lock().expect("selection poisoned").clone();
            let remote = client
                .fetch(run.as_deref(), &mut kept)
                .map_err(|error| format!("{error:#}"));
            if updates.send(Update::Remote(Box::new(remote))).is_err() {
                return;
            }
            // An action asks for a refresh at once rather than at the next tick.
            if let Err(mpsc::RecvTimeoutError::Disconnected) = woken.recv_timeout(REFRESH) {
                return;
            }
        });
    }

    let mut terminal = enter()?;
    let result = (|| -> Result<()> {
        loop {
            while let Ok(update) = inbox.try_recv() {
                match update {
                    Update::Remote(remote) => match *remote {
                        Ok(remote) => {
                            console.remote = remote;
                            console.offline = None;
                        }
                        Err(error) => console.offline = Some(error),
                    },
                    Update::Done(outcome) => {
                        console.notice = Some(outcome.unwrap_or_else(|error| error));
                        let _ = wake.send(());
                    }
                }
            }
            terminal.draw(|frame| view::render(frame, &console))?;
            if !event::poll(Duration::from_millis(100))? {
                continue;
            }
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            console.notice = None;
            let action = console.key(key);
            *selected.lock().expect("selection poisoned") = console.run.clone();
            match action {
                None => {}
                Some(Action::Quit) => return Ok(()),
                Some(Action::OpenAgents) => {
                    leave(&mut terminal)?;
                    let status = std::process::Command::new(std::env::current_exe()?)
                        .args(["attach", "-s", &session.name, "--tui", "--agents"])
                        .status();
                    terminal = enter()?;
                    if let Err(error) = status {
                        console.notice =
                            Some(format!("could not open the agent dashboard: {error}"));
                    }
                }
                Some(action) => {
                    let Some((label, path, body)) = request(&console, action) else {
                        continue;
                    };
                    console.notice = Some(format!("{label}..."));
                    let (client, updates) = (client.clone(), updates.clone());
                    std::thread::spawn(move || {
                        let outcome = client
                            .post(&path, &body)
                            .map(|_| format!("{label}: done"))
                            .map_err(|error| format!("{label} failed: {error:#}"));
                        let _ = updates.send(Update::Done(outcome));
                    });
                }
            }
        }
    })();
    leave(&mut terminal)?;
    result
}

/// The endpoint an action calls: the same request Mission Control makes.
fn request(console: &Console, action: Action) -> Option<(String, String, serde_json::Value)> {
    Some(match action {
        Action::Send(text) => (
            "Sending".into(),
            "/v1/chat".into(),
            json!({"text": text, "conversation_id": console.remote.conversation_id}),
        ),
        Action::Deploy(index) => {
            let design = console.designs().get(index).copied()?;
            (
                format!("Deploying {}", design.preview.team),
                "/v1/runs".into(),
                json!({
                    "program": design.program,
                    "files": design.files,
                    "inputs": design.inputs,
                    "conversation_id": console.remote.conversation_id,
                }),
            )
        }
        Action::Pause(id) => (
            format!("Pausing {id}"),
            format!("/v1/runs/{id}/pause"),
            json!({}),
        ),
        Action::Resume(id) => (
            format!("Resuming {id}"),
            format!("/v1/runs/{id}/resume"),
            json!({}),
        ),
        Action::Stop(id) => (
            format!("Stopping {id}"),
            format!("/v1/runs/{id}/stop"),
            json!({}),
        ),
        Action::Quit | Action::OpenAgents => return None,
    })
}

type Screen = Terminal<CrosstermBackend<std::io::Stdout>>;

fn enter() -> Result<Screen> {
    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    terminal.clear()?;
    Ok(terminal)
}

fn leave(terminal: &mut Screen) -> Result<()> {
    disable_raw_mode()?;
    terminal.backend_mut().execute(LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

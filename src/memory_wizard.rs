//! Local operator entry point for pairing and reviewing saved memory.
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::io::{self, IsTerminal, Write};
use std::path::Path;

fn ask(label: &str) -> Result<String> {
    print!("{label} ");
    io::stdout().flush()?;
    let mut line = String::new();
    if io::stdin().read_line(&mut line)? == 0 {
        bail!("input closed");
    }
    Ok(line.trim().to_owned())
}

fn read_key() -> Result<String> {
    if !io::stdin().is_terminal() {
        return ask("Memory operator key:");
    }
    use crossterm::{
        event::{self, Event, KeyCode, KeyModifiers},
        terminal,
    };
    print!("Memory operator key: ");
    io::stdout().flush()?;
    terminal::enable_raw_mode()?;
    let result = (|| {
        let mut key = String::new();
        loop {
            if let Event::Key(k) = event::read()? {
                match k.code {
                    KeyCode::Enter => return Ok(key),
                    KeyCode::Esc => bail!("cancelled"),
                    KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                        bail!("cancelled")
                    }
                    KeyCode::Backspace => {
                        key.pop();
                    }
                    KeyCode::Char(ch) => key.push(ch),
                    _ => {}
                }
            }
        }
    })();
    terminal::disable_raw_mode()?;
    println!();
    result
}

pub fn run(root: &Path, send: impl Fn(&str, Value) -> Result<Value>) -> Result<()> {
    let key = match crate::knowledge::pair_operator(root)? {
        Some(key) => {
            println!("Memory operator key: {key}\nSave this key in your password manager. Enter it in Mission Control or this wizard to manage memory.");
            key
        }
        None => read_key()?,
    };
    let call = |request| send(&key, request);
    let mut offset = 0;
    loop {
        let page = call(json!({"action":"list","offset":offset}))?;
        let records = page["records"]
            .as_array()
            .context("invalid memory listing")?;
        let review = page["review_required"] == true;
        println!(
            "\nSaved memories  ·  Human review {}",
            if review { "on" } else { "off" }
        );
        for (i, r) in records.iter().enumerate() {
            println!(
                "{}  {}  [{}] {}",
                i + 1,
                r["key"],
                r["value"]["status"],
                r["value"]["text"]
            );
        }
        let choice = ask("Choose a record number, save, projects, review, next, first, or quit:")?;
        match choice.as_str() {
            "quit" | "q" => return Ok(()),
            "next" => {
                if let Some(n) = page["next_offset"].as_u64() {
                    offset = n;
                }
            }
            "first" => offset = 0,
            "review" => {
                call(json!({"action":"review","enabled":!review}))?;
            }
            "save" => {
                call(
                    json!({"action":"save","key":ask("Name:")?, "text":ask("Memory:")?, "source":ask("Source:")?,
                    "scope":ask("Scope (ea/project/user):")?, "kind":ask("Kind (preference/decision/observation/procedure):")?}),
                )?;
            }
            "projects" => {
                let projects = call(json!({"action":"project_list"}))?;
                let entries: Vec<_> = projects["projects"]
                    .as_object()
                    .context("invalid projects")?
                    .iter()
                    .collect();
                for (i, (id, name)) in entries.iter().enumerate() {
                    println!(
                        "{}  {} {}",
                        i + 1,
                        name,
                        if projects["linked_project"].as_str() == Some(id.as_str()) {
                            "(linked)"
                        } else {
                            ""
                        }
                    );
                }
                match ask("Choose a number to link, create, unlink, or back:")?.as_str() {
                    "create" => {
                        call(json!({"action":"project_create","name":ask("Project name:")?}))?;
                    }
                    "unlink" => {
                        call(json!({"action":"project_unlink"}))?;
                    }
                    n => {
                        if let Some((id, _)) = index(n).and_then(|i| entries.get(i)) {
                            call(json!({"action":"project_link","id":id}))?;
                        }
                    }
                }
            }
            n => {
                if let Some(r) = index(n).and_then(|i| records.get(i)) {
                    let v = &r["value"];
                    println!(
                        "{}\nSource: {}  ·  revision {}",
                        v["text"], v["source"], v["revision"]
                    );
                    let action = ask(if v["status"] == "candidate" {
                        "Accept, edit, reject, or back?"
                    } else {
                        "Edit, forget, or back?"
                    })?
                    .to_lowercase();
                    let mut req = json!({"id":r["id"],"revision":v["revision"]});
                    match action.as_str() {
                        "accept" if v["status"] == "candidate" => req["action"] = json!("confirm"),
                        "forget" | "reject" => {
                            if ask("Erase this memory and its revisions? (yes/no)")? != "yes" {
                                continue;
                            }
                            req["action"] = json!("forget");
                        }
                        "edit" => {
                            req["action"] = json!("edit");
                            req["text"] = json!(ask("Revised memory:")?);
                            req["source"] = json!(ask("Source:")?);
                            req["expires_at_ms"] = v["expires_at_ms"].clone();
                        }
                        _ => continue,
                    }
                    call(req)?;
                }
            }
        }
    }
}

fn index(value: &str) -> Option<usize> {
    value.parse::<usize>().ok()?.checked_sub(1)
}

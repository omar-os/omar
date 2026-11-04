//! The terminal console's only source of truth: the session's HTTP API, the
//! same endpoints Mission Control reads. Polled rather than streamed, so a
//! dropped connection costs one refresh, never a stuck view.

use anyhow::{bail, Context, Result};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use crate::diagram::DiagramSnapshot;
use crate::serve::{ChatMessage, RunRecord};
use crate::topology::TimelineStep;

#[derive(Clone)]
pub struct Client {
    /// `host:port`, as `omar serve` binds it.
    address: String,
}

impl Client {
    pub fn new(url: &str) -> Result<Self> {
        let address = url
            .strip_prefix("http://")
            .with_context(|| format!("{url}: the console speaks plain HTTP to a local session"))?
            .trim_end_matches('/');
        Ok(Self {
            address: address.split('/').next().unwrap_or(address).to_string(),
        })
    }

    pub fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let body = self.request("GET", path, None)?;
        serde_json::from_slice(&body).with_context(|| format!("GET {path}: unexpected answer"))
    }

    pub fn post(&self, path: &str, body: &Value) -> Result<Value> {
        let body = self.request("POST", path, Some(&serde_json::to_vec(body)?))?;
        Ok(serde_json::from_slice(&body).unwrap_or(Value::Null))
    }

    fn request(&self, method: &str, path: &str, body: Option<&[u8]>) -> Result<Vec<u8>> {
        let mut stream = TcpStream::connect(&self.address)
            .with_context(|| format!("omar serve at {} is unreachable", self.address))?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        let body = body.unwrap_or_default();
        write!(
            stream,
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            self.address,
            body.len()
        )?;
        stream.write_all(body)?;
        let mut response = Vec::new();
        stream.read_to_end(&mut response)?;
        let (status, body) = parse_response(&response)?;
        if !(200..300).contains(&status) {
            let message = serde_json::from_slice::<Value>(&body)
                .ok()
                .and_then(|value| value["error"].as_str().map(str::to_string))
                .unwrap_or_else(|| format!("HTTP {status}"));
            bail!("{message}");
        }
        Ok(body)
    }
}

/// Status and body of a `Connection: close` response.
fn parse_response(response: &[u8]) -> Result<(u16, Vec<u8>)> {
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .context("omar serve closed the connection mid-answer")?;
    let head = String::from_utf8_lossy(&response[..split]);
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .context("omar serve answered without a status")?;
    Ok((status, response[split + 4..].to_vec()))
}

#[derive(Deserialize)]
struct Conversation {
    id: String,
    messages: Vec<ChatMessage>,
}

#[derive(Deserialize)]
struct Runs {
    runs: Vec<RunRecord>,
}

#[derive(Deserialize)]
struct Timeline {
    steps: Vec<TimelineStep>,
    #[serde(default)]
    truncated: bool,
}

/// What one refresh read.
#[derive(Clone, Default)]
pub struct Remote {
    pub conversation_id: Option<String>,
    pub messages: Vec<ChatMessage>,
    /// Newest first.
    pub runs: Vec<RunRecord>,
    pub run: Option<RunView>,
}

/// The selected run, as Mission Control's timeline shows it.
#[derive(Clone)]
pub struct RunView {
    pub run_id: String,
    /// The tags the program passes through, worked out from the program.
    pub steps: Vec<TimelineStep>,
    pub truncated: bool,
    /// Where the run stands: the live drawing, or the one kept when it ended.
    pub snapshot: Option<DiagramSnapshot>,
}

impl Client {
    /// `kept` holds the last drawing seen of each run: serve keeps one only
    /// for a paused run, so once a run ends, where it got to is what this
    /// client saw last, as it is for Mission Control.
    pub fn fetch(
        &self,
        selected: Option<&str>,
        kept: &mut HashMap<String, DiagramSnapshot>,
    ) -> Result<Remote> {
        let conversation: Conversation = self.get("/v1/chat")?;
        let mut runs = self.get::<Runs>("/v1/runs")?.runs;
        runs.sort_by_key(|run| std::cmp::Reverse(run.started_at));
        let run = selected
            .and_then(|id| runs.iter().find(|run| run.run_id == id))
            .or(runs.first())
            .map(|record| self.run_view(record, kept));
        Ok(Remote {
            conversation_id: Some(conversation.id),
            messages: conversation.messages,
            runs,
            run,
        })
    }

    fn run_view(&self, record: &RunRecord, kept: &mut HashMap<String, DiagramSnapshot>) -> RunView {
        let id = &record.run_id;
        let timeline = self
            .get::<Timeline>(&format!("/v1/runs/{id}/timeline"))
            .ok();
        // A live run answers through serve's relay to its own diagram port;
        // a paused one through the snapshot serve kept.
        let live = record.status.is_active() || record.status == crate::serve::RunStatus::Paused;
        let fetched: Option<DiagramSnapshot> = if live && record.diagram_address.is_some() {
            self.get(&format!("/v1/runs/{id}/diagram/v1/diagram")).ok()
        } else {
            None
        }
        .or_else(|| self.get(&format!("/v1/runs/{id}/snapshot")).ok());
        if let Some(snapshot) = &fetched {
            kept.insert(id.clone(), snapshot.clone());
        }
        RunView {
            run_id: id.clone(),
            truncated: timeline.as_ref().is_some_and(|t| t.truncated),
            steps: timeline.map(|t| t.steps).unwrap_or_default(),
            snapshot: fetched.or_else(|| kept.get(id).cloned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;

    /// A one-shot server answering every request with `answer`, returning
    /// the request line it saw.
    fn serve_once(answer: &'static str) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let mut header = String::new();
            while reader.read_line(&mut header).unwrap() > 2 {
                header.clear();
            }
            (&stream).write_all(answer.as_bytes()).unwrap();
            line
        });
        (url, handle)
    }

    #[test]
    fn a_rejection_reports_the_servers_own_reason() {
        let (url, seen) = serve_once(
            "HTTP/1.1 409 Conflict\r\nContent-Length: 27\r\n\r\n{\"error\":\"run is finished\"}",
        );
        let error = Client::new(&url)
            .unwrap()
            .post("/v1/runs/r/pause", &serde_json::json!({}))
            .unwrap_err();
        assert_eq!(error.to_string(), "run is finished");
        assert!(seen.join().unwrap().starts_with("POST /v1/runs/r/pause "));
    }

    #[test]
    fn an_answer_is_read_as_the_wire_type() {
        let (url, _) = serve_once("HTTP/1.1 200 OK\r\nContent-Length: 11\r\n\r\n{\"runs\":[]}");
        let runs: Runs = Client::new(&url).unwrap().get("/v1/runs").unwrap();
        assert!(runs.runs.is_empty());
    }

    #[test]
    fn an_unreachable_session_says_where_it_looked() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let Err(error) = Client::new(&url).unwrap().fetch(None, &mut HashMap::new()) else {
            panic!("a closed port answered");
        };
        assert!(format!("{error:#}").contains("unreachable"), "{error:#}");
    }
}

//! Jev Choice adapter for topology prompts. No coding-agent pane is spawned.
use super::{Backend, Kind, Launch, TopologyInvocation};
use crate::topology::string_enum;
use anyhow::{bail, ensure, Context, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const HTTP_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, Copy, PartialEq, Eq)]
enum OnError {
    Fail,
    Defer,
}

impl OnError {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "fail" => Ok(Self::Fail),
            "defer" => Ok(Self::Defer),
            _ => bail!("OMAR_JEV_ON_ERROR must be fail or defer"),
        }
    }
}

// Messages are controlled by the adapter, never copied from provider bodies,
// curl stderr, request headers, or credentials.
#[derive(Debug)]
struct Failure {
    kind: &'static str,
    message: &'static str,
    http_status: Option<u16>,
}

impl Failure {
    fn new(kind: &'static str, message: &'static str) -> Self {
        Self {
            kind,
            message,
            http_status: None,
        }
    }
    fn evidence(&self) -> Value {
        json!({"kind": self.kind, "message": self.message, "http_status": self.http_status})
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Jev {}: {}", self.kind, self.message)?;
        if let Some(status) = self.http_status {
            write!(f, " (HTTP {status})")?;
        }
        Ok(())
    }
}
impl std::error::Error for Failure {}

pub struct Jev;

impl Backend for Jev {
    fn kind(&self) -> Kind {
        Kind::Jev
    }
    fn aliases(&self) -> &'static [&'static str] {
        &["jev"]
    }
    fn executables(&self) -> &'static [&'static str] {
        &[]
    }
    fn default_command(&self) -> &'static str {
        "jev"
    }
    fn launch_command(&self, _launch: &Launch<'_>) -> String {
        "printf '%s\\n' 'Jev supports topology prompts only; use a Jev agent in an .omar team.' >&2; exit 1".into()
    }
    fn topology_only(&self) -> bool {
        true
    }
    fn invoke_topology(
        &self,
        invocation: &TopologyInvocation<'_>,
    ) -> Result<BTreeMap<String, Value>> {
        invoke(invocation)
    }
}

fn invoke(invocation: &TopologyInvocation<'_>) -> Result<BTreeMap<String, Value>> {
    let TopologyInvocation {
        effects,
        triggers,
        instructions,
        timeout,
    } = invocation;
    let mut decision = None;
    let mut evidence = None;
    for (port, ty) in *effects {
        if let Some(options) = string_enum(ty) {
            ensure!(
                decision.is_none(),
                "Jev requires exactly one string-enum output"
            );
            decision = Some((port, options));
        } else if ty == "string" {
            ensure!(
                evidence.is_none(),
                "Jev accepts at most one JSON evidence string output"
            );
            evidence = Some(port);
        } else {
            bail!("Jev output {port} must be a string enum or JSON evidence string");
        }
    }
    let (port, options) = decision.context("Jev requires one imported string-enum output")?;
    let on_error =
        OnError::parse(&std::env::var("OMAR_JEV_ON_ERROR").unwrap_or_else(|_| "fail".into()))?;
    if on_error == OnError::Defer {
        ensure!(
            options.iter().any(|option| option == "defer") && evidence.is_some(),
            "OMAR_JEV_ON_ERROR=defer requires a defer enum value and an evidence string output"
        );
    }
    let model = std::env::var("OMAR_JEV_MODEL").unwrap_or_else(|_| "jev-latest".into());
    ensure!(!model.trim().is_empty(), "OMAR_JEV_MODEL must not be empty");
    let criteria: BTreeMap<_, _> = options
        .iter()
        .map(|option| (option.clone(), option.clone()))
        .collect();
    let request = json!({"model": model,
        "state": triggers, "questions": {"decision": {"type": "choice", "instructions": instructions, "criteria": criteria}}});
    let result = evaluate(&request, *timeout).and_then(|response| {
        let choice = validate(&response, &options).map_err(|_| {
            Failure::new(
                "invalid_response",
                "Response does not satisfy the Choice contract",
            )
        })?;
        Ok((choice, response))
    });
    let (choice, mut response) = match result {
        Ok(answer) => answer,
        Err(error) if on_error == OnError::Defer => {
            eprintln!("Jev deferred after {}", error.kind);
            (
                "defer".to_string(),
                json!({"model": model, "error": error.evidence()}),
            )
        }
        Err(error) => return Err(error.into()),
    };
    response["context"] = json!(triggers);
    eprintln!("Jev selected {choice}");
    let mut writes = BTreeMap::from([(port.clone(), json!(choice))]);
    if let Some(evidence) = evidence {
        writes.insert(evidence.clone(), json!(response.to_string()));
    }
    Ok(writes)
}

fn evaluate(request: &Value, timeout: Duration) -> std::result::Result<Value, Failure> {
    let budget = timeout.min(HTTP_TIMEOUT);
    if budget.is_zero() {
        return Err(Failure::new("timeout", "Invocation deadline is exhausted"));
    }
    let started = Instant::now();
    let key = std::env::var("TYPESAFE_API_KEY")
        .map_err(|_| Failure::new("configuration", "TYPESAFE_API_KEY is not set"))?;
    if key.trim().is_empty() || key.contains(['\r', '\n', '\0']) {
        return Err(Failure::new(
            "configuration",
            "TYPESAFE_API_KEY is empty or invalid",
        ));
    }
    // Headers and body travel over stdin, not process arguments or files.
    let config = format!("url = \"https://api.typesafe.ai/v1/systemone\"\nheader = {}\nheader = \"Content-Type: application/json\"\ndata = {}\n",
        json!(format!("Authorization: Bearer {key}")), json!(request.to_string()));
    let mut child = Command::new("curl")
        .args([
            "-q",
            "--silent",
            "--show-error",
            "--connect-timeout",
            "5",
            "--max-time",
            &budget.as_secs_f64().to_string(),
            "--write-out",
            "\n%{http_code}",
            "--config",
            "-",
        ])
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| {
            Failure::new(
                "configuration",
                "Could not launch curl; ensure it is installed",
            )
        })?;
    let pid = child.id();
    let mut stdin = child.stdin.take().expect("piped curl stdin");
    let stdout = child.stdout.take().expect("piped curl stdout");
    let (written_tx, written_rx) = mpsc::channel();
    let writer = thread::spawn(move || {
        let _ = written_tx.send(stdin.write_all(config.as_bytes()));
    });
    let (body_tx, body_rx) = mpsc::channel();
    let reader = thread::spawn(move || {
        let mut body = Vec::new();
        let result = stdout
            .take((MAX_RESPONSE_BYTES + 1) as u64)
            .read_to_end(&mut body);
        let _ = body_tx.send(result.map(|_| body));
    });
    let mut status = None;
    let mut body = None;
    let mut written = false;
    let transport = (|| loop {
        if started.elapsed() >= budget {
            return Err(Failure::new("timeout", "Jev request exceeded its deadline"));
        }
        if !written {
            match written_rx.try_recv() {
                Ok(Ok(())) => written = true,
                Ok(Err(_)) | Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(Failure::new("transport", "Could not send the Jev request"))
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if body.is_none() {
            match body_rx.try_recv() {
                Ok(Ok(bytes)) if bytes.len() > MAX_RESPONSE_BYTES => {
                    return Err(Failure::new(
                        "invalid_response",
                        "Jev response exceeds the size limit",
                    ))
                }
                Ok(Ok(bytes)) => body = Some(bytes),
                Ok(Err(_)) | Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(Failure::new("transport", "Could not read the Jev response"))
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if status.is_none() {
            status = crate::process::child_exited(&child)
                .map_err(|_| Failure::new("transport", "Could not inspect the HTTP process"))?;
        }
        if let (Some(exit), Some(body)) = (status, body.as_ref()) {
            if written {
                if !exit {
                    return Err(Failure::new("transport", "Jev HTTP transport failed"));
                }
                return decode_http(body);
            }
        }
        thread::sleep(Duration::from_millis(10));
    })();
    if transport.is_err() {
        // Only this invocation's owned process group is terminated. Readers
        // and writers must not outlive the deadline because a child held a pipe.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
        let _ = child.kill();
    }
    // The non-reaping observation keeps the PID reserved until cleanup is done.
    let exit = child.wait();
    let _ = writer.join();
    let _ = reader.join();
    if exit.ok().and_then(|status| status.code()) == Some(28) {
        return Err(Failure::new("timeout", "Jev request exceeded its deadline"));
    }
    transport
}

fn decode_http(bytes: &[u8]) -> std::result::Result<Value, Failure> {
    let split = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .ok_or_else(|| Failure::new("transport", "HTTP status was not reported"))?;
    let status: u16 = std::str::from_utf8(&bytes[split + 1..])
        .ok()
        .and_then(|text| text.parse().ok())
        .ok_or_else(|| Failure::new("transport", "HTTP status was not reported"))?;
    if !(200..300).contains(&status) {
        let (kind, message) = match status {
            401 | 403 => ("authentication", "Jev rejected the API credentials"),
            429 => ("rate_limit", "Jev rate limit exceeded"),
            500..=599 => ("service", "Jev service is unavailable"),
            _ => ("http", "Jev rejected the request"),
        };
        return Err(Failure {
            kind,
            message,
            http_status: Some(status),
        });
    }
    serde_json::from_slice(&bytes[..split])
        .map_err(|_| Failure::new("invalid_response", "Jev response is not valid JSON"))
}

fn validate(response: &Value, options: &[String]) -> Result<String> {
    ensure!(
        response["model"]
            .as_str()
            .is_some_and(|model| !model.trim().is_empty()),
        "Missing Jev response model"
    );
    let answer = &response["answers"]["decision"];
    ensure!(answer["type"] == "choice", "Invalid Jev answer type");
    let choice = answer["choice"].as_str().context("Missing Jev choice")?;
    ensure!(
        options.iter().any(|option| option == choice),
        "Unknown Jev choice"
    );
    let confidence = answer["confidence"]
        .as_f64()
        .context("Missing Jev confidence")?;
    ensure!(
        confidence.is_finite() && (0.0..=1.0).contains(&confidence),
        "Invalid confidence"
    );
    let probabilities = answer["probabilities"]
        .as_object()
        .context("Missing probabilities")?;
    ensure!(
        probabilities.len() == options.len(),
        "Incorrect probability options"
    );
    let mut sum = 0.0;
    let mut max: f64 = 0.0;
    for option in options {
        let p = probabilities
            .get(option)
            .and_then(Value::as_f64)
            .context("Missing option probability")?;
        ensure!(
            p.is_finite() && (0.0..=1.0).contains(&p),
            "Invalid probability"
        );
        sum += p;
        max = max.max(p);
    }
    ensure!((sum - 1.0).abs() <= 1e-6, "Probabilities must sum to one");
    ensure!(
        probabilities[choice].as_f64() == Some(max),
        "Choice must have highest probability"
    );
    Ok(choice.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failures_have_safe_typed_evidence_and_policy_is_explicit() {
        assert!(matches!(OnError::parse("fail").unwrap(), OnError::Fail));
        assert!(matches!(OnError::parse("defer").unwrap(), OnError::Defer));
        assert!(OnError::parse("anything").is_err());
        for (status, kind) in [
            (401, "authentication"),
            (429, "rate_limit"),
            (503, "service"),
            (400, "http"),
        ] {
            let bytes = format!("sensitive-provider-body\n{status}");
            let error = decode_http(bytes.as_bytes()).unwrap_err();
            assert_eq!(error.kind, kind);
            assert_eq!(error.http_status, Some(status));
            assert!(!error.to_string().contains("sensitive-provider-body"));
            assert_eq!(error.evidence()["kind"], kind);
        }
        assert_eq!(
            decode_http(b"not json\n200").unwrap_err().kind,
            "invalid_response"
        );
        assert_eq!(decode_http(b"no status").unwrap_err().kind, "transport");
    }

    #[test]
    fn zero_deadline_does_not_launch_http() {
        let start = Instant::now();
        let error = evaluate(&json!({}), Duration::ZERO).unwrap_err();
        assert_eq!(error.kind, "timeout");
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn unsupported_output_shapes_fail_before_http() {
        let triggers = BTreeMap::new();
        for effects in [
            BTreeMap::from([("review.evidence".into(), "string".into())]),
            BTreeMap::from([("review.out".into(), "float".into())]),
            BTreeMap::from([
                ("review.first".into(), "string in [\"a\",\"b\"]".into()),
                ("review.second".into(), "string in [\"a\",\"b\"]".into()),
            ]),
        ] {
            let request = TopologyInvocation {
                effects: &effects,
                triggers: &triggers,
                instructions: "not sent",
                timeout: Duration::from_secs(1),
            };
            assert!(Jev.invoke_topology(&request).is_err());
        }
    }

    #[test]
    fn validates_membership_distribution_and_confidence() {
        let options = vec!["proceed".into(), "review".into(), "defer".into()];
        let response = json!({"model":"test-jev","answers":{"decision":{"type":"choice","choice":"proceed","confidence":0.9,
            "probabilities":{"proceed":0.97,"review":0.02,"defer":0.01}}}});
        assert_eq!(validate(&response, &options).unwrap(), "proceed");
        let mut missing_model = response.clone();
        missing_model["model"] = Value::Null;
        assert!(validate(&missing_model, &options).is_err());
        for (field, value) in [
            ("choice", json!("approve")),
            ("confidence", json!(2)),
            ("confidence", json!(true)),
            ("probabilities", json!({"proceed":0.97,"stop":0.03})),
            (
                "probabilities",
                json!({"proceed":0.97,"review":true,"defer":0.03}),
            ),
            (
                "probabilities",
                json!({"proceed":0.2,"review":0.1,"defer":0.1}),
            ),
        ] {
            let mut bad = response.clone();
            bad["answers"]["decision"][field] = value;
            assert!(validate(&bad, &options).is_err());
        }
    }
}

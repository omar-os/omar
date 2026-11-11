//! `omar github-webhook`: GitHub's webhook deliveries, verified, as events on
//! one run's physical action (spec §4.4).
//!
//! A listener of its own rather than a route on `omar serve`: serve's port is
//! full access and stays loopback, while this one may face the internet and can
//! do exactly one thing, schedule a signed event on the action it was given.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// GitHub caps a delivery at 25 MB; a summary needs far less of it, but the
/// signature covers the whole body, so the whole body is read.
const MAX_BODY: usize = 25 * 1024 * 1024;

/// HMAC-SHA256 (RFC 2104), which GitHub signs each delivery with.
pub fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut block = [0u8; 64];
    if key.len() > block.len() {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(block.map(|byte| byte ^ 0x36));
    inner.update(message);
    let mut outer = Sha256::new();
    outer.update(block.map(|byte| byte ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

/// Whether `header` (`sha256=<hex>`) signs `body` with `secret`. Compared in
/// constant time, so a guess learns nothing from how long a refusal took.
pub fn signed(secret: &[u8], body: &[u8], header: &str) -> bool {
    let Some(hex) = header.strip_prefix("sha256=") else {
        return false;
    };
    let Some(claimed) = decode_hex(hex) else {
        return false;
    };
    let expected = hmac_sha256(secret, body);
    claimed.len() == expected.len()
        && claimed
            .iter()
            .zip(expected)
            .fold(0u8, |diff, (a, b)| diff | (a ^ b))
            == 0
}

fn decode_hex(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(hex.get(at..at + 2)?, 16).ok())
        .collect()
}

/// What the program receives: which event, and the few fields a review
/// workflow keys on. Absent fields are left out rather than null.
pub fn summary(event: &str, delivery: &str, payload: &Value) -> Value {
    let pull = &payload["pull_request"];
    let fields = [
        ("event", json!(event)),
        ("delivery", json!(delivery)),
        ("action", payload["action"].clone()),
        ("repo", payload["repository"]["full_name"].clone()),
        (
            "pr",
            pull["number"]
                .as_u64()
                .or_else(|| payload["number"].as_u64())
                .map_or(Value::Null, |number| json!(number)),
        ),
        (
            "sha",
            pull["head"]["sha"]
                .as_str()
                .or_else(|| payload["after"].as_str())
                .map_or(Value::Null, |sha| json!(sha)),
        ),
        ("sender", payload["sender"]["login"].clone()),
    ];
    Value::Object(
        fields
            .into_iter()
            .filter(|(_, value)| !value.is_null())
            .map(|(name, value)| (name.to_string(), value))
            .collect(),
    )
}

/// One delivery: verified, summarised, handed to `forward`. Returns the HTTP
/// status and body to answer GitHub with.
pub fn handle(
    secret: &[u8],
    headers: &[(String, String)],
    body: &[u8],
    forward: &dyn Fn(Value) -> Result<Value>,
) -> (u16, Value) {
    let header = |name: &str| {
        headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    };
    if !signed(secret, body, header("X-Hub-Signature-256").unwrap_or("")) {
        return (401, json!({"error": "bad signature"}));
    }
    let event = header("X-GitHub-Event").unwrap_or("");
    let delivery = header("X-GitHub-Delivery").unwrap_or("");
    // Sent once when the hook is created: proof the secret matches, nothing
    // a program should hear.
    if event == "ping" {
        return (200, json!({"ok": "pong"}));
    }
    let payload: Value = match serde_json::from_slice(body) {
        Ok(payload) => payload,
        Err(error) => {
            return (
                400,
                json!({"error": format!("payload is not JSON: {error}")}),
            )
        }
    };
    // The port is a string: the program parses the summary it was sent.
    let value = json!(summary(event, delivery, &payload).to_string());
    match forward(value) {
        Ok(accepted) => (202, accepted),
        // The run is paused, stopping or gone. GitHub records the failure and
        // can redeliver; a program's own poll catches up on anything missed.
        Err(error) => (503, json!({"error": format!("{error:#}")})),
    }
}

/// Listen until killed, handing each verified delivery to `forward`.
pub fn listen(
    address: SocketAddr,
    secret: Vec<u8>,
    forward: Arc<dyn Fn(Value) -> Result<Value> + Send + Sync>,
) -> Result<()> {
    let listener =
        TcpListener::bind(address).with_context(|| format!("could not listen on {address}"))?;
    println!(
        "Listening for GitHub webhooks on {}",
        listener.local_addr()?
    );
    let secret = Arc::new(secret);
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let (secret, forward) = (secret.clone(), forward.clone());
        thread::spawn(move || {
            if let Err(error) = answer(stream, &secret, forward.as_ref()) {
                eprintln!("webhook connection: {error:#}");
            }
        });
    }
    Ok(())
}

fn answer(
    mut stream: TcpStream,
    secret: &[u8],
    forward: &(dyn Fn(Value) -> Result<Value> + Send + Sync),
) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            bail!("connection closed inside the headers");
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }
    let length: usize = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("Content-Length"))
        .and_then(|(_, value)| value.parse().ok())
        .unwrap_or(0);
    let (status, body) = if !request_line.starts_with("POST ") {
        (405, json!({"error": "deliveries are POSTs"}))
    } else if length > MAX_BODY {
        (413, json!({"error": "delivery too large"}))
    } else {
        let mut body = vec![0; length];
        reader.read_exact(&mut body)?;
        let (status, value) = handle(secret, &headers, &body, forward);
        let event = headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("X-GitHub-Event"))
            .map_or("-", |(_, value)| value.as_str());
        println!("{event} -> {status} {value}");
        (status, value)
    };
    let text = body.to_string();
    write!(
        stream,
        "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
        reason(status),
        text.len()
    )?;
    stream.flush()?;
    Ok(())
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        _ => "Service Unavailable",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn hmac_matches_the_rfc_and_githubs_own_example() {
        // RFC 4231, test case 2.
        assert_eq!(
            hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        // GitHub's documented example delivery.
        let header = "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17";
        assert!(signed(
            b"It's a Secret to Everybody",
            b"Hello, World!",
            header
        ));
        assert!(!signed(b"wrong", b"Hello, World!", header));
        assert!(!signed(
            b"It's a Secret to Everybody",
            b"Hello, World?",
            header
        ));
        assert!(!signed(
            b"It's a Secret to Everybody",
            b"Hello, World!",
            "sha1=00"
        ));
    }

    fn delivery(secret: &[u8], event: &str, body: &str) -> Vec<(String, String)> {
        vec![
            ("X-GitHub-Event".into(), event.into()),
            ("X-GitHub-Delivery".into(), "d-1".into()),
            (
                "x-hub-signature-256".into(),
                format!("sha256={}", hex(&hmac_sha256(secret, body.as_bytes()))),
            ),
        ]
    }

    #[test]
    fn a_signed_pull_request_event_is_forwarded_as_its_summary() {
        let forwarded = Mutex::new(Vec::new());
        let forward = |value: Value| {
            forwarded.lock().unwrap().push(value);
            Ok(json!({"tag": {"timestamp": 1, "microstep": 0}}))
        };
        let body = r#"{"action":"synchronize","number":42,
            "pull_request":{"number":42,"head":{"sha":"abc123"}},
            "repository":{"full_name":"omar-os/omar"},"sender":{"login":"dev"}}"#;
        let (status, _) = handle(
            b"s3cret",
            &delivery(b"s3cret", "pull_request", body),
            body.as_bytes(),
            &forward,
        );
        assert_eq!(status, 202);
        let sent = forwarded.lock().unwrap();
        let summary: Value = serde_json::from_str(sent[0].as_str().unwrap()).unwrap();
        assert_eq!(
            summary,
            json!({"event":"pull_request","delivery":"d-1","action":"synchronize",
                   "repo":"omar-os/omar","pr":42,"sha":"abc123","sender":"dev"})
        );
    }

    #[test]
    fn an_unsigned_delivery_or_a_ping_never_reaches_the_run() {
        let forward = |_: Value| -> Result<Value> { panic!("must not forward") };
        let body = r#"{"zen":"Keep it logically awesome."}"#;
        let mut forged = delivery(b"other", "pull_request", body);
        assert_eq!(handle(b"s3cret", &forged, body.as_bytes(), &forward).0, 401);
        forged.retain(|(name, _)| !name.eq_ignore_ascii_case("x-hub-signature-256"));
        assert_eq!(handle(b"s3cret", &forged, body.as_bytes(), &forward).0, 401);
        let ping = delivery(b"s3cret", "ping", body);
        assert_eq!(handle(b"s3cret", &ping, body.as_bytes(), &forward).0, 200);
    }

    #[test]
    fn a_refusing_run_is_a_503_github_can_redeliver() {
        let forward = |_: Value| -> Result<Value> { bail!("run is not accepting events now") };
        let body = r#"{"action":"opened","pull_request":{"number":1,"head":{"sha":"a"}}}"#;
        let (status, value) = handle(
            b"k",
            &delivery(b"k", "pull_request", body),
            body.as_bytes(),
            &forward,
        );
        assert_eq!(status, 503);
        assert!(value["error"].as_str().unwrap().contains("not accepting"));
    }
}

//! `omar gateway [--port N]`: Mission Control from another machine, behind the
//! operator's password (see `login`) rather than an SSH tunnel. It listens on
//! loopback behind an HTTPS proxy (Caddy), serves Mission Control, and relays
//! `/s/<session>/…` to that session's loopback daemon, which treats a signed
//! in browser as the operator, as it treats anyone on loopback.
//!
//! Each connection carries one request: the relay asks the daemon to close
//! after answering, so no second request rides a connection already checked.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::login::{Login, LOGIN_SECONDS};

const COOKIE: &str = "omar_login";
const MAX_AUTH_BODY: usize = 16 * 1024;
const MAX_HEAD: usize = 64 * 1024;
/// Wrong passwords before anyone must wait.
const ATTEMPTS: u32 = 5;
const LOCKOUT: Duration = Duration::from_secs(60);

struct Gateway {
    home: PathBuf,
    login: Login,
    /// Wrong passwords in a row, and when the last one came.
    failures: Mutex<(u32, Instant)>,
}

/// Serve until killed.
pub fn run(port: u16) -> Result<()> {
    run_in(port, crate::sessions::home_root())
}

fn run_in(port: u16, home: PathBuf) -> Result<()> {
    if !Login::at(&home).is_set()? {
        bail!("set a password first: omar passwd");
    }
    let listener = listen(port)?;
    let address = listener.local_addr()?;
    eprintln!(
        "Gateway on http://{address}, for this machine only: give it no firewall rule.\n\
         From elsewhere, reach it through an HTTPS proxy and open only the proxy's ports\n\
         (443, and 80 for its certificate). For Caddy, a Caddyfile of:\n\
         \n    omar.example.org {{\n        reverse_proxy {address}\n    }}\n"
    );
    serve(listener, home)
}

/// Loopback only: a password over plain HTTP could be read on the way, and
/// the login cookie is `Secure`, so only the HTTPS proxy in front reaches it.
fn listen(port: u16) -> Result<TcpListener> {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    TcpListener::bind(address).with_context(|| {
        format!("cannot listen on {address}; is something else using it? Pick another: omar gateway --port N")
    })
}

fn serve(listener: TcpListener, home: PathBuf) -> Result<()> {
    let gateway = Arc::new(Gateway {
        login: Login::at(&home),
        home,
        failures: Mutex::new((0, Instant::now())),
    });
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let gateway = gateway.clone();
        thread::spawn(move || {
            let _ = handle(stream, &gateway);
        });
    }
    Ok(())
}

struct Request {
    method: String,
    path: String,
    /// Header lines as sent, without their CRLF.
    headers: Vec<String>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim().eq_ignore_ascii_case(name).then(|| value.trim())
        })
    }

    fn cookie(&self) -> Option<&str> {
        self.header("cookie")?.split(';').find_map(|pair| {
            let (key, value) = pair.trim().split_once('=')?;
            (key == COOKIE).then_some(value)
        })
    }

    fn upgrade(&self) -> bool {
        self.header("upgrade").is_some()
    }
}

fn handle(stream: TcpStream, gateway: &Gateway) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut stream = stream;
    let Some(request) = read_head(&mut reader)? else {
        return Ok(());
    };
    // A login cookie is SameSite=Strict, but say no to any other site
    // outright: writes and WebSockets must come from this origin.
    if (request.method != "GET" && request.method != "HEAD" || request.upgrade())
        && !same_origin(&request)
    {
        return respond(
            &mut stream,
            403,
            &json!({"error": "cross-site request refused"}),
            &[],
        );
    }
    let login = request
        .cookie()
        .filter(|token| gateway.login.resolve(token).unwrap_or(false))
        .map(str::to_string);

    if let Some(route) = request.path.strip_prefix("/auth/") {
        let body = read_body(&mut reader, &request)?;
        return auth(&mut stream, gateway, &request, route, &body, login);
    }
    if let Some(rest) = request.path.strip_prefix("/s/") {
        let (session, route) = rest.split_once('/').unwrap_or((rest, ""));
        let route = format!("/{route}");
        if is_api(&route) {
            return relay(stream, reader, gateway, &request, session, &route, login);
        }
    }
    if request.method == "GET" {
        let route = request.path.split('?').next().unwrap_or("/");
        return match crate::web_assets::lookup(route) {
            Some(asset) => crate::serve::write_asset(&mut stream, asset),
            None => respond(
                &mut stream,
                404,
                &json!({"error": crate::web_assets::MISSING}),
                &[],
            ),
        };
    }
    respond(&mut stream, 404, &json!({"error": "not found"}), &[])
}

/// What a session's daemon answers, rather than the gateway's own copy of
/// Mission Control.
fn is_api(route: &str) -> bool {
    route == "/health"
        || route.starts_with("/health?")
        || route.starts_with("/v1/")
        || route.starts_with("/chats/")
}

fn auth(
    stream: &mut TcpStream,
    gateway: &Gateway,
    request: &Request,
    route: &str,
    body: &[u8],
    login: Option<String>,
) -> Result<()> {
    match (request.method.as_str(), route) {
        ("GET", "me") => match login {
            None => respond(stream, 401, &json!({"error": "sign in"}), &[]),
            Some(_) => respond(
                stream,
                200,
                &json!({"user": crate::login::USER, "sessions": sessions(gateway)}),
                &[],
            ),
        },
        ("POST", "login") => {
            if let Some(wait) = gateway.locked_out() {
                let error = format!("too many attempts; try again in {}s", wait.as_secs().max(1));
                return respond(stream, 429, &json!({"error": error}), &[]);
            }
            let field = |name: &str| -> String {
                serde_json::from_slice::<Value>(body)
                    .ok()
                    .and_then(|body| body.get(name)?.as_str().map(str::to_string))
                    .unwrap_or_default()
            };
            if !gateway.login.verify(&field("user"), &field("password"))? {
                gateway.failed();
                return respond(
                    stream,
                    401,
                    &json!({"error": "wrong username or password"}),
                    &[],
                );
            }
            *gateway.failures.lock().expect("failures poisoned") = (0, Instant::now());
            let token = gateway.login.start()?;
            let cookie = format!(
                "Set-Cookie: {COOKIE}={token}; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age={LOGIN_SECONDS}"
            );
            respond(stream, 200, &json!({"ok": true}), &[&cookie])
        }
        ("POST", "logout") => {
            if let Some(token) = &login {
                gateway.login.end(token)?;
            }
            let cookie = format!(
                "Set-Cookie: {COOKIE}=; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age=0"
            );
            respond(stream, 200, &json!({"ok": true}), &[&cookie])
        }
        _ => respond(stream, 404, &json!({"error": "not found"}), &[]),
    }
}

/// Every session `omar ls` would list, and its state.
fn sessions(gateway: &Gateway) -> Vec<Value> {
    let Ok(entries) = std::fs::read_dir(gateway.home.join("registry")) else {
        return Vec::new();
    };
    let mut sessions: Vec<Value> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let session = registered(gateway, name.strip_suffix(".json")?)?;
            Some(json!({"name": session.name, "state": session.state}))
        })
        .collect();
    sessions.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    sessions
}

fn registered(gateway: &Gateway, name: &str) -> Option<crate::sessions::Session> {
    let bytes = std::fs::read(gateway.home.join("registry").join(format!("{name}.json"))).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Hand one request to the session's daemon and splice the two
/// connections until either closes: replies, event streams and terminal
/// WebSockets alike.
fn relay(
    mut stream: TcpStream,
    reader: BufReader<TcpStream>,
    gateway: &Gateway,
    request: &Request,
    session: &str,
    route: &str,
    login: Option<String>,
) -> Result<()> {
    if login.is_none() {
        return respond(&mut stream, 401, &json!({"error": "sign in"}), &[]);
    }
    if session.is_empty() || session.contains(['/', '.', '%']) {
        return respond(&mut stream, 404, &json!({"error": "no such session"}), &[]);
    }
    let address = registered(gateway, session)
        .filter(|record| record.state == "ready")
        .and_then(|record| record.url.strip_prefix("http://").map(str::to_string));
    let Some(address) = address else {
        return respond(
            &mut stream,
            503,
            &json!({"error": format!("session '{session}' is not running")}),
            &[],
        );
    };
    let mut upstream = match TcpStream::connect(&address) {
        Ok(upstream) => upstream,
        Err(_) => {
            return respond(
                &mut stream,
                502,
                &json!({"error": format!("session '{session}' does not answer")}),
                &[],
            )
        }
    };
    let mut head = format!("{} {route} HTTP/1.1\r\nHost: {address}\r\n", request.method);
    for line in &request.headers {
        let name = line
            .split(':')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        // The daemon trusts loopback, so it gets the request as if from the
        // operator there: no credential of the gateway's, no other site, and
        // none of OMAR's own headers, which a later daemon may trust.
        if matches!(
            name.as_str(),
            "host" | "cookie" | "origin" | "connection" | "keep-alive" | "proxy-connection"
        ) || name.starts_with("x-omar-")
        {
            continue;
        }
        head.push_str(line);
        head.push_str("\r\n");
    }
    head.push_str(if request.upgrade() {
        "Connection: Upgrade\r\n\r\n"
    } else {
        "Connection: close\r\n\r\n"
    });
    upstream.write_all(head.as_bytes())?;
    upstream.write_all(reader.buffer())?;

    stream.set_read_timeout(None)?;
    let mut from_client = stream.try_clone()?;
    let mut to_upstream = upstream.try_clone()?;
    let forward = thread::spawn(move || {
        let _ = std::io::copy(&mut from_client, &mut to_upstream);
        let _ = to_upstream.shutdown(Shutdown::Write);
    });
    let _ = std::io::copy(&mut upstream, &mut stream);
    let _ = stream.shutdown(Shutdown::Both);
    let _ = upstream.shutdown(Shutdown::Both);
    let _ = forward.join();
    Ok(())
}

impl Gateway {
    fn locked_out(&self) -> Option<Duration> {
        let (count, since) = *self.failures.lock().expect("failures poisoned");
        let elapsed = since.elapsed();
        (count >= ATTEMPTS && elapsed < LOCKOUT).then(|| LOCKOUT - elapsed)
    }

    fn failed(&self) {
        let mut failures = self.failures.lock().expect("failures poisoned");
        if failures.1.elapsed() >= LOCKOUT {
            failures.0 = 0;
        }
        failures.0 += 1;
        failures.1 = Instant::now();
    }
}

/// `Origin` names the host the request came to, as the proxy passes it on.
fn same_origin(request: &Request) -> bool {
    let (Some(origin), Some(host)) = (request.header("origin"), request.header("host")) else {
        return false;
    };
    origin
        .strip_prefix("https://")
        .or_else(|| origin.strip_prefix("http://"))
        .is_some_and(|origin_host| origin_host.eq_ignore_ascii_case(host))
}

fn read_head(reader: &mut BufReader<TcpStream>) -> Result<Option<Request>> {
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("/").to_string();
    let mut headers = Vec::new();
    let mut size = line.len();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        size += line.len();
        anyhow::ensure!(size <= MAX_HEAD, "request head too large");
        headers.push(line.trim_end_matches(['\r', '\n']).to_string());
    }
    Ok(Some(Request {
        method,
        path,
        headers,
    }))
}

fn read_body(reader: &mut BufReader<TcpStream>, request: &Request) -> Result<Vec<u8>> {
    let length: usize = request
        .header("content-length")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    anyhow::ensure!(length <= MAX_AUTH_BODY, "request too large");
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    Ok(body)
}

fn respond(stream: &mut TcpStream, status: u16, body: &Value, extra: &[&str]) -> Result<()> {
    let reason = match status {
        200 => "OK",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        429 => "Too Many Requests",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Bad Request",
    };
    let body = body.to_string();
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n",
        body.len()
    );
    for line in extra {
        head.push_str(line);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(body.as_bytes())?;
    stream.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Harness {
        _home: tempfile::TempDir,
        home: PathBuf,
        address: SocketAddr,
    }

    fn start() -> Harness {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().to_path_buf();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let served = path.clone();
        thread::spawn(move || serve(listener, served));
        Harness {
            _home: home,
            home: path,
            address,
        }
    }

    /// One request; returns status, headers and body.
    fn send(
        address: SocketAddr,
        method: &str,
        path: &str,
        headers: &[&str],
        body: &str,
    ) -> (u16, String, Value) {
        let mut stream = TcpStream::connect(address).unwrap();
        let mut request =
            format!("{method} {path} HTTP/1.1\r\nHost: {address}\r\nOrigin: http://{address}\r\n");
        for header in headers {
            request.push_str(header);
            request.push_str("\r\n");
        }
        request.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
        (
            head[9..12].parse().unwrap(),
            head.to_string(),
            serde_json::from_str(body).unwrap_or(Value::Null),
        )
    }

    fn login(address: SocketAddr, user: &str, password: &str) -> (u16, Option<String>) {
        let body = json!({ "user": user, "password": password }).to_string();
        let (status, head, _) = send(address, "POST", "/auth/login", &[], &body);
        let cookie = head
            .lines()
            .find_map(|line| line.strip_prefix("Set-Cookie: "))
            .and_then(|value| value.split(';').next())
            .map(str::to_string);
        (status, cookie)
    }

    /// A stand-in daemon that echoes the request head it received.
    fn echo_daemon(home: &std::path::Path, session: &str) {
        register(home, session, "ready", echo());
    }

    fn register(home: &std::path::Path, session: &str, state: &str, url: String) {
        let registry = home.join("registry");
        std::fs::create_dir_all(&registry).unwrap();
        let record = json!({
            "name": session, "protocol": 1, "incarnation": "x", "state": state, "pid": 1,
            "url": url, "socket": "/nonexistent", "directory": home.join("sessions").join(session),
            "executable": "/bin/true", "source_executable": "/bin/true", "version": "0",
            "build_id": "x", "tmux_server": "x"
        });
        std::fs::write(registry.join(format!("{session}.json")), record.to_string()).unwrap();
    }

    fn echo() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut head = String::new();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                        break;
                    }
                    head.push_str(&line);
                }
                let body = json!({"head": head}).to_string();
                let mut stream = stream;
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        url
    }

    const PASSWORD: &str = "correct horse battery";

    #[test]
    fn the_operator_signs_in_and_reaches_every_session_as_on_loopback() {
        let harness = start();
        let address = harness.address;
        Login::at(&harness.home).set_password(PASSWORD).unwrap();
        echo_daemon(&harness.home, "demo");
        register(&harness.home, "idle", "stopped", echo());

        assert_eq!(send(address, "GET", "/auth/me", &[], "").0, 401);
        assert_eq!(send(address, "GET", "/s/demo/v1/chats", &[], "").0, 401);
        assert_eq!(login(address, "root", "wrong").0, 401);
        assert_eq!(login(address, "admin", PASSWORD).0, 401);
        let (status, cookie) = login(address, "root", PASSWORD);
        assert_eq!(status, 200);
        let cookie = cookie.expect("login cookie");
        let header = format!("Cookie: {cookie}");

        let (status, _, me) = send(address, "GET", "/auth/me", &[&header], "");
        assert_eq!(status, 200);
        assert_eq!(
            me["sessions"],
            json!([{"name": "demo", "state": "ready"}, {"name": "idle", "state": "stopped"}])
        );
        assert_eq!(me["user"], "root");
        assert_eq!(me["sessions"].as_array().unwrap().len(), 2);

        // The daemon hears neither the cookie nor any OMAR header the
        // browser made up.
        let (status, _, echoed) = send(
            address,
            "GET",
            "/s/demo/chats/c1/v1/chat",
            &[&header, "X-Omar-User: mallory"],
            "",
        );
        assert_eq!(status, 200);
        let head = echoed["head"].as_str().unwrap();
        assert!(
            head.starts_with("GET /chats/c1/v1/chat HTTP/1.1\r\n"),
            "{head}"
        );
        assert!(
            !head.contains("mallory") && !head.contains(COOKIE),
            "{head}"
        );
        assert!(head.contains("Connection: close\r\n"), "{head}");

        // A terminal's WebSocket keeps its upgrade, so the daemon can switch
        // protocols on the same connection.
        let (_, _, echoed) = send(
            address,
            "GET",
            "/s/demo/v1/agent/terminal",
            &[
                &header,
                "Connection: Upgrade",
                "Upgrade: websocket",
                "Sec-WebSocket-Key: a2V5",
            ],
            "",
        );
        let head = echoed["head"].as_str().unwrap();
        assert!(
            head.contains("Upgrade: websocket\r\n") && head.contains("Connection: Upgrade\r\n"),
            "{head}"
        );
        assert!(
            head.contains("Sec-WebSocket-Key: a2V5\r\n") && !head.contains("Connection: close"),
            "{head}"
        );

        // Stopped, unknown or malformed sessions; another site; signed out.
        assert_eq!(
            send(address, "GET", "/s/idle/v1/chats", &[&header], "").0,
            503
        );
        assert_eq!(
            send(address, "GET", "/s/../v1/chats", &[&header], "").0,
            404
        );
        let mut stream = TcpStream::connect(address).unwrap();
        write!(stream, "POST /s/demo/v1/chats HTTP/1.1\r\nHost: {address}\r\nOrigin: https://evil.example\r\n{header}\r\nContent-Length: 0\r\n\r\n").unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 403"), "{response}");
        assert_eq!(send(address, "POST", "/auth/logout", &[&header], "").0, 200);
        assert_eq!(
            send(address, "GET", "/s/demo/v1/chats", &[&header], "").0,
            401
        );

        // A new password signs every browser out.
        let (_, cookie) = login(address, "root", PASSWORD);
        let header = format!("Cookie: {}", cookie.unwrap());
        Login::at(&harness.home)
            .set_password("another long password")
            .unwrap();
        assert_eq!(send(address, "GET", "/auth/me", &[&header], "").0, 401);
    }

    #[test]
    fn repeated_wrong_passwords_lock_sign_in_for_a_minute() {
        let harness = start();
        Login::at(&harness.home).set_password(PASSWORD).unwrap();
        for _ in 0..ATTEMPTS {
            assert_eq!(login(harness.address, "root", "wrong").0, 401);
        }
        assert_eq!(login(harness.address, "root", PASSWORD).0, 429);
    }

    #[test]
    fn the_gateway_needs_a_password_and_listens_on_loopback_only() {
        let home = tempfile::tempdir().unwrap();
        let error = run_in(0, home.path().into()).unwrap_err();
        assert!(error.to_string().contains("omar passwd"), "{error}");
        let listener = listen(0).unwrap();
        assert!(listener.local_addr().unwrap().ip().is_loopback());
        let taken = listener.local_addr().unwrap().port();
        let error = listen(taken).unwrap_err();
        assert!(error.to_string().contains("--port"), "{error}");
    }
}

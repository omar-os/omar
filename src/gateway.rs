//! `omar gateway`: one address where team members sign in and reach the
//! sessions they belong to, like a Slack workspace switcher. It listens on
//! loopback behind an HTTPS proxy (Caddy), serves Mission Control, and relays
//! `/s/<session>/…` to that session's loopback daemon, naming the member in
//! `X-Omar-User`. The daemon decides what that member may see (see `serve`).
//!
//! Each connection carries one request: the relay asks the daemon to close
//! after answering, so no second request rides a connection already checked.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::accounts::{Accounts, LOGIN_SECONDS};

const COOKIE: &str = "omar_login";
const MAX_AUTH_BODY: usize = 16 * 1024;
const MAX_HEAD: usize = 64 * 1024;
/// Wrong passwords for one account before it must wait.
const ATTEMPTS: u32 = 5;
const LOCKOUT: Duration = Duration::from_secs(60);

struct Gateway {
    home: PathBuf,
    accounts: Accounts,
    failures: Mutex<HashMap<String, (u32, Instant)>>,
}

/// Serve until killed.
pub fn run(address: SocketAddr) -> Result<()> {
    if !address.ip().is_loopback() {
        bail!(
            "the gateway listens on loopback; put an HTTPS proxy in front of it \
             (its login cookie is Secure, so plain HTTP elsewhere cannot keep a login)"
        );
    }
    let listener = TcpListener::bind(address)
        .with_context(|| format!("failed to bind the gateway at {address}"))?;
    let home = crate::sessions::home_root();
    eprintln!(
        "Gateway on http://{}; accounts in {}",
        listener.local_addr()?,
        home.join("accounts.json").display()
    );
    serve(listener, home)
}

fn serve(listener: TcpListener, home: PathBuf) -> Result<()> {
    let gateway = Arc::new(Gateway {
        accounts: Accounts::at(&home),
        home,
        failures: Mutex::default(),
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
        .and_then(|token| Some((token.to_string(), gateway.accounts.resolve(token).ok()??)));

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
    login: Option<(String, (String, crate::accounts::Account))>,
) -> Result<()> {
    let field = |name: &str| -> String {
        serde_json::from_slice::<Value>(body)
            .ok()
            .and_then(|body| body.get(name)?.as_str().map(str::to_string))
            .unwrap_or_default()
    };
    match (request.method.as_str(), route) {
        ("GET", "me") => match login {
            None => respond(stream, 401, &json!({"error": "sign in"}), &[]),
            Some((_, (user, account))) => {
                let sessions = memberships(gateway, &user);
                respond(
                    stream,
                    200,
                    &json!({"user": user, "must_change": account.must_change, "sessions": sessions}),
                    &[],
                )
            }
        },
        ("POST", "login") => {
            let user = field("user");
            if let Some(wait) = gateway.locked_out(&user) {
                let error = format!("too many attempts; try again in {}s", wait.as_secs().max(1));
                return respond(stream, 429, &json!({"error": error}), &[]);
            }
            match gateway.accounts.verify(&user, &field("password"))? {
                Some(account) => {
                    gateway
                        .failures
                        .lock()
                        .expect("failures poisoned")
                        .remove(&user);
                    let token = gateway.accounts.login(&user)?;
                    let cookie = format!(
                        "Set-Cookie: {COOKIE}={token}; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age={LOGIN_SECONDS}"
                    );
                    respond(
                        stream,
                        200,
                        &json!({"user": user, "must_change": account.must_change}),
                        &[&cookie],
                    )
                }
                None => {
                    gateway.failed(&user);
                    respond(
                        stream,
                        401,
                        &json!({"error": "wrong account name or password"}),
                        &[],
                    )
                }
            }
        }
        ("POST", "logout") => {
            if let Some((token, _)) = &login {
                gateway.accounts.logout(token)?;
            }
            let cookie = format!(
                "Set-Cookie: {COOKIE}=; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age=0"
            );
            respond(stream, 200, &json!({"ok": true}), &[&cookie])
        }
        ("POST", "password") => {
            let Some((token, (user, _))) = &login else {
                return respond(stream, 401, &json!({"error": "sign in"}), &[]);
            };
            match gateway.accounts.change_password(
                user,
                &field("current"),
                &field("new"),
                Some(token),
            ) {
                Ok(()) => respond(stream, 200, &json!({"ok": true}), &[]),
                Err(error) => respond(stream, 400, &json!({"error": error.to_string()}), &[]),
            }
        }
        _ => respond(stream, 404, &json!({"error": "not found"}), &[]),
    }
}

/// The sessions `user` belongs to, with their role and whether they are up.
fn memberships(gateway: &Gateway, user: &str) -> Vec<Value> {
    let Ok(entries) = std::fs::read_dir(gateway.home.join("sessions")) else {
        return Vec::new();
    };
    let mut sessions: Vec<Value> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let role = crate::team::role(&entry.path(), user)?;
            let state =
                registered(gateway, &name).map_or("stopped".into(), |session| session.state);
            Some(json!({"name": name, "role": role, "state": state}))
        })
        .collect();
    sessions.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    sessions
}

fn registered(gateway: &Gateway, name: &str) -> Option<crate::sessions::Session> {
    let bytes = std::fs::read(gateway.home.join("registry").join(format!("{name}.json"))).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Hand one request to the session's daemon, as `user`, and splice the two
/// connections until either closes: replies, event streams and terminal
/// WebSockets alike.
fn relay(
    mut stream: TcpStream,
    reader: BufReader<TcpStream>,
    gateway: &Gateway,
    request: &Request,
    session: &str,
    route: &str,
    login: Option<(String, (String, crate::accounts::Account))>,
) -> Result<()> {
    let Some((_, (user, account))) = login else {
        return respond(&mut stream, 401, &json!({"error": "sign in"}), &[]);
    };
    if account.must_change {
        return respond(
            &mut stream,
            403,
            &json!({"error": "change your password first"}),
            &[],
        );
    }
    let directory = gateway.home.join("sessions").join(session);
    if session.contains(['/', '.']) || crate::team::role(&directory, &user).is_none() {
        return respond(
            &mut stream,
            403,
            &json!({"error": "not a member of this session"}),
            &[],
        );
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
    let mut head = format!(
        "{} {route} HTTP/1.1\r\nHost: {address}\r\nX-Omar-User: {user}\r\n",
        request.method
    );
    for line in &request.headers {
        let name = line
            .split(':')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        // The daemon trusts loopback: it must see the gateway's word for who
        // is asking, never the browser's, and no credential of the gateway's.
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
    fn locked_out(&self, user: &str) -> Option<Duration> {
        let failures = self.failures.lock().expect("failures poisoned");
        let (count, since) = failures.get(user)?;
        let elapsed = since.elapsed();
        (*count >= ATTEMPTS && elapsed < LOCKOUT).then(|| LOCKOUT - elapsed)
    }

    fn failed(&self, user: &str) {
        let mut failures = self.failures.lock().expect("failures poisoned");
        let entry = failures
            .entry(user.to_string())
            .or_insert((0, Instant::now()));
        if entry.1.elapsed() >= LOCKOUT {
            *entry = (0, Instant::now());
        }
        entry.0 += 1;
        entry.1 = Instant::now();
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
        let body = json!({"user": user, "password": password}).to_string();
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
        let registry = home.join("registry");
        std::fs::create_dir_all(&registry).unwrap();
        let record = json!({
            "name": session, "protocol": 1, "incarnation": "x", "state": "ready", "pid": 1,
            "url": url, "socket": "/nonexistent", "directory": home.join("sessions").join(session),
            "executable": "/bin/true", "source_executable": "/bin/true", "version": "0",
            "build_id": "x", "tmux_server": "x"
        });
        std::fs::write(registry.join(format!("{session}.json")), record.to_string()).unwrap();
    }

    #[test]
    fn a_member_signs_in_changes_the_temporary_password_and_reaches_their_session() {
        let harness = start();
        let address = harness.address;
        let accounts = Accounts::at(&harness.home);
        let temporary = accounts.add("alice").unwrap();
        accounts.add("bob").unwrap();
        let session = harness.home.join("sessions/demo");
        crate::team::add_member(&session, &harness.home, "alice", crate::team::Role::Admin)
            .unwrap();
        echo_daemon(&harness.home, "demo");

        assert_eq!(send(address, "GET", "/auth/me", &[], "").0, 401);
        assert_eq!(send(address, "GET", "/s/demo/v1/chats", &[], "").0, 401);
        assert_eq!(login(address, "alice", "wrong").0, 401);
        let (status, cookie) = login(address, "alice", &temporary);
        assert_eq!(status, 200);
        let cookie = cookie.expect("login cookie");
        let header = format!("Cookie: {cookie}");

        // A temporary password opens nothing but the password change.
        let (status, _, me) = send(address, "GET", "/auth/me", &[&header], "");
        assert_eq!((status, me["must_change"].as_bool()), (200, Some(true)));
        assert_eq!(
            me["sessions"],
            json!([{"name": "demo", "role": "admin", "state": "ready"}])
        );
        assert_eq!(
            send(address, "GET", "/s/demo/v1/chats", &[&header], "").0,
            403
        );
        let change = json!({"current": temporary, "new": "a much longer secret"}).to_string();
        assert_eq!(
            send(address, "POST", "/auth/password", &[&header], &change).0,
            200
        );

        // The daemon hears the gateway's name for the member, never the
        // browser's, and no cookie.
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
        assert!(head.contains("X-Omar-User: alice\r\n"), "{head}");
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

        // Not a member; another site; signed out.
        let bob = Accounts::at(&harness.home).reset("bob").unwrap();
        let (_, bob_cookie) = login(address, "bob", &bob);
        let bob_header = format!("Cookie: {}", bob_cookie.unwrap());
        let change = json!({"current": bob, "new": "bob's longer secret"}).to_string();
        assert_eq!(
            send(address, "POST", "/auth/password", &[&bob_header], &change).0,
            200
        );
        assert_eq!(
            send(address, "GET", "/s/demo/v1/chats", &[&bob_header], "").0,
            403
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
    }

    #[test]
    fn repeated_wrong_passwords_lock_the_account_for_a_minute() {
        let harness = start();
        let temporary = Accounts::at(&harness.home).add("alice").unwrap();
        for _ in 0..ATTEMPTS {
            assert_eq!(login(harness.address, "alice", "wrong").0, 401);
        }
        assert_eq!(login(harness.address, "alice", &temporary).0, 429);
    }

    #[test]
    fn the_gateway_refuses_to_listen_off_loopback() {
        let error = run("0.0.0.0:0".parse().unwrap()).unwrap_err();
        assert!(error.to_string().contains("HTTPS proxy"), "{error}");
    }
}

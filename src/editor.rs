//! On-demand code-server, behind a loopback capability-authenticated gateway.
//! A private Unix socket keeps the unauthenticated upstream off the network.
use crate::workspace::Workspace;
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::os::unix::{fs::PermissionsExt, net::UnixStream, process::CommandExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::thread;
use std::time::{Duration, Instant};
use uuid::Uuid;

#[derive(Default)]
pub struct Editors {
    sessions: BTreeMap<String, Editor>,
}
struct Editor {
    child: Child,
    gateway: Arc<Gateway>,
    directory: PathBuf,
}
struct Gateway {
    address: SocketAddr,
    socket: PathBuf,
    ticket: String,
    cookie: String,
    stopping: AtomicBool,
    connections: AtomicUsize,
    last_used: Mutex<Instant>,
}
impl Editors {
    pub fn open(&mut self, root: &Path, workspace: &Workspace) -> Result<String> {
        if let Some(editor) = self.sessions.get_mut(&workspace.id) {
            if editor.child.try_wait()?.is_none() {
                *editor
                    .gateway
                    .last_used
                    .lock()
                    .expect("editor clock poisoned") = Instant::now();
                return Ok(editor.url());
            }
        }
        self.sessions.remove(&workspace.id);
        let editor = Editor::start(root, workspace)?;
        let url = editor.url();
        self.sessions.insert(workspace.id.clone(), editor);
        Ok(url)
    }
    pub fn connected(&mut self) -> bool {
        self.sessions.retain(|_, e| {
            e.gateway.connections.load(Ordering::SeqCst) > 0
                || e.gateway
                    .last_used
                    .lock()
                    .expect("editor clock poisoned")
                    .elapsed()
                    < Duration::from_secs(60)
        });
        self.sessions
            .values()
            .any(|e| e.gateway.connections.load(Ordering::SeqCst) > 0)
    }
    pub fn stop(&mut self, id: &str) {
        self.sessions.remove(id);
    }
    pub fn stop_all(&mut self) {
        self.sessions.clear();
    }
}
impl Editor {
    fn url(&self) -> String {
        format!(
            "http://{}/open/{}",
            self.gateway.address, self.gateway.ticket
        )
    }
    fn start(root: &Path, workspace: &Workspace) -> Result<Self> {
        // Socket paths must fit sockaddr_un even on macOS. Keep this directly
        // under /tmp, with an unpredictable name and owner-only permissions.
        let directory = PathBuf::from(format!("/tmp/omar-editor-{}", Uuid::new_v4().simple()));
        fs::create_dir(&directory)?;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
        let result = Self::spawn(root, workspace, &directory);
        if result.is_err() {
            let _ = fs::remove_dir_all(&directory);
        }
        result
    }
    fn spawn(root: &Path, workspace: &Workspace, directory: &Path) -> Result<Self> {
        let socket = directory.join("http.sock");
        let data = root.join("editors").join(&workspace.id);
        fs::create_dir_all(&data)?;
        fs::set_permissions(&data, fs::Permissions::from_mode(0o700))?;
        let config = directory.join("config.yaml");
        fs::write(&config, "{}\n")?;
        let log = fs::File::create(data.join("code-server.log"))?;
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let gateway = Arc::new(Gateway {
            address: listener.local_addr()?,
            socket: socket.clone(),
            ticket: Uuid::new_v4().simple().to_string(),
            cookie: format!("omar_editor_{}", workspace.id.replace('-', "")),
            stopping: AtomicBool::new(false),
            connections: AtomicUsize::new(0),
            last_used: Mutex::new(Instant::now()),
        });
        let executable =
            std::env::var_os("OMAR_CODE_SERVER_BIN").unwrap_or_else(|| "code-server".into());
        let child = Command::new(executable)
            .args(["--auth", "none", "--socket-mode", "0600", "--disable-telemetry", "--disable-update-check", "--disable-proxy"])
            .arg("--config").arg(config).arg("--socket").arg(socket)
            .arg("--user-data-dir").arg(data.join("data"))
            .arg("--extensions-dir").arg(root.join("editors/extensions"))
            .arg(workspace.worktree(root)).current_dir(workspace.worktree(root))
            .env("OMAR_WORKTREE", workspace.worktree(root)).env("OMAR_TEMP", workspace.temp(root)).env("TMPDIR", workspace.temp(root))
            .env_remove("PASSWORD").env_remove("HASHED_PASSWORD").env_remove("GITHUB_TOKEN")
            .stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log)
            .process_group(0).spawn()
            .context("Could not start code-server. Install code-server from https://coder.com/docs/code-server/install, then try again (or set OMAR_CODE_SERVER_BIN).")?;
        let mut editor = Self {
            child,
            gateway: gateway.clone(),
            directory: directory.to_path_buf(),
        };
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            anyhow::ensure!(
                editor.child.try_wait()?.is_none(),
                "code-server exited; see {}",
                data.join("code-server.log").display()
            );
            if UnixStream::connect(&gateway.socket).is_ok() {
                break;
            }
            anyhow::ensure!(
                Instant::now() < deadline,
                "code-server did not become ready; see {}",
                data.join("code-server.log").display()
            );
            thread::sleep(Duration::from_millis(50));
        }
        thread::spawn(move || {
            while !gateway.stopping.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let gateway = gateway.clone();
                        thread::spawn(move || {
                            let _ = relay(stream, gateway);
                        });
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(25))
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(editor)
    }
}
impl Drop for Editor {
    fn drop(&mut self) {
        self.gateway.stopping.store(true, Ordering::SeqCst);
        // All editor children share the process group. Keep the child unreaped
        // until after signaling so its group ID cannot be recycled.
        let _ = Command::new("kill")
            .args(["-KILL", "--", &format!("-{}", self.child.id())])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.directory);
    }
}
struct Connection(Arc<Gateway>);
impl Drop for Connection {
    fn drop(&mut self) {
        *self.0.last_used.lock().expect("editor clock poisoned") = Instant::now();
        self.0.connections.fetch_sub(1, Ordering::SeqCst);
    }
}
fn respond(stream: &mut TcpStream, status: &str, headers: &str, body: &str) -> Result<()> {
    write!(stream, "HTTP/1.1 {status}\r\nConnection: close\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\n{headers}Content-Length: {}\r\n\r\n{body}", body.len())?;
    Ok(())
}
fn relay(mut stream: TcpStream, gateway: Arc<Gateway>) -> Result<()> {
    // Accepted sockets inherit O_NONBLOCK on macOS. Blocking writes must
    // handle large IDE bundles without treating backpressure as EOF.
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request = Vec::new();
    loop {
        let mut line = Vec::new();
        let count = reader.by_ref().take(16385).read_until(b'\n', &mut line)?;
        anyhow::ensure!(
            count > 0 && line.len() <= 16384 && request.len() + line.len() <= 65536,
            "invalid editor request headers"
        );
        let end = line == b"\r\n";
        request.extend(line);
        if end {
            break;
        }
    }
    let text = std::str::from_utf8(&request)?;
    let mut lines = text.split("\r\n");
    let mut start = lines.next().unwrap_or_default().split_whitespace();
    let method = start.next().unwrap_or_default();
    let path = start.next().unwrap_or_default();
    let mut headers = BTreeMap::new();
    for line in lines {
        if let Some((key, value)) = line.split_once(':') {
            headers.insert(key.to_ascii_lowercase(), value.trim());
        }
    }
    let expected_host = gateway.address.to_string();
    let expected_origin = format!("http://{expected_host}");
    if headers.get("host") != Some(&expected_host.as_str())
        || headers.get("origin").is_some_and(|o| *o != expected_origin)
    {
        return respond(&mut stream, "403 Forbidden", "", "Editor origin rejected");
    }
    if method == "GET" && path == format!("/open/{}", gateway.ticket) {
        return respond(
            &mut stream,
            "303 See Other",
            &format!(
                "Location: /\r\nSet-Cookie: {}={}; HttpOnly; SameSite=Strict; Path=/\r\n",
                gateway.cookie, gateway.ticket
            ),
            "",
        );
    }
    let cookie = format!("{}={}", gateway.cookie, gateway.ticket);
    if !headers
        .get("cookie")
        .is_some_and(|v| v.split(';').any(|c| c.trim() == cookie))
    {
        return respond(
            &mut stream,
            "403 Forbidden",
            "",
            "Open this editor from Mission Control.",
        );
    }
    anyhow::ensure!(!gateway.stopping.load(Ordering::SeqCst), "editor stopped");
    gateway.connections.fetch_add(1, Ordering::SeqCst);
    let _connection = Connection(gateway.clone());
    let mut upstream = UnixStream::connect(&gateway.socket)?;
    upstream.set_write_timeout(Some(Duration::from_secs(10)))?;
    upstream.write_all(&request)?;
    // Preserve bytes BufReader already consumed after the headers (including
    // early WebSocket frames). Then relay raw HTTP/WebSocket traffic.
    upstream.write_all(reader.buffer())?;
    drop(reader);
    let mut upstream_reader = upstream.try_clone()?;
    let mut downstream = stream.try_clone()?;
    let stopped = Arc::new(AtomicBool::new(false));
    let read_stop = stopped.clone();
    let read_gateway = gateway.clone();
    upstream_reader.set_read_timeout(Some(Duration::from_millis(250)))?;
    stream.set_read_timeout(Some(Duration::from_millis(250)))?;
    let back = thread::spawn(move || {
        pump(
            &mut upstream_reader,
            &mut downstream,
            &read_stop,
            &read_gateway.stopping,
        );
        read_stop.store(true, Ordering::SeqCst);
        let _ = downstream.shutdown(Shutdown::Both);
    });
    pump(&mut stream, &mut upstream, &stopped, &gateway.stopping);
    stopped.store(true, Ordering::SeqCst);
    let _ = upstream.shutdown(Shutdown::Both);
    let _ = back.join();
    Ok(())
}
fn pump(
    input: &mut impl Read,
    output: &mut impl Write,
    closed: &AtomicBool,
    stopping: &AtomicBool,
) {
    let mut buffer = [0; 32 * 1024];
    while !closed.load(Ordering::SeqCst) && !stopping.load(Ordering::SeqCst) {
        match input.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                if output.write_all(&buffer[..n]).is_err() {
                    break;
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue
            }
            Err(_) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn editor_gateway_relays_large_assets_without_truncation() {
        use std::os::unix::net::UnixListener;
        let directory = tempfile::tempdir_in("/tmp").unwrap();
        let socket = directory.path().join("test.sock");
        let upstream = UnixListener::bind(&socket).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let gateway = Arc::new(Gateway {
            address,
            socket,
            ticket: "secret".into(),
            cookie: "test_cookie".into(),
            stopping: AtomicBool::new(false),
            connections: AtomicUsize::new(0),
            last_used: Mutex::new(Instant::now()),
        });
        let mut client = TcpStream::connect(address).unwrap();
        let (server, _) = listener.accept().unwrap();
        let handler = thread::spawn(move || relay(server, gateway).unwrap());
        let backend = thread::spawn(move || {
            let (mut stream, _) = upstream.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
            }
            let bytes = vec![b'x'; 4 * 1024 * 1024];
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len()
            )
            .unwrap();
            stream.write_all(&bytes).unwrap();
        });
        write!(
            client,
            "GET /bundle.js HTTP/1.1\r\nHost: {address}\r\nCookie: test_cookie=secret\r\n\r\n"
        )
        .unwrap();
        thread::sleep(Duration::from_millis(100));
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        assert_eq!(
            response.split_once("\r\n\r\n").unwrap().1.len(),
            4 * 1024 * 1024
        );
        backend.join().unwrap();
        handler.join().unwrap();
    }

    #[test]
    fn editor_gateway_requires_capability_cookie_and_its_own_origin() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let gateway = Arc::new(Gateway {
            address,
            socket: PathBuf::from("/missing"),
            ticket: "secret".into(),
            cookie: "test_cookie".into(),
            stopping: AtomicBool::new(false),
            connections: AtomicUsize::new(0),
            last_used: Mutex::new(Instant::now()),
        });
        for (path, extra, expected) in [
            ("/", "", "403 Forbidden"),
            (
                "/open/secret",
                "Origin: https://attacker.example\r\n",
                "403 Forbidden",
            ),
            ("/open/secret", "", "303 See Other"),
        ] {
            let gateway = gateway.clone();
            let mut client = TcpStream::connect(address).unwrap();
            let (server, _) = listener.accept().unwrap();
            let handler = thread::spawn(move || relay(server, gateway).unwrap());
            write!(
                client,
                "GET {path} HTTP/1.1\r\nHost: {address}\r\n{extra}\r\n"
            )
            .unwrap();
            let mut response = String::new();
            client.read_to_string(&mut response).unwrap();
            assert!(response.contains(expected), "{response}");
            handler.join().unwrap();
        }
    }
}

use super::*;
use crate::messaging::{runtime, tests::fixture};

#[test]
fn migration_keeps_connection_identity_and_discards_old_workflow_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let original = dir.path().join("config.json");
    let value = json!({"runtimeUrl":"http://127.0.0.1:1234/chats/stale-chat", "workspaceId":"T1",
        "allowedUsers":["U1"],"allowedChannels":["C1"],
        "workflows":{"demo":{"programPath":"demo.omar","outputPort":"run.result"}}});
    std::fs::write(&original, value.to_string()).unwrap();
    let imported = import(&original, "slack").unwrap();
    assert_eq!(imported["runtimeUrl"], RUNTIME_URL);
    assert_eq!(imported["autoChat"], true);
    assert_eq!(imported["accountId"], "T1");
    assert_eq!(imported["allowedConversations"], json!(["C1"]));
    assert!(imported.get("workflows").is_none());
    assert!(imported.get("workflowCatalog").is_none());
    assert_eq!(
        serde_json::from_slice::<Value>(&std::fs::read(&original).unwrap()).unwrap(),
        value
    );
}

#[tokio::test]
async fn automatic_chat_is_runtime_owned_and_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let server = crate::serve::Serve::start(
        "127.0.0.1:0".parse().unwrap(),
        &crate::config::Config::default(),
        dir.path(),
        0,
    )
    .unwrap();
    let root = format!("http://{}", server.address());
    let mut c = fixture();
    c.platform = "slack".into();
    c.auto_chat = true;
    c.runtime_url = root.clone();
    let original = OmarRuntime::new(root.clone())
        .unwrap()
        .call("/v1/chat", None)
        .await
        .unwrap()["id"]
        .clone();
    runtime::bind_chat(&mut c).await.unwrap();
    let first = c.runtime_url.clone();
    assert!(!c.auto_chat);
    let active = OmarRuntime::new(root.clone())
        .unwrap()
        .call("/v1/chat", None)
        .await
        .unwrap()["id"]
        .clone();
    assert_eq!(active, original);
    c.auto_chat = true;
    c.runtime_url = root;
    runtime::bind_chat(&mut c).await.unwrap();
    assert_eq!(c.runtime_url, first);
    drop(server);
    let server = crate::serve::Serve::start(
        "127.0.0.1:0".parse().unwrap(),
        &crate::config::Config::default(),
        dir.path(),
        0,
    )
    .unwrap();
    c.auto_chat = true;
    c.runtime_url = format!("http://{}", server.address());
    runtime::bind_chat(&mut c).await.unwrap();
    assert_eq!(
        c.runtime_url.split("/chats/").nth(1),
        first.split("/chats/").nth(1)
    );
    let slack = c.runtime_url.clone();
    c.auto_chat = true;
    c.platform = "discord".into();
    c.runtime_url = format!("http://{}", server.address());
    runtime::bind_chat(&mut c).await.unwrap();
    assert_ne!(c.runtime_url, slack);
}

#[test]
fn root_urls_require_explicit_automatic_chat_and_reject_remote_hosts() {
    let mut c = fixture();
    c.runtime_url = RUNTIME_URL.into();
    assert!(c.validate().is_err());
    c.auto_chat = true;
    assert!(c.validate().is_ok());
    for url in [
        "http://example.com:45073",
        "http://127.0.0.1:45073/other",
        "http://user@localhost:45073",
        "https://localhost:45073",
    ] {
        c.runtime_url = url.into();
        assert!(c.validate().is_err(), "{url}");
    }
}

#[test]
fn connect_lock_prevents_duplicates_and_is_released_on_exit() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("messaging");
    let first = connector_lock(&folder, "slack").unwrap();
    assert!(connector_lock(&folder, "slack").is_err());
    let other = connector_lock(&folder, "discord").unwrap();
    drop(first);
    assert!(connector_lock(&folder, "slack").is_ok());
    drop(other);
}

#[test]
fn runtime_selection_restarts_the_saved_port_owner_without_repurposing_others() {
    let session = |name: &str, url: &str, state: &str| {
        serde_json::from_value::<crate::sessions::Session>(json!({
            "name":name,"url":url,"state":state,"protocol":1,"incarnation":"i","pid":0,
            "socket":"/tmp/test.sock","directory":"/tmp/test","executable":"/tmp/omar",
            "source_executable":"/tmp/omar","version":"test","build_id":"test","tmux_server":"test"
        }))
        .unwrap()
    };
    assert_eq!(runtime_session(&[]).unwrap(), "messaging");
    let old = session("slack-research", RUNTIME_URL, "stopped");
    assert_eq!(
        runtime_session(std::slice::from_ref(&old)).unwrap(),
        "slack-research"
    );
    assert!(runtime_session(&[session("other", RUNTIME_URL, "unreachable")]).is_err());
    assert!(runtime_session(&[session("messaging", "http://127.0.0.1:9000", "stopped")]).is_err());
    assert!(runtime_session(&[old, session("second", RUNTIME_URL, "stopped")]).is_err());
}

#[cfg(unix)]
#[test]
fn connector_exit_unlocks_even_when_a_child_inherits_the_descriptor() {
    let dir = tempfile::tempdir().unwrap();
    let lock = connector_lock(dir.path(), "discord").unwrap();
    let mut pipe = [0; 2];
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0);
    if pid == 0 {
        // Only async-signal-safe operations after fork in a multithreaded test.
        unsafe {
            libc::close(pipe[1]);
            let mut byte = 0u8;
            libc::read(pipe[0], (&mut byte as *mut u8).cast(), 1);
            libc::_exit(0);
        }
    }
    unsafe {
        libc::close(pipe[0]);
    }
    drop(lock);
    let replacement = connector_lock(dir.path(), "discord");
    unsafe {
        libc::write(pipe[1], [1u8].as_ptr().cast(), 1);
        libc::close(pipe[1]);
        libc::waitpid(pid, std::ptr::null_mut(), 0);
    }
    assert!(replacement.is_ok());
}

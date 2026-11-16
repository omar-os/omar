//! A saved trigger owns a pinned draft and a durable inbox. HTTP admission
//! and dispatch never depend on whichever workflow a Studio tab has selected.
//! Any sender works: the trigger names its auth, event and delivery-ID headers.
use super::*;
use anyhow::{ensure, Context};
use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::Sha256;

/// How a delivery is recognised. GitHub is one preset of these values.
#[derive(Clone, Serialize, Deserialize)]
struct Source {
    /// "hmac-sha256" (signature of the raw body) or "token" (shared secret).
    #[serde(default = "hmac_auth")]
    auth: String,
    auth_header: String,
    /// Optional: only requests whose event header equals `event` start runs.
    #[serde(default)]
    event_header: String,
    #[serde(default)]
    event: String,
    /// Optional: repeated IDs return the first receipt instead of a new run.
    #[serde(default)]
    id_header: String,
}

fn hmac_auth() -> String {
    "hmac-sha256".into()
}

fn header_name(name: &str) -> bool {
    name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn check_source(mut src: Source) -> Result<Source> {
    for h in [
        &mut src.auth_header,
        &mut src.event_header,
        &mut src.id_header,
    ] {
        *h = h.trim().to_ascii_lowercase();
        ensure!(
            header_name(h),
            "Header names use letters, digits and dashes."
        );
    }
    ensure!(
        matches!(src.auth.as_str(), "hmac-sha256" | "token"),
        "Choose HMAC-SHA256 signature or shared token authentication."
    );
    ensure!(
        !src.auth_header.is_empty(),
        "Name the header that carries the signature or token."
    );
    if src.event_header.is_empty() {
        src.event.clear();
    }
    ensure!(
        src.event_header.is_empty()
            || (!src.event.is_empty()
                && src.event.len() <= 128
                && src.event.bytes().all(|c| c.is_ascii_graphic())),
        "Enter the event value to match."
    );
    Ok(src)
}

#[derive(Clone, Serialize, Deserialize)]
struct Trigger {
    id: String,
    revision: u64,
    #[serde(flatten)]
    source: Source,
    secret_name: String,
    mapping: BTreeMap<String, String>,
    enabled: bool,
    team: String,
    program: String,
    filename: String,
    files: BTreeMap<String, String>,
    inputs: BTreeMap<String, Value>,
    types: BTreeMap<String, String>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Delivery {
    id: String,
    run_id: String,
    revision: u64,
    team: String,
    received_at: u64,
    status: String,
    // Only accepted, mapped inputs and the pinned request. Drop after dispatch.
    request: Option<Value>,
    error: Option<String>,
}

#[derive(Default, Serialize, Deserialize)]
struct Inbox {
    trigger: Option<Trigger>,
    deliveries: Vec<Delivery>,
}

// Resolve a dedicated local key on each delivery so rotation needs no restart.
fn signing_key(name: &str) -> Result<Vec<u8>> {
    use std::io::Read;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    ensure!(
        !name.is_empty()
            && name.len() <= 80
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b)),
        "Use a signing key name containing letters, digits, dashes or underscores."
    );
    let path = crate::sessions::home_root()
        .join("webhook-secrets")
        .join(name);
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let meta = file.metadata()?;
    ensure!(
        meta.is_file() && meta.permissions().mode() & 0o077 == 0,
        "Signing key must be a private regular file."
    );
    let mut key = Vec::new();
    file.take(4097).read_to_end(&mut key)?;
    if key.last() == Some(&b'\n') {
        key.pop();
        if key.last() == Some(&b'\r') {
            key.pop();
        }
    }
    ensure!(
        !key.is_empty() && key.len() <= 4096,
        "Invalid signing key length."
    );
    Ok(key)
}

fn path(context: &Context_) -> PathBuf {
    crate::ea::ea_state_dir(context.ea_id, &context.omar_dir).join("webhook-inbox.json")
}

fn load(context: &Context_) -> Result<Inbox> {
    match fs::read(path(context)) {
        Ok(raw) => Ok(serde_json::from_slice(&raw)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Inbox::default()),
        Err(e) => Err(e.into()),
    }
}

fn receipt(row: &Delivery) -> Value {
    json!({"delivery_id":row.id,"run_id":row.run_id,"revision":row.revision,"received_at":row.received_at,"status":row.status,"error":row.error})
}

fn view(book: &Inbox) -> Value {
    json!({"trigger":book.trigger.as_ref().map(|t| {
            let mut v = json!(t.source);
            v.as_object_mut().unwrap().extend([
                ("id".into(), json!(t.id)),
                ("secret_name".into(), json!(t.secret_name)),
                ("mapping".into(), json!(t.mapping)),
                ("enabled".into(), json!(t.enabled)),
                ("revision".into(), json!(t.revision)),
            ]);
            v
        }),
        "deliveries":book.deliveries.iter().rev().take(100).map(receipt).collect::<Vec<_>>()})
}

pub(super) fn armed(context: &Context_) -> bool {
    context.webhook_active.load(Ordering::Relaxed)
}

// Finished receipts kept for display and duplicate suppression. Queued rows are never dropped.
const KEEP_FINISHED: usize = 100;

// An enabled trigger keeps the daemon up. Only queued rows make the dispatcher read the inbox.
fn sync(context: &Context_, book: &Inbox) {
    let queued = book.deliveries.iter().any(|d| d.request.is_some());
    context.webhook_queued.store(queued, Ordering::Relaxed);
    context.webhook_active.store(
        queued || book.trigger.as_ref().is_some_and(|t| t.enabled),
        Ordering::Relaxed,
    );
}

fn prune(book: &mut Inbox) {
    let finished = book
        .deliveries
        .iter()
        .filter(|d| d.request.is_none())
        .count();
    let mut drop = finished.saturating_sub(KEEP_FINISHED);
    book.deliveries.retain(|d| {
        if drop > 0 && d.request.is_none() {
            drop -= 1;
            return false;
        }
        true
    });
}

pub(super) fn resume(context: &Context_) {
    if let Ok(book) = load(context) {
        sync(context, &book);
    }
}

#[derive(Deserialize)]
struct Activation {
    revision: u64,
    #[serde(flatten)]
    source: Source,
    secret_name: String,
    mapping: BTreeMap<String, String>,
}

pub(super) fn settings(context: &Context_, method: &str, body: &[u8]) -> (u16, Value) {
    let _guard = context
        .webhook_operation
        .lock()
        .expect("webhook lock poisoned");
    let mut book = match load(context) {
        Ok(b) => b,
        Err(_) => return (500, json!({"error":"Cannot read the webhook inbox."})),
    };
    if method == "GET" {
        return (200, view(&book));
    }
    if method != "POST" {
        return (405, json!({"error":"Method not allowed."}));
    }
    let value: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"error":"Expected trigger settings."})),
    };
    if value.get("enabled") == Some(&json!(false)) {
        if let Some(t) = &mut book.trigger {
            t.enabled = false;
        }
    } else {
        let setup = (|| -> Result<Trigger> {
            let a: Activation = serde_json::from_value(value)?;
            let source = check_source(a.source)?;
            signing_key(&a.secret_name).context("Create the referenced mode-0600 file in the runtime home’s webhook-secrets directory.")?;
            let ws = context
                .history
                .lock()
                .expect("history poisoned")
                .conversation(&context.conversation_id)
                .workspace
                .clone();
            ensure!(
                ws.revision == a.revision,
                "The saved revision changed. Refresh before activating."
            );
            let draft = ws
                .draft
                .context("Save a draft before activating a trigger.")?;
            let staged = stage_program(
                &draft.design.program,
                Some(&draft.filename),
                &draft.design.files,
            )
            .map_err(|_| anyhow::anyhow!("Cannot stage the saved draft."))?;
            let state =
                topology::load_staged_program(&staged.path).and_then(|bc| topology::verify(&bc));
            let vm = staged.finish(state).map_err(anyhow::Error::msg)?;
            let types: BTreeMap<_, _> = topology::open_inputs(&vm)
                .into_iter()
                .map(|n| (n.clone(), vm.ports[&n].ty.clone()))
                .collect();
            for (n, pointer) in &a.mapping {
                ensure!(types.contains_key(n) && types[n]!="path","Map an open data input. Filesystem paths must be supplied by the local workflow.");
                ensure!(
                    pointer.starts_with('/') && pointer.len() <= 256,
                    "Use a JSON Pointer such as /repository/full_name."
                );
            }
            let mut defaults = draft.design.inputs;
            // Mapped sample values are replaced by the incoming event. All
            // remaining values must satisfy the verified local port types.
            for key in a.mapping.keys() {
                defaults.remove(key);
            }
            for (n, v) in &defaults {
                let port = vm
                    .ports
                    .get(n)
                    .context("A saved input was removed from the workflow.")?;
                ensure!(
                    port.kind == PortKind::Input && topology::validate_value(&port.ty, v).is_ok(),
                    "Invalid saved input for {n}."
                );
            }
            for n in types.keys() {
                ensure!(
                    a.mapping.contains_key(n) || defaults.contains_key(n),
                    "Supply a value or mapping for {n}."
                );
            }
            Ok(Trigger {
                id: book
                    .trigger
                    .as_ref()
                    .map(|t| t.id.clone())
                    .unwrap_or_else(|| Uuid::new_v4().to_string()),
                revision: ws.revision,
                source,
                secret_name: a.secret_name,
                mapping: a.mapping,
                enabled: true,
                team: vm.team,
                program: draft.design.program,
                filename: draft.filename,
                files: draft.design.files,
                inputs: defaults,
                types,
            })
        })();
        match setup {
            Ok(t) => book.trigger = Some(t),
            Err(e) => return (400, json!({"error":e.to_string()})),
        }
    }
    if topology::write_json_atomic(&path(context), &book).is_err() {
        return (500, json!({"error":"Could not persist trigger."}));
    }
    sync(context, &book);
    (200, view(&book))
}

// Hex (GitHub, most senders) or base64 (e.g. Shopify), with an optional "sha256=" prefix.
fn signed(secret: &[u8], bytes: &[u8], signature: &str) -> bool {
    let sig = signature.strip_prefix("sha256=").unwrap_or(signature);
    let tag = if sig.len() == 64 && sig.bytes().all(|b| b.is_ascii_hexdigit()) {
        (0..32)
            .map(|i| u8::from_str_radix(&sig[i * 2..i * 2 + 2], 16).unwrap())
            .collect()
    } else {
        match base64::engine::general_purpose::STANDARD.decode(sig) {
            Ok(tag) => tag,
            Err(_) => return false,
        }
    };
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret) else {
        return false;
    };
    mac.update(bytes);
    mac.verify_slice(&tag).is_ok()
}

fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn authentic(src: &Source, secret: &[u8], bytes: &[u8], value: &str) -> bool {
    match src.auth.as_str() {
        "token" => same(
            value.strip_prefix("Bearer ").unwrap_or(value).as_bytes(),
            secret,
        ),
        _ => signed(secret, bytes, value),
    }
}

pub(super) fn accept(
    context: &Context_,
    trigger: &str,
    headers: &BTreeMap<String, Option<String>>,
    bytes: &[u8],
) -> (u16, Value) {
    let _guard = context
        .webhook_operation
        .lock()
        .expect("webhook lock poisoned");
    let mut book = match load(context) {
        Ok(b) => b,
        Err(_) => return (500, json!({"error":"Cannot read webhook inbox."})),
    };
    let Some(t) = book.trigger.as_ref().filter(|t| t.id == trigger) else {
        return (404, json!({"error":"Unknown trigger."}));
    };
    let src = &t.source;
    // Absent -> Ok(None). Sent more than once -> Err: ambiguous, refuse.
    let header = |key: &str| match headers.get(key) {
        None => Ok(None),
        Some(None) => Err(()),
        Some(Some(v)) => Ok(Some(v.as_str())),
    };
    let Ok(secret) = signing_key(&t.secret_name) else {
        return (503, json!({"error":"Signing secret unavailable."}));
    };
    let Ok(auth) = header(&src.auth_header) else {
        return (400, json!({"error":"Repeated authentication header."}));
    };
    if secret.is_empty() || !authentic(src, &secret, bytes, auth.unwrap_or("")) {
        return (401, json!({"error":"Invalid webhook signature or token."}));
    }
    let id = if src.id_header.is_empty() {
        None
    } else {
        match header(&src.id_header) {
            Ok(Some(id))
                if id.len() <= 128
                    && id
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"-_.:".contains(&c)) =>
            {
                Some(id.to_owned())
            }
            _ => return (400, json!({"error":"Missing or invalid delivery ID."})),
        }
    };
    if let Some(row) = id
        .as_ref()
        .and_then(|id| book.deliveries.iter().find(|row| &row.id == id))
    {
        return (200, receipt(row));
    }
    if !t.enabled {
        return (409, json!({"error":"Trigger paused."}));
    }
    if !src.event_header.is_empty() && header(&src.event_header) != Ok(Some(src.event.as_str())) {
        return (422, json!({"error":"Event does not match trigger."}));
    }
    let Ok(payload) = serde_json::from_slice::<Value>(bytes) else {
        return (400, json!({"error":"Invalid JSON."}));
    };
    let mut inputs = t.inputs.clone();
    for (name, pointer) in &t.mapping {
        let Some(v) = payload.pointer(pointer) else {
            return (
                422,
                json!({"error":format!("Missing mapped input {name}.")}),
            );
        };
        if topology::validate_value(&t.types[name], v).is_err() {
            return (422, json!({"error":format!("Incorrect type for {name}.")}));
        }
        inputs.insert(name.clone(), v.clone());
    }
    if book
        .deliveries
        .iter()
        .filter(|d| d.request.is_some())
        .count()
        >= 128
    {
        return (429, json!({"error":"Delivery queue full. Retry later."}));
    }
    // Use typed input validated above; bare strings must keep their exact bytes at the CLI boundary.
    let raw: Vec<String> = inputs
        .iter()
        .map(|(name, v)| {
            format!(
                "{name}={}",
                v.as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| v.to_string())
            )
        })
        .collect();
    let request = json!({"program":t.program,"filename":t.filename,"files":t.files,"raw_inputs":raw,"conversation_id":context.conversation_id});
    let row = Delivery {
        id: id.unwrap_or_else(|| Uuid::new_v4().to_string()),
        run_id: Uuid::new_v4().to_string(),
        revision: t.revision,
        team: t.team.clone(),
        received_at: now_unix(),
        status: "queued".into(),
        request: Some(request),
        error: None,
    };
    let response = receipt(&row);
    book.deliveries.push(row);
    match topology::write_json_atomic(&path(context), &book) {
        Ok(()) => {
            sync(context, &book);
            (202, response)
        }
        Err(_) => (500, json!({"error":"Delivery could not be saved."})),
    }
}

// One dispatcher per daemon. Admission and runner retries share the persisted
// run ID. A provider retry cannot create another run after a process restart.
pub(super) fn dispatch(context: &Arc<Context_>) -> Result<()> {
    if !context.webhook_queued.load(Ordering::Relaxed) {
        return Ok(());
    }
    let next = {
        let _lock = context
            .webhook_operation
            .lock()
            .expect("webhook lock poisoned");
        let book = load(context)?;
        book.deliveries
            .into_iter()
            .find(|row| row.request.is_some())
    };
    let Some(row) = next else {
        return Ok(());
    };
    if !run_record_path(context, &row.run_id).exists()
        && find_active_run(&context.runs.lock().expect("runs poisoned"), &row.team).is_some()
    {
        return Ok(());
    }
    let request = serde_json::to_vec(row.request.as_ref().unwrap())?;
    let (code, outcome) = start_run_id(context, &request, Some(&row.run_id));
    if code == 409 && outcome.get("run_id").is_some() {
        return Ok(());
    }
    let _lock = context
        .webhook_operation
        .lock()
        .expect("webhook lock poisoned");
    let mut book = load(context)?;
    if let Some(record) = book.deliveries.iter_mut().find(|r| r.run_id == row.run_id) {
        if code == 201 || run_record_path(context, &row.run_id).is_file() {
            record.status = "started".into();
        } else {
            record.status = "failed".into();
            record.error = Some(format!("Run admission returned HTTP {code}."));
        }
        record.request = None;
    }
    prune(&mut book);
    topology::write_json_atomic(&path(context), &book)?;
    sync(context, &book);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: usize, queued: bool) -> Delivery {
        Delivery {
            id: id.to_string(),
            run_id: id.to_string(),
            revision: 1,
            team: "T".into(),
            received_at: id as u64,
            status: if queued { "queued" } else { "started" }.into(),
            request: queued.then(|| json!({})),
            error: None,
        }
    }

    fn source(auth: &str) -> Source {
        Source {
            auth: auth.into(),
            auth_header: "x-sig".into(),
            event_header: String::new(),
            event: String::new(),
            id_header: String::new(),
        }
    }

    #[test]
    fn any_sender_authenticates_by_hex_or_base64_signature_or_token() {
        let body = br#"{"ref":"x"}"#;
        let mut mac = Hmac::<Sha256>::new_from_slice(b"k").unwrap();
        mac.update(body);
        let tag = mac.finalize().into_bytes();
        let hex: String = tag.iter().map(|b| format!("{b:02x}")).collect();
        let b64 = base64::engine::general_purpose::STANDARD.encode(tag);
        let hmac = source("hmac-sha256");
        assert!(authentic(&hmac, b"k", body, &format!("sha256={hex}")));
        assert!(authentic(&hmac, b"k", body, &hex.to_uppercase()));
        assert!(authentic(&hmac, b"k", body, &b64));
        assert!(!authentic(&hmac, b"other", body, &hex));
        assert!(!authentic(&hmac, b"k", b"tampered", &b64));
        assert!(!authentic(&hmac, b"k", body, ""));
        let token = source("token");
        assert!(authentic(&token, b"s3cret", body, "s3cret"));
        assert!(authentic(&token, b"s3cret", body, "Bearer s3cret"));
        assert!(!authentic(&token, b"s3cret", body, "s3cre"));
        assert!(check_source(source("basic")).is_err());
        let mut github = source("hmac-sha256");
        github.auth_header = "X-Hub-Signature-256".into();
        github.event_header = "X-GitHub-Event".into();
        github.event = "push".into();
        let github = check_source(github).unwrap();
        assert_eq!(github.auth_header, "x-hub-signature-256");
        let mut bare = source("token");
        bare.event = "ignored".into();
        assert_eq!(check_source(bare).unwrap().event, "");
    }

    #[test]
    fn prune_keeps_queued_rows_and_newest_finished_receipts() {
        let mut book = Inbox::default();
        book.deliveries.push(row(0, true));
        book.deliveries
            .extend((1..=KEEP_FINISHED + 5).map(|i| row(i, false)));
        book.deliveries.push(row(KEEP_FINISHED + 6, true));
        prune(&mut book);
        assert_eq!(book.deliveries.len(), KEEP_FINISHED + 2);
        assert_eq!(book.deliveries[0].id, "0");
        assert_eq!(book.deliveries[1].id, "6");
        assert!(book.deliveries.last().unwrap().request.is_some());
        prune(&mut book);
        assert_eq!(book.deliveries.len(), KEEP_FINISHED + 2);
    }
}

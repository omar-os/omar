//! A saved trigger owns a pinned draft and a durable inbox. HTTP admission
//! and dispatch never depend on whichever workflow a Studio tab has selected.
use super::*;
use anyhow::{ensure, Context};
use hmac::{Hmac, Mac};
use sha2::Sha256;

#[derive(Clone, Serialize, Deserialize)]
struct Trigger {
    id: String,
    revision: u64,
    event: String,
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
    json!({"trigger":book.trigger.as_ref().map(|t| json!({"id":t.id,"event":t.event,"secret_name":t.secret_name,"mapping":t.mapping,"enabled":t.enabled,"revision":t.revision})),
        "deliveries":book.deliveries.iter().rev().take(100).map(receipt).collect::<Vec<_>>()})
}

pub(super) fn armed(context: &Context_) -> bool {
    context.webhook_active.load(Ordering::Relaxed)
}

pub(super) fn resume(context: &Context_) {
    if let Ok(book) = load(context) {
        context.webhook_active.store(
            book.trigger.is_some_and(|t| t.enabled)
                || book.deliveries.iter().any(|r| r.request.is_some()),
            Ordering::Relaxed,
        );
    }
}

#[derive(Deserialize)]
struct Activation {
    revision: u64,
    event: String,
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
            ensure!(
                !a.event.is_empty()
                    && a.event.len() <= 80
                    && a.event.bytes().all(|c| c.is_ascii_lowercase() || c == b'_'),
                "Choose a GitHub event name."
            );
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
                event: a.event,
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
    context.webhook_active.store(
        book.trigger.as_ref().is_some_and(|t| t.enabled)
            || book.deliveries.iter().any(|d| d.request.is_some()),
        Ordering::Relaxed,
    );
    (200, view(&book))
}

fn signed(secret: &[u8], bytes: &[u8], signature: &str) -> bool {
    let Some(hex) = signature.strip_prefix("sha256=") else {
        return false;
    };
    if hex.len() != 64 || !hex.is_ascii() {
        return false;
    }
    let mut tag = [0; 32];
    for (i, byte) in tag.iter_mut().enumerate() {
        let Ok(v) = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16) else {
            return false;
        };
        *byte = v;
    }
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret) else {
        return false;
    };
    mac.update(bytes);
    mac.verify_slice(&tag).is_ok()
}

pub(super) fn accept(
    context: &Context_,
    trigger: &str,
    headers: &BTreeMap<String, String>,
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
    let header = |key: &str| headers.get(key).map(String::as_str).unwrap_or("");
    let Ok(secret) = signing_key(&t.secret_name) else {
        return (503, json!({"error":"Signing secret unavailable."}));
    };
    if secret.is_empty() || !signed(&secret, bytes, header("x-hub-signature-256")) {
        return (401, json!({"error":"Invalid webhook signature."}));
    }
    let id = header("x-github-delivery");
    if id.is_empty() || id.len() > 96 || !id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
    {
        return (400, json!({"error":"Invalid delivery ID."}));
    }
    if let Some(row) = book.deliveries.iter().find(|row| row.id == id) {
        return (200, receipt(row));
    }
    if !t.enabled {
        return (409, json!({"error":"Trigger paused."}));
    }
    if header("x-github-event") != t.event {
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
        id: id.into(),
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
        Ok(()) => (202, response),
        Err(_) => (500, json!({"error":"Delivery could not be saved."})),
    }
}

// One dispatcher per daemon. Admission and runner retries share the persisted
// run ID. A provider retry cannot create another run after a process restart.
pub(super) fn dispatch(context: &Arc<Context_>) -> Result<()> {
    if !armed(context) {
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
    topology::write_json_atomic(&path(context), &book)?;
    context.webhook_active.store(
        book.trigger.as_ref().is_some_and(|t| t.enabled)
            || book.deliveries.iter().any(|d| d.request.is_some()),
        Ordering::Relaxed,
    );
    Ok(())
}

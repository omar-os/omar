//! The operator's saved connection settings are shared by chats in this runtime.
//! Credential fields describe an environment binding; the catalog never looks it up.
use anyhow::{anyhow, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::Path,
};

pub(super) const ROUTE: &str = "/v1/task-integrations";

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
enum Auth {
    None,
    Bearer { variable: String },
    ApiKey { variable: String, header: String },
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
enum Connection {
    Http {
        id: String,
        name: String,
        url: String,
        method: String,
        auth: Auth,
    },
    Webhook {
        id: String,
        name: String,
        url: String,
        method: String,
        auth: Auth,
    },
    Mcp {
        id: String,
        name: String,
        server: String,
    },
}

impl Connection {
    fn id(&self) -> &str {
        match self {
            Self::Http { id, .. } | Self::Webhook { id, .. } | Self::Mcp { id, .. } => id,
        }
    }

    fn summary(&self) -> (&str, &str) {
        match self {
            Self::Http { name, .. } => (name, "http"),
            Self::Webhook { name, .. } => (name, "webhook"),
            Self::Mcp { name, .. } => (name, "mcp"),
        }
    }

    fn valid(&self) -> bool {
        let identifier = |s: &str| {
            !s.is_empty()
                && s.len() <= 256
                && s.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
        };
        let (name, target_valid) = match self {
            Self::Mcp { name, server, .. } => (name, identifier(server)),
            Self::Http {
                name,
                url,
                method,
                auth,
                ..
            }
            | Self::Webhook {
                name,
                url,
                method,
                auth,
                ..
            } => {
                let valid_url = url.len() <= 4096
                    && !url.contains(['#', '\\'])
                    && url.parse::<tungstenite::http::Uri>().is_ok_and(|uri| {
                        matches!(uri.scheme_str(), Some("https" | "http"))
                            && uri
                                .authority()
                                .is_some_and(|a| !a.host().is_empty() && !a.as_str().contains('@'))
                    });
                let valid_method = match self {
                    Self::Webhook { .. } => method == "POST",
                    _ => matches!(
                        method.as_str(),
                        "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD"
                    ),
                };
                let valid_auth = match auth {
                    Auth::None => true,
                    Auth::Bearer { variable } | Auth::ApiKey { variable, .. } => {
                        identifier(variable)
                            && !variable.contains('-')
                            && !variable.as_bytes()[0].is_ascii_digit()
                    }
                };
                let valid_header = match auth {
                    Auth::ApiKey { header, .. } => {
                        header.len() <= 256
                            && tungstenite::http::HeaderName::from_bytes(header.as_bytes()).is_ok()
                    }
                    _ => true,
                };
                (
                    name,
                    valid_url && valid_method && valid_auth && valid_header && !url.contains("$("),
                )
            }
        };
        identifier(self.id())
            && !name.trim().is_empty()
            && name.chars().count() <= 80
            && !name.contains("$(")
            && target_valid
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Change {
    before: Option<Connection>,
    connection: Connection,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Batch {
    changes: Vec<Change>,
    #[serde(default)]
    import_only: bool,
}

fn load(dir: &Path) -> Result<BTreeMap<String, Connection>> {
    match fs::read(dir.join("task-integrations.json")) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(e.into()),
    }
}

/// Discovery shares only the names, kinds and stable IDs needed to design a task.
pub(super) fn search(dir: &Path, body: &[u8]) -> (u16, Value) {
    #[derive(Deserialize)]
    struct Search {
        query: String,
    }
    let Ok(Search { query }) = serde_json::from_slice(body) else {
        return (
            400,
            json!({"error":"Supply an integration name or an empty search."}),
        );
    };
    let Ok(entries) = load(dir) else {
        return (
            500,
            json!({"error":"Saved integrations could not be read."}),
        );
    };
    let query = query.trim().to_lowercase();
    let mut matches = entries.values().filter_map(|entry| {
        let (name, kind) = entry.summary();
        (name.to_lowercase().contains(&query)
            || entry.id().eq_ignore_ascii_case(&query)
            || kind == query)
            .then(|| json!({"id":entry.id(), "name":name, "type":kind}))
    });
    let integrations: Vec<_> = matches.by_ref().take(50).collect();
    (
        200,
        json!({"integrations":integrations, "more":matches.next().is_some()}),
    )
}

const MARKER: &str = "[[integrations:";
pub(super) type Bindings = BTreeMap<String, Vec<String>>;

/// Expand only a unique marker at the end of a real, compiled agent task.
/// Ordinary task text and string interpolation stay in the original source.
pub(super) fn compile_design(
    dir: &Path,
    source: String,
    files: &BTreeMap<String, String>,
    bindings: &Bindings,
) -> Result<(String, crate::topology::VmState)> {
    let state = super::compile_preview(&source, files)?;
    let mut slots = BTreeSet::new();
    for task in state.reactions.values() {
        // Previously bound tasks already carry their reviewed snapshot. A
        // display name or URL inside it is never a new binding instruction.
        let prompt = task
            .prompt
            .split("\n\n<omar-connections")
            .next()
            .unwrap_or_default()
            .trim_end();
        if let Some((_, suffix)) = prompt.split_once(MARKER) {
            let slot = suffix
                .strip_suffix("]]")
                .context("Place the integration marker at the end of the task prompt.")?;
            slots.insert(slot);
        }
    }
    if slots.is_empty() && bindings.is_empty() {
        return Ok((source, state));
    }
    ensure!(
        slots
            .iter()
            .copied()
            .eq(bindings.keys().map(String::as_str)),
        "Each task integration marker needs a matching task_integrations binding."
    );
    ensure!(
        bindings.len() <= 128,
        "Too many integration bindings in one design."
    );
    let entries = load(dir).context("Could not read saved integrations")?;
    let guidance: &str =
        serde_json::from_str(include_str!("../../web/app/lib/connection-guidance.json"))?;
    let mut edits = Vec::new();
    let mut size = source.len();
    for (slot, ids) in bindings {
        ensure!(
            !slot.is_empty()
                && slot.len() <= 80
                && slot.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'),
            "Use a short alphanumeric integration slot name."
        );
        let marker = format!("{MARKER}{slot}]]");
        let mut positions = source.match_indices(&marker);
        let (start, _) = positions
            .next()
            .ok_or_else(|| anyhow!("Missing task marker {marker}"))?;
        let tail = &source[start + marker.len()..];
        let after = tail.trim_start();
        ensure!(
            positions.next().is_none() && after.starts_with('"'),
            "Place {marker} exactly once at the end of a task prompt."
        );
        let length = marker.len() + tail.len() - after.len();
        ensure!(
            !ids.is_empty(),
            "Choose at least one saved integration for {slot}."
        );
        let mut names = BTreeSet::new();
        let mut selected = Vec::new();
        for id in ids {
            let entry = entries.get(id).ok_or_else(|| {
                anyhow!("Unknown saved integration ID '{id}'. Search Settings again.")
            })?;
            ensure!(
                entry.valid() && names.insert(entry.summary().0.trim().to_lowercase()),
                "Invalid or duplicate integration for task {slot}."
            );
            selected.push(entry);
        }
        let metadata = json!({"connections":selected, "instructions":guidance});
        let block = format!(
            "\n\n<omar-connections version=\"1\">\n{}\n</omar-connections>",
            serde_json::to_string_pretty(&metadata)?
        );
        let escaped = block.replace('\\', r"\\").replace('"', r#"\""#);
        size = size - length + escaped.len();
        ensure!(
            size <= super::MAX_BODY_BYTES,
            "Design with saved integrations exceeds the program size limit."
        );
        edits.push((start, length, escaped));
    }
    edits.sort_unstable_by_key(|edit| std::cmp::Reverse(edit.0));
    let mut result = source;
    for (start, len, replacement) in edits {
        result.replace_range(start..start + len, &replacement);
    }
    let state = super::compile_preview(&result, files)?;
    Ok((result, state))
}

pub(super) fn request(dir: &Path, method: &str, raw: &[u8]) -> (u16, Value) {
    let Ok(mut entries) = load(dir) else {
        return (
            500,
            json!({"error":"Saved integrations could not be read."}),
        );
    };
    if method == "POST" {
        let Ok(batch) = serde_json::from_slice::<Batch>(raw) else {
            return (
                400,
                json!({"error":"Invalid integration settings. Use only credential variable names."}),
            );
        };
        let mut dirty = false;
        for change in batch.changes {
            let next = change.connection;
            let id = next.id();
            if !next.valid() || change.before.as_ref().is_some_and(|old| old.id() != id) {
                return (
                    400,
                    json!({"error":"Invalid integration name, endpoint, method, or credential reference."}),
                );
            }
            let existing = entries.get(id);
            if existing == Some(&next) || (batch.import_only && existing.is_some()) {
                continue;
            }
            if existing.is_some() && existing != change.before.as_ref() {
                return (
                    409,
                    json!({"error":"This saved integration changed. Reopen the picker and select its current settings."}),
                );
            }
            entries.insert(id.to_owned(), next);
            dirty = true;
        }
        if dirty {
            let bytes = serde_json::to_vec(&entries).expect("serializable settings");
            if bytes.len() > 4 * 1024 * 1024 {
                return (
                    413,
                    json!({"error":"The saved integrations catalog is full."}),
                );
            }
            if crate::manager::write_private_file(&dir.join("task-integrations.json"), &bytes)
                .is_err()
            {
                return (
                    500,
                    json!({"error":"Could not save integrations in this runtime."}),
                );
            }
        }
    } else if method != "GET" {
        return (405, json!({"error":"Unsupported integration operation."}));
    }
    (
        200,
        json!({"connections": entries.values().collect::<Vec<_>>()}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_survives_restart_and_rejects_stale_or_secret_bearing_batches() {
        let temp = tempfile::tempdir().unwrap();
        let call = |request: Value| {
            super::request(temp.path(), "POST", &serde_json::to_vec(&request).unwrap()).0
        };
        let first = json!({"id":"api","name":"Content","type":"http","url":"https://example.com/items","method":"GET","auth":{"type":"bearer","variable":"CONTENT_TOKEN"}});
        assert_eq!(call(json!({"changes":[{"connection":first}]})), 200);
        let mut edited = first.clone();
        edited["name"] = json!("Editorial");
        assert_eq!(
            call(json!({"changes":[{"before":first,"connection":edited}]})),
            200
        );
        assert_eq!(
            call(json!({"changes":[{"before":first,"connection":edited}]})),
            200
        );
        let second = json!({"id":"tools","name":"Tools","type":"mcp","server":"github"});
        assert_eq!(
            call(json!({"changes":[{"connection":second},{"connection":first}]})),
            409
        );
        assert_eq!(
            call(json!({"importOnly":true,"changes":[{"connection":first}]})),
            200
        );
        let mut secret = first.clone();
        secret["auth"]["value"] = json!("should never be persisted");
        assert_eq!(call(json!({"changes":[{"connection":secret}]})), 400);
        assert_eq!(
            request(temp.path(), "GET", &[]).1,
            json!({"connections":[edited]})
        );
    }
}

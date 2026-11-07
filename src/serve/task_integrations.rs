//! The operator's saved connection settings are shared by chats in this runtime.
//! Credential fields describe an environment binding; the catalog never looks it up.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::BTreeMap, fs, io, path::Path};

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

pub(super) fn request(dir: &Path, method: &str, raw: &[u8]) -> (u16, Value) {
    let path = dir.join("task-integrations.json");
    let mut entries: BTreeMap<String, Connection> = match fs::read(&path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(entries) => entries,
            Err(_) => {
                return (
                    500,
                    json!({"error":"Saved integrations could not be read."}),
                )
            }
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => BTreeMap::new(),
        Err(_) => {
            return (
                500,
                json!({"error":"Saved integrations could not be read."}),
            )
        }
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
            if crate::manager::write_private_file(&path, &bytes).is_err() {
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

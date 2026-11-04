//! Reviewed local workflow packages. Catalog data is not executable code.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

pub mod calculations;
pub mod program;
pub mod repository;
pub mod result;
pub mod runtime;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub id: String,
    pub title: String,
    pub text: String,
    #[serde(default)]
    pub valid_until: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub goal: String,
    pub sources: Vec<Source>,
    #[serde(default)]
    pub data: BTreeMap<String, Value>,
}

pub fn catalog() -> Result<Value> {
    let mut catalog: Value = serde_json::from_str(include_str!("../../templates/catalog.json"))?;
    ensure!(
        catalog["schema_version"] == 1,
        "unsupported template catalog schema"
    );
    let mut ids = BTreeSet::new();
    for template in catalog["templates"]
        .as_array_mut()
        .context("missing catalog")?
    {
        let id = template["id"].as_str().context("template ID missing")?;
        ensure!(
            identifier(id) && ids.insert(id.to_owned()),
            "invalid or duplicate template ID"
        );
        ensure!(
            matches!(template["status"].as_str(), Some("preview" | "runnable")),
            "unknown template readiness"
        );
        for key in ["title", "version", "publisher", "job", "adapter"] {
            ensure!(
                template[key].as_str().is_some_and(|s| !s.trim().is_empty()),
                "template {id} is missing {key}"
            );
        }
        if template["status"] == "runnable" {
            for key in [
                "operation",
                "complexity",
                "consequence",
                "scope_reason",
                "draft_instructions",
                "review_instructions",
            ] {
                ensure!(
                    template["design"][key]
                        .as_str()
                        .is_some_and(|s| !s.trim().is_empty()),
                    "runnable template {id} is missing its design {key}"
                );
            }
        }
        let fields = template["fields"]
            .as_array()
            .context("template fields missing")?;
        let mut keys = BTreeSet::new();
        for field in fields {
            let key = field["key"].as_str().context("field key missing")?;
            ensure!(
                identifier(key) && keys.insert(key),
                "invalid or duplicate template field"
            );
        }
        let digest = crate::template_library::digest(&serde_json::to_vec(template)?);
        template["digest"] = json!(digest);
    }
    Ok(catalog)
}

pub fn package(id: &str, digest: Option<&str>) -> Result<Value> {
    let package = catalog()?["templates"]
        .as_array()
        .context("missing catalog")?
        .iter()
        .find(|t| t["id"] == id)
        .cloned()
        .context("unknown template")?;
    if let Some(digest) = digest {
        ensure!(
            package["digest"] == digest,
            "template changed; reload and review its current version"
        );
    }
    Ok(package)
}

pub fn identifier(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 80
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
}

/// Parsing and exact calculations occur before a chat, plan or agent exists.
pub fn prepare(id: &str, digest: Option<&str>, input: &Input) -> Result<Value> {
    let package = package(id, digest)?;
    ensure!(
        package["status"] == "runnable",
        "this template is a preview; its real-input adapter is not yet available"
    );
    ensure!(
        !input.goal.trim().is_empty() && input.goal.len() <= 4000,
        "provide a goal of 1–4000 bytes"
    );
    ensure!(
        !input.sources.is_empty() && input.sources.len() <= 50,
        "provide 1–50 source documents"
    );
    ensure!(
        serde_json::to_vec(input)?.len() <= 100_000,
        "inputs exceed the 100 KB package limit"
    );
    let mut ids = BTreeSet::new();
    for source in &input.sources {
        ensure!(
            identifier(&source.id) && ids.insert(&source.id),
            "invalid or duplicate source ID"
        );
        ensure!(
            !source.title.trim().is_empty()
                && source.title.len() <= 200
                && !source.text.trim().is_empty(),
            "each source needs a title and text"
        );
        if let Some(date) = &source.valid_until {
            chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
                .context("source expiry must be YYYY-MM-DD")?;
        }
    }
    for field in package["fields"]
        .as_array()
        .context("missing field definitions")?
    {
        let key = field["key"].as_str().context("field key missing")?;
        ensure!(
            input.data.contains_key(key),
            "missing required input: {key}"
        );
    }
    let allowed: BTreeSet<_> = package["fields"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|f| f["key"].as_str())
        .collect();
    ensure!(
        input.data.keys().all(|k| allowed.contains(k.as_str())),
        "input contains an unknown field"
    );
    let calculated = calculations::calculate(package["adapter"].as_str().unwrap_or(""), input)?;
    Ok(
        json!({"template_id":id,"version":package["version"],"digest":package["digest"],"input":input,"calculated":calculated}),
    )
}

pub fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests;

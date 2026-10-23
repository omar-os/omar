use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::Path};

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub runtime_url: String,
    #[serde(default = "slack")]
    pub platform: String,
    #[serde(alias = "workspaceId")]
    pub account_id: String,
    pub allowed_users: Vec<String>,
    #[serde(alias = "allowedChannels")]
    pub allowed_conversations: Vec<String>,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub output_port: String,
    #[serde(default = "timeout")]
    pub timeout_seconds: u64,
    #[serde(default)]
    program_path: String,
    #[serde(default)]
    files: BTreeMap<String, String>,
    #[serde(default)]
    inputs: BTreeMap<String, Value>,
    #[serde(default)]
    raw_inputs: Vec<String>,
    #[serde(skip)]
    pub request: Value,
    #[serde(default)]
    pub workflows: BTreeMap<String, Workflow>,
}
fn slack() -> String {
    "slack".into()
}
fn timeout() -> u64 {
    60
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let mut config: Self =
            serde_json::from_slice(&std::fs::read(path).context("Read messaging configuration")?)
                .context("Invalid messaging configuration")?;
        config.validate()?;
        let folder = path.parent().unwrap_or(Path::new("."));
        if config.workflows.is_empty() {
            config.request = load_request(
                folder,
                &config.program_path,
                &config.files,
                &config.inputs,
                &config.raw_inputs,
                config.timeout_seconds,
            )?;
        } else {
            for workflow in config.workflows.values_mut() {
                workflow.request = load_request(
                    folder,
                    &workflow.program_path,
                    &workflow.files,
                    &workflow.inputs,
                    &workflow.raw_inputs,
                    workflow.timeout_seconds,
                )?;
            }
        }
        Ok(config)
    }

    pub fn validate(&mut self) -> Result<()> {
        let url = reqwest::Url::parse(&self.runtime_url).context("Invalid runtime URL")?;
        let chat = url
            .path()
            .strip_prefix("/chats/")
            .unwrap_or("")
            .trim_end_matches('/');
        ensure!(
            url.scheme() == "http"
                && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
                && !chat.is_empty()
                && chat.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                && (url.path() == format!("/chats/{chat}")
                    || url.path() == format!("/chats/{chat}/")),
            "runtimeUrl must be loopback HTTP scoped to /chats/<id>"
        );
        ensure!(
            !self.platform.is_empty() && !self.account_id.is_empty(),
            "Missing messaging identity"
        );
        if self.workflows.is_empty() {
            validate_workflow(
                &self.command,
                &self.program_path,
                &self.output_port,
                self.timeout_seconds,
                &self.raw_inputs,
            )?;
        } else {
            ensure!(
                self.command.is_empty()
                    && self.program_path.is_empty()
                    && self.output_port.is_empty()
                    && self.files.is_empty()
                    && self.inputs.is_empty()
                    && self.raw_inputs.is_empty(),
                "Use either workflows or legacy workflow fields, not both"
            );
            for (name, workflow) in &self.workflows {
                validate_workflow(
                    name,
                    &workflow.program_path,
                    &workflow.output_port,
                    workflow.timeout_seconds,
                    &workflow.raw_inputs,
                )?;
                ensure!(
                    !workflow.description.contains(['\n', '\r']),
                    "Workflow description must be one line"
                );
            }
        }
        ensure!(
            [&self.allowed_users, &self.allowed_conversations]
                .iter()
                .all(|ids| !ids.is_empty() && ids.iter().all(|id| !id.is_empty())),
            "Allowlists must contain IDs"
        );
        self.runtime_url = url.as_str().trim_end_matches('/').to_string();
        Ok(())
    }
    pub fn select(&self, name: &str) -> Option<Selected<'_>> {
        if self.workflows.is_empty() {
            (name == self.command).then_some(Selected {
                request: &self.request,
                output_port: &self.output_port,
                timeout_seconds: self.timeout_seconds,
            })
        } else {
            self.workflows.get(name).map(|workflow| Selected {
                request: &workflow.request,
                output_port: &workflow.output_port,
                timeout_seconds: workflow.timeout_seconds,
            })
        }
    }
    pub fn listing(&self) -> String {
        let mut result = String::from("Available workflows:");
        if self.workflows.is_empty() {
            result.push_str(&format!("\n- {}", self.command));
        } else {
            for (name, workflow) in &self.workflows {
                result.push_str(&format!("\n- {name}"));
                if !workflow.description.is_empty() {
                    result.push_str(&format!(" — {}", workflow.description));
                }
            }
        }
        result
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Workflow {
    #[serde(default)]
    description: String,
    program_path: String,
    output_port: String,
    #[serde(default = "timeout")]
    timeout_seconds: u64,
    #[serde(default)]
    files: BTreeMap<String, String>,
    #[serde(default)]
    inputs: BTreeMap<String, Value>,
    #[serde(default)]
    raw_inputs: Vec<String>,
    #[serde(skip)]
    pub request: Value,
}

pub struct Selected<'a> {
    pub request: &'a Value,
    pub output_port: &'a str,
    pub timeout_seconds: u64,
}
fn validate_workflow(
    name: &str,
    program: &str,
    output: &str,
    timeout: u64,
    raw: &[String],
) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
        "Invalid workflow name"
    );
    ensure!(
        !program.is_empty() && !output.is_empty(),
        "Missing workflow source/output"
    );
    ensure!((1..=3600).contains(&timeout), "Invalid timeoutSeconds");
    ensure!(
        raw.iter().all(|s| s.contains('=')),
        "rawInputs must contain NAME=VALUE strings"
    );
    Ok(())
}
fn load_request(
    folder: &Path,
    program: &str,
    imports: &BTreeMap<String, String>,
    inputs: &BTreeMap<String, Value>,
    raw: &[String],
    timeout: u64,
) -> Result<Value> {
    let source = folder.join(program);
    let mut files = BTreeMap::new();
    for (name, disk) in imports {
        files.insert(
            name,
            std::fs::read_to_string(folder.join(disk)).context("Read workflow import")?,
        );
    }
    Ok(
        json!({"program":std::fs::read_to_string(&source).context("Read workflow source")?,
        "filename":source.file_name().and_then(|s|s.to_str()).context("Invalid workflow filename")?,
        "files":files,"inputs":inputs,"raw_inputs":raw,"timeout_seconds":timeout,"replace":false}),
    )
}

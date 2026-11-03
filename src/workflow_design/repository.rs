//! Trusted, snapshot-bound executable checks before scheduler delivery.
//! The operator installs suites locally; models can only refer to summaries.
use super::*;
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Suite {
    pub schema_version: u32,
    pub id: String,
    pub description: String,
    pub reviewed_by: String,
    pub reviewed_at: u64,
    pub adapter: String,
    pub image: String,
    pub entrypoint: String,
    pub files: BTreeMap<String, String>,
    pub allowed_patch_paths: Vec<String>,
    pub timeout_ms: u64,
    pub protected_tests: Vec<Regression>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Regression {
    pub id: String,
    pub request: Value,
    pub expected: Value,
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 80
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn hex_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn safe_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 240
        && !value.contains(['\\', '\0', '\n', '\r'])
        && value
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != ".." && p != ".git")
}

impl Suite {
    pub fn digest(&self) -> Result<String> {
        // Canonical serde Value sorts object keys, including nested expectations.
        Ok(super::digest(&serde_json::to_vec(&serde_json::to_value(
            self,
        )?)?))
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == 1 && identifier(&self.id),
            "invalid repository verifier version or ID"
        );
        ensure!(
            !self.description.trim().is_empty()
                && self.description.len() <= 2000
                && !self.reviewed_by.trim().is_empty()
                && self.reviewed_by.len() <= 200
                && self.reviewed_at > 0
                && self.reviewed_at <= super::now(),
            "repository verifier needs dated operator review"
        );
        ensure!(
            self.adapter == "python_json_api_v1",
            "unsupported repository verifier adapter"
        );
        ensure!(
            self.image.strip_prefix("sha256:").is_some_and(hex_digest),
            "repository verifier needs an immutable local image ID"
        );
        let parts: Vec<_> = self.entrypoint.split(':').collect();
        let python_id = |s: &str| {
            !s.is_empty()
                && s.len() <= 100
                && s.bytes().enumerate().all(|(i, b)| {
                    b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit())
                })
        };
        ensure!(
            parts.len() == 2 && parts[0].split('.').all(python_id) && python_id(parts[1]),
            "invalid Python API entrypoint"
        );
        ensure!(
            !self.files.is_empty()
                && self.files.len() <= 64
                && self
                    .files
                    .iter()
                    .all(|(p, c)| safe_path(p) && !c.contains('\0') && c.len() <= 65536)
                && self.files.values().map(String::len).sum::<usize>() <= 262144,
            "repository snapshot exceeds supported file bounds"
        );
        for path in self.files.keys() {
            ensure!(
                !self
                    .files
                    .keys()
                    .any(|other| other.starts_with(&format!("{path}/"))),
                "repository file/directory conflict"
            );
        }
        let paths: BTreeSet<_> = self.allowed_patch_paths.iter().collect();
        ensure!(
            !paths.is_empty()
                && paths.len() == self.allowed_patch_paths.len()
                && paths.iter().all(|p| self.files.contains_key(*p)),
            "invalid repository patch allowlist"
        );
        ensure!(
            self.timeout_ms > 0 && self.timeout_ms <= 60000,
            "repository verifier timeout must be 1–60000 ms"
        );
        let ids: BTreeSet<_> = self.protected_tests.iter().map(|t| &t.id).collect();
        ensure!(
            !ids.is_empty()
                && ids.len() <= 100
                && ids.len() == self.protected_tests.len()
                && ids.iter().all(|id| identifier(id)),
            "repository verifier needs 1–100 distinct regression cases"
        );
        ensure!(
            serde_json::to_vec(&self.protected_tests)?.len() <= 262144,
            "repository verifier test payload exceeds 256 KiB"
        );
        Ok(())
    }

    pub fn summary(&self) -> Result<Value> {
        Ok(
            json!({"id":self.id,"digest":self.digest()?,"description":self.description,
            "adapter":self.adapter,"image":self.image,"entrypoint":self.entrypoint,
            "snapshot_digest":super::digest(&serde_json::to_vec(&self.files)?),
            "allowed_patch_paths":self.allowed_patch_paths,"test_count":self.protected_tests.len(),
            "timeout_ms":self.timeout_ms,"reviewed_by":self.reviewed_by,"reviewed_at":self.reviewed_at,
            "meaning":"Operator-owned regression cases; passing is not independent semantic acceptance."}),
        )
    }
}

pub fn load(omar_dir: &Path, id: &str) -> Result<Suite> {
    ensure!(identifier(id), "invalid repository verifier ID");
    let path = omar_dir
        .join("workflow-verifiers")
        .join(format!("{id}.json"));
    ensure!(
        fs::metadata(&path)?.len() <= 1_048_576,
        "repository verifier record exceeds 1 MiB"
    );
    let suite: Suite = serde_json::from_slice(&fs::read(path)?)?;
    suite.validate()?;
    ensure!(
        suite.id == id,
        "repository verifier filename does not match ID"
    );
    Ok(suite)
}

pub fn catalog(omar_dir: &Path) -> Result<Vec<Value>> {
    let path = omar_dir.join("workflow-verifiers");
    if !path.exists() {
        return Ok(vec![]);
    }
    let mut names = Vec::new();
    for file in fs::read_dir(path)? {
        let path = file?.path();
        if path.extension().is_some_and(|e| e == "json") {
            ensure!(
                names.len() < 32,
                "repository verifier catalog exceeds 32 suites"
            );
            names.push(
                path.file_stem()
                    .context("invalid verifier filename")?
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
    names.sort();
    names
        .iter()
        .map(|id| load(omar_dir, id)?.summary())
        .collect()
}

/// Run at proposal/save and again at admission, then stage these exact records.
pub fn resolve(
    omar_dir: &Path,
    plan: &WorkflowPlanRevision,
    state: &crate::topology::VmState,
) -> Result<BTreeMap<String, Suite>> {
    let mut suites = BTreeMap::new();
    for step in &plan.draft.steps {
        let mut checked_ports = BTreeSet::new();
        for check in &step.output_checks {
            if let OutputCheck::RepositoryPatch {
                verifier_id,
                verifier_digest,
                repository_input,
                port,
                ..
            } = check
            {
                ensure!(checked_ports.len() < 4 && checked_ports.insert(port),
                    "a step supports at most four repository checks, with one suite per output port");
                ensure!(
                    hex_digest(verifier_digest),
                    "invalid repository verifier digest"
                );
                let suite = load(omar_dir, verifier_id)?;
                ensure!(
                    suite.digest()? == *verifier_digest,
                    "revision conflict: repository verifier changed; review its current digest"
                );
                let raw = plan
                    .inputs
                    .get(repository_input)
                    .context("repository check must bind a supplied workflow input")?;
                let input: Value = match raw.as_str() {
                    Some(s) => serde_json::from_str(s)?,
                    None => raw.clone(),
                };
                ensure!(input.get("files") == Some(&serde_json::to_value(&suite.files)?)
                    && input.get("allowed_patch_paths") == Some(&json!(suite.allowed_patch_paths))
                    && input.get("entrypoint") == Some(&json!(suite.entrypoint)),
                    "revision conflict: repository input does not match the reviewed verifier snapshot, paths or entrypoint");
                suites.insert(verifier_id.clone(), suite);
            }
        }
    }
    if !suites.is_empty() {
        // A native worker with unrestricted tools could read the private suite.
        // This is a native tool restriction, not an OS security boundary.
        for (id, agent) in &state.agents {
            if !crate::topology::is_web_backend(&agent.backend) {
                ensure!(plan.profiles.get(id).is_some_and(|p| p.isolation == ExecutionIsolation::PortsOnly),
                    "protected repository checks require ports_only profiles for every native workflow agent");
            }
        }
    }
    Ok(suites)
}

fn private_directory(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

pub fn stage(run_dir: &Path, suites: &BTreeMap<String, Suite>) -> Result<()> {
    if suites.is_empty() {
        return Ok(());
    }
    let path = run_dir.join("repository-verifiers");
    private_directory(&path)?;
    for (id, suite) in suites {
        crate::topology::write_json_atomic(&path.join(format!("{id}.json")), suite)?;
    }
    Ok(())
}

pub struct RuntimeContext<'a> {
    pub directory: Option<&'a Path>,
    pub invocation_id: &'a str,
    pub reaction: &'a str,
    pub stop_dir: Option<&'a Path>,
    pub deadline: Instant,
}

fn cancelled(context: &RuntimeContext<'_>) -> bool {
    context.stop_dir.is_some_and(crate::deploy::stop_requested)
}

/// The wrapper is embedded in the binary, never selected by model/path input.
/// Candidate code only runs inside the checked, pinned container.
fn execute(
    suite: &Suite,
    candidate: &Value,
    path: &Path,
    context: &RuntimeContext<'_>,
) -> Result<Value> {
    ensure!(
        !cancelled(context) && Instant::now() < context.deadline,
        "repository verification cancelled or invocation deadline exhausted"
    );
    private_directory(path)?;
    fs::write(
        path.join("repository_sandbox.py"),
        include_str!("../../evals/workflow-quality/repository_sandbox.py"),
    )?;
    fs::write(
        path.join("repository_grade.py"),
        include_str!("../../evals/workflow-quality/repository_grade.py"),
    )?;
    fs::write(path.join("worker.py"), include_str!("repository_worker.py"))?;
    let remaining = context.deadline.saturating_duration_since(Instant::now());
    crate::topology::write_json_atomic(
        &path.join("request.json"),
        &json!({"suite":suite,"candidate":candidate,
        "timeout_ms":suite.timeout_ms.min(remaining.as_millis() as u64)}),
    )?;
    let mut child = Command::new("python3")
        .arg("-I")
        .arg("-B")
        .arg(path.join("worker.py"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("repository verification requires Python 3 and a local Docker daemon")?;
    let cleanup_deadline = Instant::now()
        + remaining.min(Duration::from_millis(suite.timeout_ms))
        + Duration::from_secs(100);
    let mut stopped = false;
    loop {
        if let Some(status) = child.try_wait()? {
            ensure!(
                status.success(),
                "repository verifier infrastructure failed; output was not delivered"
            );
            break;
        }
        if cancelled(context) || Instant::now() >= context.deadline {
            stopped = true;
            // Cooperative cancellation lets the trusted worker remove its container.
            fs::write(path.join("cancel"), b"stop")?;
        }
        if Instant::now() >= cleanup_deadline {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("repository verifier exceeded its cleanup deadline; infrastructure status unknown, inspect local containers");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    ensure!(
        !stopped && !cancelled(context) && Instant::now() < context.deadline,
        "repository verification cancelled or invocation deadline exhausted; output discarded"
    );
    let report = path.join("result.json");
    ensure!(
        fs::metadata(&report)?.len() <= 16384,
        "invalid repository verifier report size"
    );
    Ok(serde_json::from_slice(&fs::read(report)?)?)
}

pub fn check_and_route(
    context: RuntimeContext<'_>,
    checks: &[OutputCheck],
    gate: Option<&recovery::VerificationGate>,
    writes: BTreeMap<String, Value>,
    receipts: &mut Vec<RepositoryCheckReceipt>,
) -> Result<(BTreeMap<String, Value>, Option<VerificationEvent>)> {
    let mut validation = validation::check_outputs(checks, &writes);
    if validation.is_ok() {
        for (index, check) in checks.iter().enumerate() {
            let OutputCheck::RepositoryPatch {
                verifier_id,
                verifier_digest,
                port,
                ..
            } = check
            else {
                continue;
            };
            let Some(candidate) = writes.get(port) else {
                continue;
            };
            ensure!(identifier(verifier_id), "invalid repository verifier ID");
            let run_dir = context
                .directory
                .and_then(Path::parent)
                .context("repository checks require an admitted run receipt directory")?;
            let suite_path = run_dir
                .join("repository-verifiers")
                .join(format!("{verifier_id}.json"));
            ensure!(
                fs::metadata(&suite_path)?.len() <= 1_048_576,
                "invalid staged verifier size"
            );
            let suite: Suite = serde_json::from_slice(
                &fs::read(suite_path).context("missing admitted repository verifier")?,
            )?;
            suite.validate()?;
            ensure!(
                suite.id == *verifier_id && suite.digest()? == *verifier_digest,
                "staged repository verifier digest mismatch"
            );
            let artifact = format!(
                "repository-checks/{}-{index}",
                super::digest(context.invocation_id.as_bytes())
            );
            let path = run_dir.join(&artifact);
            ensure!(
                !path.exists(),
                "repository verification artifact already exists"
            );
            let started = Instant::now();
            let result = execute(&suite, candidate, &path, &context);
            let status = match &result {
                Ok(v) if v["status"] == "passed" => "passed",
                Ok(v) if v["status"] == "failed" => "failed",
                _ if cancelled(&context) => "cancelled",
                _ => "infrastructure_failure",
            };
            receipts.push(RepositoryCheckReceipt {
                invocation_id: context.invocation_id.into(),
                reaction: context.reaction.into(),
                verifier_id: verifier_id.clone(),
                verifier_digest: verifier_digest.clone(),
                image: suite.image.clone(),
                candidate_digest: super::digest(&serde_json::to_vec(candidate)?),
                status: status.into(),
                test_count: suite.protected_tests.len(),
                elapsed_ms: started.elapsed().as_millis() as u64,
                artifact,
            });
            let report = result?;
            ensure!(
                status != "infrastructure_failure",
                "repository verification infrastructure unavailable; output was not delivered"
            );
            if status == "failed" {
                validation = Err(anyhow::anyhow!(
                    "repository regression check failed for verifier '{}': {}",
                    verifier_id,
                    report["failure_kind"]
                        .as_str()
                        .unwrap_or("candidate rejected")
                ));
                break;
            }
        }
    }
    recovery::route_checked(context.reaction, checks, gate, writes, validation)
}

#[cfg(test)]
pub(crate) mod tests;

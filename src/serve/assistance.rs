//! Loopback, chat-scoped advisory endpoints. This module never invokes a run.
use super::*;
use crate::decisions::workflow::*;
use sha2::{Digest, Sha256};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Register {
    id: String,
    profile_id: String,
    confirmed: bool,
    criterion: Option<AdviceCriterion>,
    payload: Payload,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Payload {
    Draft {
        text: String,
    },
    Proposal {
        program: String,
        inputs: BTreeMap<String, Value>,
        scenario_id: String,
        scenario_version: String,
    },
    Artifact {
        workspace_id: String,
        snapshot_id: String,
        path: String,
        start: usize,
        end: usize,
    },
    Run {
        run_id: String,
        source_id: String,
        source_sha256: String,
        start: usize,
        end: usize,
        responsibility_map: ResponsibilityMap,
    },
}

fn digest(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

fn backend() -> Option<&'static str> {
    if !crate::backend_probe::command_succeeds_with_timeout(
        "tmux",
        &["-V"],
        Duration::from_millis(1500),
    ) {
        return None;
    }
    [("codex", "Codex"), ("claude", "ClaudeCode")]
        .into_iter()
        .find(|(binary, _)| crate::backend_probe::backend_version_probe_succeeds(binary))
        .map(|(_, name)| name)
}

pub(super) fn catalog() -> (u16, Value) {
    (
        200,
        json!({"version": CATALOG_VERSION, "templates": templates(backend())}),
    )
}

pub(super) fn snapshots(context: &Context_) -> (u16, Value) {
    let result = (|| -> Result<Value> {
        let workspaces = crate::workspace::list(&context.omar_dir, context.ea_id)?;
        let mut entries = Vec::new();
        for workspace in workspaces.into_iter().take(100) {
            let snapshots = workspace.snapshots(&context.omar_dir)?;
            entries.push(json!({"workspace_id": workspace.id, "instance": workspace.instance, "snapshots": snapshots.into_iter().rev().take(100).collect::<Vec<_>>() }));
        }
        Ok(
            json!({"workspaces": entries, "capability": "saved UTF-8 Markdown, plain text and text reports; 64 KiB files, 16 KiB selected evidence"}),
        )
    })();
    answer(result)
}

pub(super) fn register(context: &Context_, body: &[u8]) -> (u16, Value) {
    answer((|| -> Result<Value> {
        let request: Register = serde_json::from_slice(body)?;
        anyhow::ensure!(
            request.confirmed,
            "confirm the selected evidence and criterion first"
        );
        let input = assemble(context, request)?;
        Ok(json!(context.decisions.register_subject(input)?))
    })())
}

pub(super) fn artifact_preview(context: &Context_, body: &[u8]) -> (u16, Value) {
    #[derive(Deserialize)]
    struct Request {
        workspace_id: String,
        snapshot_id: String,
        path: String,
    }
    answer((|| -> Result<Value> {
        let r: Request = serde_json::from_slice(body)?;
        let workspace = crate::workspace::Workspace::load(&context.omar_dir, &r.workspace_id)?;
        anyhow::ensure!(
            workspace.ea_id == context.ea_id,
            "unknown artifact workspace"
        );
        let (commit, raw) =
            workspace.read_advisory_text(&context.omar_dir, &r.snapshot_id, &r.path)?;
        let (text, complete) = artifact_text(&r.path, &raw)?;
        Ok(
            json!({"revision": commit, "text": text, "characters": text.chars().count(), "complete": complete}),
        )
    })())
}

fn excerpt(text: &str, start: usize, end: usize) -> Result<AdviceEvidence> {
    let chars: Vec<_> = text.char_indices().collect();
    anyhow::ensure!(
        start < end && end <= chars.len(),
        "selection range is outside source"
    );
    let text = &text[chars[start].0..chars.get(end).map_or(text.len(), |c| c.0)];
    anyhow::ensure!(text.len() <= 16 * 1024, "selection is too large");
    Ok(AdviceEvidence {
        id: "excerpt-1".into(),
        text: text.into(),
        start,
        end,
    })
}

fn assemble(context: &Context_, request: Register) -> Result<AdviceInput> {
    anyhow::ensure!(
        Uuid::parse_str(&request.id).is_ok(),
        "invalid subject identity"
    );
    let mut subject = AdviceSubject {
        id: request.id,
        kind: SubjectKind::Draft,
        chat_id: context.conversation_id.clone(),
        workspace_id: format!("ea:{}", context.ea_id),
        revision: String::new(),
        sha256: String::new(),
    };
    let mut failures = Vec::new();
    let mut eligible = Vec::new();
    let mut roles = None;
    let (criterion, evidence, complete) = match request.payload {
        Payload::Draft { text } => {
            anyhow::ensure!(
                request.profile_id == "template-fit-v1",
                "profile does not accept draft evidence"
            );
            subject.revision = digest(&text);
            eligible = templates(backend())
                .into_iter()
                .filter(|t| t.eligible)
                .map(|t| t.id)
                .collect();
            let criterion = AdviceCriterion {
                id: "template-fit".into(),
                version: CATALOG_VERSION.into(),
                text: "Choose a supported finite workflow for this confirmed brief.".into(),
                requires_complete: true,
            };
            (criterion, excerpt(&text, 0, text.chars().count())?, true)
        }
        Payload::Proposal {
            program,
            inputs,
            scenario_id,
            scenario_version,
        } => {
            anyhow::ensure!(
                request.profile_id == "scenario-coverage-v1",
                "profile does not accept proposal evidence"
            );
            let criterion = reviewer_scenario();
            anyhow::ensure!(
                criterion.id == scenario_id && criterion.version == scenario_version,
                "unknown scenario or version"
            );
            anyhow::ensure!(
                program.len() <= 16 * 1024 && serde_json::to_vec(&inputs)?.len() <= 16 * 1024,
                "proposal is too large"
            );
            subject.kind = SubjectKind::Proposal;
            subject.revision = digest(&serde_json::to_string(&(&program, &inputs))?);
            // The compiler runs here; supplied browser previews/diagnostics are never trusted.
            let (status, checked) =
                check_program(&serde_json::to_vec(&json!({"program": program}))?);
            if status != 200 || checked["ok"] != true {
                failures.push(format!(
                    "Compiler did not accept this proposal: {}",
                    checked
                        .get("errors")
                        .or_else(|| checked.get("error"))
                        .unwrap_or(&Value::Null)
                ));
            } else {
                let snapshot = serde_json::from_value(checked["preview"].clone())?;
                failures.extend(scenario_failures(&snapshot, &inputs));
            }
            (
                criterion,
                excerpt(&program, 0, program.chars().count())?,
                true,
            )
        }
        Payload::Artifact {
            workspace_id,
            snapshot_id,
            path,
            start,
            end,
        } => {
            anyhow::ensure!(
                request.profile_id == "artifact-requirement-v1",
                "profile does not accept artifact evidence"
            );
            let workspace = crate::workspace::Workspace::load(&context.omar_dir, &workspace_id)?;
            anyhow::ensure!(
                workspace.ea_id == context.ea_id,
                "unknown artifact workspace"
            );
            let (commit, raw) =
                workspace.read_advisory_text(&context.omar_dir, &snapshot_id, &path)?;
            let (text, inspectable) = artifact_text(&path, &raw)?;
            failures.extend(reported_test_failures(&path, &text));
            let criterion = request
                .criterion
                .context("confirm a versioned requirement first")?;
            subject.kind = SubjectKind::Artifact;
            subject.workspace_id = workspace_id;
            subject.revision = format!("{commit}:{path}:{}", digest(&raw));
            let complete = inspectable && start == 0 && end == text.chars().count();
            (criterion, excerpt(&text, start, end)?, complete)
        }
        Payload::Run {
            run_id,
            source_id,
            source_sha256,
            start,
            end,
            responsibility_map,
        } => {
            anyhow::ensure!(
                request.profile_id == "review-owner-v1",
                "profile does not accept run evidence"
            );
            anyhow::ensure!(
                context
                    .runs
                    .lock()
                    .expect("runs poisoned")
                    .contains_key(&run_id),
                "unknown run"
            );
            let mut cursor = None;
            let mut sources = Vec::new();
            for _ in 0..2 {
                let (page, _, next) = context.decisions.sources_page(&run_id, cursor.as_deref())?;
                sources.extend(page);
                cursor = next;
                if cursor.is_none() {
                    break;
                }
            }
            let source = sources
                .iter()
                .find(|s| s.source_id == source_id)
                .context("unknown source")?;
            anyhow::ensure!(source.sha256 == source_sha256, "source digest changed");
            anyhow::ensure!(
                !sources
                    .iter()
                    .any(|newer| newer.reaction_id == source.reaction_id
                        && newer.port == source.port
                        && newer.sequence > source.sequence),
                "source is stale"
            );
            subject.kind = SubjectKind::Run;
            subject.revision = format!("{run_id}:{}:{}", source.invocation_id, source.sha256);
            roles = Some(responsibility_map);
            (
                AdviceCriterion {
                    id: "review-owner".into(),
                    version: roles.as_ref().unwrap().version.clone(),
                    text: "Choose a declared responsibility for this selected finding.".into(),
                    requires_complete: false,
                },
                excerpt(&source.text, start, end)?,
                true,
            )
        }
    };
    let mut input = AdviceInput {
        subject,
        profile_id: request.profile_id,
        criterion,
        evidence: vec![evidence],
        complete,
        deterministic_failures: failures,
        eligible_templates: eligible,
        responsibility_map: roles,
    };
    // CAS/idempotency bind the exact evidence, criterion, scope and catalog.
    input.subject.sha256 = digest(&serde_json::to_string(&input)?);
    Ok(input)
}

pub(super) fn subject(context: &Context_, method: &str, route: &str, body: &[u8]) -> (u16, Value) {
    answer((|| -> Result<Value> {
        let parts: Vec<_> = route
            .trim_start_matches("/v1/assist/subjects/")
            .split('/')
            .collect();
        let id = parts[0];
        anyhow::ensure!(Uuid::parse_str(id).is_ok(), "unknown subject");
        let chat = &context.conversation_id;
        match (method, parts.as_slice()) {
            ("GET", [_]) => Ok(json!(context.decisions.subject_state(chat, id)?)),
            ("POST", [_, "mode"]) => {
                #[derive(Deserialize)]
                struct Request {
                    sha256: String,
                    mode: DecisionMode,
                }
                let r: Request = serde_json::from_slice(body)?;
                Ok(json!(context
                    .decisions
                    .set_subject_mode(chat, id, &r.sha256, r.mode)?))
            }
            ("POST", [_, "evaluations"]) => {
                #[derive(Deserialize)]
                struct Request {
                    sha256: String,
                    request_id: String,
                }
                let r: Request = serde_json::from_slice(body)?;
                Ok(json!(context.decisions.evaluate_subject(
                    chat,
                    id,
                    &r.sha256,
                    &r.request_id
                )?))
            }
            ("POST", [_, "decisions", decision_id, "feedback"]) => {
                context.decisions.subject_feedback(
                    chat,
                    id,
                    decision_id,
                    serde_json::from_slice(body)?,
                )?;
                Ok(json!({"ok": true}))
            }
            _ => anyhow::bail!("unknown subject route"),
        }
    })())
}

fn answer(result: Result<Value>) -> (u16, Value) {
    match result {
        Ok(value) => (200, value),
        Err(error) => assist_error(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(root: &Path) -> Serve {
        let mut config = Config::default();
        config.decision_support.enabled = true;
        Serve::start("127.0.0.1:0".parse().unwrap(), &config, root, 0).unwrap()
    }

    #[test]
    fn artifact_api_rejects_other_ea_workspaces_and_confirms_exact_revision_and_requirement() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("omar");
        fs::create_dir(&root).unwrap();
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("result.md"), "A guide for beginners. 文本").unwrap();
        let own =
            crate::workspace::Workspace::create(&root, 0, "deployment", "flow", None, &source)
                .unwrap();
        let foreign =
            crate::workspace::Workspace::create(&root, 1, "deployment-b", "flow", None, &source)
                .unwrap();
        let saved = own.snapshots(&root).unwrap().pop().unwrap();
        let other = foreign.snapshots(&root).unwrap().pop().unwrap();
        let server = server(&root);
        let context = &server.context;
        let body = |workspace: &str, snapshot: &str| {
            serde_json::to_vec(
                &json!({"workspace_id": workspace, "snapshot_id": snapshot, "path": "result.md"}),
            )
            .unwrap()
        };
        assert_eq!(artifact_preview(context, &body(&own.id, &saved.id)).0, 200);
        assert_ne!(
            artifact_preview(context, &body(&foreign.id, &other.id)).0,
            200
        );
        let id = Uuid::new_v4().to_string();
        let request = json!({"id":id, "profile_id":"artifact-requirement-v1", "confirmed":true,
            "criterion":{"id":"audience", "version":"1", "text":"Explain setup for beginners.", "requires_complete":true},
            "payload":{"kind":"artifact", "workspace_id":own.id, "snapshot_id":saved.id, "path":"result.md", "start":0, "end":25}});
        let (status, record) = register(context, &serde_json::to_vec(&request).unwrap());
        assert_eq!(status, 200, "{record}");
        assert_eq!(record["mode"], "off");
        assert!(record["subject"]["revision"]
            .as_str()
            .unwrap()
            .contains(&saved.commit));
        assert_eq!(
            record["input"]["evidence"][0]["text"],
            "A guide for beginners. 文本"
        );
        assert!(context.runs.lock().unwrap().is_empty());
        let mut unconfirmed = request;
        unconfirmed["confirmed"] = json!(false);
        assert_eq!(
            register(context, &serde_json::to_vec(&unconfirmed).unwrap()).0,
            400
        );
    }

    #[test]
    fn server_owned_subjects_reject_forged_owner_and_unknown_criteria() {
        let temp = tempfile::tempdir().unwrap();
        let server = server(temp.path());
        let context = &server.context;
        let request = json!({"id":Uuid::new_v4().to_string(),"profile_id":"scenario-coverage-v1", "confirmed":true,
            "payload":{"kind":"proposal","program":"not a program", "inputs":{}, "scenario_id":"unverified-user-invention", "scenario_version":"1"}});
        assert_eq!(
            register(context, &serde_json::to_vec(&request).unwrap()).0,
            400
        );
        assert!(context.runs.lock().unwrap().is_empty());
        let id = Uuid::new_v4().to_string();
        assert_eq!(
            subject(context, "GET", &format!("/v1/assist/subjects/{id}"), b"").0,
            404
        );
        assert_eq!(excerpt("a✅文本z", 1, 4).unwrap().text, "✅文本");
        assert!(excerpt("a✅文本z", 1, 99).is_err());
    }
}

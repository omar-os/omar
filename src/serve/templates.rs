use super::*;
use crate::template_library as library;

fn respond(result: Result<Value>) -> (u16, Value) {
    match result {
        Ok(value) => (200, value),
        Err(error) => (
            if error.to_string().contains("conflict") {
                409
            } else {
                400
            },
            json!({"error":format!("{error:#}")}),
        ),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Prepare {
    template_id: String,
    template_digest: String,
    request_id: String,
    backend: String,
    input: library::Input,
}

pub(super) fn catalog(context: &Context_) -> (u16, Value) {
    respond((|| {
        let mut catalog = library::catalog()?;
        match library::repository::catalog(&context.omar_dir) {
            Ok(suites) => catalog["repository_verifiers"] = json!(suites),
            Err(e) => {
                catalog["repository_verifiers"] = json!([]);
                catalog["repository_error"] = json!(e.to_string());
            }
        }
        Ok(catalog)
    })())
}
pub(super) fn repository(context: &Context_, body: &[u8]) -> (u16, Value) {
    respond((|| {
        #[derive(Deserialize)]
        struct Request {
            id: String,
        }
        let request: Request = serde_json::from_slice(body)?;
        let suite = library::repository::load(&context.omar_dir, &request.id)?;
        Ok(
            json!({"files":suite.files,"allowed_patch_paths":suite.allowed_patch_paths,"entrypoint":suite.entrypoint,"verifier_id":suite.id,"verifier_digest":suite.digest()?}),
        )
    })())
}
pub(super) fn plan(context: &Context_, body: &[u8]) -> (u16, Value) {
    respond((|| {
        #[derive(Deserialize)]
        struct Request {
            program: String,
            inputs: BTreeMap<String, Value>,
        }
        let request: Request = serde_json::from_slice(body)?;
        let run = library::runtime::admit(&request.program, &request.inputs, &context.omar_dir)?
            .context("not a template")?;
        Ok(
            library::program::build(&run.id, &run.digest, &run.input, &run.backend)?["plan"]
                .clone(),
        )
    })())
}
pub(super) fn prepare(
    workspaces: &Arc<Workspaces>,
    context: &Arc<Context_>,
    body: &[u8],
) -> (u16, Value) {
    respond((|| {
        let request: Prepare = serde_json::from_slice(body)?;
        Uuid::parse_str(&request.request_id)?;
        let _selection = workspaces.selection.lock().expect("selection poisoned");
        let package = library::package(&request.template_id, Some(&request.template_digest))?;
        let built = library::program::build(
            &request.template_id,
            &request.template_digest,
            &request.input,
            &request.backend,
        )?;
        let program = built["program"].as_str().context("missing program")?;
        let inputs: BTreeMap<String, Value> = serde_json::from_value(built["inputs"].clone())?;
        {
            let history = workspaces.history.lock().expect("history poisoned");
            if let Some(chat) = history
                .conversations
                .iter()
                .find(|c| c.id == request.request_id && !c.messages.is_empty())
            {
                let saved = chat
                    .messages
                    .iter()
                    .find_map(|m| m.design.as_ref())
                    .context("request conflicts with an existing conversation")?;
                anyhow::ensure!(
                    saved.program == program && saved.inputs == inputs,
                    "request conflicts with a different prepared workflow"
                );
                return Ok(json!({"conversation":chat.summary(),"replayed":true}));
            }
        }
        let state = compile_preview(program, &BTreeMap::new())?;
        encode_inputs(&state, &inputs)?;
        library::runtime::admit(program, &inputs, &context.omar_dir)?;
        let title = package["title"].as_str().context("missing title")?;
        workspaces
            .history
            .lock()
            .expect("history poisoned")
            .reserve_template(&request.request_id, title)?;
        let target = workspaces.get(&request.request_id)?;
        let _operation = target
            .chat_operation
            .lock()
            .expect("chat operation poisoned");
        let design = ProposedDesign {
            program: program.into(),
            inputs,
            preview: crate::diagram::DiagramSnapshot::from_vm_state(&state),
            files: BTreeMap::new(),
            lemmas: String::new(),
            properties: vec![],
        };
        let mut history = workspaces.history.lock().expect("history poisoned");
        history.complete_template(&request.request_id, title, &request.input.goal, design)?;
        let summary = history.conversation(&request.request_id).summary();
        drop(history);
        target.chat.lock().expect("chat poisoned").needs_context = true;
        Ok(json!({"conversation":summary,"replayed":false}))
    })())
}
fn run_root(context: &Context_, id: &str) -> Result<std::path::PathBuf> {
    anyhow::ensure!(library::identifier(id), "invalid run ID");
    anyhow::ensure!(
        context.runs.lock().expect("runs poisoned").contains_key(id),
        "run does not belong to this conversation"
    );
    Ok(crate::ea::ea_state_dir(context.ea_id, &context.omar_dir)
        .join("serve")
        .join(id))
}
pub(super) fn result(context: &Context_, body: &[u8]) -> (u16, Value) {
    respond((|| {
        #[derive(Deserialize)]
        struct Request {
            run_id: String,
        }
        let request: Request = serde_json::from_slice(body)?;
        let root = run_root(context, &request.run_id)?;
        let path = root.join("template-result.json");
        if !path.exists() {
            return Ok(json!({"status":"not_available","artifacts":[]}));
        }
        Ok(serde_json::from_slice(&fs::read(path)?)?)
    })())
}
pub(super) fn copy(context: &Context_, body: &[u8]) -> (u16, Value) {
    respond((|| {
        #[derive(Deserialize)]
        struct Request {
            run_id: String,
            revision: u64,
            content: String,
        }
        let request: Request = serde_json::from_slice(body)?;
        anyhow::ensure!(
            !request.content.trim().is_empty() && request.content.len() <= 400_000,
            "edited report needs 1–400000 bytes"
        );
        let _operation = context
            .chat_operation
            .lock()
            .expect("chat operation poisoned");
        let root = run_root(context, &request.run_id)?;
        let path = root.join("template-result.json");
        let mut value: Value = serde_json::from_slice(&fs::read(&path)?)?;
        anyhow::ensure!(
            value.get("draft").is_some(),
            "only completed deliverables can be copied"
        );
        let revision = value["edit_revision"].as_u64().unwrap_or(0);
        anyhow::ensure!(
            revision == request.revision,
            "revision conflict: reload before saving"
        );
        anyhow::ensure!(revision < 100, "copy limit reached");
        if value.get("copies").is_none() {
            value["copies"] = json!([]);
        }
        value["copies"].as_array_mut().context("invalid copies")?.push(json!({"revision":revision+1,"checks":"unchecked","created_at":library::now(),"artifact":{"name":format!("report-copy-{}.md",revision+1),"digest":library::digest(request.content.as_bytes()),"content":request.content}}));
        value["edit_revision"] = json!(revision + 1);
        topology::write_json_atomic(&path, &value)?;
        Ok(value)
    })())
}
pub(super) fn finish(context: &Context_, record: &RunRecord) -> Result<()> {
    if record.status != RunStatus::Completed {
        return Ok(());
    }
    let root = crate::ea::ea_state_dir(context.ea_id, &context.omar_dir)
        .join("serve")
        .join(&record.run_id);
    let path = root.join("template-run.json");
    if !path.exists() {
        return Ok(());
    }
    let run: library::runtime::Run = serde_json::from_slice(&fs::read(path)?)?;
    let runtime = crate::deploy::dir_for(&context.omar_dir, context.ea_id, &record.team);
    let mut value = run.bundle(&record.outputs, &runtime)?;
    value["run_id"] = json!(record.run_id);
    topology::write_json_atomic(&root.join("template-result.json"), &value)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn server() -> Serve {
        let root = std::env::temp_dir().join(format!("omar-template-{}", Uuid::new_v4()));
        let mut config = Config::default();
        config.dashboard.session_prefix = format!("template-{}-", Uuid::new_v4());
        Serve::start("127.0.0.1:0".parse().unwrap(), &config, &root, 0).unwrap()
    }
    fn prepare_sample(server: &Serve, id: &str) -> (String, ProposedDesign) {
        let package = library::package(id, None).unwrap();
        let request = json!({"template_id":id,"template_digest":package["digest"],"input":package["sample"],"backend":"Web","request_id":Uuid::new_v4().to_string()});
        let body = serde_json::to_vec(&request).unwrap();
        let (status, response) = prepare(&server.workspaces, &server.context, &body);
        assert_eq!(status, 200, "{response}");
        let (status, replayed) = prepare(&server.workspaces, &server.context, &body);
        assert_eq!(status, 200);
        assert_eq!(replayed["replayed"], true);
        let id = response["conversation"]["id"].as_str().unwrap().to_owned();
        let context = server.workspaces.get(&id).unwrap();
        let history = context.history.lock().unwrap();
        let chat = history.conversation(&id);
        assert!(chat.defer_assistant_start);
        let design = chat.messages[1].design.clone().unwrap();
        assert!(context.runs.lock().unwrap().is_empty());
        assert!(!context.chat.lock().unwrap().owned);
        (id, design)
    }
    fn execute(server: &Serve, id: &str, candidate: Value, repair: bool) -> Value {
        let (chat, design) = prepare_sample(server, id);
        let context = server.workspaces.get(&chat).unwrap();
        let (status,response)=start_run(&context,&serde_json::to_vec(&json!({"program":design.program,"inputs":design.inputs,"fast":true,"timeout_seconds":30})).unwrap());
        assert_eq!(status, 201, "{response}");
        let run = response["run_id"].as_str().unwrap();
        let deadline = Instant::now() + Duration::from_secs(45);
        let mut answered = std::collections::BTreeSet::new();
        loop {
            assert!(Instant::now() < deadline, "run timed out");
            let record = context.runs.lock().unwrap()[run].clone();
            if matches!(
                record.status,
                RunStatus::Completed | RunStatus::Failed | RunStatus::Stopped
            ) {
                assert_eq!(record.status, RunStatus::Completed, "{:?}", record.error);
                break;
            }
            let (status, panel) = panel_status(&context, run);
            if status == 200 {
                for task in panel["pending"].as_array().unwrap() {
                    let task_id = task["task_id"].as_u64().unwrap();
                    if !answered.insert(task_id) {
                        continue;
                    }
                    let reaction = task["reaction"].as_str().unwrap();
                    let port = match reaction {
                        "flow.reaction.0" => "flow.raw",
                        "flow.reaction.2" => "flow.repaired",
                        _ => "flow.result",
                    };
                    let value = if repair && reaction == "flow.reaction.1" {
                        json!({"invented":"bad"})
                    } else {
                        candidate.clone()
                    };
                    let (status,response)=panel_answer(&context,run,&serde_json::to_vec(&json!({"task_id":task_id,"agent":task["agent"],"values":{port:value.to_string()}})).unwrap());
                    assert_eq!(status, 202, "{response}");
                }
            }
            thread::sleep(Duration::from_millis(15));
        }
        let (status, value) = result(
            &context,
            &serde_json::to_vec(&json!({"run_id":run})).unwrap(),
        );
        assert_eq!(status, 200, "{value}");
        if value["status"] != "blocked" {
            let request=serde_json::to_vec(&json!({"run_id":run,"revision":0,"content":"# Reviewed copy\n\nKeep original evidence."})).unwrap();
            assert_eq!(copy(&context, &request).0, 200);
            assert_eq!(copy(&context, &request).0, 409);
            assert!(
                result(
                    &server.context,
                    &serde_json::to_vec(&json!({"run_id":run})).unwrap()
                )
                .0 >= 400
            );
            let root = run_root(&context, run).unwrap();
            let saved: Value =
                serde_json::from_slice(&fs::read(root.join("template-result.json")).unwrap())
                    .unwrap();
            assert_eq!(saved["copies"][0]["revision"], 1);
        }
        value
    }
    #[test]
    fn marketplace_business_templates_deliver_checked_artifacts_and_durable_copies() {
        let server = server();
        for id in [
            "questionnaire",
            "proposal",
            "weekly-review",
            "target-accounts",
            "discovery-plan",
            "research",
        ] {
            let package = library::package(id, None).unwrap();
            let value = execute(
                &server,
                id,
                package["example_result"].clone(),
                id == "questionnaire",
            );
            assert_eq!(value["exact_checks"], "passed");
        }
    }
    #[test]
    fn marketplace_stops_after_one_unsuccessful_correction() {
        let server = server();
        let value = execute(&server, "questionnaire", json!({"invented":"bad"}), false);
        assert_eq!(value["status"], "blocked");
        assert_eq!(value["artifacts"], json!([]));
    }

    #[test]
    fn marketplace_rejects_stale_packages_changed_source_and_invalid_inputs_before_start() {
        let server = server();
        let package = library::package("questionnaire", None).unwrap();
        let mut request = json!({"template_id":"questionnaire","template_digest":"stale","input":package["sample"],"backend":"Web","request_id":Uuid::new_v4().to_string()});
        let before = server
            .workspaces
            .history
            .lock()
            .unwrap()
            .conversations
            .len();
        assert_eq!(
            prepare(
                &server.workspaces,
                &server.context,
                &serde_json::to_vec(&request).unwrap()
            )
            .0,
            400
        );
        request["template_digest"] = package["digest"].clone();
        request["input"]["data"] = json!({});
        assert_eq!(
            prepare(
                &server.workspaces,
                &server.context,
                &serde_json::to_vec(&request).unwrap()
            )
            .0,
            400
        );
        assert_eq!(
            server
                .workspaces
                .history
                .lock()
                .unwrap()
                .conversations
                .len(),
            before
        );
        let (_, mut design) = prepare_sample(&server, "questionnaire");
        design.program.push(' ');
        assert!(
            library::runtime::admit(&design.program, &design.inputs, &server.context.omar_dir)
                .is_err()
        );
    }
    #[test]
    #[ignore = "requires local Docker and OMAR_TEMPLATE_TEST_IMAGE"]
    fn marketplace_repository_checks_original_and_candidate() {
        let server = server();
        let mut suite: library::repository::Suite =
            serde_json::from_str(include_str!("../../templates/repository-example.json")).unwrap();
        suite.image = std::env::var("OMAR_TEMPLATE_TEST_IMAGE").unwrap();
        // Use the installed public snapshot, never the catalog placeholder.
        let package = library::package("test", None).unwrap();
        let mut input = package["sample"].clone();
        input["data"]["repository"] = json!({"files":suite.files,"entrypoint":suite.entrypoint,"allowed_patch_paths":suite.allowed_patch_paths,"verifier_id":suite.id,"verifier_digest":suite.digest().unwrap()});
        topology::write_json_atomic(
            &server
                .context
                .omar_dir
                .join("workflow-verifiers/template-basket.json"),
            &suite,
        )
        .unwrap();
        let built = library::program::build(
            "test",
            package["digest"].as_str().unwrap(),
            &serde_json::from_value(input).unwrap(),
            "Web",
        )
        .unwrap();
        let inputs = serde_json::from_value(built["inputs"].clone()).unwrap();
        let binding = library::runtime::admit(
            built["program"].as_str().unwrap(),
            &inputs,
            &server.context.omar_dir,
        )
        .unwrap()
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let good = package["example_result"].to_string();
        let writes = BTreeMap::from([("flow.result".into(), json!(good))]);
        let accepted = binding
            .check(
                "flow.reaction.1",
                1,
                writes,
                directory.path(),
                Duration::from_secs(20),
            )
            .unwrap();
        assert!(accepted.contains_key("flow.result"));
        assert_eq!(
            binding.bundle(&accepted, directory.path()).unwrap()["exact_checks"],
            "passed"
        );
        let bad=json!({"changes":[{"path":"basket.py","content":"def total(items):\n    return 9\n"}],"explanation":"Wrong patch"}).to_string();
        let rejected = binding
            .check(
                "flow.reaction.1",
                2,
                BTreeMap::from([("flow.result".into(), json!(bad))]),
                directory.path(),
                Duration::from_secs(20),
            )
            .unwrap();
        assert!(rejected.contains_key("flow.retry"));
    }
}

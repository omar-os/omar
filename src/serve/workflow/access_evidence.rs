use super::*;
use std::io::Read;

const MAX_BYTES: usize = 8 * 1024 * 1024;

fn read_bounded(path: &Path, used: &mut usize) -> Result<Vec<u8>> {
    anyhow::ensure!(
        *used < MAX_BYTES,
        "access evidence exceeds the 8 MiB inspection budget"
    );
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take((MAX_BYTES - *used + 1) as u64)
        .read_to_end(&mut bytes)?;
    *used += bytes.len();
    anyhow::ensure!(
        *used <= MAX_BYTES,
        "access evidence exceeds the 8 MiB inspection budget"
    );
    Ok(bytes)
}

pub(in crate::serve) fn inspect(context: &Context_, body: &[u8]) -> (u16, Value) {
    respond((|| {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Request {
            run_id: String,
            agent: String,
        }
        let request: Request = serde_json::from_slice(body)?;
        anyhow::ensure!(
            request.agent.len() <= 256 && request.agent.split('.').all(wd::store::identifier),
            "invalid agent ID"
        );
        // Reuse the receipt endpoint's run-ID and exact chat/plan ownership gate.
        let (status, value) = super::receipt(
            context,
            &serde_json::to_vec(&json!({"run_id":request.run_id}))?,
        );
        anyhow::ensure!(status == 200, "cannot inspect this run: {}", value["error"]);
        let receipt: ExecutionReceipt = serde_json::from_value(value)?;
        anyhow::ensure!(
            receipt.run_id == request.run_id && receipt.profiles.contains_key(&request.agent),
            "agent has no saved native profile in this run"
        );
        let root = crate::ea::ea_state_dir(context.ea_id, &context.omar_dir)
            .join("serve")
            .join(&request.run_id);
        let mut used = 0;
        let mut artifacts = BTreeMap::from([(
            "normalized-receipt".to_owned(),
            wd::digest(&serde_json::to_vec(&receipt)?),
        )]);
        let mut invocations = Vec::new();
        let mut records_read = 0;
        let directory = root.join("invocations");
        if directory.exists() {
            for entry in fs::read_dir(&directory)? {
                let file = entry?.path();
                if file.extension().is_some_and(|e| e == "json") {
                    records_read += 1;
                    anyhow::ensure!(
                        records_read <= 1024,
                        "access evidence exceeds the 1024-record inspection budget"
                    );
                    let bytes = read_bounded(&file, &mut used)?;
                    let record: Value = serde_json::from_slice(&bytes)?;
                    // Unrelated agents cannot supply native access evidence.
                    if record["agent"] == request.agent {
                        let name = file
                            .file_name()
                            .context("missing invocation filename")?
                            .to_string_lossy()
                            .into_owned();
                        artifacts.insert(name, wd::digest(&bytes));
                        invocations.push(record);
                    }
                }
            }
        }
        let usage = directory
            .join("usage")
            .join(format!("{}.json", request.agent));
        let turns: Vec<Value> = if usage.exists() {
            let bytes = read_bounded(&usage, &mut used)?;
            artifacts.insert("native-usage".into(), wd::digest(&bytes));
            serde_json::from_slice(&bytes)?
        } else {
            Vec::new()
        };
        Ok(json!(wd::access::inspect_run(
            &receipt,
            &request.agent,
            &invocations,
            &turns,
            artifacts,
            wd::now()
        )?))
    })())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inspection_is_read_only_owned_and_requires_complete_native_ledger_evidence() {
        let server = crate::serve::tests::test_server();
        let context = &server.context;
        let mut registry = wd::profiles::Registry::load(&context.omar_dir).unwrap();
        registry.connection_scope = Some("test-scope".into());
        fs::write(
            context.omar_dir.join("execution-profiles.json"),
            serde_json::to_vec(&registry).unwrap(),
        )
        .unwrap();
        let program = include_str!("../../../tests/topology/src/ValidatedHandoff.omar")
            .replace("Web", "Codex");
        let (draft, _, inputs) = wd::recovery_tests::fixture(false);
        let state = compile_preview(context, &program).unwrap();
        let plan = create(context, &program, &inputs, draft, &state).unwrap();
        let root =
            crate::ea::ea_state_dir(context.ea_id, &context.omar_dir).join("serve/access-test");
        let request:StartRunRequest=serde_json::from_value(json!({"program":program,"inputs":inputs,"plan":{"id":plan.id,"revision":plan.revision}})).unwrap();
        admit(context, &request, &state, "access-test", &root).unwrap();
        let path = root.join("execution-receipt.json");
        let mut receipt: ExecutionReceipt =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        receipt.status = "completed".into();
        receipt.finished_at = Some(wd::now());
        receipt
            .backend_versions
            .insert("codex".into(), Some("synthetic-cli".into()));
        topology::write_json_atomic(&path, &receipt).unwrap();
        let payload =
            serde_json::to_vec(&json!({"run_id":"access-test","agent":"flow.writer"})).unwrap();
        let invoke = || inspect(context, &payload);
        let (status, missing) = invoke();
        assert_eq!(status, 200);
        assert_eq!(missing["status"], "insufficient_evidence");
        let invocation = root.join("invocations/first.json");
        topology::write_json_atomic(&invocation,&json!({"agent":"flow.writer","invocation_id":"first","invoked":true,"status":"contract_satisfied",
            "error":null,"resources":{"owner":"model"},"outputs":{"private":"not for candidate output"}})).unwrap();
        let usage = root.join("invocations/usage/flow.writer.json");
        let mut turn = json!({"native_request_id":"request-one","native_event":"turn.completed","process_success":null,"native_finished_at":null,"model":null,"effort":null});
        topology::write_json_atomic(&usage, &json!([turn])).unwrap();
        assert_eq!(invoke().1["status"], "insufficient_evidence");
        turn["process_success"] = json!(true);
        turn["native_finished_at"] = json!(wd::now());
        topology::write_json_atomic(&usage, &json!([turn])).unwrap();
        let before = fs::read(&path).unwrap();
        let settings = fs::read(context.omar_dir.join("execution-profiles.json")).unwrap();
        let (status, candidate) = invoke();
        assert_eq!(status, 200, "{candidate}");
        assert_eq!(candidate["status"], "ready_for_review");
        let http = crate::serve::tests::request(
            server.address(),
            "POST",
            "/v1/workflow/access-evidence",
            Some(std::str::from_utf8(&payload).unwrap()),
        );
        assert!(http.starts_with("HTTP/1.1 200"), "{http}");
        let response: Value = serde_json::from_str(http.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(response, candidate);
        assert_eq!(candidate["new_model_requests"], 0);
        assert!(candidate["observed_model"].is_null());
        assert!(!candidate.to_string().contains("not for candidate output"));
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(
            fs::read(context.omar_dir.join("execution-profiles.json")).unwrap(),
            settings
        );
        assert_eq!(
            invoke().1,
            candidate,
            "unchanged artifacts yield the same evidence digest"
        );
        assert_eq!(
            inspect(
                context,
                &serde_json::to_vec(&json!({"run_id":"access-test","agent":"../writer"})).unwrap()
            )
            .0,
            400
        );
        let other = crate::serve::tests::test_server();
        let foreign = crate::ea::ea_state_dir(other.context.ea_id, &other.context.omar_dir)
            .join("serve/access-test/execution-receipt.json");
        topology::write_json_atomic(&foreign, &receipt).unwrap();
        assert_eq!(
            inspect(&other.context, &payload).0,
            400,
            "a copied receipt cannot bypass chat ownership"
        );
    }
}

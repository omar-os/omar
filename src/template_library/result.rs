use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Citation {
    pub source_id: String,
    pub quote: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Row {
    pub id: String,
    pub text: String,
    pub status: String,
    pub citations: Vec<Citation>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Draft {
    pub summary: String,
    pub rows: Vec<Row>,
    pub limitations: Vec<String>,
}

pub fn check(id: &str, digest: &str, input_json: &str, value: &Value) -> Result<Draft> {
    let input: Input = serde_json::from_str(input_json)?;
    let prepared = prepare(id, Some(digest), &input)?;
    if id == "test" {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Change {
            path: String,
            content: String,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Patch {
            changes: Vec<Change>,
            explanation: String,
        }
        let patch: Patch = serde_json::from_value(value.clone())
            .context("patch requires changes and explanation")?;
        ensure!(
            !patch.changes.is_empty()
                && patch.changes.len() <= 16
                && !patch.explanation.trim().is_empty(),
            "provide a bounded patch and explanation"
        );
        let repository = &input.data["repository"];
        let mut seen = BTreeSet::new();
        for change in &patch.changes {
            ensure!(
                seen.insert(&change.path)
                    && repository["allowed_patch_paths"]
                        .as_array()
                        .unwrap()
                        .contains(&json!(change.path)),
                "patch contains duplicate or prohibited paths"
            );
            ensure!(
                change.content.len() <= 65536 && !change.content.contains('\0'),
                "patch file exceeds supported bounds"
            );
        }
        return Ok(Draft{summary:patch.explanation,rows:patch.changes.iter().enumerate().map(|(i,c)| Row{id:format!("change-{}",i+1),text:format!("{}\n\n{}",c.path,c.content),status:"inference".into(),citations:vec![]}).collect(),limitations:vec!["Protected cases cover the installed Python API fixture only. Review the patch before applying it to your repository.".into()]});
    }
    let draft: Draft = serde_json::from_value(value.clone())
        .context("result needs summary, rows and limitations; extra fields are not allowed")?;
    ensure!(
        !draft.summary.trim().is_empty() && draft.summary.len() <= 20_000,
        "result summary is empty or too long"
    );
    ensure!(
        !draft.rows.is_empty() && draft.rows.len() <= 500 && draft.limitations.len() <= 100,
        "invalid result row/limitation count"
    );
    let mut ids = BTreeSet::new();
    let today = chrono::Utc::now().date_naive();
    for row in &draft.rows {
        ensure!(
            identifier(&row.id) && ids.insert(&row.id),
            "duplicate or invalid result row ID"
        );
        ensure!(
            !row.text.trim().is_empty() && row.text.len() <= 8000,
            "result row needs bounded text"
        );
        ensure!(
            ["cited", "unknown", "inference"].contains(&row.status.as_str()),
            "row status must be cited, unknown or inference"
        );
        ensure!(row.citations.len() <= 20, "too many row citations");
        if row.status == "cited" {
            ensure!(
                !row.citations.is_empty(),
                "cited row needs original source quotes"
            );
        }
        for citation in &row.citations {
            let source = input
                .sources
                .iter()
                .find(|s| s.id == citation.source_id)
                .context("result cites an unknown source")?;
            ensure!(
                !citation.quote.trim().is_empty()
                    && citation.quote.len() <= 4000
                    && source.text.contains(&citation.quote),
                "citation quote does not exist in source snapshot"
            );
            if row.status == "cited" {
                if let Some(date) = &source.valid_until {
                    ensure!(
                        chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")? >= today,
                        "expired source cannot support a cited answer"
                    );
                }
            }
        }
    }
    let package = package(id, Some(digest))?;
    if let Some(sections) = package["sections"].as_array() {
        ensure!(
            draft
                .rows
                .iter()
                .map(|r| r.id.as_str())
                .collect::<BTreeSet<_>>()
                == sections.iter().filter_map(Value::as_str).collect(),
            "required report sections are missing or unexpected"
        );
    }
    if id == "questionnaire" {
        let questions = input.data["questions"]
            .as_array()
            .context("questions missing")?;
        ensure!(
            draft.rows.len() == questions.len(),
            "every question must have exactly one answer"
        );
        for (answer, question) in draft.rows.iter().zip(questions) {
            ensure!(
                answer.status != "inference",
                "questionnaire answers must cite approved evidence or remain unknown"
            );
            ensure!(
                question["id"] == answer.id,
                "question IDs and order must be preserved"
            );
            if let Some(allowed) = question["allowed_source_ids"].as_array() {
                ensure!(
                    answer
                        .citations
                        .iter()
                        .all(|c| allowed.iter().any(|a| a == &c.source_id)),
                    "answer uses a source outside the question's allowed evidence"
                );
                if allowed.is_empty() {
                    ensure!(
                        answer.status == "unknown",
                        "a question without approved evidence must remain unknown"
                    );
                }
            }
        }
    }
    if id == "target-accounts" {
        let accounts = prepared["calculated"]["accounts"].as_array().unwrap();
        ensure!(
            draft
                .rows
                .iter()
                .map(|r| r.id.as_str())
                .collect::<BTreeSet<_>>()
                == accounts.iter().filter_map(|a| a["id"].as_str()).collect(),
            "every original account must be represented exactly once"
        );
    }
    if id == "weekly-review" {
        let metrics = prepared["calculated"]["metrics"].as_array().unwrap();
        ensure!(
            draft
                .rows
                .iter()
                .map(|r| r.id.as_str())
                .collect::<BTreeSet<_>>()
                == metrics.iter().filter_map(|a| a["id"].as_str()).collect(),
            "every metric must be represented exactly once"
        );
        for metric in metrics {
            if metric["current"].is_null() || metric["previous"].is_null() {
                ensure!(
                    draft
                        .rows
                        .iter()
                        .any(|r| metric["id"] == r.id && r.status == "unknown"),
                    "missing metric sources must remain unknown"
                );
            }
        }
    }
    Ok(draft)
}

fn cell(s: &str) -> String {
    // Spreadsheet exports must not execute supplied formulas.
    let safe = if s.trim_start().starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{s}")
    } else {
        s.into()
    };
    format!("\"{}\"", safe.replace('"', "\"\""))
}

fn table_csv(rows: &[Value], keys: &[&str]) -> String {
    let mut output = format!("{}\r\n", keys.join(","));
    for row in rows {
        output.push_str(
            &keys
                .iter()
                .map(|key| match &row[*key] {
                    Value::Null => cell("Unavailable"),
                    Value::String(s) => cell(s),
                    value => value.to_string(),
                })
                .collect::<Vec<_>>()
                .join(","),
        );
        output.push_str("\r\n");
    }
    output
}

pub fn bundle(id: &str, digest: &str, input_json: &str, value: &Value) -> Result<Value> {
    let draft = check(id, digest, input_json, value)?;
    let input: Input = serde_json::from_str(input_json)?;
    let prepared = prepare(id, Some(digest), &input)?;
    let package = package(id, Some(digest))?;
    let mut markdown = format!(
        "# {}\n\n{}\n\n",
        package["title"].as_str().unwrap_or(id),
        draft.summary
    );
    let mut csv = "id,status,text,source_ids\r\n".to_owned();
    for row in &draft.rows {
        markdown.push_str(&format!(
            "## {} · {}\n\n{}\n\n",
            row.id, row.status, row.text
        ));
        for citation in &row.citations {
            markdown.push_str(&format!(
                "Source {}: {}\n\n",
                citation.source_id, citation.quote
            ));
        }
        csv.push_str(&format!(
            "{},{},{},{}\r\n",
            cell(&row.id),
            cell(&row.status),
            cell(&row.text),
            cell(
                &row.citations
                    .iter()
                    .map(|c| c.source_id.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            )
        ));
    }
    markdown.push_str("## Limitations\n\n");
    for limitation in &draft.limitations {
        markdown.push_str(&format!("- {limitation}\n"));
    }
    markdown.push_str("\nExact checks cover structure, source quotes, IDs and supplied-data calculations. Cited does not establish that a source entails an answer. Narrative and suitability remain review judgments.\n");
    if id == "proposal" {
        let c = &prepared["calculated"];
        let money = |key: &str| {
            let n = c[key].as_i64().unwrap();
            format!("{}.{:02}", n / 100, n % 100)
        };
        markdown.push_str(&format!("\n## Host-calculated pricing ({})\n\nPriced subtotal: {}\n\nDiscount: {}\n\nPriced total before tax: {}\n\nTax, payment terms and any unpriced work require separate confirmation.\n",c["currency"].as_str().unwrap(),money("priced_subtotal_cents"),money("discount_cents"),money("priced_total_before_tax_cents")));
    }
    let mut contents = BTreeMap::from([
        ("report.md", markdown),
        ("records.csv", csv),
        (
            "sources.json",
            serde_json::to_string_pretty(&input.sources)?,
        ),
        (
            "calculations.json",
            serde_json::to_string_pretty(&prepared["calculated"])?,
        ),
        ("result.json", serde_json::to_string_pretty(&draft)?),
    ]);
    let c = &prepared["calculated"];
    match id {
        "questionnaire" => {
            let rows:Vec<_>=draft.rows.iter().zip(input.data["questions"].as_array().unwrap()).map(|(answer,question)|json!({"id":answer.id,"question":question["question"],"answer":answer.text,"status":answer.status,"source_ids":answer.citations.iter().map(|c|c.source_id.clone()).collect::<Vec<_>>().join("; ")})).collect();
            contents.insert(
                "answers.csv",
                table_csv(&rows, &["id", "question", "answer", "status", "source_ids"]),
            );
        }
        "proposal" => {
            contents.insert(
                "proposal-pricing.csv",
                table_csv(
                    c["lines"].as_array().unwrap(),
                    &["id", "service_id", "quantity", "amount_cents"],
                ),
            );
        }
        "weekly-review" => {
            contents.insert(
                "weekly-metrics.csv",
                table_csv(
                    c["metrics"].as_array().unwrap(),
                    &[
                        "id",
                        "label",
                        "current",
                        "previous",
                        "delta",
                        "change_ratio",
                    ],
                ),
            );
        }
        "target-accounts" => {
            contents.insert(
                "target-accounts.csv",
                table_csv(
                    c["accounts"].as_array().unwrap(),
                    &[
                        "id",
                        "name",
                        "excluded",
                        "confirmed_score",
                        "unknown_weight",
                        "maximum",
                    ],
                ),
            );
        }
        _ => {}
    }
    let artifacts: Vec<_> = contents.iter().map(|(name, content)| json!({"name":name,"content":content,"digest":crate::template_library::digest(content.as_bytes())})).collect();
    let incomplete = draft.rows.iter().any(|r| r.status == "unknown")
        || (id == "proposal"
            && (prepared["calculated"]["all_lines_priced"] != true
                || prepared["calculated"]["tax"].is_null()
                || prepared["calculated"]["terms"].is_null()));
    Ok(
        json!({"schema_version":1,"template_id":id,"template_version":package["version"],"template_digest":digest,"input_digest":crate::template_library::digest(input_json.as_bytes()),"primary":"report.md","status":if incomplete {"needs_information"} else {"draft_for_review"},"exact_checks":"passed","semantic_acceptance":"not_independently_verified","calculated":prepared["calculated"],"draft":draft,"artifacts":artifacts}),
    )
}

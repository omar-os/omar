use super::*;

fn array<'a>(value: &'a Value, label: &str) -> Result<&'a Vec<Value>> {
    let rows = value
        .as_array()
        .with_context(|| format!("{label} must be a JSON array"))?;
    ensure!(
        !rows.is_empty() && rows.len() <= 500,
        "{label} needs 1–500 rows"
    );
    let mut ids = BTreeSet::new();
    for row in rows {
        let id = row["id"].as_str().context("every row needs a string id")?;
        ensure!(
            identifier(id) && ids.insert(id),
            "invalid or duplicate row id in {label}"
        );
    }
    Ok(rows)
}

fn natural(value: &Value, label: &str) -> Result<i64> {
    let number = value
        .as_i64()
        .with_context(|| format!("{label} must be an integer"))?;
    ensure!(
        (0..=1_000_000_000_000).contains(&number),
        "{label} is outside supported bounds"
    );
    Ok(number)
}

pub fn calculate(adapter: &str, input: &Input) -> Result<Value> {
    let d = &input.data;
    match adapter {
        "repository_repair_v1" => {
            let repository = &d["repository"];
            ensure!(
                repository["verifier_id"].as_str().is_some_and(identifier),
                "choose an installed repository verifier ID"
            );
            ensure!(
                repository["verifier_digest"]
                    .as_str()
                    .is_some_and(|s| s.len() == 64 && s.bytes().all(|c| c.is_ascii_hexdigit())),
                "provide the installed verifier digest"
            );
            ensure!(
                repository["files"]
                    .as_object()
                    .is_some_and(|v| !v.is_empty() && v.len() <= 64),
                "provide a bounded repository file snapshot"
            );
            ensure!(
                repository["allowed_patch_paths"].is_array()
                    && repository["entrypoint"].is_string(),
                "repository requires allowed_patch_paths and a Python module:function entrypoint"
            );
            Ok(
                json!({"verifier_id":repository["verifier_id"],"verifier_digest":repository["verifier_digest"],"verification":"Original must fail at least one protected case; the candidate must pass all cases in the installed isolated runner. No source checkout is modified."}),
            )
        }
        "answer_matrix_v1" => {
            let rows = array(&d["questions"], "questions")?;
            for row in rows {
                ensure!(
                    row["question"]
                        .as_str()
                        .is_some_and(|s| !s.trim().is_empty()),
                    "question text is required"
                );
                let ids = row["allowed_source_ids"].as_array().context(
                    "every question needs an explicit allowed_source_ids array; use [] for unknown",
                )?;
                ensure!(
                    ids.iter()
                        .all(|id| input.sources.iter().any(|s| id == &s.id)),
                    "question cites an unknown source ID"
                );
            }
            Ok(
                json!({"question_count":rows.len(),"question_ids":rows.iter().map(|r| &r["id"]).collect::<Vec<_>>()}),
            )
        }
        "priced_proposal_v1" => {
            let p = &d["pricing"];
            let currency = p["currency"].as_str().context("currency is required")?;
            ensure!(
                currency.len() == 3 && currency.bytes().all(|c| c.is_ascii_uppercase()),
                "use a three-letter currency code"
            );
            ensure!(
                ["USD", "EUR", "GBP", "CAD", "AUD", "NZD", "CHF"].contains(&currency),
                "this adapter supports USD, EUR, GBP, CAD, AUD, NZD and CHF (two decimal places)"
            );
            let discount = natural(&p["discount_bps"], "discount_bps")?;
            ensure!(discount <= 10_000, "discount exceeds 100%");
            let maximum = natural(
                &p["max_discount_bps"],
                "max_discount_bps from approved policy",
            )?;
            ensure!(
                maximum <= 10_000 && discount <= maximum,
                "requested discount exceeds the supplied approved policy"
            );
            let services = array(&p["services"], "services")?;
            for service in services {
                natural(&service["unit_cents"], "unit_cents")?;
            }
            let mut subtotal = 0i64;
            let mut lines = vec![];
            let mut complete = true;
            for line in array(&p["lines"], "lines")? {
                let quantity = natural(&line["quantity"], "quantity")?;
                ensure!(
                    quantity > 0 && quantity <= 1_000_000,
                    "quantity must be 1–1000000"
                );
                let service_id = line["service_id"]
                    .as_str()
                    .context("service_id is required")?;
                let amount = if let Some(service) = services.iter().find(|s| s["id"] == service_id)
                {
                    let amount = natural(&service["unit_cents"], "unit_cents")?
                        .checked_mul(quantity)
                        .context("line amount overflow")?;
                    subtotal = subtotal.checked_add(amount).context("subtotal overflow")?;
                    ensure!(
                        subtotal <= 9_007_199_254_740_991,
                        "priced subtotal exceeds the supported exact JSON integer range"
                    );
                    Some(amount)
                } else {
                    complete = false;
                    None
                };
                lines.push(json!({"id":line["id"],"service_id":service_id,"quantity":quantity,"amount_cents":amount}));
            }
            // Half-up rounding to the minor unit, declared in the receipt.
            let deduction = subtotal
                .checked_mul(discount)
                .and_then(|n| n.checked_add(5000))
                .context("discount overflow")?
                / 10_000;
            Ok(
                json!({"currency":currency,"lines":lines,"priced_subtotal_cents":subtotal,"discount_cents":deduction,"priced_total_before_tax_cents":subtotal-deduction,"all_lines_priced":complete,"tax":p.get("tax").unwrap_or(&Value::Null),"terms":p.get("terms").unwrap_or(&Value::Null),"rounding":"half-up to integer minor units; total excludes tax and unpriced work"}),
            )
        }
        "metrics_v1" => {
            for key in ["current", "previous", "timezone"] {
                ensure!(
                    d["periods"][key]
                        .as_str()
                        .is_some_and(|s| !s.trim().is_empty()),
                    "reporting {key} is required"
                );
            }
            ensure!(
                d["periods"]["current"] != d["periods"]["previous"],
                "comparison periods must differ"
            );
            let mut metrics = vec![];
            for metric in array(&d["metrics"], "metrics")? {
                let sum = |key: &str| -> Result<Option<i64>> {
                    if metric[key].is_null() {
                        return Ok(None);
                    }
                    if metric[key].as_array().is_some_and(|a| a.is_empty()) {
                        return Ok(Some(0));
                    }
                    array(&metric[key], key)?
                        .iter()
                        .try_fold(0i64, |total, row| {
                            total
                                .checked_add(natural(&row["value"], "metric value")?)
                                .context("metric total overflow")
                        })
                        .map(Some)
                };
                let current = sum("current")?;
                let previous = sum("previous")?;
                metrics.push(json!({"id":metric["id"],"label":metric["label"],"current":current,"previous":previous,"delta":current.zip(previous).map(|(c,p)| c-p),"change_ratio":current.zip(previous).and_then(|(c,p)| (p!=0).then_some((c-p) as f64/p as f64)),"formula":"sum(value) over unique source-row IDs; change=(current-previous)/previous"}));
            }
            Ok(json!({"periods":d["periods"],"metrics":metrics}))
        }
        "account_scoring_v1" => {
            let weights = d["weights"]
                .as_object()
                .context("weights must be an object")?;
            ensure!(
                !weights.is_empty() && weights.len() <= 30,
                "provide 1–30 criteria"
            );
            let mut maximum = 0i64;
            for (id, weight) in weights {
                ensure!(identifier(id), "invalid criterion ID");
                maximum = maximum
                    .checked_add(natural(weight, "weight")?)
                    .context("score overflow")?;
            }
            let mut accounts = vec![];
            for account in array(&d["accounts"], "accounts")? {
                ensure!(
                    account["name"].is_string() && account["excluded"].is_boolean(),
                    "account name and excluded flag are required"
                );
                let criteria = account["criteria"]
                    .as_object()
                    .context("account criteria missing")?;
                ensure!(
                    criteria.keys().all(|k| weights.contains_key(k)),
                    "unknown account criterion"
                );
                let (mut score, mut unknown) = (0i64, 0i64);
                for (id, weight) in weights {
                    let weight = natural(weight, "weight")?;
                    match criteria.get(id) {
                        Some(Value::Bool(true)) => score += weight,
                        Some(Value::Bool(false)) => (),
                        Some(Value::Null) | None => unknown += weight,
                        _ => anyhow::bail!("criteria must be true, false or null"),
                    }
                }
                accounts.push(json!({"id":account["id"],"name":account["name"],"excluded":account["excluded"],"confirmed_score":score,"unknown_weight":unknown,"maximum":maximum}));
            }
            accounts.sort_by(|a, b| {
                a["excluded"]
                    .as_bool()
                    .cmp(&b["excluded"].as_bool())
                    .then_with(|| {
                        b["confirmed_score"]
                            .as_i64()
                            .cmp(&a["confirmed_score"].as_i64())
                    })
                    .then_with(|| a["id"].as_str().cmp(&b["id"].as_str()))
            });
            Ok(
                json!({"accounts":accounts,"rule":"Exclusions first; confirmed-score descending; stable ID tie-break. Unknown weight is unresolved, not evidence of failure."}),
            )
        }
        "grounded_brief_v1" | "discovery_plan_v1" => Ok(json!({})),
        _ => anyhow::bail!("unsupported input adapter"),
    }
}

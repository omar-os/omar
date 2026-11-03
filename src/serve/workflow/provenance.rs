//! Grounding links are checked against chat-owned records and exact input
//! snapshots. Matching a quote proves the link, not semantic entailment.
use super::*;
use std::collections::BTreeSet;

fn proposed(reason: &str) -> StatementOrigin {
    StatementOrigin::SystemProposed {
        reason: reason.into(),
    }
}

fn normalize(draft: &mut PlanDraft) {
    for assumption in &mut draft.intent.assumptions {
        assumption
            .origin
            .get_or_insert_with(|| proposed("Unclassified design assumption; review its basis."));
    }
    for source in &mut draft.research.sources {
        source.origin.get_or_insert_with(|| proposed("Legacy or unclassified source claim; attach an exact input or operator-message reference."));
    }
    for text in &draft.intent.constraints {
        if !draft.intent.requirements.iter().any(|r| r.text == *text) {
            draft.intent.requirements.push(DesignRequirement {
                id: format!("constraint-{}", &wd::digest(text.as_bytes())[..16]),
                text: text.clone(),
                origin: proposed("Proposed constraint; compare it with the operator's request."),
            });
        }
    }
}

pub(super) fn prepare(
    draft: &mut PlanDraft,
    previous: Option<&WorkflowPlanRevision>,
) -> Result<()> {
    for question in &mut draft.questions {
        let reviewed = previous.and_then(|p| {
            p.draft.questions.iter().find(|old| {
                old.answer_record.is_some()
                    && old.id == question.id
                    && old.question == question.question
                    && old.reason == question.reason
                    && old.target_reactions == question.target_reactions
                    && old.options == question.options
                    && old.proposed_default == question.proposed_default
                    && old.required == question.required
            })
        });
        question.answer = reviewed.and_then(|old| old.answer.clone());
        question.answer_record = reviewed.and_then(|old| old.answer_record.clone());
    }
    // Only a saved review can grant acceptance. A model's accepted=true or
    // forged confirmation field is never authority to launch.
    for assumption in &mut draft.intent.assumptions {
        let old = previous.and_then(|p| {
            p.draft.intent.assumptions.iter().find(|a| {
                a.id == assumption.id
                    && a.text == assumption.text
                    && a.provenance == assumption.provenance
            })
        });
        if assumption.origin.is_none() {
            assumption.origin = old.and_then(|a| a.origin.clone());
        }
        let retained =
            old.filter(|a| a.accepted && a.confirmation.is_some() && a.origin == assumption.origin);
        assumption.accepted = retained.is_some();
        assumption.confirmation = retained.and_then(|a| a.confirmation.clone());
    }
    if let Some(previous) = previous {
        for text in &previous.draft.intent.constraints {
            if !draft.intent.constraints.contains(text) {
                draft.intent.constraints.push(text.clone());
            }
        }
        for old in &previous.draft.intent.requirements {
            if let Some(new) = draft
                .intent
                .requirements
                .iter()
                .find(|r| r.text == old.text)
            {
                anyhow::ensure!(new == old || matches!(old.origin, StatementOrigin::SystemProposed {..}),
                    "cannot silently replace the provenance of an existing requirement '{}'; start a fresh plan to change it", old.id);
            } else {
                draft.intent.requirements.push(old.clone());
            }
        }
        for old in &previous.draft.research.issues {
            if let Some(new) = draft.research.issues.iter_mut().find(|i| i.id == old.id) {
                anyhow::ensure!(new.kind == old.kind && new.description == old.description
                    && new.source_ids == old.source_ids && new.blocking == old.blocking,
                    "research issue '{}' changed identity; preserve it and record an explicit resolution", old.id);
                if new.resolution.is_none() {
                    new.resolution = old.resolution.clone();
                }
            } else {
                draft.research.issues.push(old.clone());
            }
            let unresolved = draft
                .research
                .issues
                .iter()
                .find(|i| i.id == old.id)
                .unwrap()
                .resolution
                .is_none();
            if unresolved {
                for id in &old.source_ids {
                    if let (Some(before), Some(after)) = (
                        previous.draft.research.sources.iter().find(|s| s.id == *id),
                        draft.research.sources.iter().find(|s| s.id == *id),
                    ) {
                        anyhow::ensure!(before == after,
                            "source '{id}' changed while research issue '{}' is unresolved; retain the original source and add updated evidence with a new ID", old.id);
                    }
                }
            }
        }
        // A dropped issue must not erase its source records either.
        let needed: BTreeSet<_> = draft
            .research
            .issues
            .iter()
            .flat_map(|i| &i.source_ids)
            .collect();
        for old in &previous.draft.research.sources {
            if needed.contains(&old.id) && !draft.research.sources.iter().any(|s| s.id == old.id) {
                draft.research.sources.push(old.clone());
            }
        }
    }
    normalize(draft);
    Ok(())
}

fn confirmation(context: &Context_, reference: &ConfirmationRef, text: &str) -> Result<()> {
    match reference {
        ConfirmationRef::ReviewAnswer {
            plan,
            question_id,
            answer,
        } => {
            let record = store(context)?.read(&plan.id, plan.revision)?;
            let latest = store(context)?
                .latest(&plan.id)?
                .context("confirmation plan is missing")?;
            for revision in (plan.revision..latest.revision).rev() {
                let newer = store(context)?.read(&plan.id, revision + 1)?;
                if let Some(question) = newer.draft.questions.iter().find(|q| q.id == *question_id)
                {
                    let original = record.draft.questions.iter().find(|q| q.id == *question_id);
                    anyhow::ensure!(
                        question.answer.as_ref() == Some(answer)
                            && question.answer_record.is_some()
                            && original.is_some_and(|q| q.question == question.question),
                        "saved answer was changed or withdrawn; update the dependent research"
                    );
                    break;
                }
            }
            anyhow::ensure!(
                !answer.trim().is_empty()
                    && text == answer
                    && record.draft.questions.iter().any(|q| q.id == *question_id
                        && q.answer.as_ref() == Some(answer)
                        && q.answer_record
                            .as_ref()
                            .is_some_and(|review| review.id == record.id
                                && review.revision <= record.revision)),
                "confirmation must match an exact saved operator answer in this chat"
            );
        }
        ConfirmationRef::Assumption {
            plan,
            assumption_id,
            text_digest,
        } => {
            let record = store(context)?.read(&plan.id, plan.revision)?;
            let latest = store(context)?
                .latest(&plan.id)?
                .context("confirmation plan is missing")?;
            for revision in (plan.revision..latest.revision).rev() {
                let newer = store(context)?.read(&plan.id, revision + 1)?;
                if let Some(assumption) = newer
                    .draft
                    .intent
                    .assumptions
                    .iter()
                    .find(|a| a.id == *assumption_id)
                {
                    anyhow::ensure!(assumption.text == text && assumption.accepted && assumption.confirmation.is_some(),
                        "assumption confirmation was changed or withdrawn; review the dependent research");
                    break;
                }
            }
            anyhow::ensure!(
                *text_digest == wd::digest(text.as_bytes())
                    && record
                        .draft
                        .intent
                        .assumptions
                        .iter()
                        .any(|a| a.id == *assumption_id
                            && a.text == text
                            && a.accepted
                            && a.confirmation.is_some()),
                "confirmation must match an accepted assumption in this chat"
            );
        }
    }
    Ok(())
}

fn origin(
    context: &Context_,
    inputs: &BTreeMap<String, Value>,
    value: &mut StatementOrigin,
    text: &str,
) -> Result<()> {
    match value {
        StatementOrigin::SystemProposed { reason } => {
            anyhow::ensure!(
                !reason.trim().is_empty() && reason.len() <= 4000,
                "proposed statements need a bounded reason"
            );
        }
        StatementOrigin::UserStated {
            message_sequence,
            quote,
        } => {
            let history = context.history.lock().expect("history poisoned");
            let conversation = history.conversation(&context.conversation_id);
            anyhow::ensure!(
                !quote.trim().is_empty()
                    && quote.len() <= 16_384
                    && text == quote
                    && conversation
                        .messages
                        .iter()
                        .any(|m| m.sequence == *message_sequence
                            && m.role == ChatRole::Operator
                            && m.text.contains(quote.as_str())),
                "user-stated text must quote an actual operator message in this chat"
            );
        }
        StatementOrigin::SourceObserved {
            input_port,
            json_pointer,
            quote,
            content_digest,
            source_reported_date,
            capture,
        } => {
            let input = inputs
                .get(input_port)
                .context("research source refers to an unknown workflow input")?;
            // Omar ports are often strings carrying structured JSON. A pointer
            // may select within that JSON; an empty pointer observes the raw string.
            anyhow::ensure!(
                json_pointer.len() <= 4000,
                "research JSON pointer is too long"
            );
            let decoded = if !json_pointer.is_empty() {
                input
                    .as_str()
                    .map(serde_json::from_str::<Value>)
                    .transpose()
                    .context("research input is not valid JSON for the supplied pointer")?
            } else {
                None
            };
            let value = decoded
                .as_ref()
                .unwrap_or(input)
                .pointer(json_pointer)
                .context("research source JSON pointer does not exist")?;
            let content = match value.as_str() {
                Some(s) => s.to_owned(),
                None => serde_json::to_string(value)?,
            };
            anyhow::ensure!(
                !quote.trim().is_empty()
                    && quote.len() <= 16_384
                    && content.contains(quote.as_str()),
                "research quote does not occur in the referenced input snapshot"
            );
            let digest = wd::digest(content.as_bytes());
            anyhow::ensure!(
                content_digest.is_empty() || *content_digest == digest,
                "research input content changed; revise the observation before running"
            );
            *content_digest = digest;
            if let Some(reference) = capture {
                let (captured, _) = research::ledger(context)?.capture(reference)?;
                anyhow::ensure!(
                    wd::research::content(&captured)?.contains(quote.as_str()),
                    "research quote does not occur in the referenced host capture"
                );
            }
            if let Some(date) = source_reported_date {
                let parsed = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
                    .context("source-reported date must be YYYY-MM-DD")?;
                anyhow::ensure!(
                    parsed.format("%Y-%m-%d").to_string() == *date,
                    "source-reported date must be YYYY-MM-DD"
                );
                anyhow::ensure!(content.contains(date.as_str()),
                    "source-reported date is absent from the observed content; select the source including its date metadata or leave the date unknown");
            }
        }
        StatementOrigin::UserConfirmed {
            confirmation: reference,
        } => confirmation(context, reference, text)?,
    }
    Ok(())
}

pub(super) fn validate(
    context: &Context_,
    draft: &mut PlanDraft,
    inputs: &BTreeMap<String, Value>,
) -> Result<()> {
    normalize(draft);
    anyhow::ensure!(
        draft.intent.requirements.len() <= 100 && draft.research.issues.len() <= 100,
        "too many requirements or research issues"
    );
    let mut ids = BTreeSet::new();
    let mut texts = BTreeSet::new();
    for requirement in &mut draft.intent.requirements {
        anyhow::ensure!(
            wd::store::identifier(&requirement.id)
                && ids.insert(requirement.id.clone())
                && !requirement.text.trim().is_empty()
                && texts.insert(requirement.text.clone())
                && draft.intent.constraints.contains(&requirement.text),
            "invalid or duplicate typed requirement"
        );
        origin(context, inputs, &mut requirement.origin, &requirement.text)?;
    }
    for assumption in &mut draft.intent.assumptions {
        origin(
            context,
            inputs,
            assumption.origin.as_mut().unwrap(),
            &assumption.text,
        )?;
        if let Some(reference) = &assumption.confirmation {
            confirmation(context, reference, &assumption.text)?;
        }
    }
    for source in &mut draft.research.sources {
        anyhow::ensure!(
            wd::store::identifier(&source.id) && !source.finding.trim().is_empty(),
            "invalid research source finding"
        );
        origin(
            context,
            inputs,
            source.origin.as_mut().unwrap(),
            &source.finding,
        )?;
    }
    let sources: BTreeSet<_> = draft.research.sources.iter().map(|s| &s.id).collect();
    ids.clear();
    for issue in &mut draft.research.issues {
        let unique: BTreeSet<_> = issue.source_ids.iter().collect();
        anyhow::ensure!(
            wd::store::identifier(&issue.id)
                && ids.insert(issue.id.clone())
                && !issue.description.trim().is_empty()
                && issue.description.len() <= 4000
                && unique.len() == issue.source_ids.len()
                && unique.iter().all(|s| sources.contains(*s)),
            "invalid research issue or unknown source reference"
        );
        anyhow::ensure!(
            issue.kind != ResearchIssueKind::Contradiction || unique.len() >= 2,
            "a contradiction must identify at least two distinct sources"
        );
        if let Some(resolution) = &mut issue.resolution {
            anyhow::ensure!(
                matches!(
                    resolution.origin,
                    StatementOrigin::UserStated { .. } | StatementOrigin::UserConfirmed { .. } | StatementOrigin::SourceObserved { .. }
                ),
                "a research issue needs linked evidence or an explicit operator decision to resolve"
            );
            origin(context, inputs, &mut resolution.origin, &resolution.text)?;
        }
    }
    Ok(())
}

pub(super) fn confirm_assumptions(
    plan: &mut WorkflowPlanRevision,
    accepted: &[String],
    expected: u32,
) -> Result<()> {
    let revision = expected.checked_add(1).context("revision overflow")?;
    for assumption in &mut plan.draft.intent.assumptions {
        if accepted.contains(&assumption.id) {
            if !assumption.accepted || assumption.confirmation.is_none() {
                assumption.confirmation = Some(ConfirmationRef::Assumption {
                    plan: PlanRef {
                        id: plan.id.clone(),
                        revision,
                    },
                    assumption_id: assumption.id.clone(),
                    text_digest: wd::digest(assumption.text.as_bytes()),
                });
            }
            assumption.accepted = true;
        } else {
            assumption.accepted = false;
            assumption.confirmation = None;
        }
    }
    Ok(())
}

pub(super) fn validate_saved(context: &Context_, plan: &WorkflowPlanRevision) -> Result<()> {
    research::validate_saved(context, &plan.draft)?;
    let mut checked = plan.draft.clone();
    validate(context, &mut checked, &plan.inputs)?;
    anyhow::ensure!(
        checked == plan.draft,
        "research provenance needs a new saved review"
    );
    for assumption in &plan.draft.intent.assumptions {
        anyhow::ensure!(
            !assumption.accepted || assumption.confirmation.is_some(),
            "accepted assumption has no saved review confirmation"
        );
    }
    Ok(())
}

/// Portable bundles retain input-bound observations, but never transfer chat
/// authority or approvals. Cross-chat claims become proposals for fresh review.
pub(super) fn detach_chat(draft: &mut PlanDraft) {
    draft.research.session_id = None;
    draft.research.usage = None;
    fn detach(origin: &mut StatementOrigin) {
        if let StatementOrigin::SourceObserved { capture, .. } = origin {
            *capture = None;
        }
        if matches!(
            origin,
            StatementOrigin::UserStated { .. } | StatementOrigin::UserConfirmed { .. }
        ) {
            *origin = proposed("Imported statement; original chat references were removed. Review it in this chat.");
        }
    }
    for question in &mut draft.questions {
        question.answer = None;
        question.answer_record = None;
    }
    for assumption in &mut draft.intent.assumptions {
        assumption.accepted = false;
        assumption.confirmation = None;
        if let Some(origin) = &mut assumption.origin {
            detach(origin);
        }
    }
    for requirement in &mut draft.intent.requirements {
        detach(&mut requirement.origin);
    }
    for source in &mut draft.research.sources {
        if let Some(origin) = &mut source.origin {
            detach(origin);
        }
    }
    for issue in &mut draft.research.issues {
        issue.resolution = None;
    }
}

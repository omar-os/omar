//! Mutation: changing a run's program while it runs.
//!
//! A mutation is applied to a paused run. The plan is the diff between the
//! program the run's resume point holds and the new one, as instructions: the
//! removals, then the declarations that are new or changed. The compiler
//! computes it from the two sources (`omarc --diff`, proved correct in
//! `lang/Omar/Mutation.lean`); the runtime checks that it turns the one program
//! into the other before executing anything. Executing it means
//! carrying the checkpoint over to the new program — events for ports that are
//! gone are dropped, state is reset where the program says so, new timers are
//! armed, and replaced instances get fresh workspaces — and publishing the
//! result as a `mutation` checkpoint, which the resume then continues from.
//! The checkpoint before it still holds the old program, so a rollback to it
//! undoes the mutation.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::checkpoint::{self, ExecutionState, QueuedTag, Trigger};
use crate::topology::{self, Bytecode, Instruction, VmState};

pub fn op_name(instruction: &Instruction) -> String {
    serde_json::to_value(instruction)
        .ok()
        .and_then(|value| value["op"].as_str().map(str::to_string))
        .unwrap_or_default()
}

/// What pairs a declaration in one program with its counterpart in another:
/// its kind and its qualified name. Reactions are named by position within
/// their instance, so inserting one updates those after it.
fn key(instruction: &Instruction) -> Option<(&'static str, String)> {
    Some(match instruction {
        Instruction::DefineType { name, .. } => ("type", name.clone()),
        Instruction::DeclareInstance { name, .. } => ("instance", name.clone()),
        Instruction::SpawnAgent { name, .. } => ("agent", name.clone()),
        Instruction::DefinePort { name, .. } => ("port", name.clone()),
        Instruction::DeclareTimer { name, .. } => ("timer", name.clone()),
        Instruction::DeclareState { name, .. } => ("state", name.clone()),
        Instruction::DeclareParam { name, .. } => ("param", name.clone()),
        Instruction::ConnectPorts { source, target, .. } => {
            ("connection", format!("{source} -> {target}"))
        }
        Instruction::InstallReaction { id, .. } => ("reaction", id.clone()),
        _ => return None,
    })
}

type Declarations<'a> = BTreeMap<(&'static str, String), &'a Instruction>;

fn declarations(bytecode: &Bytecode) -> Declarations<'_> {
    bytecode
        .instructions
        .iter()
        .filter_map(|instruction| key(instruction).map(|key| (key, instruction)))
        .collect()
}

/// Whether a plan is anything but its frame.
pub fn changes(plan: &Bytecode) -> &[Instruction] {
    let n = plan.instructions.len();
    if n < 2 {
        return &[];
    }
    &plan.instructions[1..n - 1]
}

/// Run `plan` against `from`'s declarations and require exactly `to`'s.
///
/// A plan is checked as a whole before anything happens: every removal names
/// something there, every declaration something not, a reaction updated is
/// one that exists, and what results is the new program, nothing more or less.
pub fn check(from: &Bytecode, plan: &Bytecode, to: &Bytecode) -> Result<()> {
    match plan.instructions.first() {
        Some(Instruction::BeginMutation { team }) if *team == from.team && *team == to.team => {}
        Some(Instruction::BeginMutation { .. }) => {
            bail!("a mutation keeps the program's main: the plan, the run and the new program disagree")
        }
        _ => bail!("mutation plan must begin with begin_mutation"),
    }
    if !matches!(plan.instructions.last(), Some(Instruction::CommitPlan))
        || plan.instructions.len() < 2
    {
        bail!("mutation plan must end with commit_plan");
    }
    let mut declared: BTreeMap<(&'static str, String), Instruction> = declarations(from)
        .into_iter()
        .map(|(key, instruction)| (key, instruction.clone()))
        .collect();
    for (index, instruction) in changes(plan).iter().enumerate() {
        let index = index + 1;
        let removed = match instruction {
            Instruction::RemoveType { name } => Some(("type", name.clone())),
            Instruction::RemoveInstance { name } => Some(("instance", name.clone())),
            Instruction::KillAgent { name } => Some(("agent", name.clone())),
            Instruction::RemovePort { name } => Some(("port", name.clone())),
            Instruction::RemoveTimer { name } => Some(("timer", name.clone())),
            Instruction::RemoveState { name } => Some(("state", name.clone())),
            Instruction::RemoveParam { name } => Some(("param", name.clone())),
            Instruction::DisconnectPorts { source, target } => {
                Some(("connection", format!("{source} -> {target}")))
            }
            Instruction::RemoveReaction { id } => Some(("reaction", id.clone())),
            _ => None,
        };
        if let Some(removed) = removed {
            if declared.remove(&removed).is_none() {
                bail!(
                    "instruction {index} removes {} '{}', which is not there",
                    removed.0,
                    removed.1
                );
            }
            continue;
        }
        if let Some((id, installed)) = update_as_install(instruction) {
            if declared
                .insert(("reaction", id.clone()), installed)
                .is_none()
            {
                bail!("instruction {index} updates reaction '{id}', which is not there");
            }
            continue;
        }
        let Some(key) = key(instruction) else {
            bail!(
                "instruction {index} ({}) cannot appear in a mutation plan",
                op_name(instruction)
            );
        };
        if declared.insert(key.clone(), instruction.clone()).is_some() {
            bail!(
                "instruction {index} declares {} '{}', which is already there",
                key.0,
                key.1
            );
        }
    }
    let expected: BTreeMap<_, _> = declarations(to)
        .into_iter()
        .map(|(key, instruction)| (key, instruction.clone()))
        .collect();
    if declared != expected {
        let differs: Vec<String> = declared
            .keys()
            .chain(expected.keys())
            .filter(|key| declared.get(*key) != expected.get(*key))
            .map(|(kind, name)| format!("{kind} '{name}'"))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        bail!(
            "mutation plan does not produce the new program: {}",
            differs.join(", ")
        );
    }
    Ok(())
}

/// An update as the reaction it leaves installed, under its id.
fn update_as_install(instruction: &Instruction) -> Option<(String, Instruction)> {
    match instruction.clone() {
        Instruction::UpdateReaction {
            id,
            agent,
            triggers,
            effects,
            contract,
            prompt,
            body,
            instance,
            within,
        } => Some((
            id.clone(),
            Instruction::InstallReaction {
                id,
                agent,
                triggers,
                effects,
                contract,
                prompt,
                body,
                instance,
                within,
            },
        )),
        _ => None,
    }
}

/// Carry a paused run's execution state over the plan. Returns what was lost
/// on the way: queued events for ports and timers the new program has not got.
pub fn migrate(plan: &Bytecode, state: &mut ExecutionState) -> Vec<String> {
    let mut queue: BTreeMap<(u64, u64), BTreeMap<String, Value>> = state
        .queue
        .drain(..)
        .map(|tag| ((tag.timestamp, tag.microstep), tag.events))
        .collect();
    let mut dropped = Vec::new();
    let mut drop_events = |queue: &mut BTreeMap<(u64, u64), BTreeMap<String, Value>>,
                           name: &str| {
        for ((t, m), events) in queue.iter_mut() {
            if events.remove(name).is_some() {
                dropped.push(format!("{name} at ({t}, {m})"));
            }
        }
    };
    let now = state.completed_tag;
    for instruction in changes(plan) {
        match instruction {
            Instruction::RemovePort { name } => {
                drop_events(&mut queue, name);
                state.outputs.remove(name);
            }
            Instruction::RemoveTimer { name } => drop_events(&mut queue, name),
            Instruction::RemoveState { name } => {
                state.state_vars.remove(name);
            }
            Instruction::DeclareState { name, initial, .. } => {
                state.state_vars.insert(name.clone(), initial.clone());
            }
            // Armed from now, as a fresh run arms it from its start: the
            // offset is counted from the last tag the run completed.
            Instruction::DeclareTimer {
                name,
                offset,
                shutdown: false,
                ..
            } => {
                let tag = match now {
                    None => (*offset, 0),
                    Some((t, m)) if *offset == 0 => (t, m + 1),
                    Some((t, _)) => (t.saturating_add(*offset), 0),
                };
                queue
                    .entry(tag)
                    .or_default()
                    .insert(name.clone(), json!(tag.0));
            }
            _ => {}
        }
    }
    state.queue = queue
        .into_iter()
        .filter(|(_, events)| !events.is_empty())
        .map(|((timestamp, microstep), events)| QueuedTag {
            timestamp,
            microstep,
            events,
        })
        .collect();
    dropped
}

/// One line per change, for a person to read.
pub fn describe(instruction: &Instruction) -> String {
    let op = op_name(instruction);
    let what = match instruction {
        Instruction::SpawnAgent { name, backend, .. } => format!("{name} ({backend})"),
        Instruction::DefinePort { name, kind, ty, .. } => {
            format!(
                "{name} : {ty} ({})",
                serde_json::to_value(kind)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default()
            )
        }
        Instruction::DeclareInstance { name, team, .. } => format!("{name} = {team}"),
        Instruction::ConnectPorts {
            source,
            target,
            delay,
        } => match delay {
            Some(delay) => format!("{source} -> {target} after {delay}ns"),
            None => format!("{source} -> {target}"),
        },
        Instruction::DisconnectPorts { source, target } => format!("{source} -> {target}"),
        Instruction::InstallReaction { id, agent, .. }
        | Instruction::UpdateReaction { id, agent, .. } => {
            if agent.is_empty() {
                id.clone()
            } else {
                format!("{id} on {agent}")
            }
        }
        Instruction::RemoveReaction { id } => id.clone(),
        other => serde_json::to_value(other)
            .ok()
            .and_then(|v| v.get("name").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_default(),
    };
    format!("{op} {what}")
}

/// What applying a mutation did.
pub struct Applied {
    pub checkpoint: String,
    pub changes: Vec<String>,
    pub dropped: Vec<String>,
}

/// The source of the program a paused team's resume would run.
pub fn paused_source(runtime_dir: &Path) -> Result<PathBuf> {
    let store = checkpoint::Store::new(runtime_dir);
    let point = store
        .resume_point()?
        .context("the run has no checkpoint to mutate")?;
    store.source(&point.id)?.with_context(|| {
        format!(
            "checkpoint {} has no saved source to diff against; \
             resume the run and let it checkpoint again",
            point.id
        )
    })
}

/// Apply `to`, compiled from `source`, to a paused team: have the compiler
/// plan it against the resume point's source, check the plan, carry the
/// checkpoint over and publish it as the new resume point. Nothing is
/// published unless every step succeeds. `None` when the programs do not
/// differ.
pub fn apply(omar_dir: &Path, ea_id: u32, to: &Bytecode, source: &Path) -> Result<Option<Applied>> {
    let to_state = topology::verify(to)?;
    let runtime_dir = crate::deploy::dir_for(omar_dir, ea_id, &to.team);
    let record = crate::deploy::DeploymentRecord::load(&runtime_dir)?
        .with_context(|| format!("no deployment '{}'", to.team))?;
    anyhow::ensure!(
        record.state == crate::deploy::DeploymentState::Paused,
        "deployment '{}' is {}; a mutation applies to a paused run",
        to.team,
        record.state
    );
    let store = checkpoint::Store::new(&runtime_dir);
    let point = store
        .resume_point()?
        .context("the run has no checkpoint to mutate")?;
    let (manifest, mut state, program) = store.load(&point.id)?;
    let from: Bytecode = serde_json::from_value(program)?;
    let plan = topology::compile_mutation(&paused_source(&runtime_dir)?, source)?;
    let new_source = checkpoint::Source::read(source)?;
    if changes(&plan).is_empty() {
        return Ok(None);
    }
    check(&from, &plan, to)?;
    let dropped = migrate(&plan, &mut state);
    for name in to_state.state_vars.keys() {
        anyhow::ensure!(
            state.state_vars.contains_key(name),
            "state variable '{name}' has no value after the mutation"
        );
    }
    let (workspaces, created) = carry_workspaces(omar_dir, ea_id, &manifest, &plan, &to_state)?;
    let result = (|| -> Result<String> {
        let (existing, _) = store.list()?;
        let new = checkpoint::Manifest {
            version: checkpoint::FORMAT,
            id: uuid::Uuid::new_v4().to_string(),
            sequence: existing.iter().map(|m| m.sequence).max().unwrap_or(0) + 1,
            parent: Some(manifest.id.clone()),
            deployment_id: manifest.deployment_id.clone(),
            team: manifest.team.clone(),
            ea_id: manifest.ea_id,
            created_at: crate::deploy::now_unix(),
            completed_tag: state.completed_tag,
            next_tag: state.next_tag(),
            trigger: Trigger::Mutation,
            policy: manifest.policy,
            pace: manifest.pace.clone(),
            program_sha256: checkpoint::sha256(&serde_json::to_vec(&serde_json::to_value(to)?)?),
            program_path: manifest.program_path.clone(),
            state_sha256: checkpoint::sha256(&serde_json::to_vec_pretty(&state)?),
            elapsed_ns: state.elapsed_ns,
            workspaces,
            agents: to_state
                .agents
                .iter()
                .map(|(name, agent)| {
                    (
                        name.clone(),
                        checkpoint::AgentContext {
                            backend: agent.backend.clone(),
                            restoration: checkpoint::FRESH_CONVERSATION.to_string(),
                        },
                    )
                })
                .collect(),
            mutation: changes(&plan).to_vec(),
        };
        store.publish(&new, &state, &serde_json::to_value(to)?, Some(&new_source))?;
        Ok(new.id)
    })();
    match result {
        Ok(checkpoint) => Ok(Some(Applied {
            checkpoint,
            changes: changes(&plan).iter().map(describe).collect(),
            dropped,
        })),
        Err(error) => {
            for workspace in created {
                let _ = workspace.remove(omar_dir);
            }
            Err(error)
        }
    }
}

/// The resume point's files for every instance the new program keeps, and a
/// new, empty workspace for each it adds or replaces. A removed instance's
/// workspace is left as it is, still reachable from the checkpoints before.
fn carry_workspaces(
    omar_dir: &Path,
    ea_id: u32,
    manifest: &checkpoint::Manifest,
    plan: &Bytecode,
    to: &VmState,
) -> Result<(
    BTreeMap<String, checkpoint::ArtifactRef>,
    Vec<crate::workspace::Workspace>,
)> {
    let declared: BTreeSet<&str> = changes(plan)
        .iter()
        .filter_map(|instruction| match instruction {
            Instruction::DeclareInstance { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    let mut artifacts = BTreeMap::new();
    let mut created = Vec::new();
    let result = (|| -> Result<()> {
        for (instance, parent) in crate::workspace::instance_owners(to) {
            if let Some(kept) = manifest.workspaces.get(&instance) {
                if !declared.contains(instance.as_str()) {
                    artifacts.insert(instance, kept.clone());
                    continue;
                }
            }
            let workspace = crate::workspace::Workspace::create(
                omar_dir,
                ea_id,
                &manifest.deployment_id,
                &instance,
                parent,
            )
            .with_context(|| format!("create a workspace for instance '{instance}'"))?;
            created.push(workspace.clone());
            let snapshot = workspace
                .snapshots(omar_dir)?
                .into_iter()
                .next()
                .context("a new workspace has no initial snapshot")?;
            artifacts.insert(
                instance,
                checkpoint::ArtifactRef {
                    workspace_id: workspace.id.clone(),
                    snapshot_id: snapshot.id,
                    commit: snapshot.commit,
                },
            );
        }
        Ok(())
    })();
    if let Err(error) = result {
        for workspace in &created {
            let _ = workspace.remove(omar_dir);
        }
        return Err(error);
    }
    Ok((artifacts, created))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two `Node` instances in a relay: what `main { n1 = Node(1) n2 =
    /// Node(2) n1.out -> n2.token }` compiles to, less what these tests do
    /// not exercise.
    fn relay(second_arg: i64, prompt: &str, extra: Vec<Value>) -> Bytecode {
        let mut instructions = vec![
            json!({"op":"begin_plan","team":"Relay"}),
            json!({"op":"declare_instance","name":"n1","parent":"","team":"Node"}),
            json!({"op":"declare_instance","name":"n2","parent":"","team":"Node"}),
            json!({"op":"spawn_agent","instance":"n1","name":"n1.agent","backend":"Stub"}),
            json!({"op":"spawn_agent","instance":"n2","name":"n2.agent","backend":"Stub"}),
            json!({"op":"define_port","instance":"n1","kind":"input","name":"n1.token","type":"int"}),
            json!({"op":"define_port","instance":"n1","kind":"output","name":"n1.out","type":"int"}),
            json!({"op":"define_port","instance":"n2","kind":"input","name":"n2.token","type":"int"}),
            json!({"op":"define_port","instance":"n2","kind":"output","name":"n2.out","type":"int"}),
            json!({"op":"declare_state","instance":"n2","name":"n2.seen","type":"int","initial":0}),
            json!({"op":"declare_param","instance":"n1","name":"n1.idx","type":"int","value":1}),
            json!({"op":"declare_param","instance":"n2","name":"n2.idx","type":"int","value":second_arg}),
            json!({"op":"connect_ports","source":"n1.out","target":"n2.token"}),
            json!({"op":"install_reaction","instance":"n1","id":"n1.reaction.0","agent":"n1.agent","triggers":["n1.token"],"effects":["n1.out"],"contract":"n1.out","prompt":prompt}),
            json!({"op":"install_reaction","instance":"n2","id":"n2.reaction.0","agent":"n2.agent","triggers":["n2.token"],"effects":["n2.out"],"contract":"n2.out","prompt":"pass it on"}),
        ];
        instructions.extend(extra);
        instructions.push(json!({"op":"commit_plan"}));
        let bytecode: Bytecode =
            serde_json::from_value(json!({"version":1,"team":"Relay","instructions":instructions}))
                .unwrap();
        topology::verify(&bytecode).unwrap();
        bytecode
    }

    /// A plan as the compiler writes one: `begin_mutation`, the changes,
    /// `commit_plan`. How plans are computed is the compiler's, and proved
    /// there; these check what the runtime does with one.
    fn plan(changes: Vec<Value>) -> Bytecode {
        let mut instructions = vec![json!({"op":"begin_mutation","team":"Relay"})];
        instructions.extend(changes);
        instructions.push(json!({"op":"commit_plan"}));
        serde_json::from_value(json!({"version":1,"team":"Relay","instructions":instructions}))
            .unwrap()
    }

    /// What replacing `n2` with `Node(5)` takes: everything in it removed and
    /// declared again.
    fn replace_n2() -> Vec<Value> {
        let b = relay(5, "go", vec![]);
        let mut changes = vec![
            json!({"op":"remove_reaction","id":"n2.reaction.0"}),
            json!({"op":"remove_param","name":"n2.idx"}),
            json!({"op":"remove_state","name":"n2.seen"}),
            json!({"op":"remove_port","name":"n2.out"}),
            json!({"op":"remove_port","name":"n2.token"}),
            json!({"op":"kill_agent","name":"n2.agent"}),
            json!({"op":"remove_instance","name":"n2"}),
        ];
        for instruction in &b.instructions {
            let value = serde_json::to_value(instruction).unwrap();
            let owned = value["instance"] == "n2" || value["name"] == "n2";
            if owned && value["op"] != "connect_ports" {
                changes.push(value);
            }
        }
        changes
    }

    #[test]
    fn an_empty_plan_keeps_the_program() {
        let a = relay(2, "go", vec![]);
        let plan = plan(vec![]);
        assert!(changes(&plan).is_empty());
        check(&a, &plan, &a).unwrap();
    }

    #[test]
    fn a_plan_that_produces_the_new_program_is_accepted() {
        let a = relay(2, "go", vec![]);
        let b = relay(
            2,
            "go faster",
            vec![
                json!({"op":"define_port","instance":"n2","kind":"output","name":"n2.echo","type":"int"}),
                json!({"op":"connect_ports","source":"n2.echo","target":"n1.token","delay":0}),
            ],
        );
        let mut update = serde_json::to_value(&b.instructions[13]).unwrap();
        update["op"] = json!("update_reaction");
        let forward = plan(vec![
            update.clone(),
            json!({"op":"define_port","instance":"n2","kind":"output","name":"n2.echo","type":"int"}),
            json!({"op":"connect_ports","source":"n2.echo","target":"n1.token","delay":0}),
        ]);
        check(&a, &forward, &b).unwrap();
        assert_eq!(
            changes(&forward).iter().map(describe).collect::<Vec<_>>(),
            [
                "update_reaction n1.reaction.0 on n1.agent",
                "define_port n2.echo : int (output)",
                "connect_ports n2.echo -> n1.token after 0ns",
            ]
        );
        let mut back = serde_json::to_value(&a.instructions[13]).unwrap();
        back["op"] = json!("update_reaction");
        let backward = plan(vec![
            json!({"op":"disconnect_ports","source":"n2.echo","target":"n1.token"}),
            json!({"op":"remove_port","name":"n2.echo"}),
            back,
        ]);
        check(&b, &backward, &a).unwrap();
    }

    #[test]
    fn a_replaced_instance_starts_over() {
        let a = relay(2, "go", vec![]);
        let b = relay(5, "go", vec![]);
        let plan = plan(replace_n2());
        check(&a, &plan, &b).unwrap();
        let mut state = ExecutionState {
            version: checkpoint::FORMAT,
            completed_tag: Some((10, 0)),
            queue: vec![QueuedTag {
                timestamp: 10,
                microstep: 1,
                events: BTreeMap::from([
                    ("n2.token".to_string(), json!(7)),
                    ("n1.token".to_string(), json!(3)),
                ]),
            }],
            outputs: BTreeMap::new(),
            state_vars: BTreeMap::from([("n2.seen".to_string(), json!(4))]),
            elapsed_ns: 10,
        };
        let dropped = migrate(&plan, &mut state);
        assert_eq!(dropped, ["n2.token at (10, 1)"]);
        assert_eq!(
            state.queue[0].events.keys().collect::<Vec<_>>(),
            ["n1.token"]
        );
        assert_eq!(state.state_vars["n2.seen"], json!(0), "state starts over");
    }

    #[test]
    fn a_new_timer_is_armed_from_the_last_tag_and_a_removed_one_disarmed() {
        let a = relay(
            2,
            "go",
            vec![
                json!({"op":"declare_timer","instance":"n1","name":"n1.old","offset":0,"period":5}),
            ],
        );
        let tick =
            json!({"op":"declare_timer","instance":"n1","name":"n1.tick","offset":3,"period":0});
        let now =
            json!({"op":"declare_timer","instance":"n1","name":"n1.now","offset":0,"period":5});
        let b = relay(2, "go", vec![tick.clone(), now.clone()]);
        let plan = plan(vec![
            json!({"op":"remove_timer","name":"n1.old"}),
            tick,
            now,
        ]);
        check(&a, &plan, &b).unwrap();
        let mut state = ExecutionState {
            version: checkpoint::FORMAT,
            completed_tag: Some((20, 2)),
            queue: vec![QueuedTag {
                timestamp: 25,
                microstep: 0,
                events: BTreeMap::from([("n1.old".to_string(), Value::Null)]),
            }],
            outputs: BTreeMap::new(),
            state_vars: BTreeMap::from([("n2.seen".to_string(), json!(4))]),
            elapsed_ns: 20,
        };
        let dropped = migrate(&plan, &mut state);
        assert_eq!(dropped, ["n1.old at (25, 0)"]);
        let queued: Vec<_> = state
            .queue
            .iter()
            .map(|tag| {
                (
                    tag.timestamp,
                    tag.microstep,
                    tag.events.keys().cloned().collect::<Vec<_>>(),
                )
            })
            .collect();
        assert_eq!(
            queued,
            [
                (20, 3, vec!["n1.now".to_string()]),
                (23, 0, vec!["n1.tick".to_string()]),
            ]
        );
        assert_eq!(
            state.state_vars["n2.seen"],
            json!(4),
            "kept state keeps its value"
        );
    }

    #[test]
    fn a_plan_that_does_not_produce_the_new_program_is_refused() {
        let a = relay(2, "go", vec![]);
        let b = relay(5, "go", vec![]);
        // Leaving the parameter out keeps n2's old argument.
        let mut changes = replace_n2();
        changes.retain(|c| c["name"] != "n2.idx");
        let error = check(&a, &plan(changes), &b).unwrap_err().to_string();
        assert!(error.contains("param 'n2.idx'"), "{error}");
        // Removing something that is not there.
        let mut changes = replace_n2();
        changes.insert(0, json!({"op":"kill_agent","name":"ghost"}));
        let error = check(&a, &plan(changes), &b).unwrap_err().to_string();
        assert!(error.contains("agent 'ghost'"), "{error}");
        // Declaring something already there.
        let error = check(
            &a,
            &plan(vec![
                json!({"op":"declare_instance","name":"n1","team":"Node","parent":""}),
            ]),
            &a,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("already there"), "{error}");
        // A plan for another program.
        let mut other = plan(vec![]);
        other.instructions[0] = Instruction::BeginMutation {
            team: "Other".into(),
        };
        let error = check(&a, &other, &a).unwrap_err().to_string();
        assert!(error.contains("keeps the program's main"), "{error}");
    }

    #[test]
    fn a_program_cannot_carry_mutation_instructions() {
        let mut a = relay(2, "go", vec![]);
        let at = a.instructions.len() - 1;
        a.instructions.insert(
            at,
            Instruction::KillAgent {
                name: "n1.agent".into(),
            },
        );
        let error = topology::verify(&a).unwrap_err().to_string();
        assert!(error.contains("belongs to a mutation plan"), "{error}");
    }
}

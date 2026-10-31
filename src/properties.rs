//! Properties of a program, proved against the reactor semantics.
//!
//! A property is a Lean theorem about every run of a program
//! (`lang/Omar/Reactor.lean`). The assistant writes the statement and the
//! proof; `omarc --prove` frames them around the model it generated from the
//! program and has Lean check them, so what is shown as proved is what Lean
//! accepted about this exact program.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use ts_rs::TS;

/// How long Lean may take over a program's properties, all checked at once.
const PROVE_TIMEOUT: Duration = Duration::from_secs(300);

/// A property as the assistant states it.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct Property {
    /// A Lean name: letters, digits and underscores.
    pub name: String,
    /// What it guarantees, in a sentence the operator can read.
    #[serde(default)]
    pub description: String,
    /// The Lean proposition, about the run `e`.
    pub statement: String,
    /// The Lean proof of it.
    pub proof: String,
}

/// A line of Lean's complaint about a property.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct ProofProblem {
    /// Counted from the theorem's first line; 0 when it is about the whole.
    pub line: u32,
    pub column: u32,
    pub message: String,
}

/// What Lean made of one property.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct PropertyVerdict {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub proved: bool,
    /// The theorem as it was checked.
    pub theorem: String,
    /// The whole Lean file: the model, the shared lemmas and the theorem.
    pub source: String,
    /// The ports, reactions and timers its statement names.
    #[serde(default)]
    pub mentions: Vec<String>,
    #[serde(default)]
    pub problems: Vec<ProofProblem>,
}

/// Check `properties` of the program at `program`, which is staged with the
/// files it imports. `lemmas` are Lean declarations every property may use.
pub fn prove(
    program: &Path,
    lemmas: &str,
    properties: &[Property],
) -> Result<Vec<PropertyVerdict>> {
    let directory = program.parent().unwrap_or_else(|| Path::new("."));
    let request = directory.join(".properties.json");
    let verdicts = directory.join(".verdicts.json");
    fs::write(
        &request,
        serde_json::to_vec(&json!({"lemmas": lemmas, "properties": properties}))?,
    )?;
    let compiler = crate::topology::resolve_omarc();
    let mut child = Command::new(&compiler)
        .arg("--local-imports")
        .arg("--prove")
        .arg(program)
        .arg(&request)
        .arg(&verdicts)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| {
            format!(
                "failed to invoke OMAR compiler '{}'; install omarc beside omar, \
                 add it to PATH, or set OMARC_BIN",
                compiler.display()
            )
        })?;
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() > PROVE_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            bail!("Lean did not finish checking within {PROVE_TIMEOUT:?}");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    if !status.success() {
        let mut stderr = String::new();
        if let Some(mut pipe) = child.stderr.take() {
            use std::io::Read;
            let _ = pipe.read_to_string(&mut stderr);
        }
        bail!("omarc could not check the properties: {}", stderr.trim());
    }
    let mut checked: Vec<PropertyVerdict> = serde_json::from_slice(&fs::read(&verdicts)?)
        .context("omarc wrote verdicts this runtime cannot read")?;
    let _ = fs::remove_file(&request);
    let _ = fs::remove_file(&verdicts);
    let _ = fs::remove_dir_all(directory.join(".verdicts.json.d"));
    // omarc answers in the order it was asked; the description is the
    // operator's, carried across rather than sent through Lean.
    for (verdict, property) in checked.iter_mut().zip(properties) {
        verdict.description = property.description.clone();
    }
    Ok(checked)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The trading desk's properties are proved by the real compiler, and a
    /// proof that only assumes its conclusion is not.
    #[test]
    fn a_property_is_proved_by_lean_and_an_assumption_is_not() {
        if !crate::topology::resolve_omarc().is_file() {
            eprintln!("skipping: omarc has not been built");
            return;
        }
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/verification");
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("Desk.omar");
        fs::copy(root.join("Desk.omar"), &program).unwrap();
        let request: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join("desk-properties.json")).unwrap()).unwrap();
        let mut properties: Vec<Property> =
            serde_json::from_value(request["properties"].clone()).unwrap();
        properties[0].description = "Orders are approved.".into();
        properties.push(Property {
            name: "assumed".into(),
            description: String::new(),
            statement: "∀ g, ¬ e.invoked g \"x.reaction.0\"".into(),
            proof: "sorry".into(),
        });
        let verdicts = prove(&program, request["lemmas"].as_str().unwrap(), &properties).unwrap();
        assert_eq!(verdicts.len(), 3);
        assert!(verdicts[0].proved && verdicts[1].proved, "{verdicts:#?}");
        assert_eq!(verdicts[0].description, "Orders are approved.");
        assert_eq!(verdicts[0].mentions, ["x.order", "r.approved"]);
        assert!(!verdicts[2].proved);
        assert!(verdicts[2].problems[0].message.contains("sorry"));
    }
}

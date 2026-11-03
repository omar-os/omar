//! Offline, two-phase operator curation. No fetches, model calls or write API is
//! exposed to the designer. Review produces a new registry file for installation.
use super::*;
use anyhow::{ensure, Context, Result};
use clap::Subcommand;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[derive(Debug, Subcommand)]
pub enum Action {
    /// Freeze a source-bound record and the current registry for review
    Prepare {
        #[arg(long)]
        registry: PathBuf,
        #[arg(long)]
        record: PathBuf,
        #[arg(long)]
        snapshot: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    /// Accept/reject a frozen packet; write a new registry without installing it
    Review {
        #[arg(long)]
        registry: PathBuf,
        #[arg(long)]
        packet: PathBuf,
        #[arg(long)]
        reviewer: String,
        #[arg(long)]
        rationale: String,
        /// Explicitly accept this published result for candidate selection only
        #[arg(long, conflicts_with = "reject", required_unless_present = "reject")]
        accept: bool,
        #[arg(long, conflicts_with = "accept", required_unless_present = "accept")]
        reject: bool,
        #[arg(long)]
        output: PathBuf,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Packet {
    schema_version: u32,
    created_at: u64,
    registry_revision: String,
    record_digest: String,
    record: CapabilityEvidence,
}

fn read(path: &Path, max: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take((max + 1) as u64)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= max,
        "input exceeds {max} bytes: {}",
        path.display()
    );
    Ok(bytes)
}

fn registry(path: &Path) -> Result<profiles::Registry> {
    let registry: profiles::Registry = serde_json::from_slice(&read(path, 8 * 1024 * 1024)?)?;
    registry.validate()?;
    Ok(registry)
}

fn write_new(path: &Path, value: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let temporary = parent.join(format!(".benchmark-import-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        // Atomic, no-clobber publication; never overwrite a registry/packet.
        fs::hard_link(&temporary, path)?;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    let _ = fs::remove_file(&temporary);
    result
}

pub fn run(action: Action) -> Result<()> {
    execute(action, now())
}

fn execute(action: Action, at: u64) -> Result<()> {
    match action {
        Action::Prepare {
            registry: registry_path,
            record,
            snapshot,
            output,
        } => {
            let registry = registry(&registry_path)?;
            let mut record: CapabilityEvidence =
                serde_json::from_slice(&read(&record, 256 * 1024)?)?;
            ensure!(
                record.reviewed_at == 0
                    && !record.locally_qualified
                    && record.qualification.is_none(),
                "imports must begin as unreviewed published evidence"
            );
            ensure!(
                !registry.evidence.iter().any(|e| e.id == record.id),
                "evidence ID already exists; use a new versioned ID"
            );
            let profile = registry
                .profiles
                .iter()
                .find(|p| p.id == record.profile_id && p.version == record.profile_version)
                .context("unknown target profile/version")?;
            let result = record
                .benchmark_result
                .as_mut()
                .context("numerical benchmark_result is required")?;
            ensure!(
                result.review.is_none(),
                "import record already has a review"
            );
            ensure!(
                result.retrieved_at <= at && record.expires_at > at,
                "prepare requires a nonfuture retrieval and unexpired record"
            );
            result.profile_digest = profile.configuration_digest();
            result.source_row.snapshot =
                String::from_utf8(read(&snapshot, benchmark::MAX_SNAPSHOT_BYTES)?)?;
            result.source_row.snapshot_digest = digest(result.source_row.snapshot.as_bytes());
            benchmark::validate(&record)?;
            let mut candidate = registry.clone();
            candidate.evidence.push(record.clone());
            candidate.validate()?;
            let packet = Packet {
                schema_version: 1,
                created_at: at,
                registry_revision: registry.revision(),
                record_digest: benchmark::content_digest(&record),
                record,
            };
            write_new(&output, &packet)?;
            println!(
                "Prepared {} for operator review. No registry changed; no network or model calls.",
                output.display()
            );
        }
        Action::Review {
            registry: registry_path,
            packet,
            reviewer,
            rationale,
            accept,
            reject,
            output,
        } => {
            ensure!(accept != reject, "select exactly one of accept/reject");
            let mut registry = registry(&registry_path)?;
            let packet: Packet = serde_json::from_slice(&read(&packet, 1024 * 1024)?)?;
            ensure!(
                packet.schema_version == 1
                    && packet.created_at <= at
                    && packet.registry_revision == registry.revision(),
                "registry changed or invalid packet; prepare a fresh review"
            );
            let mut record = packet.record;
            benchmark::validate(&record)?;
            ensure!(
                packet.record_digest == benchmark::content_digest(&record)
                    && record.reviewed_at == 0
                    && record
                        .benchmark_result
                        .as_ref()
                        .is_some_and(|b| b.review.is_none()),
                "packet changed or is already reviewed"
            );
            ensure!(
                record.expires_at > at && !registry.evidence.iter().any(|e| e.id == record.id),
                "evidence expired or ID already exists"
            );
            record.reviewed_at = at;
            record
                .benchmark_result
                .as_mut()
                .context("missing published result")?
                .review = Some(BenchmarkReview {
                accepted: accept,
                reviewer,
                reviewed_at: at,
                rationale,
                content_digest: packet.record_digest,
            });
            registry.evidence.push(record);
            registry.validate()?;
            // Re-read before writing a replacement; no automatic installation.
            ensure!(
                self::registry(&registry_path)?.revision() == packet.registry_revision,
                "registry changed during review"
            );
            write_new(&output, &registry)?;
            println!("Wrote reviewed registry to {}. Original registry unchanged; inspect and install explicitly. Published scores never grant local qualification.", output.display());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use benchmark::tests::AT;

    #[test]
    fn offline_review_preserves_registry_freezes_sources_and_rejects_changes() {
        let dir = tempfile::tempdir().unwrap();
        let registry = dir.path().join("registry.json");
        let record = dir.path().join("record.json");
        let snapshot = dir.path().join("snapshot.json");
        let packet = dir.path().join("packet.json");
        let output = dir.path().join("reviewed.json");
        let (original, _) = benchmark::tests::fixture();
        fs::write(&registry, serde_json::to_vec(&original).unwrap()).unwrap();
        fs::write(
            &record,
            include_str!("../../evals/workflow-quality/benchmark-import/record.example.json"),
        )
        .unwrap();
        fs::write(
            &snapshot,
            include_str!("../../evals/workflow-quality/benchmark-import/snapshot.example.json"),
        )
        .unwrap();
        let before = fs::read(&registry).unwrap();
        let prepare = || Action::Prepare {
            registry: registry.clone(),
            record: record.clone(),
            snapshot: snapshot.clone(),
            output: packet.clone(),
        };
        execute(prepare(), AT).unwrap();
        assert!(
            execute(prepare(), AT).is_err(),
            "cannot overwrite a review packet"
        );
        // The packet owns the exact snapshot; editing its source cannot silently
        // change what the operator reviews.
        fs::write(&snapshot, "changed original file").unwrap();
        let action = |accept| Action::Review {
            registry: registry.clone(),
            packet: packet.clone(),
            reviewer: "Synthetic reviewer".into(),
            rationale: "Synthetic test only".into(),
            accept,
            reject: !accept,
            output: output.clone(),
        };
        execute(action(true), AT + 1).unwrap();
        assert_eq!(fs::read(&registry).unwrap(), before);
        let accepted = self::registry(&output).unwrap();
        let imported = accepted.evidence.last().unwrap();
        assert_eq!(
            benchmark::assess(imported, &accepted.profiles[0], AT + 1).status,
            CapabilityEvidenceStatus::Candidate
        );
        assert!(!imported.locally_qualified);
        assert!(
            execute(action(true), AT + 1).is_err(),
            "cannot overwrite output or install in place"
        );
        fs::remove_file(&output).unwrap();
        execute(action(false), AT + 1).unwrap();
        let rejected = self::registry(&output).unwrap();
        assert_eq!(
            benchmark::assess(
                rejected.evidence.last().unwrap(),
                &rejected.profiles[0],
                AT + 1
            )
            .status,
            CapabilityEvidenceStatus::Insufficient
        );
        fs::remove_file(&output).unwrap();
        let mut changed = original.clone();
        changed.profiles[0].effort = Some("low".into());
        fs::write(&registry, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(
            execute(action(true), AT + 1).is_err(),
            "stale registry must be re-prepared"
        );
        fs::write(&registry, before).unwrap();
        let mut edited: Packet = serde_json::from_slice(&fs::read(&packet).unwrap()).unwrap();
        edited.record.limitations = "changed after preparation".into();
        fs::write(&packet, serde_json::to_vec(&edited).unwrap()).unwrap();
        assert!(
            execute(action(true), AT + 1).is_err(),
            "packet content digest binds all evidence"
        );
        assert!(!output.exists());
    }

    #[test]
    fn bounded_inputs_and_expired_reviews_never_write_output() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("input");
        fs::write(&path, "1234").unwrap();
        assert!(read(&path, 3).is_err());
        let (registry, record) = benchmark::tests::fixture();
        let registry_path = dir.path().join("registry");
        fs::write(&registry_path, serde_json::to_vec(&registry).unwrap()).unwrap();
        let packet = Packet {
            schema_version: 1,
            created_at: AT,
            registry_revision: registry.revision(),
            record_digest: benchmark::content_digest(&record),
            record,
        };
        let packet_path = dir.path().join("packet");
        write_new(&packet_path, &packet).unwrap();
        let output = dir.path().join("output");
        assert!(execute(
            Action::Review {
                registry: registry_path,
                packet: packet_path,
                reviewer: "test".into(),
                rationale: "test".into(),
                accept: true,
                reject: false,
                output: output.clone()
            },
            packet.record.expires_at
        )
        .is_err());
        assert!(!output.exists());
    }
}

//! Per-instance working directories and immutable Git-backed file snapshots.
//! This is file versioning, not a runtime checkpoint or a security sandbox.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use uuid::Uuid;

const FORMAT: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub version: u32,
    pub id: String,
    pub ea_id: u32,
    pub deployment_id: String,
    pub instance: String,
    pub parent_instance: Option<String>,
    pub source: PathBuf,
    pub restored_from: Option<(String, String)>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub version: u32,
    pub id: String,
    pub workspace_id: String,
    pub commit: String,
    pub created_at: u64,
    pub label: String,
    pub sequence: u64,
    pub parent: Option<String>,
}

fn valid_id(id: &str) -> Result<()> {
    anyhow::ensure!(
        Uuid::parse_str(id)?.to_string() == id,
        "noncanonical workspace/snapshot id"
    );
    Ok(())
}

impl Workspace {
    pub fn root(&self, root: &Path) -> PathBuf {
        root.join("workspaces").join(&self.id)
    }
    pub fn worktree(&self, root: &Path) -> PathBuf {
        self.root(root).join("worktree")
    }
    pub fn temp(&self, root: &Path) -> PathBuf {
        self.root(root).join("temp")
    }
    fn admin(&self, root: &Path) -> PathBuf {
        root.join("workspace-history").join(&self.id)
    }
    fn git(&self, root: &Path) -> Command {
        let mut cmd = Command::new("git");
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                cmd.env_remove(key);
            }
        }
        // Do not run user hooks, filters, signing, or credentials while storing files.
        cmd.env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_COUNT", "0")
            .env("GIT_AUTHOR_NAME", "OMAR")
            .env("GIT_AUTHOR_EMAIL", "workspace@omar.local")
            .env("GIT_COMMITTER_NAME", "OMAR")
            .env("GIT_COMMITTER_EMAIL", "workspace@omar.local")
            .arg("--git-dir")
            .arg(self.admin(root).join("repository.git"))
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.fsync=committed",
            ]);
        cmd
    }
    pub fn load(root: &Path, id: &str) -> Result<Self> {
        valid_id(id)?;
        let value: Self = serde_json::from_slice(&fs::read(
            root.join("workspace-history")
                .join(id)
                .join("workspace.json"),
        )?)?;
        anyhow::ensure!(
            value.version == FORMAT && value.id == id,
            "unsupported or mismatched workspace metadata"
        );
        Ok(value)
    }
    fn save(&self, root: &Path) -> Result<()> {
        save_metadata(&self.admin(root).join("workspace.json"), self)
    }
    fn allocate(
        root: &Path,
        ea_id: u32,
        deployment_id: &str,
        instance: &str,
        parent: Option<String>,
        source: &Path,
    ) -> Result<Self> {
        let workspace = Self {
            version: FORMAT,
            id: Uuid::new_v4().to_string(),
            ea_id,
            deployment_id: deployment_id.into(),
            instance: instance.into(),
            parent_instance: parent,
            source: source.to_path_buf(),
            restored_from: None,
        };
        fs::create_dir_all(workspace.admin(root).join("snapshots"))?;
        fs::create_dir_all(workspace.temp(root))?;
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(workspace.admin(root), fs::Permissions::from_mode(0o700))?;
        fs::set_permissions(workspace.root(root), fs::Permissions::from_mode(0o700))?;
        let mut git = workspace.git(root);
        // init does not use --git-dir to choose its destination.
        git.args(["init", "--bare", "--quiet", "--object-format=sha1"])
            .arg(workspace.admin(root).join("repository.git"));
        output(git, None)?;
        Ok(workspace)
    }
    pub fn create(
        root: &Path,
        ea_id: u32,
        deployment_id: &str,
        instance: &str,
        parent: Option<String>,
        source: &Path,
    ) -> Result<Self> {
        let source = source
            .canonicalize()
            .context("resolve workspace source directory")?;
        anyhow::ensure!(source.is_dir(), "workspace source is not a directory");
        let workspace = Self::allocate(root, ea_id, deployment_id, instance, parent, &source)?;
        let result = (|| {
            fs::create_dir(workspace.worktree(root))?;
            seed(&source, &workspace.worktree(root), root)?;
            let snapshot = workspace.snapshot(root, "Initial workspace")?;
            // Convert the seeded directory into a linked worktree without checking
            // files out through attributes/filters. The files already match the tree.
            fs::remove_dir_all(workspace.worktree(root))?;
            workspace.attach(root, &snapshot.commit)?;
            workspace.save(root)?;
            Ok(())
        })();
        if let Err(error) = result {
            let _ = fs::remove_dir_all(workspace.root(root));
            let _ = fs::remove_dir_all(workspace.admin(root));
            return Err(error);
        }
        Ok(workspace)
    }
    fn lock(&self, root: &Path) -> Result<fs::File> {
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.admin(root).join("lock"))?;
        file.try_lock()
            .context("workspace versioning operation already in progress")?;
        Ok(file)
    }
    pub fn snapshot(&self, root: &Path, label: &str) -> Result<Snapshot> {
        let _lock = self.lock(root)?;
        let previous = self.snapshots(root)?.pop();
        let sequence = previous
            .as_ref()
            .map_or(Some(1), |s| s.sequence.checked_add(1))
            .context("snapshot sequence exhausted")?;
        let id = Uuid::new_v4().to_string();
        let index = self.admin(root).join(format!("{id}.index"));
        let result = (|| {
            let mut entries = Vec::new();
            collect(&self.worktree(root), &self.worktree(root), &mut entries)?;
            entries.sort();
            let mut info = Vec::new();
            for path in entries {
                let full = self.worktree(root).join(&path);
                let metadata = fs::symlink_metadata(&full)?;
                use std::os::unix::ffi::OsStrExt;
                use std::os::unix::fs::PermissionsExt;
                let mut git = self.git(root);
                git.args(["hash-object", "-w", "--no-filters"]);
                let (mode, hash) = if metadata.file_type().is_symlink() {
                    git.arg("--stdin");
                    (
                        "120000",
                        text(output(
                            git,
                            Some(fs::read_link(&full)?.as_os_str().as_bytes()),
                        )?)?,
                    )
                } else {
                    git.arg("--").arg(&full);
                    (
                        if metadata.permissions().mode() & 0o111 != 0 {
                            "100755"
                        } else {
                            "100644"
                        },
                        text(output(git, None)?)?,
                    )
                };
                info.extend_from_slice(format!("{mode} {hash}\t").as_bytes());
                info.extend_from_slice(path.as_os_str().as_bytes());
                info.push(0);
            }
            let mut git = self.git(root);
            git.env("GIT_INDEX_FILE", &index)
                .args(["read-tree", "--empty"]);
            output(git, None)?;
            let mut git = self.git(root);
            git.env("GIT_INDEX_FILE", &index)
                .args(["update-index", "-z", "--index-info"]);
            output(git, Some(&info))?;
            let mut git = self.git(root);
            git.env("GIT_INDEX_FILE", &index).arg("write-tree");
            let tree = text(output(git, None)?)?;
            let mut git = self.git(root);
            git.args([
                "commit-tree",
                &tree,
                "-m",
                &format!("OMAR snapshot {id}\n\n{label}"),
            ]);
            if let Some(previous) = &previous {
                git.args(["-p", &previous.commit]);
            }
            let commit = text(output(git, None)?)?;
            let mut git = self.git(root);
            git.args(["update-ref", &format!("refs/omar/snapshots/{id}"), &commit]);
            output(git, None)?;
            let snapshot = Snapshot {
                version: FORMAT,
                id,
                workspace_id: self.id.clone(),
                commit,
                created_at: crate::deploy::now_unix(),
                label: label.into(),
                sequence,
                parent: previous.as_ref().map(|s| s.id.clone()),
            };
            save_metadata(
                &self
                    .admin(root)
                    .join("snapshots")
                    .join(format!("{}.json", snapshot.id)),
                &snapshot,
            )?;
            Ok(snapshot)
        })();
        let _ = fs::remove_file(index);
        result
    }
    pub fn snapshots(&self, root: &Path) -> Result<Vec<Snapshot>> {
        let mut snapshots: Vec<Snapshot> = Vec::new();
        for entry in fs::read_dir(self.admin(root).join("snapshots"))? {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "json") {
                snapshots.push(serde_json::from_slice(&fs::read(path)?)?);
            }
        }
        for snapshot in &snapshots {
            anyhow::ensure!(
                snapshot.version == FORMAT && snapshot.workspace_id == self.id,
                "unsupported or mismatched snapshot metadata"
            );
        }
        snapshots.sort_by_key(|s| s.sequence);
        Ok(snapshots)
    }
    pub fn ensure_inactive(&self, root: &Path) -> Result<()> {
        let deployments = crate::ea::ea_state_dir(self.ea_id, root).join("topologies");
        if !deployments.exists() {
            return Ok(());
        }
        for entry in fs::read_dir(deployments)? {
            if let Some(record) = crate::deploy::DeploymentRecord::load(&entry?.path())? {
                anyhow::ensure!(record.deployment_id != self.deployment_id || !record.is_active() || !record.runner_alive(),
                    "stop the topology before taking a file snapshot; coordinated live checkpoints are not supported yet");
            }
        }
        Ok(())
    }
    pub fn restore(&self, root: &Path, snapshot_id: &str) -> Result<Self> {
        valid_id(snapshot_id)?;
        let _lock = self.lock(root)?;
        let snapshot: Snapshot = serde_json::from_slice(&fs::read(
            self.admin(root)
                .join("snapshots")
                .join(format!("{snapshot_id}.json")),
        )?)?;
        anyhow::ensure!(
            snapshot.version == FORMAT
                && snapshot.workspace_id == self.id
                && snapshot.id == snapshot_id,
            "unsupported or mismatched snapshot metadata"
        );
        anyhow::ensure!(
            snapshot.commit.len() == 40 && snapshot.commit.bytes().all(|c| c.is_ascii_hexdigit()),
            "invalid snapshot commit"
        );
        let mut git = self.git(root);
        git.args([
            "rev-parse",
            "--verify",
            &format!("refs/omar/snapshots/{snapshot_id}^{{commit}}"),
        ]);
        anyhow::ensure!(
            text(output(git, None)?)? == snapshot.commit,
            "snapshot metadata does not match its Git reference"
        );
        let mut restored = Self::allocate(
            root,
            self.ea_id,
            &self.deployment_id,
            &self.instance,
            self.parent_instance.clone(),
            &self.source,
        )?;
        let result = (|| {
            let mut git = restored.git(root);
            git.arg("fetch")
                .arg(self.admin(root).join("repository.git"))
                .arg(&snapshot.commit);
            output(git, None)?;
            restored.attach(root, &snapshot.commit)?;
            restored.restored_from = Some((self.id.clone(), snapshot_id.into()));
            restored.snapshot(root, "Restored workspace")?;
            restored.save(root)?;
            Ok(())
        })();
        if let Err(error) = result {
            let _ = fs::remove_dir_all(restored.root(root));
            let _ = fs::remove_dir_all(restored.admin(root));
            return Err(error);
        }
        Ok(restored)
    }
    fn attach(&self, root: &Path, commit: &str) -> Result<()> {
        let mut git = self.git(root);
        git.args([
            "worktree",
            "add",
            "--no-checkout",
            "-b",
            &format!("workspace/{}", self.id),
        ])
        .arg(self.worktree(root))
        .arg(commit);
        output(git, None)?;
        // Materialize blobs directly: .gitattributes must not alter checkpoint bytes.
        let mut git = self.git(root);
        git.args(["ls-tree", "-rz", commit]);
        for record in output(git, None)?
            .split(|b| *b == 0)
            .filter(|r| !r.is_empty())
        {
            let tab = record
                .iter()
                .position(|b| *b == b'\t')
                .context("invalid Git tree")?;
            let header = std::str::from_utf8(&record[..tab])?;
            let fields: Vec<_> = header.split_whitespace().collect();
            anyhow::ensure!(
                fields.len() == 3 && fields[1] == "blob",
                "unsupported Git tree entry"
            );
            use std::os::unix::ffi::OsStrExt;
            let path = Path::new(std::ffi::OsStr::from_bytes(&record[tab + 1..]));
            safe_relative(path)?;
            let target = self.worktree(root).join(path);
            fs::create_dir_all(target.parent().context("missing file parent")?)?;
            let mut git = self.git(root);
            git.args(["cat-file", "blob", fields[2]]);
            if fields[0] == "120000" {
                let bytes = output(git, None)?;
                std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(&bytes), target)?;
            } else {
                let file = fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&target)?;
                let result = git
                    .stdin(Stdio::null())
                    .stdout(file)
                    .stderr(Stdio::piped())
                    .output()?;
                anyhow::ensure!(
                    result.status.success(),
                    "restore Git blob: {}",
                    String::from_utf8_lossy(&result.stderr)
                );
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(
                    target,
                    fs::Permissions::from_mode(if fields[0] == "100755" { 0o755 } else { 0o644 }),
                )?;
            }
        }
        let mut git = self.git(root);
        git.arg("-C")
            .arg(self.worktree(root))
            .args(["read-tree", commit]);
        // Explicit --git-dir targets the repository's index; the linked worktree
        // index is initialized separately, without changing any other workspace.
        let pointer = fs::read_to_string(self.worktree(root).join(".git"))?;
        let admin = pointer
            .trim()
            .strip_prefix("gitdir: ")
            .context("invalid worktree pointer")?;
        git.env("GIT_INDEX_FILE", Path::new(admin).join("index"));
        output(git, None)?;
        Ok(())
    }
}

pub fn list(root: &Path, ea_id: u32) -> Result<Vec<Workspace>> {
    let directory = root.join("workspace-history");
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut result = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry.path().join("workspace.json").is_file() {
            let workspace = Workspace::load(root, &entry.file_name().to_string_lossy())?;
            if workspace.ea_id == ea_id {
                result.push(workspace);
            }
        }
    }
    result.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(result)
}

pub fn for_topology(
    root: &Path,
    ea_id: u32,
    deployment_id: &str,
    state: &crate::topology::VmState,
    source: &Path,
) -> Result<BTreeMap<String, Workspace>> {
    let mut owners: BTreeMap<String, Option<String>> = state
        .instances
        .iter()
        .map(|(id, instance)| {
            (
                id.clone(),
                (!instance.parent.is_empty()).then(|| instance.parent.clone()),
            )
        })
        .collect();
    if state.agents.values().any(|a| a.instance.is_empty())
        || state.reactions.values().any(|r| r.instance.is_empty())
        || owners.is_empty()
    {
        owners.insert(String::new(), None);
    }
    owners
        .into_iter()
        .map(|(instance, parent)| {
            let workspace =
                Workspace::create(root, ea_id, deployment_id, &instance, parent, source)?;
            Ok((instance, workspace))
        })
        .collect()
}

fn output(mut cmd: Command, input: Option<&[u8]>) -> Result<Vec<u8>> {
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("start Git (required for team workspaces)")?;
    if let Some(bytes) = input {
        child.stdin.take().context("Git stdin")?.write_all(bytes)?;
    }
    drop(child.stdin.take());
    let result = child.wait_with_output()?;
    anyhow::ensure!(
        result.status.success(),
        "Git: {}",
        String::from_utf8_lossy(&result.stderr).trim()
    );
    Ok(result.stdout)
}
fn text(bytes: Vec<u8>) -> Result<String> {
    Ok(String::from_utf8(bytes)?.trim().to_owned())
}

fn save_metadata(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path.parent().context("metadata directory")?;
    let temporary = parent.join(format!(".{}.tmp", Uuid::new_v4()));
    let result = (|| {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&serde_json::to_vec_pretty(value)?)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}
fn safe_relative(path: &Path) -> Result<()> {
    anyhow::ensure!(
        !path.as_os_str().is_empty()
            && path
                .components()
                .all(|c| matches!(c, Component::Normal(n) if n != ".git")),
        "unsafe workspace path: {}",
        path.display()
    );
    Ok(())
}
fn collect(base: &Path, directory: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_name() == ".git" {
            anyhow::ensure!(
                directory == base,
                "nested Git repositories are not supported in snapshots"
            );
            continue;
        }
        let kind = entry.file_type()?;
        if kind.is_dir() {
            collect(base, &entry.path(), paths)?;
        } else if kind.is_file() || kind.is_symlink() {
            paths.push(entry.path().strip_prefix(base)?.to_path_buf());
        } else {
            bail!("cannot snapshot special file {}", entry.path().display());
        }
    }
    Ok(())
}
fn seed(source: &Path, destination: &Path, root: &Path) -> Result<()> {
    let root = root.canonicalize()?;
    anyhow::ensure!(
        !source.starts_with(&root),
        "workspace source must be outside OMAR state directory"
    );
    let mut git = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            git.env_remove(key);
        }
    }
    git.arg("-C").arg(source).args([
        "ls-files",
        "--cached",
        "--others",
        "--exclude-standard",
        "-z",
        "--",
        ".",
    ]);
    let mut paths = match git.output() {
        Ok(result) if result.status.success() => {
            use std::os::unix::ffi::OsStrExt;
            result
                .stdout
                .split(|b| *b == 0)
                .filter(|p| !p.is_empty())
                .map(|p| PathBuf::from(std::ffi::OsStr::from_bytes(p)))
                .collect::<Vec<_>>()
        }
        _ => {
            let mut paths = Vec::new();
            collect_seed(source, source, &root, &mut paths)?;
            paths
        }
    };
    paths.sort();
    paths.dedup();
    for path in paths {
        safe_relative(&path)?;
        let from = source.join(&path);
        if from.starts_with(&root) {
            continue;
        }
        let metadata = match fs::symlink_metadata(&from) {
            Ok(metadata) => metadata,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        anyhow::ensure!(
            !metadata.is_dir(),
            "submodules require an explicit source workspace: {}",
            from.display()
        );
        let to = destination.join(path);
        fs::create_dir_all(to.parent().context("source parent")?)?;
        if metadata.file_type().is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(from)?, to)?;
        } else if metadata.is_file() {
            fs::copy(from, to)?;
        } else {
            bail!("cannot seed special file {}", from.display());
        }
    }
    Ok(())
}

fn collect_seed(
    base: &Path,
    directory: &Path,
    excluded: &Path,
    paths: &mut Vec<PathBuf>,
) -> Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry.path().starts_with(excluded) || entry.file_name() == ".git" {
            anyhow::ensure!(
                entry.file_name() != ".git" || directory == base,
                "nested Git repositories require an explicit source directory"
            );
            continue;
        }
        let kind = entry.file_type()?;
        if kind.is_dir() {
            collect_seed(base, &entry.path(), excluded, paths)?;
        } else if kind.is_file() || kind.is_symlink() {
            paths.push(entry.path().strip_prefix(base)?.to_path_buf());
        } else {
            bail!("cannot seed special file {}", entry.path().display());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    fn setup() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let root = dir.path().join("omar state");
        let source = dir.path().join("source files");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&source).unwrap();
        (dir, root, source)
    }
    fn create(root: &Path, source: &Path) -> Workspace {
        Workspace::create(
            root,
            7,
            "deployment",
            "team.child",
            Some("team".into()),
            source,
        )
        .unwrap()
    }
    fn git_at(path: &Path, args: &[&str]) -> Vec<u8> {
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(path).args(args);
        output(cmd, None).unwrap()
    }

    #[test]
    fn restores_all_file_bytes_without_changing_the_source_or_live_workspace() {
        let (_dir, root, source) = setup();
        fs::write(source.join("original"), "source").unwrap();
        let ws = create(&root, &source);
        let tree = ws.worktree(&root);
        fs::write(tree.join(".gitignore"), "*.bin\nignored/\n").unwrap();
        fs::write(tree.join(".gitattributes"), "* text eol=lf\n").unwrap();
        fs::write(tree.join("artifact.bin"), b"\0\xff\r\n\x01").unwrap();
        fs::create_dir(tree.join("ignored")).unwrap();
        fs::write(tree.join("ignored/report"), b"untracked\r\n").unwrap();
        fs::write(tree.join("run.sh"), "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(tree.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(tree.join("space\nand newline"), "name preserved").unwrap();
        symlink("missing-target", tree.join("link")).unwrap();
        fs::write(ws.temp(&root).join("discard"), "scratch").unwrap();
        git_at(&tree, &["add", "original"]);
        let before_index = git_at(&tree, &["ls-files", "--stage", "-z"]);
        let snapshot = ws.snapshot(&root, "safe files").unwrap();
        assert_eq!(snapshot.sequence, 2);
        assert!(snapshot.parent.is_some());
        assert_eq!(before_index, git_at(&tree, &["ls-files", "--stage", "-z"]));
        fs::write(tree.join("artifact.bin"), "newer work").unwrap();
        fs::remove_file(tree.join("original")).unwrap();
        let reopened = Workspace::load(&root, &ws.id).unwrap();
        let restored = reopened.restore(&root, &snapshot.id).unwrap();
        let restored_tree = restored.worktree(&root);
        assert_eq!(
            fs::read(restored_tree.join("artifact.bin")).unwrap(),
            b"\0\xff\r\n\x01"
        );
        assert_eq!(
            fs::read(restored_tree.join("ignored/report")).unwrap(),
            b"untracked\r\n"
        );
        assert_eq!(fs::read(restored_tree.join("original")).unwrap(), b"source");
        assert_eq!(
            fs::read_to_string(tree.join("artifact.bin")).unwrap(),
            "newer work"
        );
        assert!(!tree.join("original").exists());
        assert_eq!(
            fs::read_to_string(source.join("original")).unwrap(),
            "source"
        );
        assert_eq!(
            fs::read_link(restored_tree.join("link")).unwrap(),
            Path::new("missing-target")
        );
        assert_ne!(
            fs::metadata(restored_tree.join("run.sh"))
                .unwrap()
                .permissions()
                .mode()
                & 0o111,
            0
        );
        assert!(restored_tree.join("space\nand newline").exists());
        assert_eq!(fs::read_dir(restored.temp(&root)).unwrap().count(), 0);
        assert_eq!(restored.restored_from, Some((ws.id.clone(), snapshot.id)));
        assert_eq!(list(&root, 7).unwrap().len(), 2);
        assert!(list(&root, 8).unwrap().is_empty());
        assert!(tree.join(".git").is_file());
        assert!(!tree.join("repository.git").exists());
    }

    #[test]
    fn seeds_git_sources_with_local_changes_and_without_ignored_files() {
        let (_dir, root, source) = setup();
        git_at(&source, &["init", "--quiet"]);
        fs::write(source.join("tracked"), "before").unwrap();
        fs::write(source.join("deleted"), "before").unwrap();
        git_at(&source, &["add", "tracked", "deleted"]);
        fs::write(source.join("tracked"), "local edits").unwrap();
        fs::remove_file(source.join("deleted")).unwrap();
        fs::write(source.join(".gitignore"), "cache\n").unwrap();
        fs::write(source.join("cache"), "not seeded").unwrap();
        fs::write(source.join("untracked"), "included").unwrap();
        let before = git_at(&source, &["status", "--porcelain=v1", "-z"]);
        let ws = create(&root, &source);
        assert_eq!(
            fs::read_to_string(ws.worktree(&root).join("tracked")).unwrap(),
            "local edits"
        );
        assert!(ws.worktree(&root).join("untracked").exists());
        assert!(!ws.worktree(&root).join("deleted").exists());
        assert!(!ws.worktree(&root).join("cache").exists());
        assert_eq!(before, git_at(&source, &["status", "--porcelain=v1", "-z"]));
    }

    #[test]
    fn nested_instances_and_successive_deployments_have_independent_workspaces() {
        let (_dir, root, source) = setup();
        let state: crate::topology::VmState = serde_json::from_value(serde_json::json!({
            "version": 1, "team": "Example", "instances": {
                "one": {"team": "Team", "parent": ""},
                "one.child": {"team": "Team", "parent": "one"},
                "two": {"team": "Team", "parent": ""}},
            "agents": {"one.a": {"backend": "stub", "instance": "one"},
                "one.b": {"backend": "stub", "instance": "one"}},
            "ports": {}, "connections": [], "reactions": {}
        }))
        .unwrap();
        let first = for_topology(&root, 7, "first", &state, &source).unwrap();
        let second = for_topology(&root, 7, "second", &state, &source).unwrap();
        assert_eq!(first.len(), 3);
        assert_ne!(first["one"].id, first["one.child"].id);
        assert_ne!(first["one"].id, second["one"].id);
        assert_eq!(first["one.child"].parent_instance.as_deref(), Some("one"));
        fs::write(first["one"].worktree(&root).join("artifact"), "one").unwrap();
        assert!(!first["two"].worktree(&root).join("artifact").exists());
        assert!(!second["one"].worktree(&root).join("artifact").exists());
    }

    #[test]
    fn rejects_invalid_ids_versions_special_files_and_concurrent_operations() {
        let (_dir, root, source) = setup();
        let ws = create(&root, &source);
        assert!(Workspace::load(&root, "../../escape").is_err());
        assert!(ws.restore(&root, "../../escape").is_err());
        let lock = ws.lock(&root).unwrap();
        assert!(ws.snapshot(&root, "locked").is_err());
        drop(lock);
        let socket =
            std::os::unix::net::UnixListener::bind(ws.worktree(&root).join("socket")).unwrap();
        assert!(ws.snapshot(&root, "socket").is_err());
        drop(socket);
        fs::remove_file(ws.worktree(&root).join("socket")).unwrap();
        let snapshot = ws.snapshot(&root, "valid").unwrap();
        let path = ws
            .admin(&root)
            .join("snapshots")
            .join(format!("{}.json", snapshot.id));
        let mut snapshot = snapshot;
        snapshot.version = FORMAT + 1;
        crate::topology::write_json_atomic(&path, &snapshot).unwrap();
        assert!(ws.restore(&root, &snapshot.id).is_err());
    }

    #[test]
    fn live_deployments_refuse_manual_snapshots_and_state_directory_is_not_seeded() {
        let (dir, root, _) = setup();
        let ws = create(&root, dir.path());
        assert!(!ws.worktree(&root).join("omar state").exists());
        let mut deployment =
            crate::deploy::DeploymentRecord::create("Example", BTreeMap::new(), 60);
        deployment.deployment_id = ws.deployment_id.clone();
        let deployment_dir = crate::deploy::dir_for(&root, 7, "Example");
        deployment.save(&deployment_dir).unwrap();
        assert!(ws.ensure_inactive(&root).is_err());
        deployment
            .advance(crate::deploy::DeploymentState::Failed, Some("test"))
            .unwrap();
        deployment.save(&deployment_dir).unwrap();
        ws.ensure_inactive(&root).unwrap();
    }
}

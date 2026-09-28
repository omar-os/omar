//! Read-only artifact previews. Editing is delegated to code-server.
use anyhow::{bail, Context, Result};
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};
#[cfg(test)]
use std::fs;
use std::io::Read;
use std::path::{Component, Path};

use crate::workspace::Workspace;
const PREVIEW_BYTES: u64 = 512 * 1024;
const MAX_ENTRIES: usize = 2000;

#[derive(Default, Deserialize)]
pub struct Selection {
    #[serde(default)]
    pub path: String,
    pub snapshot: Option<String>,
}

fn relative(path: &str) -> Result<()> {
    anyhow::ensure!(!path.contains('\0'), "invalid path");
    anyhow::ensure!(
        Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(s) if s != ".git")),
        "use a relative workspace path without .git or parent components"
    );
    Ok(())
}

// Single-component openat calls retain each parent handle and atomically refuse
// symlinks. NONBLOCK prevents a concurrently substituted FIFO from hanging a read.
fn open_child(
    dir: &cap_std::fs::Dir,
    name: &std::ffi::OsStr,
    directory: bool,
) -> Result<std::fs::File> {
    use std::os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
    };
    let name = std::ffi::CString::new(name.as_bytes())?;
    let flags = libc::O_RDONLY
        | libc::O_CLOEXEC
        | libc::O_NOFOLLOW
        | libc::O_NONBLOCK
        | if directory { libc::O_DIRECTORY } else { 0 };
    // The path is a validated single component; the returned descriptor is owned.
    let fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

fn live_directory(ws: &Workspace, root: &Path, path: &str) -> Result<cap_std::fs::Dir> {
    use std::os::unix::fs::OpenOptionsExt;
    relative(path)?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(ws.worktree(root))?;
    let mut dir = cap_std::fs::Dir::from_std_file(file);
    for part in Path::new(path).components() {
        dir = cap_std::fs::Dir::from_std_file(open_child(&dir, part.as_os_str(), true)?);
    }
    Ok(dir)
}

fn revision(ws: &Workspace, root: &Path, id: &str) -> Result<String> {
    let snapshot = ws
        .snapshots(root)?
        .into_iter()
        .find(|s| s.id == id)
        .context("unknown snapshot")?;
    let result = ws
        .git(root)
        .args([
            "rev-parse",
            "--verify",
            &format!("refs/omar/snapshots/{id}^{{commit}}"),
        ])
        .output()?;
    anyhow::ensure!(
        result.status.success()
            && String::from_utf8_lossy(&result.stdout).trim() == snapshot.commit,
        "snapshot reference does not match metadata"
    );
    Ok(snapshot.commit)
}

pub fn browse(ws: &Workspace, root: &Path, selection: &Selection) -> Result<Value> {
    relative(&selection.path)?;
    let mut entries = Vec::new();
    if let Some(id) = &selection.snapshot {
        let commit = revision(ws, root, id)?;
        let tree = if selection.path.is_empty() {
            commit
        } else {
            format!("{commit}:{}", selection.path)
        };
        let result = ws.git(root).args(["ls-tree", "-lz", &tree]).output()?;
        anyhow::ensure!(result.status.success(), "not a snapshot directory");
        for entry in result
            .stdout
            .split(|b| *b == 0)
            .filter(|b| !b.is_empty())
            .take(MAX_ENTRIES + 1)
        {
            let (header, name) = entry.split_at(
                entry
                    .iter()
                    .position(|b| *b == b'\t')
                    .context("invalid Git entry")?,
            );
            let fields: Vec<_> = std::str::from_utf8(header)?.split_whitespace().collect();
            entries.push(json!({"name": String::from_utf8_lossy(&name[1..]), "kind": if fields[0] == "120000" {"symlink"} else if fields[1] == "tree" {"directory"} else {"file"}, "size": fields.get(3).and_then(|s| s.parse::<u64>().ok())}));
        }
    } else {
        let dir = live_directory(ws, root, &selection.path)?;
        for entry in dir.entries()? {
            let entry = entry?;
            if entry.file_name() == ".git" {
                continue;
            }
            let meta = dir.symlink_metadata(entry.file_name())?;
            entries.push(json!({"name": entry.file_name().to_string_lossy(), "kind": if meta.file_type().is_symlink() {"symlink"} else if meta.is_dir() {"directory"} else if meta.is_file() {"file"} else {"special"}, "size": meta.len()}));
            if entries.len() > MAX_ENTRIES {
                break;
            }
        }
    }
    let truncated = entries.len() > MAX_ENTRIES;
    entries.truncate(MAX_ENTRIES);
    entries.sort_by_key(|entry| {
        (
            entry["kind"] != "directory",
            entry["name"].as_str().unwrap_or_default().to_owned(),
        )
    });
    Ok(json!({"entries": entries, "truncated": truncated}))
}

pub fn preview(ws: &Workspace, root: &Path, selection: &Selection) -> Result<Value> {
    relative(&selection.path)?;
    anyhow::ensure!(!selection.path.is_empty(), "select a file");
    let bytes = if let Some(id) = &selection.snapshot {
        let commit = revision(ws, root, id)?;
        // Literal pathspec: wildcard filenames must not select other artifacts.
        let listing = ws
            .git(root)
            .args([
                "--literal-pathspecs",
                "ls-tree",
                "-lz",
                &commit,
                "--",
                &selection.path,
            ])
            .output()?;
        let header = listing
            .stdout
            .split(|b| *b == b'\t')
            .next()
            .context("missing file")?;
        let fields: Vec<_> = std::str::from_utf8(header)?.split_whitespace().collect();
        anyhow::ensure!(
            fields.len() == 4 && matches!(fields[0], "100644" | "100755") && fields[1] == "blob",
            "select a regular snapshot file"
        );
        let size: u64 = fields[3].parse()?;
        if size > PREVIEW_BYTES {
            return Ok(json!({"kind":"large", "size":size}));
        }
        let result = ws
            .git(root)
            .args(["cat-file", "blob", fields[2]])
            .output()?;
        anyhow::ensure!(result.status.success(), "cannot read snapshot blob");
        result.stdout
    } else {
        let path = Path::new(&selection.path);
        let parent = path
            .parent()
            .context("file has no parent")?
            .to_str()
            .context("invalid path")?;
        let dir = live_directory(ws, root, parent)?;
        let file = open_child(&dir, path.file_name().context("select a file")?, false)?;
        let metadata = file.metadata()?;
        anyhow::ensure!(metadata.is_file(), "select a regular file");
        if metadata.len() > PREVIEW_BYTES {
            return Ok(json!({"kind":"large", "size":metadata.len()}));
        }
        let mut bytes = Vec::new();
        file.take(PREVIEW_BYTES + 1).read_to_end(&mut bytes)?;
        bytes
    };
    if bytes.len() as u64 > PREVIEW_BYTES {
        bail!("file grew beyond preview limit; refresh");
    }
    let mime = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else {
        None
    };
    if let Some(mime) = mime {
        return Ok(
            json!({"kind":"image", "size":bytes.len(), "url":format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(&bytes))}),
        );
    }
    if !bytes.contains(&0) {
        if let Ok(text) = std::str::from_utf8(&bytes) {
            return Ok(json!({"kind":"text", "size":bytes.len(), "text":text}));
        }
    }
    Ok(json!({"kind":"binary", "size":bytes.len()}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn previews_history_without_following_links_or_rendering_active_content() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        fs::create_dir_all(source.join("nested")).unwrap();
        fs::write(source.join("nested/report.txt"), "before").unwrap();
        fs::write(source.join("page.html"), "<script>alert(1)</script>").unwrap();
        let root = dir.path().join("state");
        let ws = Workspace::create(&root, 0, "run", "team", None, &source).unwrap();
        let id = ws.snapshots(&root).unwrap()[0].id.clone();
        fs::write(ws.worktree(&root).join("nested/report.txt"), "after").unwrap();
        let mut selection = Selection {
            path: "nested/report.txt".into(),
            snapshot: Some(id.clone()),
        };
        assert_eq!(preview(&ws, &root, &selection).unwrap()["text"], "before");
        selection.snapshot = None;
        assert_eq!(preview(&ws, &root, &selection).unwrap()["text"], "after");
        selection.path = "page.html".into();
        assert_eq!(preview(&ws, &root, &selection).unwrap()["kind"], "text");
        std::os::unix::fs::symlink(&source, ws.worktree(&root).join("escape")).unwrap();
        for path in [
            "../source/page.html",
            "/etc/passwd",
            ".git",
            "escape/page.html",
        ] {
            selection.path = path.into();
            assert!(preview(&ws, &root, &selection).is_err());
        }
        selection.path = "nested".into();
        selection.snapshot = Some(id);
        assert_eq!(
            browse(&ws, &root, &selection).unwrap()["entries"][0]["name"],
            "report.txt"
        );
        // Retained parent handles survive path replacement without following the new link.
        let held = live_directory(&ws, &root, "nested").unwrap();
        fs::rename(
            ws.worktree(&root).join("nested"),
            ws.worktree(&root).join("saved"),
        )
        .unwrap();
        std::os::unix::fs::symlink(source.join("nested"), ws.worktree(&root).join("nested"))
            .unwrap();
        let mut text = String::new();
        open_child(&held, std::ffi::OsStr::new("report.txt"), false)
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        assert_eq!(text, "after");
        selection.snapshot = None;
        assert!(browse(&ws, &root, &selection).is_err());
        selection.path = "nested/report.txt".into();
        assert!(preview(&ws, &root, &selection).is_err());
        let fifo = ws.worktree(&root).join("fifo");
        assert!(std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success());
        selection.path = "fifo".into();
        assert!(preview(&ws, &root, &selection).is_err());
        selection.path = "".into();
        selection.snapshot = None;
        assert!(!browse(&ws, &root, &selection).unwrap()["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["name"] == ".git"));
    }
}

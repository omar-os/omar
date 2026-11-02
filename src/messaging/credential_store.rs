//! Shared private-file storage for messaging credentials.
use anyhow::{ensure, Context, Result};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
};
fn private_directory(path: &Path, create: bool) -> Result<()> {
    if create {
        fs::create_dir_all(path.parent().context("Invalid credential path")?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            match fs::DirBuilder::new().mode(0o700).create(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }
        #[cfg(not(unix))]
        anyhow::bail!("Saved messaging credentials currently require Unix file permissions; use environment variables");
    }
    let meta =
        fs::symlink_metadata(path).context("Cannot inspect messaging credentials directory")?;
    ensure!(
        meta.is_dir() && !meta.file_type().is_symlink(),
        "messaging credentials directory must not be a symlink"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        ensure!(
            meta.uid() == unsafe { libc::geteuid() },
            "messaging credentials directory has another owner"
        );
        if create {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        } else {
            ensure!(
                meta.permissions().mode() & 0o077 == 0,
                "messaging credentials directory must be owner-only (0700)"
            );
        }
    }
    Ok(())
}

pub(super) fn save<T: serde::Serialize>(path: &Path, credentials: &T) -> Result<()> {
    let parent = path
        .parent()
        .context("Invalid messaging credentials path")?;
    private_directory(parent, true)?;
    let temporary = parent.join(format!(".messaging-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = crate::paths::create_private_file(&temporary)?;
        file.write_all(&serde_json::to_vec(credentials)?)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub(super) fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    private_directory(
        path.parent()
            .context("Invalid messaging credentials path")?,
        false,
    )?;
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options
        .open(path)
        .context("Cannot open saved messaging credentials")?;
    let meta = file.metadata()?;
    ensure!(
        meta.is_file() && meta.len() <= 16 * 1024,
        "Invalid messaging credentials file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        ensure!(
            meta.uid() == unsafe { libc::geteuid() }
                && meta.permissions().mode() & 0o077 == 0
                && meta.nlink() == 1,
            "messaging credentials file must have one owner-only link (0600)"
        );
    }
    let mut bytes = Vec::new();
    file.take(16 * 1024 + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 16 * 1024,
        "Invalid messaging credentials file"
    );
    let credentials: T = serde_json::from_slice(&bytes).map_err(|_| {
        anyhow::anyhow!("Invalid saved messaging credentials; run the messaging login command")
    })?;
    Ok(credentials)
}

pub(super) fn remove(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .context("Invalid messaging credentials path")?;
    if !parent.exists() {
        return Ok(());
    }
    private_directory(parent, false)?;
    match fs::remove_file(path) {
        Ok(()) => {
            fs::File::open(parent)?.sync_all()?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

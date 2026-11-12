//! One local Slack installation. Secrets never enter workflow config or logs.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize)]
pub struct Credentials {
    bot_token: String,
    app_token: String,
}

impl Credentials {
    fn validate(&self) -> Result<()> {
        for (token, prefix) in [(&self.bot_token, "xoxb-"), (&self.app_token, "xapp-")] {
            ensure!(token.starts_with(prefix) && token.len() > prefix.len() && token.len() <= 4096
                && token.is_ascii() && !token.bytes().any(|b| b.is_ascii_whitespace() || b.is_ascii_control()),
                "Invalid Slack token format; run `omar messaging slack login` to configure both tokens");
        }
        Ok(())
    }
    pub fn into_tokens(self) -> (String, String) {
        (self.bot_token, self.app_token)
    }
}

fn path() -> Result<PathBuf> {
    Ok(dirs::config_dir()
        .context("Cannot locate user configuration directory")?
        .join("omar/credentials/slack.json"))
}

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
        anyhow::bail!("Saved Slack credentials currently require Unix file permissions; use environment variables");
    }
    let meta = fs::symlink_metadata(path).context("Cannot inspect Slack credentials directory")?;
    ensure!(
        meta.is_dir() && !meta.file_type().is_symlink(),
        "Slack credentials directory must not be a symlink"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        ensure!(
            meta.uid() == unsafe { libc::geteuid() },
            "Slack credentials directory has another owner"
        );
        if create {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        } else {
            ensure!(
                meta.permissions().mode() & 0o077 == 0,
                "Slack credentials directory must be owner-only (0700)"
            );
        }
    }
    Ok(())
}

fn save(path: &Path, credentials: &Credentials) -> Result<()> {
    credentials.validate()?;
    let parent = path.parent().context("Invalid Slack credentials path")?;
    private_directory(parent, true)?;
    let temporary = parent.join(format!(".slack-{}.tmp", uuid::Uuid::new_v4()));
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

fn read(path: &Path) -> Result<Credentials> {
    private_directory(
        path.parent().context("Invalid Slack credentials path")?,
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
        .context("Cannot open saved Slack credentials")?;
    let meta = file.metadata()?;
    ensure!(
        meta.is_file() && meta.len() <= 16 * 1024,
        "Invalid Slack credentials file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        ensure!(
            meta.uid() == unsafe { libc::geteuid() }
                && meta.permissions().mode() & 0o077 == 0
                && meta.nlink() == 1,
            "Slack credentials file must have one owner-only link (0600)"
        );
    }
    let mut bytes = Vec::new();
    file.take(16 * 1024 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 16 * 1024, "Invalid Slack credentials file");
    let credentials: Credentials = serde_json::from_slice(&bytes).map_err(|_| {
        anyhow::anyhow!("Invalid saved Slack credentials; run `omar messaging slack login`")
    })?;
    credentials.validate()?;
    Ok(credentials)
}

fn resolve(path: &Path, bot: Option<String>, app: Option<String>) -> Result<Credentials> {
    let saved = if bot.is_none() || app.is_none() {
        Some(read(path).with_context(|| "Configure Slack once with `omar messaging slack login`, or set SLACK_BOT_TOKEN and SLACK_APP_TOKEN")?)
    } else {
        None
    };
    let credentials = Credentials {
        bot_token: bot
            .or_else(|| saved.as_ref().map(|s| s.bot_token.clone()))
            .unwrap_or_default(),
        app_token: app
            .or_else(|| saved.map(|s| s.app_token))
            .unwrap_or_default(),
    };
    credentials.validate()?;
    Ok(credentials)
}

pub fn load() -> Result<Credentials> {
    resolve(
        &path()?,
        std::env::var("SLACK_BOT_TOKEN").ok(),
        std::env::var("SLACK_APP_TOKEN").ok(),
    )
}

pub fn login() -> Result<()> {
    eprintln!(
        "Save Slack tokens locally (hidden input; file protected by Linux user permissions)."
    );
    let credentials = Credentials {
        bot_token: rpassword::prompt_password("Slack bot token (xoxb-): ")?,
        app_token: rpassword::prompt_password("Slack app token (xapp-): ")?,
    };
    let path = path()?;
    save(&path, &credentials)?;
    println!(
        "Saved Slack credentials to {}. Slack validates them when the connector starts.",
        path.display()
    );
    println!("Environment variables override saved tokens. Run `omar messaging slack logout` to remove the saved copy.");
    Ok(())
}

fn remove(path: &Path) -> Result<()> {
    let parent = path.parent().context("Invalid Slack credentials path")?;
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
pub fn logout() -> Result<()> {
    remove(&path()?)?;
    println!("Removed saved Slack credentials. Exported environment tokens and running connectors are unchanged; Slack tokens are not revoked.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tokens() -> Credentials {
        Credentials {
            bot_token: "xoxb-test-bot".into(),
            app_token: "xapp-test-app".into(),
        }
    }
    fn location(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("credentials/slack.json")
    }

    #[test]
    fn saved_credentials_roundtrip_and_rotation_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = location(&dir);
        save(&path, &tokens()).unwrap();
        assert_eq!(read(&path).unwrap().bot_token, "xoxb-test-bot");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let rotated = Credentials {
            bot_token: "xoxb-rotated".into(),
            ..tokens()
        };
        save(&path, &rotated).unwrap();
        assert_eq!(read(&path).unwrap().bot_token, "xoxb-rotated");
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }
    #[test]
    fn environment_overrides_saved_tokens_individually_and_as_pair() {
        let dir = tempfile::tempdir().unwrap();
        let path = location(&dir);
        let pair = resolve(
            &path,
            Some("xoxb-environment".into()),
            Some("xapp-environment".into()),
        )
        .unwrap();
        assert_eq!(pair.app_token, "xapp-environment");
        save(&path, &tokens()).unwrap();
        assert_eq!(
            resolve(&path, Some("xoxb-override".into()), None)
                .unwrap()
                .app_token,
            "xapp-test-app"
        );
        assert_eq!(
            resolve(&path, None, Some("xapp-override".into()))
                .unwrap()
                .bot_token,
            "xoxb-test-bot"
        );
        assert!(resolve(&path, Some("".into()), None).is_err());
    }
    #[test]
    fn missing_credentials_explain_one_time_setup() {
        let dir = tempfile::tempdir().unwrap();
        assert!(resolve(&location(&dir), None, None)
            .err()
            .unwrap()
            .to_string()
            .contains("slack login"));
    }
    #[test]
    fn invalid_or_multiline_tokens_are_not_saved_or_echoed_in_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = location(&dir);
        for token in [
            "private-invalid",
            "xoxb-",
            "xoxb-contains secret",
            "xoxb-line\nbreak",
        ] {
            let credentials = Credentials {
                bot_token: token.into(),
                ..tokens()
            };
            let error = save(&path, &credentials).unwrap_err();
            assert!(!error.to_string().contains(token));
            assert!(!path.exists());
        }
    }
    #[test]
    fn corrupt_store_errors_do_not_echo_contents() {
        let dir = tempfile::tempdir().unwrap();
        let path = location(&dir);
        save(&path, &tokens()).unwrap();
        fs::write(&path, "secret malformed content").unwrap();
        assert!(!format!("{:#}", read(&path).err().unwrap()).contains("secret malformed content"));
    }
    #[test]
    fn refuses_symlink_directory_file_and_public_permissions() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let dir = tempfile::tempdir().unwrap();
        let path = location(&dir);
        save(&path, &tokens()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read(&path).is_err());
        fs::remove_file(&path).unwrap();
        let other = dir.path().join("other");
        fs::write(&other, "sentinel").unwrap();
        symlink(&other, &path).unwrap();
        assert!(read(&path).is_err());
        remove(&path).unwrap();
        assert_eq!(fs::read_to_string(&other).unwrap(), "sentinel");
        fs::remove_dir(path.parent().unwrap()).unwrap();
        symlink(dir.path(), path.parent().unwrap()).unwrap();
        assert!(save(&path, &tokens()).is_err());
    }
    #[test]
    fn logout_is_idempotent_and_does_not_remove_other_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = location(&dir);
        remove(&path).unwrap();
        save(&path, &tokens()).unwrap();
        let other = path.parent().unwrap().join("other.json");
        fs::write(&other, "other").unwrap();
        remove(&path).unwrap();
        remove(&path).unwrap();
        assert!(!path.exists());
        assert!(other.exists());
    }
}

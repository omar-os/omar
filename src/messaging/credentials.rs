//! Local messaging credentials. Secrets never enter workflow config or logs.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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

fn save(path: &Path, credentials: &Credentials) -> Result<()> {
    credentials.validate()?;
    super::credential_store::save(path, credentials)
}
fn read(path: &Path) -> Result<Credentials> {
    let credentials: Credentials = super::credential_store::read(path)?;
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
    super::credential_store::remove(path)
}
pub fn logout() -> Result<()> {
    remove(&path()?)?;
    println!("Removed saved Slack credentials. Exported environment tokens and running connectors are unchanged; Slack tokens are not revoked.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
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

#[derive(Serialize, Deserialize)]
struct DiscordCredentials {
    token: String,
}
fn discord_path() -> Result<PathBuf> {
    Ok(dirs::config_dir()
        .context("Cannot locate user configuration directory")?
        .join("omar/credentials/discord.json"))
}
fn validate_discord(token: &str) -> Result<()> {
    ensure!(
        !token.is_empty()
            && token.len() <= 4096
            && token.is_ascii()
            && !token
                .bytes()
                .any(|b| b.is_ascii_whitespace() || b.is_ascii_control()),
        "Invalid Discord token format; run `omar messaging discord login`"
    );
    Ok(())
}
pub(super) fn discord_load() -> Result<String> {
    let token = match std::env::var("DISCORD_BOT_TOKEN") {
        Ok(token) => token,
        Err(std::env::VarError::NotPresent) => {
            let saved: DiscordCredentials = super::credential_store::read(&discord_path()?)
                .context(
                "Configure Discord with `omar messaging discord login`, or set DISCORD_BOT_TOKEN",
            )?;
            saved.token
        }
        Err(_) => anyhow::bail!("Invalid DISCORD_BOT_TOKEN"),
    };
    validate_discord(&token)?;
    Ok(token)
}
pub(super) fn discord_login() -> Result<()> {
    eprintln!("Save Discord bot token locally (hidden input; protected by Unix file permissions).");
    let token = rpassword::prompt_password("Discord bot token: ")?;
    validate_discord(&token)?;
    super::credential_store::save(&discord_path()?, &DiscordCredentials { token })?;
    println!("Saved Discord credentials. Discord validates the token at connector startup. DISCORD_BOT_TOKEN overrides the saved token.");
    Ok(())
}
pub(super) fn discord_logout() -> Result<()> {
    super::credential_store::remove(&discord_path()?)?;
    println!("Removed saved Discord credentials. Environment tokens and running connectors are unchanged; the token is not revoked.");
    Ok(())
}

#[cfg(test)]
mod discord_tests {
    use super::*;
    #[test]
    fn discord_store_is_private_and_does_not_overwrite_slack() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let slack = dir.path().join("credentials/slack.json");
        let discord = dir.path().join("credentials/discord.json");
        save(
            &slack,
            &Credentials {
                bot_token: "xoxb-fixture".into(),
                app_token: "xapp-fixture".into(),
            },
        )
        .unwrap();
        let original = std::fs::read(&slack).unwrap();
        super::super::credential_store::save(
            &discord,
            &DiscordCredentials {
                token: "fixture-discord-token".into(),
            },
        )
        .unwrap();
        let saved: DiscordCredentials = super::super::credential_store::read(&discord).unwrap();
        assert_eq!(saved.token, "fixture-discord-token");
        assert_eq!(
            std::fs::metadata(&discord).unwrap().permissions().mode() & 0o777,
            0o600
        );
        super::super::credential_store::remove(&discord).unwrap();
        assert_eq!(std::fs::read(&slack).unwrap(), original);
        for token in ["", "private token", "private\nsecret"] {
            let error = validate_discord(token).unwrap_err();
            if !token.is_empty() {
                assert!(!error.to_string().contains(token));
            }
        }
    }
}

pub(super) fn ensure(platform: &str) -> Result<()> {
    use std::io::IsTerminal;
    let result = if platform == "slack" {
        load().map(|_| ())
    } else {
        discord_load().map(|_| ())
    };
    match result {
        Ok(()) => Ok(()),
        Err(error) if !std::io::stdin().is_terminal() => Err(error),
        Err(_) => {
            if platform == "slack" {
                login()?;
                load().map(|_| ())
            } else {
                discord_login()?;
                discord_load().map(|_| ())
            }
        }
    }
}

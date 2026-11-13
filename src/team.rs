//! A session's team: which accounts (see `accounts`) are members, which of
//! them administer it, the API keys its admins provide for everyone's
//! agents, and the room its members talk in. All live in the session's directory, so a session carries its
//! team with it the way a Slack workspace does.
//!
//! Privacy is enforced by Mission Control, not by the operating system: every
//! member's agents run as the one Unix user that runs OMAR, and can read
//! whatever that user can.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Member,
    Admin,
}

/// Who may see a chat and the workflows it runs. Only its owner changes it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    /// The owner only.
    #[default]
    Private,
    /// Every member of the session.
    Team,
    /// Members, and anyone on the session's view port.
    Public,
}

impl Visibility {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "private" => Some(Self::Private),
            "team" => Some(Self::Team),
            "public" => Some(Self::Public),
            _ => None,
        }
    }
}

/// Provider keys an admin may hand the session's agents.
pub const KEY_VARS: &[&str] = &["ANTHROPIC_API_KEY", "OPENAI_API_KEY"];

fn members_path(session: &Path) -> PathBuf {
    session.join("members.json")
}

fn keys_path(session: &Path) -> PathBuf {
    session.join("team-keys.json")
}

/// The session's members and their roles; empty when it has no team.
pub fn members(session: &Path) -> Result<BTreeMap<String, Role>> {
    read_map(&members_path(session))
}

pub fn role(session: &Path, user: &str) -> Option<Role> {
    members(session).ok()?.get(user).copied()
}

/// Add an existing account, or change its role.
pub fn add_member(session: &Path, home: &Path, user: &str, role: Role) -> Result<()> {
    if crate::accounts::Accounts::at(home).get(user)?.is_none() {
        bail!("no account '{user}'; omar user add {user} creates it");
    }
    change(&members_path(session), |members| {
        if role == Role::Member && is_last_admin(members, user) {
            bail!("'{user}' is the session's last admin");
        }
        members.insert(user.to_string(), role);
        Ok(())
    })
}

pub fn remove_member(session: &Path, user: &str) -> Result<()> {
    change(
        &members_path(session),
        |members: &mut BTreeMap<String, Role>| {
            if !members.contains_key(user) {
                bail!("'{user}' is not a member");
            }
            if is_last_admin(members, user) {
                bail!("'{user}' is the session's last admin");
            }
            members.remove(user);
            Ok(())
        },
    )
}

fn is_last_admin(members: &BTreeMap<String, Role>, user: &str) -> bool {
    members.get(user) == Some(&Role::Admin)
        && members
            .values()
            .filter(|role| **role == Role::Admin)
            .count()
            == 1
}

/// Set one of [`KEY_VARS`] for agents started from now on; empty clears it.
pub fn set_key(session: &Path, var: &str, value: &str) -> Result<()> {
    if !KEY_VARS.contains(&var) {
        bail!(
            "{var} is not a provider key; use one of {}",
            KEY_VARS.join(", ")
        );
    }
    let value = value.trim();
    change(
        &keys_path(session),
        |keys: &mut BTreeMap<String, String>| {
            if value.is_empty() {
                keys.remove(var);
            } else {
                keys.insert(var.to_string(), value.to_string());
            }
            Ok(())
        },
    )
}

/// The keys an admin set, as their admins see them: `sk-…f3a9`.
pub fn masked_keys(session: &Path) -> Result<BTreeMap<String, String>> {
    Ok(read_map::<String>(&keys_path(session))?
        .into_iter()
        .map(|(var, value)| (var, mask(&value)))
        .collect())
}

/// What a new agent pane in the session `OMAR_STATE_DIR` names is started
/// with. Read at every launch, so a key an admin sets reaches the next agent.
pub fn agent_environment() -> Vec<(String, String)> {
    std::env::var_os("OMAR_STATE_DIR")
        .and_then(|dir| read_map::<String>(&keys_path(Path::new(&dir))).ok())
        .map(|keys| keys.into_iter().collect())
        .unwrap_or_default()
}

/// One line in a session's team room, which every member reads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoomMessage {
    /// Counts up from 1, so a client asks only for what is newer.
    pub seq: u64,
    pub user: String,
    pub text: String,
    /// Unix seconds.
    pub at: u64,
}

/// The room keeps this many messages; older ones are dropped.
pub const ROOM_KEEP: usize = 500;
pub const ROOM_MAX_CHARS: usize = 2000;

fn room_path(session: &Path) -> PathBuf {
    session.join("room.json")
}

/// Messages after `after`, oldest first.
pub fn room(session: &Path, after: u64) -> Result<Vec<RoomMessage>> {
    Ok(read_room(&room_path(session))?
        .into_iter()
        .filter(|message| message.seq > after)
        .collect())
}

/// The newest message's `seq`; 0 for an empty room.
pub fn room_seq(session: &Path) -> u64 {
    read_room(&room_path(session))
        .ok()
        .and_then(|messages| messages.last().map(|message| message.seq))
        .unwrap_or(0)
}

pub fn post(session: &Path, user: &str, text: &str) -> Result<RoomMessage> {
    let text = text.trim();
    if text.is_empty() {
        bail!("say something");
    }
    if text.chars().count() > ROOM_MAX_CHARS {
        bail!("keep it under {ROOM_MAX_CHARS} characters");
    }
    let path = room_path(session);
    let _lock = crate::accounts::Lock::acquire(&path)?;
    let mut messages = read_room(&path)?;
    let message = RoomMessage {
        seq: messages.last().map_or(1, |last| last.seq + 1),
        user: user.to_string(),
        text: text.to_string(),
        at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs()),
    };
    messages.push(message.clone());
    let excess = messages.len().saturating_sub(ROOM_KEEP);
    messages.drain(..excess);
    crate::accounts::write_private_json(&path, &messages)?;
    Ok(message)
}

fn read_room(path: &Path) -> Result<Vec<RoomMessage>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("cannot read {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error).with_context(|| format!("cannot read {}", path.display())),
    }
}

fn mask(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= 8 {
        return "…".into();
    }
    let head: String = chars[..3].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}…{tail}")
}

fn read_map<V: for<'de> Deserialize<'de>>(path: &Path) -> Result<BTreeMap<String, V>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("cannot read {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(error) => Err(error).with_context(|| format!("cannot read {}", path.display())),
    }
}

fn change<V: Serialize + for<'de> Deserialize<'de>>(
    path: &Path,
    apply: impl FnOnce(&mut BTreeMap<String, V>) -> Result<()>,
) -> Result<()> {
    let _lock = crate::accounts::Lock::acquire(path)?;
    let mut map = read_map(path)?;
    apply(&mut map)?;
    crate::accounts::write_private_json(path, &map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn members_are_existing_accounts_and_a_session_keeps_an_admin() {
        let home = tempfile::tempdir().unwrap();
        let session = home.path().join("sessions/demo");
        let accounts = crate::accounts::Accounts::at(home.path());
        accounts.add("alice").unwrap();
        accounts.add("bob").unwrap();

        let missing = add_member(&session, home.path(), "carol", Role::Member);
        assert!(missing
            .unwrap_err()
            .to_string()
            .contains("no account 'carol'"));
        add_member(&session, home.path(), "alice", Role::Admin).unwrap();
        add_member(&session, home.path(), "bob", Role::Member).unwrap();
        assert_eq!(role(&session, "alice"), Some(Role::Admin));
        assert_eq!(role(&session, "bob"), Some(Role::Member));
        assert_eq!(role(&session, "carol"), None);

        let demote = add_member(&session, home.path(), "alice", Role::Member);
        assert!(demote.unwrap_err().to_string().contains("last admin"));
        assert!(remove_member(&session, "alice").is_err());
        remove_member(&session, "bob").unwrap();
        assert_eq!(role(&session, "bob"), None);
    }

    #[test]
    fn keys_are_masked_for_admins_and_given_whole_to_agents() {
        let home = tempfile::tempdir().unwrap();
        let session = home.path();
        assert!(set_key(session, "PATH", "/tmp").is_err());
        set_key(session, "ANTHROPIC_API_KEY", " sk-ant-0123456789f3a9 ").unwrap();
        assert_eq!(
            masked_keys(session).unwrap()["ANTHROPIC_API_KEY"],
            "sk-…f3a9"
        );
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(keys_path(session))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let keys: BTreeMap<String, String> = read_map(&keys_path(session)).unwrap();
        assert_eq!(keys["ANTHROPIC_API_KEY"], "sk-ant-0123456789f3a9");
        set_key(session, "ANTHROPIC_API_KEY", "").unwrap();
        assert!(masked_keys(session).unwrap().is_empty());
    }

    #[test]
    fn the_room_keeps_the_newest_messages_in_order() {
        let session = tempfile::tempdir().unwrap();
        let session = session.path();
        assert_eq!(room_seq(session), 0);
        assert!(post(session, "alice", "  ").is_err());
        assert!(post(session, "alice", &"x".repeat(ROOM_MAX_CHARS + 1)).is_err());
        for n in 1..=ROOM_KEEP + 2 {
            post(session, "alice", &format!("hi {n}")).unwrap();
        }
        let all = room(session, 0).unwrap();
        assert_eq!(all.len(), ROOM_KEEP);
        assert_eq!(all[0].text, "hi 3");
        assert_eq!(room_seq(session), (ROOM_KEEP + 2) as u64);
        let newer = room(session, room_seq(session) - 1).unwrap();
        assert_eq!(newer.len(), 1);
        assert_eq!(newer[0].text, format!("hi {}", ROOM_KEEP + 2));
    }
}

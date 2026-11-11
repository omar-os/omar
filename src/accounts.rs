//! Server-wide accounts for the team gateway: one per person, shared by every
//! session they are a member of (see `team`). Like `/etc/shadow`, the store
//! holds only Argon2id hashes, so no one, the admin included, can read a
//! password back. An admin hands out a temporary password once; the account
//! must change it before the gateway lets it into any session.
//!
//! Logins are random tokens held by the browser; only their SHA-256 is stored,
//! so the file alone cannot be replayed as a cookie.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use argon2::password_hash::rand_core::{OsRng, RngCore};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MIN_PASSWORD: usize = 12;
/// How long a login lasts before the person signs in again.
pub const LOGIN_SECONDS: u64 = 14 * 24 * 60 * 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    /// PHC string: algorithm, parameters, salt and hash, never the password.
    hash: String,
    /// Set by `add` and `reset`: the password is one an admin has seen.
    pub must_change: bool,
    #[serde(default)]
    pub disabled: bool,
    pub created_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Login {
    user: String,
    expires_at: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Store {
    version: u32,
    accounts: BTreeMap<String, Account>,
    /// SHA-256 of each login token, hex.
    #[serde(default)]
    logins: BTreeMap<String, Login>,
}

/// The accounts file under one OMAR home. Every change holds an exclusive
/// lock, so the CLI and a running gateway never lose each other's writes.
pub struct Accounts {
    path: PathBuf,
}

impl Accounts {
    pub fn at(home: &Path) -> Self {
        Self {
            path: home.join("accounts.json"),
        }
    }

    /// Create an account; returns its temporary password.
    pub fn add(&self, name: &str) -> Result<String> {
        validate_user(name)?;
        self.change(|store| {
            if store.accounts.contains_key(name) {
                bail!(
                    "account '{name}' already exists; omar user reset {name} issues a new password"
                );
            }
            let password = temporary_password();
            store.accounts.insert(
                name.to_string(),
                Account {
                    hash: hash(&password)?,
                    must_change: true,
                    disabled: false,
                    created_at: now(),
                },
            );
            Ok(password)
        })
    }

    /// Replace a forgotten password with a new temporary one, and sign the
    /// account out everywhere.
    pub fn reset(&self, name: &str) -> Result<String> {
        self.change(|store| {
            let account = store.account_mut(name)?;
            let password = temporary_password();
            account.hash = hash(&password)?;
            account.must_change = true;
            store.logins.retain(|_, login| login.user != name);
            Ok(password)
        })
    }

    pub fn set_disabled(&self, name: &str, disabled: bool) -> Result<()> {
        self.change(|store| {
            store.account_mut(name)?.disabled = disabled;
            if disabled {
                store.logins.retain(|_, login| login.user != name);
            }
            Ok(())
        })
    }

    pub fn remove(&self, name: &str) -> Result<()> {
        self.change(|store| {
            store
                .accounts
                .remove(name)
                .with_context(|| format!("no account '{name}'; see omar user ls"))?;
            store.logins.retain(|_, login| login.user != name);
            Ok(())
        })
    }

    pub fn list(&self) -> Result<BTreeMap<String, Account>> {
        Ok(self.load()?.accounts)
    }

    pub fn get(&self, name: &str) -> Result<Option<Account>> {
        Ok(self.load()?.accounts.remove(name))
    }

    /// The account, if `password` is its password and it may sign in.
    pub fn verify(&self, name: &str, password: &str) -> Result<Option<Account>> {
        let account = self.get(name)?;
        // Hash even for an unknown name, so timing does not tell which exist.
        let stored = account.as_ref().map_or(DUMMY_HASH, |a| a.hash.as_str());
        let matches = check(stored, password);
        Ok(account.filter(|account| matches && !account.disabled))
    }

    /// Change a password, given the current one. Ends every other login of
    /// the account; `keep` is the token of the login asking, if any.
    pub fn change_password(
        &self,
        name: &str,
        current: &str,
        new: &str,
        keep: Option<&str>,
    ) -> Result<()> {
        if new.chars().count() < MIN_PASSWORD {
            bail!("a password has at least {MIN_PASSWORD} characters");
        }
        if new == current {
            bail!("choose a password different from the current one");
        }
        let hashed = hash(new)?;
        self.change(|store| {
            let account = store.account_mut(name)?;
            if account.disabled || !check(&account.hash, current) {
                bail!("the current password is wrong");
            }
            account.hash = hashed;
            account.must_change = false;
            let keep = keep.map(token_digest);
            store
                .logins
                .retain(|digest, login| login.user != name || Some(digest) == keep.as_ref());
            Ok(())
        })
    }

    /// Start a login for an account that just proved its password; returns
    /// the token the browser keeps.
    pub fn login(&self, name: &str) -> Result<String> {
        let mut bytes = [0u8; 32];
        OsRng.fill_bytes(&mut bytes);
        let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        let digest = token_digest(&token);
        self.change(|store| {
            store.account_mut(name)?;
            let now = now();
            store.logins.retain(|_, login| login.expires_at > now);
            store.logins.insert(
                digest,
                Login {
                    user: name.to_string(),
                    expires_at: now + LOGIN_SECONDS,
                },
            );
            Ok(())
        })?;
        Ok(token)
    }

    /// Who a login token belongs to, while it is live and the account usable.
    pub fn resolve(&self, token: &str) -> Result<Option<(String, Account)>> {
        let mut store = self.load()?;
        let Some(login) = store.logins.remove(&token_digest(token)) else {
            return Ok(None);
        };
        if login.expires_at <= now() {
            return Ok(None);
        }
        Ok(store
            .accounts
            .remove(&login.user)
            .filter(|account| !account.disabled)
            .map(|account| (login.user, account)))
    }

    pub fn logout(&self, token: &str) -> Result<()> {
        let digest = token_digest(token);
        self.change(|store| {
            store.logins.remove(&digest);
            Ok(())
        })
    }

    fn load(&self) -> Result<Store> {
        match fs::read(&self.path) {
            Ok(bytes) => {
                let store: Store = serde_json::from_slice(&bytes)
                    .with_context(|| format!("cannot read {}", self.path.display()))?;
                anyhow::ensure!(
                    store.version == 1,
                    "{} has version {}; this omar reads version 1",
                    self.path.display(),
                    store.version
                );
                Ok(store)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Store {
                version: 1,
                ..Store::default()
            }),
            Err(error) => {
                Err(error).with_context(|| format!("cannot read {}", self.path.display()))
            }
        }
    }

    fn change<T>(&self, apply: impl FnOnce(&mut Store) -> Result<T>) -> Result<T> {
        let _lock = Lock::acquire(&self.path)?;
        let mut store = self.load()?;
        let result = apply(&mut store)?;
        write_private_json(&self.path, &store)?;
        Ok(result)
    }
}

impl Store {
    fn account_mut(&mut self, name: &str) -> Result<&mut Account> {
        self.accounts
            .get_mut(name)
            .with_context(|| format!("no account '{name}'; see omar user ls"))
    }
}

/// Account names: what a person types to sign in, and what other members see.
pub fn validate_user(name: &str) -> Result<()> {
    anyhow::ensure!(
        !name.is_empty()
            && name.len() <= 32
            && name
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'_'),
        "account names use 1–32 lowercase letters, digits, '-' or '_'"
    );
    Ok(())
}

fn hash(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|error| anyhow::anyhow!("cannot hash password: {error}"))
}

fn check(stored: &str, password: &str) -> bool {
    PasswordHash::new(stored).is_ok_and(|hash| {
        Argon2::default()
            .verify_password(password.as_bytes(), &hash)
            .is_ok()
    })
}

/// An Argon2id hash of a password no one knows, checked against when the
/// name is unknown.
const DUMMY_HASH: &str =
    "$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHRzb21lc2FsdA$HlMrPLsBjXHdrYSXPQwjpIbHxnB8wNjNqMKbIFD2rFU";

/// 16 characters from 96 random bits, without look-alikes, so it survives
/// being read aloud or copied by hand.
fn temporary_password() -> String {
    const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyzACDEFGHJKLMNPQRTUVWXY346789";
    let mut bytes = [0u8; 16];
    OsRng.fill_bytes(&mut bytes);
    bytes
        .iter()
        .map(|b| ALPHABET[*b as usize % ALPHABET.len()] as char)
        .collect()
}

fn token_digest(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Replace `path` with `value` as JSON, mode 0600, atomically.
pub(crate) fn write_private_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut file = crate::paths::create_private_file(&tmp)?;
        file.write_all(&serde_json::to_vec_pretty(value)?)?;
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(tmp);
    }
    result.with_context(|| format!("cannot write {}", path.display()))
}

/// An exclusive `flock` on `<path>.lock`, released on drop.
pub(crate) struct Lock(File);

impl Lock {
    pub(crate) fn acquire(path: &Path) -> Result<Self> {
        let lock = path.with_extension("lock");
        if let Some(parent) = lock.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock)?;
        anyhow::ensure!(
            unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&file), libc::LOCK_EX) } == 0,
            "cannot lock {}",
            lock.display()
        );
        Ok(Self(file))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(std::os::fd::AsRawFd::as_raw_fd(&self.0), libc::LOCK_UN);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Accounts) {
        let home = tempfile::tempdir().unwrap();
        let accounts = Accounts::at(home.path());
        (home, accounts)
    }

    #[test]
    fn the_store_holds_a_hash_never_the_password() {
        let (home, accounts) = store();
        let temporary = accounts.add("alice").unwrap();
        let file = fs::read_to_string(home.path().join("accounts.json")).unwrap();
        assert!(
            !file.contains(&temporary),
            "temporary password stored in clear"
        );
        assert!(file.contains("$argon2id$"), "{file}");
        accounts
            .change_password("alice", &temporary, "a much longer secret", None)
            .unwrap();
        let file = fs::read_to_string(home.path().join("accounts.json")).unwrap();
        assert!(!file.contains("a much longer secret"));
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(home.path().join("accounts.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn a_temporary_password_must_be_changed_and_then_stops_working() {
        let (_home, accounts) = store();
        let temporary = accounts.add("alice").unwrap();
        assert_eq!(temporary.len(), 16);
        let account = accounts
            .verify("alice", &temporary)
            .unwrap()
            .expect("signs in");
        assert!(account.must_change);
        assert!(accounts.verify("alice", "wrong").unwrap().is_none());
        assert!(accounts.verify("nobody", &temporary).unwrap().is_none());

        let short = accounts.change_password("alice", &temporary, "short", None);
        assert!(short.unwrap_err().to_string().contains("at least 12"));
        let wrong = accounts.change_password("alice", "not it", "a much longer secret", None);
        assert!(wrong
            .unwrap_err()
            .to_string()
            .contains("current password is wrong"));
        accounts
            .change_password("alice", &temporary, "a much longer secret", None)
            .unwrap();
        assert!(accounts.verify("alice", &temporary).unwrap().is_none());
        let account = accounts
            .verify("alice", "a much longer secret")
            .unwrap()
            .unwrap();
        assert!(!account.must_change);
    }

    #[test]
    fn logins_resolve_until_logout_reset_disable_or_removal() {
        let (home, accounts) = store();
        let temporary = accounts.add("alice").unwrap();
        let token = accounts.login("alice").unwrap();
        let file = fs::read_to_string(home.path().join("accounts.json")).unwrap();
        assert!(!file.contains(&token), "login token stored in clear");
        assert_eq!(accounts.resolve(&token).unwrap().unwrap().0, "alice");
        assert!(accounts.resolve("forged").unwrap().is_none());

        accounts.logout(&token).unwrap();
        assert!(accounts.resolve(&token).unwrap().is_none());

        let token = accounts.login("alice").unwrap();
        accounts.set_disabled("alice", true).unwrap();
        assert!(accounts.resolve(&token).unwrap().is_none());
        assert!(accounts.verify("alice", &temporary).unwrap().is_none());
        accounts.set_disabled("alice", false).unwrap();

        let token = accounts.login("alice").unwrap();
        let reset = accounts.reset("alice").unwrap();
        assert_ne!(reset, temporary);
        assert!(accounts.resolve(&token).unwrap().is_none());

        let token = accounts.login("alice").unwrap();
        accounts.remove("alice").unwrap();
        assert!(accounts.resolve(&token).unwrap().is_none());
    }

    #[test]
    fn changing_a_password_keeps_only_the_login_that_asked() {
        let (_home, accounts) = store();
        let temporary = accounts.add("alice").unwrap();
        let here = accounts.login("alice").unwrap();
        let elsewhere = accounts.login("alice").unwrap();
        accounts
            .change_password("alice", &temporary, "a much longer secret", Some(&here))
            .unwrap();
        assert!(accounts.resolve(&here).unwrap().is_some());
        assert!(accounts.resolve(&elsewhere).unwrap().is_none());
    }

    #[test]
    fn names_are_validated_and_unique() {
        let (_home, accounts) = store();
        for bad in ["", "Alice", "a b", "../x", &"x".repeat(33)] {
            assert!(accounts.add(bad).is_err(), "{bad:?} accepted");
        }
        accounts.add("alice").unwrap();
        assert!(accounts
            .add("alice")
            .unwrap_err()
            .to_string()
            .contains("already exists"));
    }

    #[test]
    fn the_dummy_hash_parses_so_unknown_names_cost_a_real_check() {
        assert!(PasswordHash::new(DUMMY_HASH).is_ok());
    }
}

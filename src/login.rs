//! The operator's password for `omar gateway`, so Mission Control can be
//! reached from another machine without an SSH tunnel. Like `/etc/shadow`,
//! the store holds only an Argon2id hash: the password cannot be read back.
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
/// How long a login lasts before the browser signs in again.
pub const LOGIN_SECONDS: u64 = 14 * 24 * 60 * 60;

#[derive(Debug, Default, Serialize, Deserialize)]
struct Store {
    version: u32,
    /// PHC string: algorithm, parameters, salt and hash, never the password.
    hash: Option<String>,
    /// SHA-256 of each login token, hex, and when it expires.
    #[serde(default)]
    logins: BTreeMap<String, u64>,
}

/// `login.json` under one OMAR home. Every change holds an exclusive lock, so
/// `omar passwd` and a running gateway never lose each other's writes.
pub struct Login {
    path: PathBuf,
}

impl Login {
    pub fn at(home: &Path) -> Self {
        Self {
            path: home.join("login.json"),
        }
    }

    /// Set or replace the password; every browser signed in must sign in again.
    pub fn set_password(&self, password: &str) -> Result<()> {
        if password.chars().count() < MIN_PASSWORD {
            bail!("use at least {MIN_PASSWORD} characters");
        }
        let hash = hash(password)?;
        self.change(|store| {
            store.hash = Some(hash);
            store.logins.clear();
            Ok(())
        })
    }

    pub fn is_set(&self) -> Result<bool> {
        Ok(self.load()?.hash.is_some())
    }

    pub fn verify(&self, password: &str) -> Result<bool> {
        // Checked against a hash either way, so an unset password takes as
        // long to refuse as a wrong one.
        let stored = self.load()?.hash;
        let matches = check(stored.as_deref().unwrap_or(DUMMY_HASH), password);
        Ok(stored.is_some() && matches)
    }

    /// Start a login for a browser that just proved the password; returns the
    /// token it keeps.
    pub fn start(&self) -> Result<String> {
        let mut bytes = [0u8; 32];
        OsRng.fill_bytes(&mut bytes);
        let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        let digest = token_digest(&token);
        self.change(|store| {
            let now = now();
            store.logins.retain(|_, expires_at| *expires_at > now);
            store.logins.insert(digest, now + LOGIN_SECONDS);
            Ok(())
        })?;
        Ok(token)
    }

    /// Whether a token is a live login.
    pub fn resolve(&self, token: &str) -> Result<bool> {
        Ok(self
            .load()?
            .logins
            .get(&token_digest(token))
            .is_some_and(|expires_at| *expires_at > now()))
    }

    pub fn end(&self, token: &str) -> Result<()> {
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

    fn change(&self, apply: impl FnOnce(&mut Store) -> Result<()>) -> Result<()> {
        let _lock = Lock::acquire(&self.path)?;
        let mut store = self.load()?;
        apply(&mut store)?;
        write_private_json(&self.path, &store)
    }
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

/// An Argon2id hash of a password no one knows.
const DUMMY_HASH: &str =
    "$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHRzb21lc2FsdA$HlMrPLsBjXHdrYSXPQwjpIbHxnB8wNjNqMKbIFD2rFU";

fn token_digest(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Replace `path` with `value` as JSON, mode 0600, atomically.
fn write_private_json(path: &Path, value: &impl Serialize) -> Result<()> {
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
struct Lock(File);

impl Lock {
    fn acquire(path: &Path) -> Result<Self> {
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

/// Ask on the terminal without echoing; from a pipe, read one line, so
/// `printf '%s\n' "$PASSWORD" | omar passwd` works in scripts.
pub fn read_password(prompt: &str) -> Result<String> {
    use std::io::{BufRead, IsTerminal};
    let stdin = std::io::stdin();
    let mut line = String::new();
    if !stdin.is_terminal() {
        stdin.lock().read_line(&mut line)?;
        return Ok(line.trim_end_matches(['\r', '\n']).to_string());
    }
    eprint!("{prompt}");
    let fd = libc::STDIN_FILENO;
    let mut terminal: libc::termios = unsafe { std::mem::zeroed() };
    anyhow::ensure!(
        unsafe { libc::tcgetattr(fd, &mut terminal) } == 0,
        "cannot read the terminal's settings"
    );
    let saved = terminal;
    terminal.c_lflag &= !libc::ECHO;
    unsafe { libc::tcsetattr(fd, libc::TCSANOW, &terminal) };
    let read = stdin.lock().read_line(&mut line);
    unsafe { libc::tcsetattr(fd, libc::TCSANOW, &saved) };
    eprintln!();
    read?;
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_password_is_stored_hashed_and_replacing_it_signs_everyone_out() {
        let home = tempfile::tempdir().unwrap();
        let login = Login::at(home.path());
        assert!(!login.is_set().unwrap());
        assert!(!login.verify("").unwrap());
        assert!(login.set_password("too short").is_err());

        login.set_password("correct horse battery").unwrap();
        assert!(login.is_set().unwrap());
        assert!(login.verify("correct horse battery").unwrap());
        assert!(!login.verify("correct horse").unwrap());
        let stored = fs::read_to_string(&login.path).unwrap();
        assert!(stored.contains("$argon2id$"), "{stored}");
        assert!(!stored.contains("correct horse"), "{stored}");
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&login.path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);

        let token = login.start().unwrap();
        assert!(login.resolve(&token).unwrap());
        assert!(!stored.contains(&token));
        assert!(!login.resolve("forged").unwrap());
        let other = login.start().unwrap();
        login.end(&other).unwrap();
        assert!(!login.resolve(&other).unwrap());
        assert!(login.resolve(&token).unwrap());

        login.set_password("a different long one").unwrap();
        assert!(!login.resolve(&token).unwrap());
        assert!(!login.verify("correct horse battery").unwrap());
    }

    #[test]
    fn expired_logins_are_refused() {
        let home = tempfile::tempdir().unwrap();
        let login = Login::at(home.path());
        login.set_password("correct horse battery").unwrap();
        let token = login.start().unwrap();
        login
            .change(|store| {
                for expires_at in store.logins.values_mut() {
                    *expires_at = now() - 1;
                }
                Ok(())
            })
            .unwrap();
        assert!(!login.resolve(&token).unwrap());
    }
}

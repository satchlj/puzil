//! API keys at rest: the OS keychain (macOS) or Secret Service (Linux), with a 0600 JSON file
//! as the fallback. Only the API key is secret; account metadata lives in the global DB.
//!
//! Fallback rules:
//! - `PUZIL_DEV_SECRETS=file` forces the file, and `PUZIL_DEV_SECRETS=os` forces the OS store.
//! - Debug builds on macOS default to the file: every rebuild changes the ad-hoc signature,
//!   so the item ACL stops matching and SecurityAgent blocks on a modal password prompt.
//! - Linux without a Secret Service provider (no gnome-keyring or KeePassXC) uses the file.

use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use anyhow::{Context, Result};
use keyring_core::{CredentialStore, Entry, Error as KErr};

const SERVICE: &str = "com.satchlj.puzil";
/// The service name before 0.1.3. A key found only there moves to [`SERVICE`] when read.
const OLD_SERVICE: &str = "earth.benson.puzil";

pub enum Secrets {
    Os(Arc<CredentialStore>),
    File(PathBuf),
}

/// The keychain account for one Zulip login. Keyed by user id, not email: with restricted
/// email visibility `/users/me` reports a `user123@realm` address that differs from the
/// login email, and emails can change.
pub fn account_key(realm: &str, user_id: u64) -> String {
    format!("{user_id} @ {realm}")
}

impl Secrets {
    /// Picks the backend. `fallback` is the file used when the OS store is unavailable or
    /// disabled.
    pub fn open(fallback: PathBuf) -> Self {
        let forced = std::env::var("PUZIL_DEV_SECRETS").ok();
        let default_file = cfg!(all(debug_assertions, target_os = "macos"));
        if forced.as_deref() == Some("file") || (default_file && forced.as_deref() != Some("os")) {
            return Self::File(fallback);
        }
        match os_store() {
            Ok(s) => Self::Os(s),
            Err(e) => {
                tracing::warn!("OS secret store unavailable ({e}); using {}", fallback.display());
                Self::File(fallback)
            }
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Self::Os(s) => format!("os: {}", s.vendor()),
            Self::File(p) => format!("file: {}", p.display()),
        }
    }

    pub fn set(&self, account: &str, secret: &str) -> Result<()> {
        match self {
            Self::Os(s) => Ok(Entry::new_in_store(s, SERVICE, account)?.set_password(secret)?),
            Self::File(p) => {
                let mut m = read_file(p)?;
                m.insert(account.to_string(), secret.to_string());
                write_file(p, &m)
            }
        }
    }

    pub fn get(&self, account: &str) -> Result<Option<String>> {
        match self {
            Self::Os(s) => match Entry::new_in_store(s, SERVICE, account)?.get_password() {
                Ok(v) => Ok(Some(v)),
                Err(KErr::NoEntry) => {
                    let old = Entry::new_in_store(s, OLD_SERVICE, account)?;
                    match old.get_password() {
                        Ok(v) => {
                            // Moved, then forgotten under the old name; if the move fails the
                            // old copy stays, and the next read tries again.
                            if Entry::new_in_store(s, SERVICE, account)?.set_password(&v).is_ok() {
                                let _ = old.delete_credential();
                                tracing::info!("moved a key to the {SERVICE} keychain service");
                            }
                            Ok(Some(v))
                        }
                        Err(KErr::NoEntry) => Ok(None),
                        Err(e) => Err(e.into()),
                    }
                }
                Err(e) => Err(e.into()),
            },
            Self::File(p) => Ok(read_file(p)?.remove(account)),
        }
    }

    pub fn delete(&self, account: &str) -> Result<()> {
        match self {
            Self::Os(s) => match {
                // Signing out also clears a key not yet moved from the old name.
                let _ = Entry::new_in_store(s, OLD_SERVICE, account).map(|e| e.delete_credential());
                Entry::new_in_store(s, SERVICE, account)?.delete_credential()
            } {
                Ok(()) | Err(KErr::NoEntry) => Ok(()),
                Err(e) => Err(e.into()),
            },
            Self::File(p) => {
                let mut m = read_file(p)?;
                if m.remove(account).is_some() {
                    write_file(p, &m)?;
                }
                Ok(())
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn os_store() -> Result<Arc<CredentialStore>> {
    Ok(apple_native_keyring_store::keychain::Store::new()?)
}

#[cfg(target_os = "linux")]
fn os_store() -> Result<Arc<CredentialStore>> {
    // Store::new connects to the session bus and opens the default collection, so a missing
    // provider fails here rather than on first use.
    Ok(zbus_secret_service_keyring_store::Store::new()?)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn os_store() -> Result<Arc<CredentialStore>> {
    anyhow::bail!("no OS secret store on this platform")
}

fn read_file(p: &PathBuf) -> Result<BTreeMap<String, String>> {
    match std::fs::read(p) {
        Ok(b) => serde_json::from_slice(&b).with_context(|| format!("parsing {}", p.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(e.into()),
    }
}

fn write_file(p: &PathBuf, m: &BTreeMap<String, String>) -> Result<()> {
    use std::io::Write;
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Write a 0600 temp file and rename it over the old one, so a crash never leaves a
    // half-written or world-readable file.
    let tmp = p.with_extension("tmp");
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    let mut f = o.open(&tmp)?;
    f.write_all(&serde_json::to_vec_pretty(m)?)?;
    f.sync_all()?;
    std::fs::rename(&tmp, p)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    /// Touches the real OS keychain (a dummy item, removed again): run by hand with
    /// `cargo test -p puzil-zulip -- --ignored moves_keys`.
    #[test]
    #[ignore]
    fn moves_keys_from_the_old_service() {
        use super::*;
        let store = os_store().unwrap();
        let account = "0 @ https://puzil-migration-test.invalid";
        Entry::new_in_store(&store, OLD_SERVICE, account).unwrap().set_password("dummy").unwrap();
        let secrets = Secrets::Os(store.clone());
        assert_eq!(secrets.get(account).unwrap().as_deref(), Some("dummy"));
        assert!(matches!(Entry::new_in_store(&store, OLD_SERVICE, account).unwrap().get_password(), Err(KErr::NoEntry)));
        assert_eq!(Entry::new_in_store(&store, SERVICE, account).unwrap().get_password().unwrap(), "dummy");
        secrets.delete(account).unwrap();
        assert_eq!(secrets.get(account).unwrap(), None);
    }

    #[test]
    fn file_store_roundtrip_and_mode() {
        let dir = std::env::temp_dir().join(format!("puzil-secrets-test-{}", std::process::id()));
        let p = dir.join("secrets.json");
        let s = super::Secrets::File(p.clone());
        s.set("a", "1").unwrap();
        s.set("b", "2").unwrap();
        assert_eq!(s.get("a").unwrap().as_deref(), Some("1"));
        s.delete("a").unwrap();
        assert_eq!(s.get("a").unwrap(), None);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}

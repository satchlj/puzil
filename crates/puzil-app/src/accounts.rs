//! Signed-in accounts: `accounts.json` in the data directory, API keys in the secret store.

use std::path::PathBuf;

use anyhow::{Context, Result};
use puzil_zulip::{
    Credentials,
    secrets::{Secrets, account_key},
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Account {
    pub realm: String,
    pub email: String,
    pub user_id: u64,
    #[serde(default)]
    pub realm_name: Option<String>,
}

/// `PUZIL_DATA_DIR`, else the gitignored `reference/data/` in debug builds, else the
/// platform's per-user application data directory.
pub fn data_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("PUZIL_DATA_DIR") {
        return d.into();
    }
    if cfg!(debug_assertions) {
        return PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../reference/data");
    }
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Puzil")
    } else {
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".local/share")).join("puzil")
    }
}

/// Where logs go: `~/Library/Logs/Puzil` on macOS, `<data dir>/logs` elsewhere and in
/// debug builds.
pub fn log_dir() -> PathBuf {
    if cfg!(all(target_os = "macos", not(debug_assertions))) && std::env::var_os("PUZIL_DATA_DIR").is_none() {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        return home.join("Library/Logs/Puzil");
    }
    data_dir().join("logs")
}

pub fn secrets() -> Secrets {
    Secrets::open(data_dir().join("secrets.json"))
}

fn accounts_path() -> PathBuf {
    data_dir().join("accounts.json")
}

pub fn load() -> Vec<Account> {
    std::fs::read_to_string(accounts_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn save(v: &[Account]) -> Result<()> {
    std::fs::create_dir_all(data_dir())?;
    let tmp = accounts_path().with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(v)?)?;
    std::fs::rename(tmp, accounts_path())?;
    Ok(())
}

/// Stores verified credentials (`user_id` set) and records the account.
pub fn add(c: &Credentials, realm_name: Option<String>) -> Result<Account> {
    let user_id = c.user_id.context("credentials are not verified")?;
    secrets().set(&account_key(&c.realm, user_id), &c.api_key)?;
    let a = Account { realm: c.realm.clone(), email: c.email.clone(), user_id, realm_name };
    let mut all = load();
    all.retain(|x| !(x.realm == a.realm && x.user_id == a.user_id));
    all.push(a.clone());
    save(&all)?;
    Ok(a)
}

pub fn credentials(a: &Account) -> Result<Credentials> {
    let key = secrets().get(&account_key(&a.realm, a.user_id))?.context("no API key stored for this account")?;
    Ok(Credentials { realm: a.realm.clone(), email: a.email.clone(), api_key: key, user_id: Some(a.user_id) })
}

/// Signs out: forgets the key and the account entry. The message cache stays.
pub fn remove(a: &Account) -> Result<()> {
    secrets().delete(&account_key(&a.realm, a.user_id))?;
    let mut all = load();
    all.retain(|x| x != a);
    save(&all)
}

pub fn store_path(a: &Account) -> PathBuf {
    data_dir().join(puzil_store::account_file(&a.realm, a.user_id))
}

//! Wording and keys that differ by platform: ⌘ and Finder on macOS, Ctrl and the desktop's
//! file manager elsewhere.

/// A shortcut label with the platform's main modifier: `key!("K")` is `⌘K` or `Ctrl+K`.
#[cfg(target_os = "macos")]
macro_rules! key {
    ($k:literal) => {
        concat!("⌘", $k)
    };
}
#[cfg(not(target_os = "macos"))]
macro_rules! key {
    ($k:literal) => {
        concat!("Ctrl+", $k)
    };
}

/// A keystroke with the platform's main modifier, for `KeyBinding::new`: `chord!("q")` is
/// `cmd-q` or `ctrl-q`.
#[cfg(target_os = "macos")]
macro_rules! chord {
    ($k:literal) => {
        concat!("cmd-", $k)
    };
}
#[cfg(not(target_os = "macos"))]
macro_rules! chord {
    ($k:literal) => {
        concat!("ctrl-", $k)
    };
}

pub const OS_NAME: &str = if cfg!(target_os = "macos") { "macOS" } else { "the system" };
pub const SHOW_IN_FILES: &str = if cfg!(target_os = "macos") { "Show in Finder" } else { "Show in Files" };
pub const SHOW_LOGS: &str = if cfg!(target_os = "macos") { "Show Logs in Finder" } else { "Show Logs in Files" };
pub const SECRET_STORE: &str = if cfg!(target_os = "macos") { "the Keychain" } else { "the system keyring" };

//! Zulip client core: REST client, register/events actor, domain model, authentication
//! (zuliprc import, password, SSO via `mobile_flow_otp`) and secret storage.

pub mod auth;
pub mod client;
pub mod events;
pub mod model;
pub mod perms;
pub mod queue;
pub mod replay;
pub mod secrets;
pub mod types;
pub mod zuliprc;

pub use auth::{Credentials, ServerSettings};
pub use client::{ApiError, Client};
pub use model::State;

/// Parses `zulip://login?...` and other URLs consistently: realm URLs are kept without a
/// trailing slash so `realm + "/api/v1/..."` is always well-formed.
pub(crate) fn normalize_realm(s: &str) -> anyhow::Result<url::Url> {
    let s = s.trim();
    let s = if s.contains("://") { s.to_string() } else { format!("https://{s}") };
    let mut u = url::Url::parse(&s)?;
    anyhow::ensure!(matches!(u.scheme(), "https" | "http"), "realm must be http(s): {s}");
    u.set_query(None);
    u.set_fragment(None);
    let path = u.path().trim_end_matches('/').to_string();
    u.set_path(&path);
    Ok(u)
}

/// A user-typed server address as a canonical realm URL (`https://host[/path]`).
pub fn canonical_realm(s: &str) -> anyhow::Result<String> {
    Ok(realm_str(&normalize_realm(s)?))
}

pub(crate) fn realm_str(u: &url::Url) -> String {
    u.as_str().trim_end_matches('/').to_string()
}

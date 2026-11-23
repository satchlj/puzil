//! `zuliprc` import (the INI file the web app's "Download zuliprc" produces).

use anyhow::{Context, Result, bail};

use crate::auth::Credentials;

/// Parses the `[api]` section: `email`, `key`, `site`. Other sections and keys are ignored.
pub fn parse(text: &str) -> Result<Credentials> {
    let (mut section, mut email, mut key, mut site) = (String::new(), None, None, None);
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(['#', ';']) {
            continue;
        }
        if let Some(s) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = s.trim().to_string();
            continue;
        }
        if section != "api" {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = v.trim().to_string();
        match k.trim() {
            "email" => email = Some(v),
            "key" => key = Some(v),
            "site" => site = Some(v),
            _ => {}
        }
    }
    let (Some(email), Some(api_key), Some(site)) = (email, key, site) else {
        bail!("zuliprc needs email, key and site in [api]");
    };
    let realm = crate::realm_str(&crate::normalize_realm(&site)?);
    Ok(Credentials { realm, email, api_key, user_id: None })
}

pub fn load(path: &std::path::Path) -> Result<Credentials> {
    parse(&std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?)
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses_web_download() {
        let c = super::parse("[api]\nemail=a@b.c\nkey=abc123\nsite=https://x.zulipchat.com/\n").unwrap();
        assert_eq!(c.realm, "https://x.zulipchat.com");
        assert_eq!((c.email.as_str(), c.api_key.as_str()), ("a@b.c", "abc123"));
    }
    #[test]
    fn ignores_other_sections() {
        let c = super::parse("[other]\nkey=no\n[api]\n# c\nkey = k\nemail = e\nsite = chat.example.org\n").unwrap();
        assert_eq!((c.api_key.as_str(), c.realm.as_str()), ("k", "https://chat.example.org"));
    }
}

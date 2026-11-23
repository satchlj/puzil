//! Sign-in: server discovery, email and password, and SSO through `mobile_flow_otp`.
//!
//! SSO flow (the one the Zulip mobile app uses):
//! 1. Generate a 32-byte one-time pad and hex-encode it, giving 64 hex characters.
//! 2. Open `realm + login_url` in the user's browser with `mobile_flow_otp=<otp>` added.
//! 3. After sign-in, the server redirects to
//!    `zulip://login?realm=…&email=…&user_id=…&otp_encrypted_api_key=…`.
//! 4. The API key is `hex(api_key) XOR otp`, nibble by nibble. Only this process knows the
//!    OTP, so a forged callback decrypts to garbage and fails verification.

use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::{normalize_realm, realm_str};

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Credentials {
    /// Normalized realm URL, without a trailing slash.
    pub realm: String,
    pub email: String,
    pub api_key: String,
    /// Known after `/users/me` or from an SSO callback; zuliprc lacks it.
    pub user_id: Option<u64>,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("realm", &self.realm)
            .field("email", &self.email)
            .field("api_key", &"<redacted>")
            .field("user_id", &self.user_id)
            .finish()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExternalMethod {
    pub name: String,
    pub display_name: String,
    #[serde(default)]
    pub display_icon: Option<String>,
    pub login_url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerSettings {
    // Servers send both `realm_url` and the deprecated `realm_uri`; old ones only the latter.
    #[serde(default)]
    pub realm_url: String,
    #[serde(default, skip_serializing)]
    realm_uri: Option<String>,
    #[serde(default)]
    pub realm_name: Option<String>,
    #[serde(default)]
    pub realm_icon: Option<String>,
    pub zulip_version: String,
    #[serde(default)]
    pub zulip_feature_level: u32,
    #[serde(default)]
    pub email_auth_enabled: bool,
    #[serde(default)]
    pub require_email_format_usernames: bool,
    #[serde(default)]
    pub external_authentication_methods: Vec<ExternalMethod>,
}

pub fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("Puzil/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("reqwest client")
}

#[derive(Deserialize)]
struct ApiError {
    #[serde(default)]
    msg: String,
    #[serde(default)]
    code: Option<String>,
}

async fn api_json<T: serde::de::DeserializeOwned>(resp: reqwest::Response) -> Result<T> {
    let status = resp.status();
    let body = resp.bytes().await?;
    if !status.is_success() {
        let e: ApiError = serde_json::from_slice(&body).unwrap_or(ApiError { msg: String::new(), code: None });
        bail!("HTTP {status}: {} ({})", e.msg, e.code.unwrap_or_default());
    }
    serde_json::from_slice(&body).context("decoding response")
}

/// `GET /api/v1/server_settings`, which needs no authentication. Accepts `chat.example.org`,
/// a full URL, or a URL with a path that has a trailing slash.
pub async fn server_settings(http: &reqwest::Client, realm: &str) -> Result<ServerSettings> {
    let realm = normalize_realm(realm)?;
    let resp = http.get(format!("{}/api/v1/server_settings", realm_str(&realm))).send().await?;
    let mut s: ServerSettings = api_json(resp).await?;
    let url = if s.realm_url.is_empty() { s.realm_uri.take().unwrap_or_default() } else { s.realm_url.clone() };
    s.realm_url = realm_str(&normalize_realm(&url)?);
    Ok(s)
}

/// `POST /api/v1/fetch_api_key`. Returns the user's existing API key; it does not rotate it.
pub async fn password_login(http: &reqwest::Client, s: &ServerSettings, username: &str, password: &str) -> Result<Credentials> {
    ensure!(s.email_auth_enabled, "this server does not accept password sign-in");
    #[derive(Deserialize)]
    struct R {
        api_key: String,
        email: String,
        user_id: Option<u64>,
    }
    let resp = http
        .post(format!("{}/api/v1/fetch_api_key", s.realm_url))
        .form(&[("username", username), ("password", password)])
        .send()
        .await?;
    let r: R = api_json(resp).await?;
    Ok(Credentials { realm: s.realm_url.clone(), email: r.email, api_key: r.api_key, user_id: r.user_id })
}

#[derive(Debug, Clone, Deserialize)]
pub struct Me {
    pub user_id: u64,
    pub email: String,
    pub full_name: String,
}

/// `GET /api/v1/users/me`: the cheapest authenticated call, used to verify any credential.
pub async fn verify(http: &reqwest::Client, c: &Credentials) -> Result<Me> {
    let resp = http
        .get(format!("{}/api/v1/users/me", c.realm))
        .basic_auth(&c.email, Some(&c.api_key))
        .send()
        .await?;
    api_json(resp).await
}

/// A pending SSO sign-in. Keep exactly one per realm; drop it after a callback or a cancel.
pub struct SsoAttempt {
    pub realm: String,
    otp: String,
    pub browser_url: String,
}

impl std::fmt::Debug for SsoAttempt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SsoAttempt").field("realm", &self.realm).finish_non_exhaustive()
    }
}

pub fn generate_otp() -> String {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).expect("OS randomness");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

impl SsoAttempt {
    /// Builds the browser URL. `login_url` is relative (`/accounts/login/social/google`) or
    /// absolute, and may already carry a query (SAML adds `?idp=…`), so the OTP is added as a
    /// query pair rather than by string concatenation.
    pub fn new(s: &ServerSettings, method: &ExternalMethod) -> Result<Self> {
        Self::with_otp(s, method, generate_otp())
    }

    fn with_otp(s: &ServerSettings, method: &ExternalMethod, otp: String) -> Result<Self> {
        let base = url::Url::parse(&format!("{}/", s.realm_url))?;
        let mut u = base.join(method.login_url.trim_start_matches('/'))?;
        ensure!(u.origin() == base.origin(), "login_url points off-realm: {}", method.login_url);
        u.query_pairs_mut().append_pair("mobile_flow_otp", &otp);
        Ok(Self { realm: s.realm_url.clone(), otp, browser_url: u.into() })
    }

    /// Consumes a `zulip://login?…` callback. Checks that it is for this attempt's realm,
    /// then decrypts the key. Call [`verify`] afterwards: a wrong OTP still yields a
    /// plausible-looking string.
    pub fn complete(&self, callback: &str) -> Result<Credentials> {
        let cb = parse_callback(callback)?;
        let cb_realm = realm_str(&normalize_realm(&cb.realm)?);
        ensure!(cb_realm == self.realm, "callback is for {cb_realm}, expected {}", self.realm);
        let api_key = otp_decrypt(&cb.otp_encrypted_api_key, &self.otp)?;
        Ok(Credentials { realm: self.realm.clone(), email: cb.email, api_key, user_id: cb.user_id })
    }
}

#[derive(Debug)]
pub struct Callback {
    pub realm: String,
    pub email: String,
    pub user_id: Option<u64>,
    pub otp_encrypted_api_key: String,
}

pub fn parse_callback(s: &str) -> Result<Callback> {
    let u = url::Url::parse(s.trim()).context("callback is not a URL")?;
    ensure!(u.scheme() == "zulip", "not a zulip:// URL");
    // `zulip://login?…` parses with host "login" (non-special scheme with authority).
    ensure!(u.host_str() == Some("login") || u.path().trim_matches('/') == "login", "not a login callback");
    let (mut realm, mut email, mut user_id, mut key) = (None, None, None, None);
    for (k, v) in u.query_pairs() {
        match &*k {
            "realm" => realm = Some(v.into_owned()),
            "email" => email = Some(v.into_owned()),
            "user_id" => user_id = v.parse().ok(),
            "otp_encrypted_api_key" => key = Some(v.into_owned()),
            _ => {}
        }
    }
    Ok(Callback {
        realm: realm.ok_or_else(|| anyhow!("callback lacks realm"))?,
        email: email.ok_or_else(|| anyhow!("callback lacks email"))?,
        user_id,
        otp_encrypted_api_key: key.ok_or_else(|| anyhow!("callback lacks otp_encrypted_api_key"))?,
    })
}

/// Inverse of Zulip's `otp_encrypt_api_key`: XOR the hex digits, then hex-decode to ASCII.
pub fn otp_decrypt(hex_cipher: &str, otp: &str) -> Result<String> {
    ensure!(hex_cipher.len() == otp.len(), "ciphertext and OTP lengths differ");
    let nib = |c: char| c.to_digit(16).ok_or_else(|| anyhow!("non-hex digit"));
    let xored: Vec<u32> = hex_cipher.chars().zip(otp.chars()).map(|(a, b)| Ok(nib(a)? ^ nib(b)?)).collect::<Result<_>>()?;
    let bytes: Vec<u8> = xored.chunks(2).map(|p| (p[0] << 4 | p[1]) as u8).collect();
    let key = String::from_utf8(bytes).context("decrypted key is not text")?;
    ensure!(key.chars().all(|c| c.is_ascii_alphanumeric()), "decrypted key is not a Zulip API key");
    Ok(key)
}

#[cfg(test)]
pub(crate) fn otp_encrypt(api_key: &str, otp: &str) -> String {
    // Mirror of zerver/lib/utils.py, for tests.
    let hex: String = api_key.bytes().map(|b| format!("{b:02x}")).collect();
    hex.chars().zip(otp.chars()).map(|(a, b)| format!("{:x}", a.to_digit(16).unwrap() ^ b.to_digit(16).unwrap())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(realm: &str) -> ServerSettings {
        ServerSettings {
            realm_url: realm.into(),
            realm_uri: None,
            realm_name: None,
            realm_icon: None,
            zulip_version: "12".into(),
            zulip_feature_level: 513,
            email_auth_enabled: true,
            require_email_format_usernames: true,
            external_authentication_methods: vec![],
        }
    }
    fn method(login_url: &str) -> ExternalMethod {
        ExternalMethod { name: "x".into(), display_name: "X".into(), display_icon: None, login_url: login_url.into() }
    }

    #[test]
    fn otp_roundtrip() {
        let otp = generate_otp();
        assert_eq!(otp.len(), 64);
        let key = "aBcDeFgHiJkLmNoPqRsTuVwXyZ012345";
        assert_eq!(otp_decrypt(&otp_encrypt(key, &otp), &otp).unwrap(), key);
    }

    #[test]
    fn login_url_query_join() {
        let s = settings("https://x.zulipchat.com");
        let a = SsoAttempt::with_otp(&s, &method("/accounts/login/social/google"), "ab".repeat(32)).unwrap();
        assert_eq!(a.browser_url, format!("https://x.zulipchat.com/accounts/login/social/google?mobile_flow_otp={}", "ab".repeat(32)));
        let a = SsoAttempt::with_otp(&s, &method("/accounts/login/social/saml/?idp=okta"), "cd".repeat(32)).unwrap();
        assert!(a.browser_url.ends_with(&format!("/saml/?idp=okta&mobile_flow_otp={}", "cd".repeat(32))), "{}", a.browser_url);
        // Realms hosted under a path keep it.
        let s = settings("https://example.org/zulip");
        let a = SsoAttempt::with_otp(&s, &method("/accounts/login/social/github"), "ef".repeat(32)).unwrap();
        assert!(a.browser_url.starts_with("https://example.org/zulip/accounts/login/social/github?"), "{}", a.browser_url);
        assert!(SsoAttempt::with_otp(&s, &method("https://evil.example/x"), "00".repeat(32)).is_err());
    }

    #[test]
    fn callback_complete_and_realm_check() {
        let s = settings("https://x.zulipchat.com");
        let a = SsoAttempt::new(&s, &method("/accounts/login/social/google")).unwrap();
        let key = "0123456789abcdefghijABCDEFGHIJ01";
        let cb = format!(
            "zulip://login?realm=https%3A%2F%2Fx.zulipchat.com&email=a%40b.c&user_id=7&otp_encrypted_api_key={}",
            otp_encrypt(key, &a.otp)
        );
        let c = a.complete(&cb).unwrap();
        assert_eq!((c.api_key.as_str(), c.email.as_str(), c.user_id), (key, "a@b.c", Some(7)));
        let other = cb.replace("x.zulipchat.com", "y.zulipchat.com");
        assert!(a.complete(&other).is_err());
        // A different attempt's OTP never yields this key.
        let b = SsoAttempt::new(&s, &method("/accounts/login/social/google")).unwrap();
        assert!(b.complete(&cb).map(|c| c.api_key != key).unwrap_or(true));
    }
}

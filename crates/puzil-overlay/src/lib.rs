//! Iliad Overlay: the service behind votes, scores and inline passage reactions, spoken
//! exactly as `overlay.js` (0.3.4) and its desktop relay speak it.
//!
//! Requests are JSON to `<service><path>` with `Authorization: Bearer <token>`. A status
//! of 0 means the service could not be reached. Sign-in is the bot handshake: the service
//! hands out a one-time code, we send it to the overlay bot as a Zulip direct message,
//! and the service returns a token once its bot has seen who sent it. No Zulip credential
//! ever goes to the service.

use std::{collections::HashMap, time::Duration};

use puzil_zulip::{Client, client::Dest};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const DEFAULT_SERVICE: &str = "https://mcp.iliad.ac/overlay";
/// The overlay.js version whose behaviour this follows.
pub const PROTOCOL_VERSION: &str = "0.3.4";
/// Realms where the overlay is on (the desktop app's default `overlayHosts`).
pub const DEFAULT_HOSTS: &[&str] = &["alignment-theory.zulipchat.com"];

pub fn enabled_for(realm: &str) -> bool {
    let host = realm.split("://").nth(1).unwrap_or(realm).split('/').next().unwrap_or("");
    match std::env::var("PUZIL_OVERLAY_HOSTS") {
        Ok(v) => v.split(',').any(|h| h.trim() == host),
        Err(_) => DEFAULT_HOSTS.contains(&host),
    }
}

pub fn service_url() -> String {
    std::env::var("PUZIL_OVERLAY_SERVICE").unwrap_or_else(|_| DEFAULT_SERVICE.into())
}

#[derive(Debug, Clone)]
pub struct Response {
    /// HTTP status, or 0 when unreachable.
    pub status: u16,
    pub body: Value,
}

impl Response {
    /// The service's `detail` message, or a generic one.
    pub fn detail(&self) -> String {
        match &self.body["detail"] {
            Value::String(s) => s.clone(),
            Value::Null => format!("the overlay service answered {}", self.status),
            other => other.to_string(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Health {
    #[serde(default)]
    pub extra_streams: Vec<u64>,
    #[serde(default)]
    pub latest_version: Option<String>,
    #[serde(default)]
    pub download_url: Option<String>,
    #[serde(default)]
    pub dev_auth: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Score {
    pub score: i64,
    pub my_vote: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Scores {
    #[serde(default)]
    pub topics: HashMap<String, Score>,
    #[serde(default)]
    pub messages: HashMap<String, Score>,
}

/// One emoji on one passage. Offsets are UTF-16 units into the message's text index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InlineReaction {
    pub start: usize,
    pub end: usize,
    pub hash: String,
    pub emoji: String,
    #[serde(default)]
    pub user_ids: Vec<u64>,
    #[serde(default)]
    pub mine: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Message,
    Topic,
}

impl Kind {
    fn path(self) -> &'static str {
        match self {
            Kind::Message => "message",
            Kind::Topic => "topic",
        }
    }
}

/// What a vote is about: a message, or a topic identified by its first message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoteTarget {
    pub id: u64,
    pub stream_id: u64,
    pub sender_id: u64,
    pub sender_is_bot: bool,
}

#[derive(Clone)]
pub struct Service {
    base: String,
    http: reqwest::Client,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SignIn {
    Token(String),
    /// The service couldn't be reached; try again later.
    Unreachable,
    /// The service said no (or its bot never saw the code). Don't ask again until the user
    /// retries: overlay.js `signInFailed`.
    Refused(String),
}

impl Service {
    pub fn new(base: impl Into<String>) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(concat!("Puzil/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(20))
            .build()
            .expect("reqwest client");
        Self { base: base.into().trim_end_matches('/').to_string(), http }
    }

    pub async fn request(&self, method: reqwest::Method, path: &str, body: Option<&Value>, token: Option<&str>) -> Response {
        let mut req = self.http.request(method, format!("{}{path}", self.base));
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        if let Some(b) = body {
            req = req.json(b);
        }
        let Ok(resp) = req.send().await else { return Response { status: 0, body: Value::Null } };
        let status = resp.status().as_u16();
        let json = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).is_some_and(|v| v.starts_with("application/json"));
        let text = resp.text().await.unwrap_or_default();
        let body = if json {
            serde_json::from_str(&text).unwrap_or(Value::Null)
        } else {
            json!({"detail": format!("the overlay service failed ({status})")})
        };
        Response { status, body }
    }

    pub async fn health(&self) -> Option<Health> {
        let r = self.request(reqwest::Method::GET, "/health", None, None).await;
        (r.status == 200).then(|| serde_json::from_value(r.body).unwrap_or_default())
    }

    /// The handshake. `stored` is a token from an earlier run; it is checked with
    /// `/auth/me` first. `on_code_sent` is not needed: the direct message is deleted again
    /// once the service has answered, whatever the answer.
    pub async fn sign_in(&self, zulip: &Client, user_id: u64, stored: Option<&str>, health: &Health) -> SignIn {
        if health.dev_auth {
            let r = self.request(reqwest::Method::POST, "/dev/login", Some(&json!({"user_id": user_id})), None).await;
            return match (r.status, r.body["token"].as_str()) {
                (200, Some(t)) => SignIn::Token(t.into()),
                (0, _) => SignIn::Unreachable,
                _ => SignIn::Refused(r.detail()),
            };
        }
        if let Some(t) = stored {
            let me = self.request(reqwest::Method::GET, "/auth/me", None, Some(t)).await;
            if me.status == 200 && me.body["user_id"].as_u64() == Some(user_id) {
                return SignIn::Token(t.into());
            }
            if me.status == 0 {
                return SignIn::Unreachable;
            }
            if me.status == 403 {
                return SignIn::Refused(me.detail());
            }
        }
        let started = self.request(reqwest::Method::POST, "/auth/start", None, None).await;
        if started.status != 200 {
            return if started.status == 0 { SignIn::Unreachable } else { SignIn::Refused(started.detail()) };
        }
        let (Some(code), Some(bot)) = (started.body["code"].as_str(), started.body["bot_user_id"].as_u64()) else {
            return SignIn::Refused("the overlay service sent no sign-in code".into());
        };
        let sent = match zulip.send_message(&Dest::Dm(vec![bot]), code).await {
            Ok(id) => id,
            Err(e) => return SignIn::Refused(format!("could not send the sign-in code to the overlay bot: {e}")),
        };
        let tidy = || async {
            if let Err(e) = zulip.delete_message(sent).await {
                tracing::debug!("could not delete the sign-in message: {e}");
            }
        };
        for _ in 0..8 {
            let r = self.request(reqwest::Method::POST, "/auth/finish", Some(&json!({"code": code})), None).await;
            if r.status == 200 && r.body["user_id"].as_u64() == Some(user_id) {
                tidy().await;
                return match r.body["token"].as_str() {
                    Some(t) => SignIn::Token(t.into()),
                    None => SignIn::Refused("the overlay service returned no token".into()),
                };
            }
            if r.status != 404 {
                tidy().await;
                return if r.status == 0 { SignIn::Unreachable } else { SignIn::Refused(r.detail()) };
            }
            tokio::time::sleep(Duration::from_millis(1500)).await;
        }
        tidy().await;
        SignIn::Refused("could not sign in to the overlay service (its bot did not receive the code)".into())
    }
}

/// A signed-in session with the service.
#[derive(Clone)]
pub struct Session {
    pub service: Service,
    pub token: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CallError {
    Unreachable,
    /// The token is no longer valid: sign in again.
    Unauthorized,
    Refused(String),
    Decode(String),
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable => f.write_str("the overlay service cannot be reached"),
            Self::Unauthorized => f.write_str("the overlay service no longer accepts our sign-in"),
            Self::Refused(d) | Self::Decode(d) => f.write_str(d),
        }
    }
}

impl Session {
    async fn call<T: serde::de::DeserializeOwned>(&self, method: reqwest::Method, path: &str, body: Value) -> Result<T, CallError> {
        let r = self.service.request(method, path, Some(&body), Some(&self.token)).await;
        match r.status {
            0 => Err(CallError::Unreachable),
            401 => Err(CallError::Unauthorized),
            s if s >= 400 => Err(CallError::Refused(r.detail())),
            _ => serde_json::from_value(r.body).map_err(|e| CallError::Decode(e.to_string())),
        }
    }

    /// Scores of topics (by first message id) and messages. Only ids from channels the
    /// service covers may be passed (see [`covers`]).
    pub async fn scores(&self, topics: &[u64], messages: &[u64]) -> Result<Scores, CallError> {
        self.call(reqwest::Method::POST, "/scores", json!({"topics": topics, "messages": messages})).await
    }

    /// Sets our vote (-1, 0 or 1) and returns the new score.
    pub async fn vote(&self, kind: Kind, target: VoteTarget, value: i64) -> Result<Score, CallError> {
        let body = json!({
            "value": value,
            "target": {"stream_id": target.stream_id, "sender_id": target.sender_id, "sender_is_bot": target.sender_is_bot},
        });
        self.call(reqwest::Method::PUT, &format!("/votes/{}/{}", kind.path(), target.id), body).await
    }

    pub async fn inline_query(&self, ids: &[u64]) -> Result<HashMap<u64, Vec<InlineReaction>>, CallError> {
        #[derive(Deserialize)]
        struct R {
            #[serde(default)]
            reactions: HashMap<String, Vec<InlineReaction>>,
        }
        let r: R = self.call(reqwest::Method::POST, "/inline/query", json!({"ids": ids})).await?;
        Ok(r.reactions.into_iter().filter_map(|(k, v)| Some((k.parse().ok()?, v))).collect())
    }

    /// Adds (`on`) or removes our `emoji` on a passage; returns the message's reactions.
    pub async fn put_inline(&self, message: u64, start: usize, end: usize, hash: &str, emoji: &str, on: bool) -> Result<Vec<InlineReaction>, CallError> {
        #[derive(Deserialize)]
        struct R {
            reactions: Vec<InlineReaction>,
        }
        let body = json!({"start": start, "end": end, "hash": hash, "emoji": emoji, "on": on});
        let r: R = self.call(reqwest::Method::PUT, &format!("/inline/{message}"), body).await?;
        Ok(r.reactions)
    }
}

/// Whether votes and inline reactions are offered in a channel: public channels, plus any
/// private ones the service lists. DM and other private ids are never sent to the service.
pub fn covers(invite_only: Option<bool>, stream_id: u64, health: &Health) -> bool {
    health.extra_streams.contains(&stream_id) || invite_only == Some(false)
}

// ---- Replies and threads (data lives in Zulip) ----

pub const THREAD_LINK_TEXT: &str = "thread from this message";
pub const SEPARATOR: &str = " / ";
pub const RESOLVED_PREFIX: &str = "✔ ";
pub const MAX_TOPIC_CHARS: usize = 60;

/// One of the six default one-click reactions: (Zulip name, emoji code, glyph).
pub const QUICK: &[(&str, &str, &str)] = &[
    ("+1", "1f44d", "👍"),
    ("heart", "2764", "❤️"),
    ("joy", "1f602", "😂"),
    ("tada", "1f389", "🎉"),
    ("thinking", "1f914", "🤔"),
    ("eyes", "1f440", "👀"),
];

/// The glyph for an emoji name used in inline reactions (overlay.js's palette).
pub fn palette_glyph(name: &str) -> Option<&'static str> {
    const PALETTE: &[(&str, &str)] = &[
        ("+1", "👍"), ("-1", "👎"), ("heart", "❤️"), ("joy", "😂"), ("tada", "🎉"), ("thinking", "🤔"),
        ("eyes", "👀"), ("100", "💯"), ("fire", "🔥"), ("pray", "🙏"), ("rocket", "🚀"), ("clap", "👏"),
        ("raised_hands", "🙌"), ("wave", "👋"), ("check", "✅"), ("cross_mark", "❌"), ("cry", "😢"),
        ("open_mouth", "😮"),
    ];
    PALETTE.iter().find(|(n, _)| *n == name).map(|(_, g)| *g)
}

pub fn unresolved(topic: &str) -> &str {
    topic.strip_prefix(RESOLVED_PREFIX).unwrap_or(topic)
}

/// Zulip's encoding for names inside a `#narrow` URL fragment (web/src/internal_url.ts).
pub fn encode_hash_component(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'~' => out.push(b as char),
            b'!' => out.push_str(".21"),
            b'\'' => out.push_str(".27"),
            b'(' => out.push_str(".28"),
            b')' => out.push_str(".29"),
            b'*' => out.push_str(".2A"),
            b'.' => out.push_str(".2E"),
            _ => out.push_str(&format!(".{b:02X}")),
        }
    }
    out
}

/// `<realm>/#narrow/channel/<id>/topic/<topic>/near/<message>`, as overlay.js writes it.
pub fn message_link(realm: &str, stream_id: u64, topic: &str, message: u64) -> String {
    format!("{realm}/#narrow/channel/{stream_id}/topic/{}/near/{message}", encode_hash_component(topic))
}

/// The first line of a reply: a silent mention of the parent's author and a link to it.
pub fn reply_header(realm: &str, author: &str, author_id: u64, stream_id: u64, topic: &str, message: u64) -> String {
    format!("@_**{author}|{author_id}** [↩]({})\n\n", message_link(realm, stream_id, topic, message))
}

/// The first line of a thread's first message, linking back to the message it branched from.
pub fn thread_header(realm: &str, author: &str, author_id: u64, stream_id: u64, topic: &str, message: u64) -> String {
    format!("@_**{author}|{author_id}** [{THREAD_LINK_TEXT}]({})\n\n", message_link(realm, stream_id, topic, message))
}

/// The id of the message a thread's first message (Markdown source) links back to.
pub fn thread_source(first_markdown: &str) -> Option<u64> {
    let line = first_markdown.lines().next()?;
    let i = line.find("/near/")? + "/near/".len();
    let digits: String = line[i..].chars().take_while(char::is_ascii_digit).collect();
    line[i + digits.len()..].starts_with(')').then(|| digits.parse().ok()).flatten()
}

/// A message's Markdown source as one line of plain text, for previews (overlay.js
/// `plainText`): no leading reply or thread header, no markup characters.
pub fn plain_text(markdown: &str) -> String {
    let mut s = markdown.trim_start();
    // Drop a leading `@_**…](…/near/123)` header line.
    if s.starts_with("@_**")
        && let Some(line_end) = s.find('\n').or(Some(s.len()))
        && s[..line_end].contains("/near/")
        && s[..line_end].trim_end().ends_with(')')
    {
        s = &s[line_end..];
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix("@_**").or_else(|| rest.strip_prefix("@**"))
            && let Some(end) = r.find("**")
        {
            out.push('@');
            out.push_str(r[..end].split('|').next().unwrap_or(""));
            rest = &r[end + 2..];
            continue;
        }
        if rest.starts_with('[')
            && let Some(close) = rest.find("](")
            && let Some(paren) = rest[close..].find(')')
        {
            out.push_str(&rest[1..close]);
            rest = &rest[close + paren + 1..];
            continue;
        }
        if let Some(r) = rest.strip_prefix("**").or_else(|| rest.strip_prefix("$$")) {
            rest = r;
            continue;
        }
        if let Some(r) = rest.strip_prefix("```") {
            rest = r.trim_start_matches(|c: char| c.is_ascii_alphanumeric());
            continue;
        }
        let c = rest.chars().next().unwrap();
        if c != '`' {
            out.push(c);
        }
        rest = &rest[c.len_utf8()..];
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn shorten(s: &str, n: usize) -> String {
    let mut it = s.chars();
    let head: String = it.by_ref().take(n).collect();
    if it.next().is_some() { format!("{head}…") } else { head }
}

/// overlay.js's "Hot": `(score + 1) / (hours since last message + 2)^1.5`.
pub fn hot(score: i64, hours_since_last: f64) -> f64 {
    (score as f64 + 1.) / (hours_since_last.max(0.) + 2.).powf(1.5)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_encoding_matches_overlay_js() {
        // encodeURIComponent, then % → . and !'()*. escaped.
        assert_eq!(encode_hash_component("a b.c"), "a.20b.2Ec");
        assert_eq!(encode_hash_component("why (not)?"), "why.20.28not.29.3F");
        assert_eq!(encode_hash_component("café"), "caf.C3.A9");
        assert_eq!(encode_hash_component("A / B"), "A.20.2F.20B");
    }

    #[test]
    fn headers_and_thread_source() {
        let h = reply_header("https://r.zulipchat.com", "Ada L", 7, 12, "My topic", 99);
        assert_eq!(h, "@_**Ada L|7** [↩](https://r.zulipchat.com/#narrow/channel/12/topic/My.20topic/near/99)\n\n");
        let t = thread_header("https://r", "Bo", 8, 3, "T", 5);
        assert_eq!(thread_source(&format!("{t}body")), Some(5));
        assert_eq!(thread_source("no link here"), None);
    }

    #[test]
    fn plain_text_like_overlay_js() {
        let md = "@_**Ada|7** [↩](https://r/#narrow/channel/1/topic/t/near/9)\n\nSee **this** [link](http://x) and @**Bo|8** with `code` $$x$$";
        assert_eq!(plain_text(md), "See this link and @Bo with code x");
        assert_eq!(shorten("abcdef", 3), "abc…");
    }

    #[test]
    fn coverage_rule() {
        let h = Health { extra_streams: vec![5], ..Default::default() };
        assert!(covers(Some(false), 1, &h));
        assert!(!covers(Some(true), 1, &h));
        assert!(covers(Some(true), 5, &h));
        assert!(!covers(None, 2, &h), "unknown channels are not covered");
    }

    #[test]
    fn hosts() {
        assert!(enabled_for("https://alignment-theory.zulipchat.com"));
        assert!(!enabled_for("https://chat.zulip.org"));
    }
}

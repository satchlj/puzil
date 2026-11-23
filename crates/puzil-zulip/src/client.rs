//! Authenticated REST client. Zulip takes form-encoded parameters whose structured values
//! are JSON-encoded strings, and answers `{"result": "success"|"error", "msg", "code", ...}`.

use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::Credentials;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// The event queue expired or was garbage-collected: re-register.
    #[error("event queue expired")]
    BadEventQueue,
    /// The credentials were refused. Retrying will not help; the user must sign in again.
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    #[error("rate limited; retry after {0:?}")]
    RateLimited(Duration),
    #[error("{code}: {msg} (HTTP {status})")]
    Api { status: u16, code: String, msg: String },
    #[error("network: {0}")]
    Network(#[from] reqwest::Error),
    #[error("bad response: {0}")]
    Decode(String),
}

impl ApiError {
    /// Worth retrying after a backoff: network trouble, rate limits and server errors.
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Network(_) | Self::RateLimited(_) => true,
            Self::Api { status, .. } => *status >= 500,
            _ => false,
        }
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

#[derive(Clone)]
pub struct Client {
    pub creds: Credentials,
    http: reqwest::Client,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client").field("creds", &self.creds).finish()
    }
}

/// Default per-request timeout. Long polls override it.
const TIMEOUT: Duration = Duration::from_secs(30);

impl Client {
    pub fn new(creds: Credentials) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(concat!("Puzil/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(10))
            .pool_idle_timeout(Duration::from_secs(90))
            .build()
            .expect("reqwest client");
        Self { creds, http }
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api/v1/{}", self.creds.realm, path.trim_start_matches('/'))
    }

    async fn send(&self, req: reqwest::RequestBuilder, timeout: Duration) -> ApiResult<Value> {
        let resp = req.basic_auth(&self.creds.email, Some(&self.creds.api_key)).timeout(timeout).send().await?;
        let status = resp.status();
        let retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<f64>().ok())
            .map(Duration::from_secs_f64);
        let body = resp.bytes().await?;
        let v: Value = serde_json::from_slice(&body)
            .map_err(|e| ApiError::Decode(format!("HTTP {status}: {e}: {}", String::from_utf8_lossy(&body[..body.len().min(200)]))))?;
        if status.is_success() && v["result"] == "success" {
            return Ok(v);
        }
        let code = v["code"].as_str().unwrap_or("").to_string();
        let msg = v["msg"].as_str().unwrap_or("").to_string();
        Err(match (status.as_u16(), code.as_str()) {
            (_, "BAD_EVENT_QUEUE_ID") => ApiError::BadEventQueue,
            (401, _) | (_, "UNAUTHORIZED") | (_, "USER_DEACTIVATED") | (_, "REALM_DEACTIVATED") => ApiError::Unauthorized(msg),
            (429, _) | (_, "RATE_LIMIT_HIT") => ApiError::RateLimited(
                retry_after.or_else(|| v["retry-after"].as_f64().map(Duration::from_secs_f64)).unwrap_or(Duration::from_secs(5)),
            ),
            (s, _) => ApiError::Api { status: s, code, msg },
        })
    }

    pub async fn get_value(&self, path: &str, params: &[(&str, String)], timeout: Option<Duration>) -> ApiResult<Value> {
        self.send(self.http.get(self.url(path)).query(params), timeout.unwrap_or(TIMEOUT)).await
    }

    pub async fn post_value(&self, path: &str, params: &[(&str, String)]) -> ApiResult<Value> {
        self.send(self.http.post(self.url(path)).form(params), TIMEOUT).await
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str, params: &[(&str, String)]) -> ApiResult<T> {
        decode(self.get_value(path, params, None).await?)
    }

    pub async fn post<T: DeserializeOwned>(&self, path: &str, params: &[(&str, String)]) -> ApiResult<T> {
        decode(self.post_value(path, params).await?)
    }
}

pub fn decode<T: DeserializeOwned>(v: Value) -> ApiResult<T> {
    serde_json::from_value(v).map_err(|e| ApiError::Decode(e.to_string()))
}

/// JSON-encodes a structured parameter value, as Zulip expects.
pub fn json(v: impl serde::Serialize) -> String {
    serde_json::to_string(&v).expect("serializable")
}

/// One page of `GET /messages`.
#[derive(Debug, serde::Deserialize)]
pub struct MessagesPage {
    pub messages: Vec<crate::types::Message>,
    #[serde(default)]
    pub found_oldest: bool,
    #[serde(default)]
    pub found_newest: bool,
    #[serde(default)]
    pub found_anchor: bool,
}

/// Where a `GET /messages` page starts.
#[derive(Debug, Clone, Copy)]
pub enum Anchor {
    Newest,
    Oldest,
    FirstUnread,
    Id(u64),
}

impl Client {
    /// `narrow` is Zulip's narrow JSON (`[]` for all messages). Returns server-rendered HTML.
    pub async fn messages(&self, narrow: &Value, anchor: Anchor, before: u32, after: u32) -> ApiResult<MessagesPage> {
        let anchor = match anchor {
            Anchor::Newest => "newest".to_string(),
            Anchor::Oldest => "oldest".to_string(),
            Anchor::FirstUnread => "first_unread".to_string(),
            Anchor::Id(i) => i.to_string(),
        };
        self.get(
            "messages",
            &[
                ("narrow", narrow.to_string()),
                ("anchor", anchor),
                ("num_before", before.to_string()),
                ("num_after", after.to_string()),
                ("apply_markdown", "true".into()),
                ("client_gravatar", "true".into()),
                ("include_anchor", "true".into()),
            ],
        )
        .await
    }
}

/// Where a message goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dest {
    Topic { stream_id: u64, topic: String },
    /// The other participants (empty or `[me]` for a self-DM).
    Dm(Vec<u64>),
}

#[derive(Debug, serde::Deserialize)]
pub struct TopicInfo {
    pub name: String,
    pub max_id: u64,
}

impl Client {
    async fn send_method(&self, method: reqwest::Method, path: &str, params: &[(&str, String)]) -> ApiResult<Value> {
        self.send(self.http.request(method, self.url(path)).form(params), TIMEOUT).await
    }

    /// Sends a message and returns its id. `read_by_sender` keeps our own message from
    /// arriving unread (non-browser clients otherwise get it unread).
    pub async fn send_message(&self, dest: &Dest, content: &str) -> ApiResult<u64> {
        let mut p = vec![("content", content.to_string()), ("read_by_sender", "true".into())];
        match dest {
            Dest::Topic { stream_id, topic } => {
                p.push(("type", "stream".into()));
                p.push(("to", stream_id.to_string()));
                p.push(("topic", topic.clone()));
            }
            Dest::Dm(ids) => {
                let ids = if ids.is_empty() { vec![self.creds.user_id.unwrap_or_default()] } else { ids.clone() };
                p.push(("type", "direct".into()));
                p.push(("to", json(ids)));
            }
        }
        let v = self.post_value("messages", &p).await?;
        v["id"].as_u64().ok_or_else(|| ApiError::Decode("send: no id".into()))
    }

    pub async fn edit_message(&self, id: u64, content: &str) -> ApiResult<()> {
        self.send_method(reqwest::Method::PATCH, &format!("messages/{id}"), &[("content", content.to_string())]).await.map(drop)
    }

    pub async fn delete_message(&self, id: u64) -> ApiResult<()> {
        self.send_method(reqwest::Method::DELETE, &format!("messages/{id}"), &[]).await.map(drop)
    }

    /// The Markdown source of a message, for editing.
    pub async fn raw_message(&self, id: u64) -> ApiResult<String> {
        let v = self.get_value(&format!("messages/{id}"), &[("apply_markdown", "false".into())], None).await?;
        v["raw_content"].as_str().or(v["message"]["content"].as_str()).map(str::to_string).ok_or_else(|| ApiError::Decode("no raw content".into()))
    }

    /// Adds or removes a flag (`read`, `starred`, …) on messages.
    pub async fn update_flags(&self, ids: &[u64], add: bool, flag: &str) -> ApiResult<()> {
        let op = if add { "add" } else { "remove" };
        self.post_value("messages/flags", &[("messages", json(ids)), ("op", op.into()), ("flag", flag.into())]).await.map(drop)
    }

    /// Topics of a channel, newest first.
    pub async fn topics(&self, stream_id: u64) -> ApiResult<Vec<TopicInfo>> {
        let v = self.get_value(&format!("users/me/{stream_id}/topics"), &[], None).await?;
        decode(v["topics"].clone())
    }

    /// Uploads a file and returns its server-relative URL (`/user_uploads/…`).
    pub async fn upload(&self, filename: &str, bytes: Vec<u8>, mime: &str) -> ApiResult<String> {
        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(filename.to_string())
            .mime_str(mime)
            .map_err(|e| ApiError::Decode(e.to_string()))?;
        let form = reqwest::multipart::Form::new().part("file", part);
        let v = self.send(self.http.post(self.url("user_uploads")).multipart(form), Duration::from_secs(300)).await?;
        v["url"].as_str().or(v["uri"].as_str()).map(str::to_string).ok_or_else(|| ApiError::Decode("upload: no url".into()))
    }

    /// An unauthenticated GET of a public JSON document (such as the emoji data).
    pub async fn public_json(&self, url: &str) -> ApiResult<Value> {
        let r = self.http.get(url).timeout(TIMEOUT).send().await?;
        let b = r.bytes().await?;
        serde_json::from_slice(&b).map_err(|e| ApiError::Decode(e.to_string()))
    }
}

impl Client {
    /// Adds (or with `add = false` removes) our reaction on a message.
    /// Marks everything in `narrow` read, a batch at a time until the server reaches the
    /// newest message (`POST /messages/flags/narrow`).
    pub async fn mark_narrow_read(&self, narrow: &Value) -> ApiResult<()> {
        let mut anchor = "oldest".to_string();
        for _ in 0..50 {
            let r = self
                .post_value(
                    "messages/flags/narrow",
                    &[
                        ("narrow", narrow.to_string()),
                        ("anchor", anchor.clone()),
                        ("include_anchor", "false".into()),
                        ("num_before", "0".into()),
                        ("num_after", "5000".into()),
                        ("op", "add".into()),
                        ("flag", "read".into()),
                    ],
                )
                .await?;
            if r["found_newest"].as_bool().unwrap_or(true) {
                break;
            }
            match r["last_processed_id"].as_u64() {
                Some(id) => anchor = id.to_string(),
                None => break,
            }
        }
        Ok(())
    }

    /// Sets one of our per-channel settings (`is_muted`, `pin_to_top`, …).
    pub async fn set_subscription_property(&self, stream_id: u64, property: &str, value: Value) -> ApiResult<()> {
        let data = serde_json::json!([{ "stream_id": stream_id, "property": property, "value": value }]);
        self.post_value("users/me/subscriptions/properties", &[("subscription_data", data.to_string())]).await.map(drop)
    }

    /// Mutes, unmutes, follows or resets a topic (0 inherit, 1 muted, 2 unmuted, 3 followed).
    pub async fn set_topic_visibility(&self, stream_id: u64, topic: &str, policy: u8) -> ApiResult<()> {
        self.post_value(
            "user_topics",
            &[("stream_id", stream_id.to_string()), ("topic", topic.to_string()), ("visibility_policy", policy.to_string())],
        )
        .await
        .map(drop)
    }

    /// Renames the topic `message` is in, for the whole topic (resolving is a rename to `✔ name`).
    pub async fn rename_topic(&self, message: u64, topic: &str) -> ApiResult<()> {
        self.send_method(
            reqwest::Method::PATCH,
            &format!("messages/{message}"),
            &[("topic", topic.to_string()), ("propagate_mode", "change_all".into()), ("send_notification_to_old_thread", "false".into()), ("send_notification_to_new_thread", "false".into())],
        )
        .await
        .map(drop)
    }

    /// Changes one of our Zulip settings (`PATCH /settings`), e.g. `send_read_receipts`.
    pub async fn set_setting(&self, name: &str, value: &Value) -> ApiResult<()> {
        let v = match value {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        self.send_method(reqwest::Method::PATCH, "settings", &[(name, v)]).await.map(drop)
    }

    /// Sets our status: away or not, text, and emoji (empty strings clear them).
    pub async fn set_status(&self, away: Option<bool>, text: Option<&str>, emoji: Option<&str>) -> ApiResult<()> {
        let mut p: Vec<(&str, String)> = Vec::new();
        if let Some(a) = away {
            p.push(("away", a.to_string()));
        }
        if let Some(t) = text {
            p.push(("status_text", t.to_string()));
        }
        if let Some(e) = emoji {
            p.push(("emoji_name", e.to_string()));
        }
        self.post_value("users/me/status", &p).await.map(drop)
    }

    /// Replaces our profile picture.
    pub async fn set_avatar(&self, filename: &str, bytes: Vec<u8>, mime: &str) -> ApiResult<()> {
        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(filename.to_string())
            .mime_str(mime)
            .map_err(|e| ApiError::Decode(e.to_string()))?;
        let form = reqwest::multipart::Form::new().part("file", part);
        self.send(self.http.post(self.url("users/me/avatar")).multipart(form), Duration::from_secs(120)).await.map(drop)
    }

    /// Removes our profile picture (back to the organization's default).
    pub async fn delete_avatar(&self) -> ApiResult<()> {
        self.send_method(reqwest::Method::DELETE, "users/me/avatar", &[]).await.map(drop)
    }

    /// Changes our password (accounts that sign in with one).
    pub async fn change_password(&self, old: &str, new: &str) -> ApiResult<()> {
        self.send_method(reqwest::Method::PATCH, "settings", &[("old_password", old.to_string()), ("new_password", new.to_string())]).await.map(drop)
    }

    /// Asks to change our email: Zulip sends a confirmation link to the new address.
    pub async fn change_email(&self, email: &str) -> ApiResult<()> {
        self.send_method(reqwest::Method::PATCH, "settings", &[("email", email.to_string())]).await.map(drop)
    }

    /// Unmutes a user.
    pub async fn unmute_user(&self, user: u64) -> ApiResult<()> {
        self.send_method(reqwest::Method::DELETE, &format!("users/me/muted_users/{user}"), &[]).await.map(drop)
    }

    /// Joins channels, by name.
    pub async fn subscribe(&self, names: &[String]) -> ApiResult<()> {
        let subs: Vec<Value> = names.iter().map(|n| serde_json::json!({ "name": n })).collect();
        self.post_value("users/me/subscriptions", &[("subscriptions", json(subs))]).await.map(drop)
    }

    /// Leaves channels, by name.
    pub async fn unsubscribe(&self, names: &[String]) -> ApiResult<()> {
        self.send_method(reqwest::Method::DELETE, "users/me/subscriptions", &[("subscriptions", json(names))]).await.map(drop)
    }

    pub async fn react(&self, id: u64, add: bool, name: &str, code: &str, kind: &str) -> ApiResult<()> {
        let p = [("emoji_name", name.to_string()), ("emoji_code", code.to_string()), ("reaction_type", kind.to_string())];
        let m = if add { reqwest::Method::POST } else { reqwest::Method::DELETE };
        self.send_method(m, &format!("messages/{id}/reactions"), &p).await.map(drop)
    }
}

impl Client {
    /// Absolute form of a realm-relative URL (`/user_uploads/…`).
    pub fn absolute(&self, url: &str) -> String {
        if url.starts_with('/') { format!("{}{url}", self.creds.realm) } else { url.to_string() }
    }

    /// Downloads a file. Realm URLs (uploads, thumbnails, the image proxy) are fetched with
    /// our credentials; other hosts get no credentials.
    pub async fn get_bytes(&self, url: &str) -> ApiResult<Vec<u8>> {
        let abs = self.absolute(url);
        let mut req = self.http.get(&abs).timeout(Duration::from_secs(120));
        if abs.starts_with(&format!("{}/", self.creds.realm)) {
            req = req.basic_auth(&self.creds.email, Some(&self.creds.api_key));
        }
        let resp = req.send().await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(ApiError::Api { status: status.as_u16(), code: "HTTP".into(), msg: format!("GET {url}") });
        }
        Ok(resp.bytes().await?.to_vec())
    }
}

impl Client {
    /// The first message of a narrow, as Markdown source (a topic's identity for Overlay
    /// votes, and a thread's link back to where it started).
    pub async fn first_message_raw(&self, narrow: &Value) -> ApiResult<Option<crate::types::Message>> {
        let page: MessagesPage = self
            .get(
                "messages",
                &[
                    ("narrow", narrow.to_string()),
                    ("anchor", "oldest".into()),
                    ("num_before", "0".into()),
                    ("num_after", "1".into()),
                    ("apply_markdown", "false".into()),
                ],
            )
            .await?;
        Ok(page.messages.into_iter().next())
    }

    /// Specific messages, as Markdown source.
    pub async fn messages_by_id_raw(&self, ids: &[u64]) -> ApiResult<Vec<crate::types::Message>> {
        let page: MessagesPage =
            self.get("messages", &[("message_ids", json(ids)), ("apply_markdown", "false".into())]).await?;
        Ok(page.messages)
    }
}

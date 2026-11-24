//! Per-account SQLite store: a write-behind cache of the domain model plus everything that
//! must survive a restart (messages, fetched ranges, drafts, the outbox).
//!
//! - One file per account, WAL mode.
//! - All writes go through one writer thread, which drains its queue into a single
//!   transaction, so a burst of events costs one fsync.
//! - Reads use a separate connection and never wait for the writer (WAL readers see the last
//!   committed state).
//! - Message text is indexed with FTS5 over external content.

use std::{
    path::Path,
    sync::{Mutex, mpsc},
    thread::JoinHandle,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use puzil_zulip::{
    State,
    model::{Changes, Conv, DmKey, TopicKey, conv_of},
    types::*,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

const SCHEMA_VERSION: i64 = 2;
const SCHEMA: &str = include_str!("schema.sql");

// ---- Conversation keys as text ----

pub fn conv_to_str(c: &Conv) -> String {
    match c {
        Conv::Topic(k) => format!("t:{}:{}", k.stream_id, k.topic),
        Conv::Dm(k) => format!("d:{}", k.0.iter().map(u64::to_string).collect::<Vec<_>>().join(",")),
    }
}

pub fn conv_from_str(s: &str) -> Option<Conv> {
    if let Some(rest) = s.strip_prefix("t:") {
        let (sid, topic) = rest.split_once(':')?;
        return Some(Conv::Topic(TopicKey { stream_id: sid.parse().ok()?, topic: topic.to_string() }));
    }
    let ids = s.strip_prefix("d:")?.split(',').map(|x| x.parse().ok()).collect::<Option<Vec<u64>>>()?;
    Some(Conv::Dm(DmKey(ids)))
}

/// Plain text of rendered message HTML, for search. Drops tags, keeps text and the TeX of
/// KaTeX math (its MathML is noise), decodes the common entities.
pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut rest = html;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(j) = rest.find('>') else { break };
        let tag = &rest[1..j];
        rest = &rest[j + 1..];
        // Skip the visible KaTeX HTML and the MathML; keep the TeX annotation's text.
        if tag.starts_with("span class=\"katex-html\"") || tag.starts_with("math") {
            let close = if tag.starts_with("math") { "</math>" } else { "</span></span>" };
            if tag.starts_with("math") {
                if let (Some(a), Some(b)) = (rest.find("<annotation encoding=\"application/x-tex\">"), rest.find(close)) {
                    let start = a + "<annotation encoding=\"application/x-tex\">".len();
                    if let Some(end) = rest[start..].find("</annotation>") {
                        if start + end < b {
                            out.push_str(&rest[start..start + end]);
                        }
                    }
                }
            }
            if let Some(k) = rest.find(close) {
                rest = &rest[k + close.len()..];
            }
            continue;
        }
        let name = tag.trim_start_matches('/').split([' ', '/']).next().unwrap_or("");
        if matches!(name, "p" | "br" | "div" | "li" | "blockquote" | "pre" | "tr" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6") {
            out.push(' ');
        }
    }
    out.push_str(rest);
    out.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&nbsp;", " ")
}

// ---- Writes ----

#[derive(Debug, Clone)]
pub enum Write {
    Users(Vec<User>),
    Streams { streams: Vec<Stream>, subs: Vec<Subscription> },
    UserTopics(Vec<(TopicKey, Visibility)>),
    MutedUsers(Vec<UserId>),
    RealmEmoji(Vec<RealmEmoji>),
    /// Replaces the whole unread set: `(id, conv, mentioned)`.
    Unreads(Vec<(MessageId, Conv, bool)>),
    UpsertUsers(Vec<User>),
    /// Unread rows to set (id, conversation, mentioned) and ids no longer unread.
    UnreadDelta { set: Vec<(MessageId, Conv, bool)>, clear: Vec<MessageId> },
    Topics(Vec<(TopicKey, String)>),
    UpsertMessages(Vec<(Message, Conv)>),
    DeleteMessages(Vec<MessageId>),
    /// All messages of `conv` with ids in `lo..=hi` are present.
    FetchedRange { conv: Conv, lo: MessageId, hi: MessageId },
    Draft { conv: Conv, body: String },
    Meta { key: String, value: String },
}

enum Cmd {
    Write(Vec<Write>),
    Flush(mpsc::Sender<()>),
}

/// Writes that bring the store in line with `state` after `changes`.
pub fn writes_for(state: &State, changes: &Changes) -> Vec<Write> {
    let mut w = vec![];
    if changes.all || (changes.users && changes.user_ids.is_empty() && !changes.muted_users) {
        w.push(Write::Users(state.users.values().cloned().collect()));
        w.push(Write::MutedUsers(state.muted_users.iter().copied().collect()));
    } else {
        // A profile edit or a signup writes that user, not the whole (often large) table.
        if !changes.user_ids.is_empty() {
            w.push(Write::UpsertUsers(changes.user_ids.iter().filter_map(|id| state.users.get(id)).cloned().collect()));
        }
        if changes.muted_users {
            w.push(Write::MutedUsers(state.muted_users.iter().copied().collect()));
        }
    }
    if changes.all || changes.streams {
        w.push(Write::Streams { streams: state.streams.values().cloned().collect(), subs: state.subs.values().cloned().collect() });
    }
    if changes.all || changes.streams || changes.realm {
        // Channel folders, permissions and user groups live in the realm record.
        w.push(Write::Meta { key: "realm".into(), value: serde_json::to_string(&state.realm).unwrap_or_default() });
    }
    if changes.all {
        w.push(Write::UserTopics(state.user_topics.iter().map(|(k, v)| (k.clone(), *v)).collect()));
        w.push(Write::RealmEmoji(state.realm_emoji.values().cloned().collect()));
        w.push(Write::Meta { key: "user_id".into(), value: state.me.to_string() });
    } else if changes.user_topics {
        w.push(Write::UserTopics(state.user_topics.iter().map(|(k, v)| (k.clone(), *v)).collect()));
    }
    if changes.all || changes.convs.iter().any(|c| matches!(c, Conv::Dm(_))) {
        let v: Vec<_> = state.recent_dms.iter().map(|(k, id)| (k.0.clone(), *id)).collect();
        w.push(Write::Meta { key: "recent_dms".into(), value: serde_json::to_string(&v).unwrap_or_default() });
    }
    if changes.all || changes.unreads_all || (changes.unreads && changes.unread_ids.is_empty()) {
        w.push(Write::Unreads(state.unread.iter().map(|(id, c)| (*id, c.clone(), state.mentions.contains(id))).collect()));
    } else if !changes.unread_ids.is_empty() {
        // Just the rows that changed: with a large backlog a full rewrite clones tens of
        // thousands of entries on the main thread and rewrites the table on every read.
        let (mut set, mut clear) = (vec![], vec![]);
        for id in &changes.unread_ids {
            match state.unread.get(id) {
                Some(c) => set.push((*id, c.clone(), state.mentions.contains(id))),
                None => clear.push(*id),
            }
        }
        w.push(Write::UnreadDelta { set, clear });
    }
    let topics: Vec<_> = changes
        .convs
        .iter()
        .filter_map(|c| match c {
            Conv::Topic(k) => state.topic_names.get(k).map(|n| (k.clone(), n.clone())),
            _ => None,
        })
        .collect();
    if changes.all {
        w.push(Write::Topics(state.topic_names.iter().map(|(k, v)| (k.clone(), v.clone())).collect()));
    } else if !topics.is_empty() {
        w.push(Write::Topics(topics));
    }
    let (mut up, mut del) = (vec![], vec![]);
    for id in &changes.messages {
        match state.messages.get(id) {
            Some(m) => up.push((m.clone(), conv_of(state.me, m))),
            None => del.push(*id),
        }
    }
    if !up.is_empty() {
        w.push(Write::UpsertMessages(up));
    }
    if !del.is_empty() {
        w.push(Write::DeleteMessages(del));
    }
    w
}

// ---- Store ----

pub struct Store {
    tx: mpsc::Sender<Cmd>,
    read: Mutex<Connection>,
    writer: Option<JoinHandle<()>>,
}

fn configure(c: &Connection) -> Result<()> {
    c.busy_timeout(Duration::from_secs(5))?;
    c.pragma_update(None, "synchronous", "NORMAL")?;
    c.pragma_update(None, "foreign_keys", "OFF")?;
    c.pragma_update(None, "temp_store", "MEMORY")?;
    Ok(())
}

/// Brings the schema up to date. Returns whether the search table still needs rebuilding
/// (version 1 to 2), which the writer thread does after opening, not here: `open` runs before
/// the first frame, and rebuilding takes seconds on a large cache. Until then searches use the
/// old table, which answers the same queries, just without the prefix indexes.
fn migrate(c: &mut Connection) -> Result<bool> {
    c.pragma_update(None, "journal_mode", "WAL")?;
    let v: i64 = c.pragma_query_value(None, "user_version", |r| r.get(0))?;
    match v {
        0 => {
            let tx = c.transaction()?;
            tx.execute_batch(SCHEMA)?;
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            tx.commit()?;
            Ok(false)
        }
        1 => Ok(true),
        SCHEMA_VERSION => Ok(false),
        v => anyhow::bail!("store schema version {v} is newer than this build ({SCHEMA_VERSION})"),
    }
}

/// Version 2: prefix indexes on the search table, rebuilt from the messages.
fn rebuild_search(c: &mut Connection) -> Result<()> {
    let t = Instant::now();
    let tx = c.transaction()?;
    tx.execute_batch(
        "DROP TABLE messages_fts;
         CREATE VIRTUAL TABLE messages_fts USING fts5(text, topic, content='messages', content_rowid='id',
             tokenize='unicode61 remove_diacritics 2', prefix='1 2');
         INSERT INTO messages_fts(messages_fts) VALUES ('rebuild');",
    )?;
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    tx.commit()?;
    tracing::info!("search index rebuilt in {:.0?}", t.elapsed());
    Ok(())
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let mut wc = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        configure(&wc)?;
        let rebuild = migrate(&mut wc)?;
        let rc = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
        configure(&rc)?;
        let (tx, rx) = mpsc::channel();
        let writer = std::thread::Builder::new().name("puzil-store-writer".into()).spawn(move || {
            if rebuild && let Err(e) = rebuild_search(&mut wc) {
                tracing::error!("rebuilding the search index: {e:#}");
            }
            writer_loop(wc, rx)
        })?;
        Ok(Self { tx, read: Mutex::new(rc), writer: Some(writer) })
    }

    pub fn write(&self, w: Vec<Write>) {
        if !w.is_empty() {
            let _ = self.tx.send(Cmd::Write(w));
        }
    }

    /// Blocks until every write queued so far is committed.
    pub fn flush(&self) {
        let (tx, rx) = mpsc::channel();
        if self.tx.send(Cmd::Flush(tx)).is_ok() {
            let _ = rx.recv();
        }
    }

    fn with_read<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T> {
        Ok(f(&self.read.lock().unwrap())?)
    }

    /// The cached model, for showing the UI before register returns. Messages are not
    /// loaded; use [`Store::messages`] per narrow. `None` if nothing was ever stored.
    pub fn load_state(&self) -> Result<Option<State>> {
        self.with_read(|c| {
            let me: Option<String> = c.query_row("SELECT value FROM meta WHERE key='user_id'", [], |r| r.get(0)).optional()?;
            let Some(me) = me.and_then(|m| m.parse().ok()) else { return Ok(None) };
            let mut s = State { me, ..Default::default() };
            let meta = |k: &str| -> rusqlite::Result<Option<String>> {
                c.query_row("SELECT value FROM meta WHERE key=?1", [k], |r| r.get(0)).optional()
            };
            if let Some(r) = meta("realm")?.and_then(|j| serde_json::from_str::<puzil_zulip::model::RealmInfo>(&j).ok()) {
                s.realm_name = r.name.clone();
                s.empty_topic_name = r.empty_topic_name.clone();
                s.feature_level = r.feature_level;
                s.realm = r;
            }
            if let Some(v) = meta("recent_dms")?.and_then(|j| serde_json::from_str::<Vec<(Vec<u64>, u64)>>(&j).ok()) {
                s.recent_dms = v.into_iter().map(|(k, id)| (DmKey(k), id)).collect();
            }
            let json_rows = |sql: &str| -> rusqlite::Result<Vec<String>> {
                c.prepare(sql)?.query_map([], |r| r.get::<_, String>(0))?.collect()
            };
            for j in json_rows("SELECT json FROM users")? {
                if let Ok(u) = serde_json::from_str::<User>(&j) {
                    s.users.insert(u.user_id, u);
                }
            }
            for j in json_rows("SELECT json FROM streams")? {
                if let Ok(st) = serde_json::from_str::<Stream>(&j) {
                    s.streams.insert(st.stream_id, st);
                }
            }
            for j in json_rows("SELECT json FROM subscriptions")? {
                if let Ok(sub) = serde_json::from_str::<Subscription>(&j) {
                    s.subs.insert(sub.stream_id, sub);
                }
            }
            for j in json_rows("SELECT json FROM realm_emoji")? {
                if let Ok(e) = serde_json::from_str::<RealmEmoji>(&j) {
                    s.realm_emoji.insert(e.id.clone(), e);
                }
            }
            let mut q = c.prepare("SELECT stream_id, topic_lc, visibility FROM user_topics")?;
            for r in q.query_map([], |r| Ok((r.get::<_, u64>(0)?, r.get::<_, String>(1)?, r.get::<_, u8>(2)?)))? {
                let (sid, t, v) = r?;
                s.user_topics.insert(TopicKey { stream_id: sid, topic: t }, v.into());
            }
            let mut q = c.prepare("SELECT stream_id, topic_lc, display FROM topics")?;
            for r in q.query_map([], |r| Ok((r.get::<_, u64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))? {
                let (sid, t, d) = r?;
                s.topic_names.insert(TopicKey { stream_id: sid, topic: t }, d);
            }
            let mut q = c.prepare("SELECT id FROM muted_users")?;
            s.muted_users = q.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
            let mut q = c.prepare("SELECT id, conv, mention FROM unread_ids")?;
            for r in q.query_map([], |r| Ok((r.get::<_, u64>(0)?, r.get::<_, String>(1)?, r.get::<_, bool>(2)?)))? {
                let (id, conv, mention) = r?;
                if let Some(conv) = conv_from_str(&conv) {
                    s.unread.insert(id, conv);
                    if mention {
                        s.mentions.insert(id);
                    }
                }
            }
            Ok(Some(s))
        })
    }

    /// Up to `limit` messages of `conv` with id below `before`, oldest first.
    pub fn messages(&self, conv: &Conv, before: Option<MessageId>, limit: usize) -> Result<Vec<Message>> {
        let key = conv_to_str(conv);
        self.with_read(|c| {
            let mut q = c.prepare_cached("SELECT json FROM messages WHERE conv = ?1 AND id < ?2 ORDER BY id DESC LIMIT ?3")?;
            let mut v: Vec<Message> = q
                .query_map(params![key, before.unwrap_or(i64::MAX as u64) as i64, limit as i64], |r| r.get::<_, String>(0))?
                .filter_map(|j| j.ok().and_then(|j| serde_json::from_str(&j).ok()))
                .collect();
            v.reverse();
            Ok(v)
        })
    }

    /// Known-complete id ranges for `conv`, ascending.
    pub fn fetched_ranges(&self, conv: &Conv) -> Result<Vec<(MessageId, MessageId)>> {
        let key = conv_to_str(conv);
        self.with_read(|c| c.prepare("SELECT lo, hi FROM fetched_ranges WHERE conv = ?1 ORDER BY lo")?.query_map([key], |r| Ok((r.get(0)?, r.get(1)?)))?.collect())
    }

    /// Full-text search over cached messages, newest first. `query` is FTS5 syntax; plain
    /// words are ANDed.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<MessageId>> {
        self.with_read(|c| {
            c.prepare_cached("SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?1 ORDER BY rowid DESC LIMIT ?2")?
                .query_map(params![query, limit as i64], |r| r.get(0))?
                .collect()
        })
    }

    /// Cached messages matching a search, newest first, each with a snippet of its text where
    /// matches are wrapped in U+0002…U+0003. `words` is FTS5 syntax (empty: filters only).
    /// `conv_prefix` narrows by conversation key (`t:<stream>:` for a channel, a full key for
    /// a topic or DM).
    pub fn search_hits(&self, words: &str, sender: Option<u64>, conv_prefix: Option<&str>, limit: usize) -> Result<Vec<SearchHit>> {
        self.with_read(|c| {
            let mut sql = if words.is_empty() {
                "SELECT m.json, substr(m.text, 1, 200) FROM messages m WHERE 1".to_string()
            } else {
                "SELECT m.json, snippet(messages_fts, 0, char(2), char(3), '…', 16) FROM messages_fts JOIN messages m ON m.id = messages_fts.rowid WHERE messages_fts MATCH :words".to_string()
            };
            if sender.is_some() {
                sql.push_str(" AND m.sender_id = :sender");
            }
            if conv_prefix.is_some() {
                sql.push_str(" AND substr(m.conv, 1, length(:conv)) = :conv");
            }
            // Ordering by the search table's own rowid (the message id) lets SQLite walk the
            // matches newest first and stop at the limit. Ordering by `m.id` sorts every match
            // first, with a snippet built for each: 50 ms for "the" instead of 0.6.
            sql.push_str(if words.is_empty() { " ORDER BY m.id DESC LIMIT :limit" } else { " ORDER BY messages_fts.rowid DESC LIMIT :limit" });
            let mut q = c.prepare(&sql)?;
            let mut args: Vec<(&str, &dyn rusqlite::ToSql)> = vec![];
            let limit = limit as i64;
            let sender_v = sender.map(|s| s as i64);
            if !words.is_empty() {
                args.push((":words", &words));
            }
            if let Some(s) = &sender_v {
                args.push((":sender", s));
            }
            if let Some(p) = &conv_prefix {
                args.push((":conv", p));
            }
            args.push((":limit", &limit));
            let rows = q.query_map(args.as_slice(), |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            Ok(rows
                .filter_map(|r| r.ok())
                .filter_map(|(j, snippet)| serde_json::from_str(&j).ok().map(|message| SearchHit { message, snippet }))
                .collect())
        })
    }

    pub fn draft(&self, conv: &Conv) -> Result<Option<String>> {
        let key = conv_to_str(conv);
        self.with_read(|c| c.query_row("SELECT body FROM drafts WHERE conv = ?1", [key], |r| r.get(0)).optional())
    }

    /// The `n` conversations with the most cached messages, with their counts (for benchmarks).
    pub fn busiest_convs(&self, n: usize) -> Result<Vec<(Conv, u64)>> {
        self.with_read(|c| {
            let mut q = c.prepare("SELECT conv, count(*) AS k FROM messages GROUP BY conv ORDER BY k DESC LIMIT ?1")?;
            let v = q
                .query_map(params![n as i64], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
                .filter_map(|r| r.ok())
                .filter_map(|(k, n)| conv_from_str(&k).map(|c| (c, n as u64)))
                .collect();
            Ok(v)
        })
    }

    pub fn message_count(&self) -> Result<u64> {
        self.with_read(|c| c.query_row("SELECT count(*) FROM messages", [], |r| r.get(0)))
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        // Closing the channel ends the writer after it commits what is queued.
        let (tx, _) = mpsc::channel();
        drop(std::mem::replace(&mut self.tx, tx));
        if let Some(h) = self.writer.take() {
            let _ = h.join();
        }
    }
}

fn writer_loop(mut c: Connection, rx: mpsc::Receiver<Cmd>) {
    while let Ok(first) = rx.recv() {
        let mut batch: Vec<Write> = vec![];
        let mut waiters = vec![];
        let mut next = Some(first);
        // Coalesce whatever else is already queued into the same transaction.
        while let Some(cmd) = next.take() {
            match cmd {
                Cmd::Write(w) => batch.extend(w),
                Cmd::Flush(t) => waiters.push(t),
            }
            if batch.len() < 5000 {
                next = rx.try_recv().ok();
            }
        }
        if !batch.is_empty() {
            if let Err(e) = apply(&mut c, &batch) {
                tracing::error!("store write failed: {e:#}");
            }
        }
        for w in waiters {
            let _ = w.send(());
        }
    }
    let _ = c.pragma_update(None, "wal_checkpoint", "PASSIVE");
}

fn apply(c: &mut Connection, batch: &[Write]) -> Result<()> {
    let tx = c.transaction()?;
    for w in batch {
        match w {
            Write::UpsertUsers(us) => {
                let mut q = tx.prepare_cached("INSERT OR REPLACE INTO users (id, json) VALUES (?1, ?2)")?;
                for u in us {
                    q.execute(params![u.user_id as i64, serde_json::to_string(u)?])?;
                }
            }
            Write::Users(us) => {
                tx.execute("DELETE FROM users", [])?;
                let mut q = tx.prepare_cached("INSERT INTO users (id, json) VALUES (?1, ?2)")?;
                for u in us {
                    q.execute(params![u.user_id as i64, serde_json::to_string(u)?])?;
                }
            }
            Write::Streams { streams, subs } => {
                tx.execute("DELETE FROM streams", [])?;
                tx.execute("DELETE FROM subscriptions", [])?;
                let mut q = tx.prepare_cached("INSERT INTO streams (id, json) VALUES (?1, ?2)")?;
                for s in streams {
                    q.execute(params![s.stream_id as i64, serde_json::to_string(s)?])?;
                }
                let mut q = tx.prepare_cached("INSERT INTO subscriptions (stream_id, json) VALUES (?1, ?2)")?;
                for s in subs {
                    q.execute(params![s.stream_id as i64, serde_json::to_string(s)?])?;
                }
            }
            Write::UserTopics(ts) => {
                tx.execute("DELETE FROM user_topics", [])?;
                let mut q = tx.prepare_cached("INSERT INTO user_topics (stream_id, topic_lc, visibility) VALUES (?1, ?2, ?3)")?;
                for (k, v) in ts {
                    q.execute(params![k.stream_id as i64, k.topic, u8::from(*v)])?;
                }
            }
            Write::MutedUsers(ids) => {
                tx.execute("DELETE FROM muted_users", [])?;
                let mut q = tx.prepare_cached("INSERT INTO muted_users (id) VALUES (?1)")?;
                for id in ids {
                    q.execute([*id as i64])?;
                }
            }
            Write::RealmEmoji(es) => {
                tx.execute("DELETE FROM realm_emoji", [])?;
                let mut q = tx.prepare_cached("INSERT INTO realm_emoji (id, json) VALUES (?1, ?2)")?;
                for e in es {
                    q.execute(params![e.id, serde_json::to_string(e)?])?;
                }
            }
            Write::UnreadDelta { set, clear } => {
                let mut q = tx.prepare_cached("INSERT OR REPLACE INTO unread_ids (id, conv, mention) VALUES (?1, ?2, ?3)")?;
                for (id, conv, m) in set {
                    q.execute(params![*id as i64, conv_to_str(conv), *m])?;
                }
                let mut d = tx.prepare_cached("DELETE FROM unread_ids WHERE id = ?1")?;
                for id in clear {
                    d.execute(params![*id as i64])?;
                }
            }
            Write::Unreads(us) => {
                tx.execute("DELETE FROM unread_ids", [])?;
                let mut q = tx.prepare_cached("INSERT INTO unread_ids (id, conv, mention) VALUES (?1, ?2, ?3)")?;
                for (id, conv, m) in us {
                    q.execute(params![*id as i64, conv_to_str(conv), *m])?;
                }
            }
            Write::Topics(ts) => {
                let mut q = tx.prepare_cached(
                    "INSERT INTO topics (stream_id, topic_lc, display) VALUES (?1, ?2, ?3)
                     ON CONFLICT (stream_id, topic_lc) DO UPDATE SET display = excluded.display",
                )?;
                for (k, d) in ts {
                    q.execute(params![k.stream_id as i64, k.topic, d])?;
                }
            }
            Write::UpsertMessages(ms) => {
                // An unchanged message (a page re-validating the cache) writes nothing: no
                // row rewrite, no search reindex through the update trigger.
                let mut q = tx.prepare_cached(
                    "INSERT INTO messages (id, conv, sender_id, ts, topic, text, json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                     ON CONFLICT (id) DO UPDATE SET conv = excluded.conv, topic = excluded.topic, text = excluded.text, json = excluded.json
                     WHERE messages.json IS NOT excluded.json",
                )?;
                let mut t = tx.prepare_cached(
                    "INSERT INTO topics (stream_id, topic_lc, display, max_id) VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT (stream_id, topic_lc) DO UPDATE SET max_id = max(max_id, excluded.max_id)",
                )?;
                for (m, conv) in ms {
                    q.execute(params![
                        m.id as i64,
                        conv_to_str(conv),
                        m.sender_id as i64,
                        m.timestamp,
                        m.subject,
                        html_to_text(&m.content),
                        serde_json::to_string(m)?
                    ])?;
                    if let Conv::Topic(k) = conv {
                        t.execute(params![k.stream_id as i64, k.topic, m.subject, m.id as i64])?;
                    }
                }
            }
            Write::DeleteMessages(ids) => {
                let mut q = tx.prepare_cached("DELETE FROM messages WHERE id = ?1")?;
                for id in ids {
                    q.execute([*id as i64])?;
                }
            }
            Write::FetchedRange { conv, lo, hi } => {
                let key = conv_to_str(conv);
                // Merge with every overlapping range.
                let (mut lo, mut hi) = (*lo as i64, *hi as i64);
                let overl: Vec<(i64, i64)> = tx
                    .prepare_cached("SELECT lo, hi FROM fetched_ranges WHERE conv = ?1 AND lo <= ?3 AND hi >= ?2")?
                    .query_map(params![key, lo, hi], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<_>>()?;
                for (l, h) in overl {
                    lo = lo.min(l);
                    hi = hi.max(h);
                    tx.execute("DELETE FROM fetched_ranges WHERE conv = ?1 AND lo = ?2", params![key, l])?;
                }
                tx.execute("INSERT INTO fetched_ranges (conv, lo, hi) VALUES (?1, ?2, ?3)", params![key, lo, hi])?;
            }
            Write::Draft { conv, body } => {
                if body.trim().is_empty() {
                    tx.execute("DELETE FROM drafts WHERE conv = ?1", [conv_to_str(conv)])?;
                } else {
                    tx.execute(
                        "INSERT INTO drafts (conv, body, updated) VALUES (?1, ?2, strftime('%s','now'))
                         ON CONFLICT (conv) DO UPDATE SET body = excluded.body, updated = excluded.updated",
                        params![conv_to_str(conv), body],
                    )?;
                }
            }
            Write::Meta { key, value } => {
                tx.execute("INSERT INTO meta (key, value) VALUES (?1, ?2) ON CONFLICT (key) DO UPDATE SET value = excluded.value", params![key, value])?;
            }
        }
    }
    tx.commit()?;
    Ok(())
}

/// Per-account database file name: stable across email changes.
/// One search result from the store.
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub message: Message,
    /// Text around the matches; matches are wrapped in U+0002…U+0003.
    pub snippet: String,
}

/// FTS5 query for plain words: each becomes a quoted prefix term, so punctuation can't
/// break the syntax and partial words still match.
pub fn fts_words(text: &str) -> String {
    text.split_whitespace()
        .map(|w| w.chars().filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-').collect::<String>())
        .filter(|w| !w.is_empty())
        .map(|w| format!("\"{}\"*", w.replace('"', "")))
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn account_file(realm: &str, user_id: UserId) -> String {
    let host: String = realm.trim_start_matches("https://").trim_start_matches("http://").chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' { c } else { '_' }).collect();
    format!("{host}-{user_id}.sqlite")
}

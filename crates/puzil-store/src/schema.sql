-- Puzil per-account store, schema version 1.
-- Structured records are kept as JSON next to the columns we query on; the JSON is the
-- server's shape, so new fields need no migration.
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE users (id INTEGER PRIMARY KEY, json TEXT NOT NULL);
CREATE TABLE streams (id INTEGER PRIMARY KEY, json TEXT NOT NULL);
CREATE TABLE subscriptions (stream_id INTEGER PRIMARY KEY, json TEXT NOT NULL);
CREATE TABLE realm_emoji (id TEXT PRIMARY KEY, json TEXT NOT NULL);
CREATE TABLE user_topics (stream_id INTEGER NOT NULL, topic_lc TEXT NOT NULL, visibility INTEGER NOT NULL,
                          PRIMARY KEY (stream_id, topic_lc)) WITHOUT ROWID;
CREATE TABLE muted_users (id INTEGER PRIMARY KEY);
-- Display names and the newest known message per topic. No unread counts: those derive
-- from unread_ids.
CREATE TABLE topics (stream_id INTEGER NOT NULL, topic_lc TEXT NOT NULL, display TEXT NOT NULL, max_id INTEGER NOT NULL DEFAULT 0,
                     PRIMARY KEY (stream_id, topic_lc)) WITHOUT ROWID;
-- rowid is the message id. `conv` is "t:<stream>:<topic_lc>" or "d:<id>,<id>,...".
CREATE TABLE messages (
  id INTEGER PRIMARY KEY,
  conv TEXT NOT NULL,
  sender_id INTEGER NOT NULL,
  ts INTEGER NOT NULL,
  topic TEXT NOT NULL,      -- display topic, for FTS
  text TEXT NOT NULL,       -- plain text of the rendered HTML, for FTS
  json TEXT NOT NULL
);
CREATE INDEX messages_conv ON messages (conv, id);
-- Contiguous id ranges known to be complete per conversation (inclusive). Lets a narrow
-- open from cache and fetch only the gap.
CREATE TABLE fetched_ranges (conv TEXT NOT NULL, lo INTEGER NOT NULL, hi INTEGER NOT NULL, PRIMARY KEY (conv, lo)) WITHOUT ROWID;
CREATE TABLE unread_ids (id INTEGER PRIMARY KEY, conv TEXT NOT NULL, mention INTEGER NOT NULL DEFAULT 0);
CREATE TABLE outbox (local_id TEXT PRIMARY KEY, queue_id TEXT, json TEXT NOT NULL, created INTEGER NOT NULL);
CREATE TABLE drafts (conv TEXT PRIMARY KEY, body TEXT NOT NULL, updated INTEGER NOT NULL);
CREATE TABLE overlay_votes (message_id INTEGER PRIMARY KEY, json TEXT NOT NULL, fetched INTEGER NOT NULL);
CREATE TABLE overlay_inline (message_id INTEGER PRIMARY KEY, json TEXT NOT NULL, fetched INTEGER NOT NULL);

-- External-content FTS: the index stores only tokens; text lives in `messages`.
-- prefix: search runs as you type, and a one- or two-letter prefix otherwise scans every
-- term that starts with it (version 2).
CREATE VIRTUAL TABLE messages_fts USING fts5(text, topic, content='messages', content_rowid='id',
                                             tokenize='unicode61 remove_diacritics 2', prefix='1 2');
CREATE TRIGGER messages_ai AFTER INSERT ON messages BEGIN
  INSERT INTO messages_fts(rowid, text, topic) VALUES (new.id, new.text, new.topic);
END;
CREATE TRIGGER messages_ad AFTER DELETE ON messages BEGIN
  INSERT INTO messages_fts(messages_fts, rowid, text, topic) VALUES ('delete', old.id, old.text, old.topic);
END;
CREATE TRIGGER messages_au AFTER UPDATE OF text, topic ON messages BEGIN
  INSERT INTO messages_fts(messages_fts, rowid, text, topic) VALUES ('delete', old.id, old.text, old.topic);
  INSERT INTO messages_fts(rowid, text, topic) VALUES (new.id, new.text, new.topic);
END;

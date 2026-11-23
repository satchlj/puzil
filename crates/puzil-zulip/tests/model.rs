//! Domain-model replay tests on synthetic events, plus any private recordings in
//! `reference/rec-*.jsonl` (gitignored), which are replayed against their final register.

use puzil_zulip::{
    State,
    events::parse_batch,
    model::{Conv, DmKey, TopicKey},
    replay::{Recording, diff_unreads},
    types::Register,
};
use serde_json::{Value, json};

const ME: u64 = 10;

fn register(unread: Value) -> Register {
    serde_json::from_value(json!({
        "queue_id": "q", "last_event_id": -1, "zulip_feature_level": 513, "user_id": ME,
        "realm_users": [
            {"user_id": 10, "full_name": "Me"}, {"user_id": 11, "full_name": "Ada"},
            {"user_id": 12, "full_name": "Bo"}, {"user_id": 13, "full_name": "Cy"}
        ],
        "subscriptions": [
            {"stream_id": 1, "name": "general", "is_muted": false},
            {"stream_id": 2, "name": "noisy", "is_muted": true}
        ],
        "never_subscribed": [{"stream_id": 3, "name": "elsewhere"}],
        "user_topics": [{"stream_id": 1, "topic_name": "Hush", "visibility_policy": 1},
                        {"stream_id": 2, "topic_name": "Signal", "visibility_policy": 3}],
        "muted_users": [{"id": 13}],
        "unread_msgs": unread
    }))
    .unwrap()
}

fn stream_msg(id: u64, stream: u64, topic: &str, sender: u64, flags: &[&str]) -> Value {
    json!({"type": "message", "flags": flags, "message": {
        "id": id, "sender_id": sender, "type": "stream", "stream_id": stream, "subject": topic,
        "display_recipient": "s", "content": "<p>x</p>", "timestamp": 0}})
}

fn dm_msg(id: u64, sender: u64, users: &[u64], flags: &[&str]) -> Value {
    let rec: Vec<Value> = users.iter().map(|u| json!({"id": u})).collect();
    json!({"type": "message", "flags": flags, "message": {
        "id": id, "sender_id": sender, "type": "private", "display_recipient": rec,
        "content": "<p>x</p>", "timestamp": 0}})
}

fn topic(s: u64, t: &str) -> Conv {
    Conv::Topic(TopicKey::new(s, t))
}

fn apply(s: &mut State, evs: Vec<Value>) {
    s.apply_all(&parse_batch(&evs));
}

#[test]
fn seeds_unreads_with_correct_keys() {
    let s = State::from_register(&register(json!({
        "pms": [{"other_user_id": 11, "unread_message_ids": [1]}, {"other_user_id": 10, "unread_message_ids": [2]}],
        "huddles": [{"user_ids_string": "10,11,12", "unread_message_ids": [3]}],
        "streams": [{"stream_id": 1, "topic": "Hello World", "unread_message_ids": [4, 5]}],
        "mentions": [5]
    })));
    assert_eq!(s.unread[&1], Conv::Dm(DmKey(vec![10, 11])));
    assert_eq!(s.unread[&2], Conv::Dm(DmKey(vec![10])), "self-DM is {{me}}");
    assert_eq!(s.unread[&3], Conv::Dm(DmKey(vec![10, 11, 12])));
    assert_eq!(s.unread[&4], topic(1, "hello world"));
    assert_eq!(s.topic_display(&TopicKey::new(1, "HELLO world")), "Hello World");
    assert!(s.mentions.contains(&5));
}

#[test]
fn new_messages_and_dm_keys_match_register_keys() {
    let mut s = State::from_register(&register(json!({})));
    apply(&mut s, vec![
        dm_msg(20, 11, &[10, 11], &[]),
        dm_msg(21, 10, &[10], &[]),
        dm_msg(22, 12, &[12, 11, 10], &["mentioned"]),
        stream_msg(23, 1, "Topic", 11, &["read"]),
        stream_msg(24, 1, "TOPIC", 11, &["wildcard_mentioned"]),
    ]);
    assert_eq!(s.unread[&20], Conv::Dm(DmKey(vec![10, 11])));
    assert_eq!(s.unread[&21], Conv::Dm(DmKey(vec![10])));
    assert_eq!(s.unread[&22], Conv::Dm(DmKey(vec![10, 11, 12])));
    assert!(!s.unread.contains_key(&23));
    assert_eq!(s.unread[&24], topic(1, "topic"));
    assert_eq!(s.mentions.iter().copied().collect::<std::collections::BTreeSet<_>>(), [22, 24].into());
}

#[test]
fn read_unread_with_details_and_mark_all() {
    let mut s = State::from_register(&register(json!({
        "streams": [{"stream_id": 1, "topic": "t", "unread_message_ids": [1, 2]}], "mentions": [2]
    })));
    apply(&mut s, vec![json!({"type": "update_message_flags", "op": "add", "flag": "read", "messages": [1, 2], "all": false})]);
    assert!(s.unread.is_empty() && s.mentions.is_empty());
    // Mark unread: messages we never loaded are filed from message_details.
    apply(&mut s, vec![json!({"type": "update_message_flags", "op": "remove", "flag": "read", "messages": [7, 8], "all": false,
        "message_details": {
            "7": {"type": "private", "mentioned": false, "user_ids": [11, 12]},
            "8": {"type": "stream", "mentioned": true, "stream_id": 1, "topic": "Moved Here", "unmuted_stream_msg": true}}})]);
    assert_eq!(s.unread[&7], Conv::Dm(DmKey(vec![10, 11, 12])));
    assert_eq!(s.unread[&8], topic(1, "moved here"));
    assert!(s.mentions.contains(&8));
    apply(&mut s, vec![json!({"type": "update_message_flags", "op": "add", "flag": "read", "messages": [], "all": true})]);
    assert!(s.unread.is_empty() && s.mentions.is_empty());
}

#[test]
fn moves_rekey_unreads_and_cache() {
    let mut s = State::from_register(&register(json!({
        "streams": [{"stream_id": 1, "topic": "old", "unread_message_ids": [1, 2, 3]}]
    })));
    apply(&mut s, vec![stream_msg(4, 1, "old", 11, &[])]);
    // change_later from 2: messages 2, 3, 4 move topic.
    apply(&mut s, vec![json!({"type": "update_message", "message_id": 2, "message_ids": [2, 3, 4], "propagate_mode": "change_later",
        "stream_id": 1, "orig_subject": "old", "subject": "New", "edit_timestamp": 5, "rendering_only": false, "flags": []})]);
    assert_eq!(s.unread[&1], topic(1, "old"));
    assert_eq!(s.unread[&3], topic(1, "new"));
    assert_eq!(s.messages[&4].subject, "New");
    // Resolve is a rename with the ✔ prefix.
    apply(&mut s, vec![json!({"type": "update_message", "message_id": 3, "message_ids": [2, 3, 4], "propagate_mode": "change_all",
        "stream_id": 1, "orig_subject": "New", "subject": "✔ New", "rendering_only": false, "flags": []})]);
    assert_eq!(s.unread[&2], topic(1, "✔ new"));
    // Channel move keeps the topic when no subject is sent.
    apply(&mut s, vec![json!({"type": "update_message", "message_id": 1, "message_ids": [1], "propagate_mode": "change_one",
        "stream_id": 1, "new_stream_id": 2, "orig_subject": "old", "rendering_only": false, "flags": []})]);
    assert_eq!(s.unread[&1], topic(2, "old"));
}

#[test]
fn content_edit_does_not_move_and_tracks_mentions() {
    let mut s = State::from_register(&register(json!({})));
    apply(&mut s, vec![stream_msg(1, 1, "t", 11, &[])]);
    apply(&mut s, vec![json!({"type": "update_message", "message_id": 1, "message_ids": [1], "rendered_content": "<p>@Me</p>",
        "edit_timestamp": 9, "rendering_only": false, "flags": ["mentioned"], "stream_id": 1})]);
    assert_eq!(s.messages[&1].content, "<p>@Me</p>");
    assert_eq!(s.messages[&1].last_edit_timestamp, Some(9));
    assert_eq!(s.unread[&1], topic(1, "t"));
    assert!(s.mentions.contains(&1));
    apply(&mut s, vec![json!({"type": "update_message", "message_id": 1, "message_ids": [1], "rendered_content": "<p>link preview</p>",
        "edit_timestamp": 10, "rendering_only": true, "flags": ["mentioned"], "stream_id": 1})]);
    assert_eq!(s.messages[&1].last_edit_timestamp, Some(9), "rendering_only keeps the edit time");
}

#[test]
fn bulk_delete_reactions_and_unknown_events() {
    let mut s = State::from_register(&register(json!({})));
    apply(&mut s, vec![stream_msg(1, 1, "t", 11, &[]), stream_msg(2, 1, "t", 11, &[])]);
    apply(&mut s, vec![
        json!({"type": "reaction", "op": "add", "message_id": 1, "emoji_name": "tada", "emoji_code": "1f389", "reaction_type": "unicode_emoji", "user_id": 11}),
        json!({"type": "reaction", "op": "add", "message_id": 1, "emoji_name": "tada", "emoji_code": "1f389", "reaction_type": "unicode_emoji", "user_id": 11}),
        json!({"type": "delete_message", "message_ids": [2], "message_type": "stream", "stream_id": 1, "topic": "t"}),
        json!({"type": "something_new", "op": "frobnicate"}),
    ]);
    assert_eq!(s.messages[&1].reactions.len(), 1, "duplicate add is idempotent");
    assert!(!s.unread.contains_key(&2) && !s.messages.contains_key(&2));
    assert_eq!(s.unmodeled.get("something_new/frobnicate"), Some(&1));
}

#[test]
fn muting_rules_for_counts() {
    let s = State::from_register(&register(json!({})));
    assert!(s.counts_unread(&topic(1, "anything")));
    assert!(!s.counts_unread(&topic(1, "hush")), "muted topic");
    assert!(!s.counts_unread(&topic(2, "other")), "muted channel");
    assert!(s.counts_unread(&topic(2, "signal")), "followed topic in muted channel");
    assert!(!s.counts_unread(&topic(3, "x")), "not subscribed");
    assert!(!s.counts_unread(&Conv::Dm(DmKey(vec![10, 13]))), "muted user 1:1");
    assert!(s.counts_unread(&Conv::Dm(DmKey(vec![10, 12, 13]))), "group DMs still count");
}

#[test]
fn replay_private_recordings() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../reference");
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        if !(name.starts_with("rec-") && name.ends_with(".jsonl")) {
            continue;
        }
        let rec = Recording::parse(&std::fs::read_to_string(&p).unwrap()).unwrap();
        let Some(f) = &rec.final_register else { continue };
        let d = diff_unreads(&rec.replay(), &State::from_register(f));
        assert!(d.is_empty(), "{name}: {d:#?}");
        eprintln!("{name}: {} events replayed, unreads match", rec.event_count());
    }
}

#[test]
fn deleting_newest_dm_message_updates_recent_dms() {
    let mut s = State::from_register(&register(json!({})));
    apply(&mut s, vec![dm_msg(30, 10, &[10, 12], &["read"])]);
    let k = DmKey(vec![10, 12]);
    assert_eq!(s.recent_dms.get(&k), Some(&30));
    apply(&mut s, vec![json!({"type": "delete_message", "message_ids": [30], "message_type": "private"})]);
    assert!(!s.recent_dms.contains_key(&k), "a DM whose only message was deleted leaves the list");
}

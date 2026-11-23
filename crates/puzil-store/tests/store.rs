use puzil_store::{Store, Write, conv_from_str, conv_to_str, html_to_text, writes_for};
use puzil_zulip::{
    State,
    events::parse_batch,
    model::{Conv, DmKey, TopicKey},
    types::Register,
};
use serde_json::json;

struct TempDb(std::path::PathBuf);
impl TempDb {
    fn new(name: &str) -> Self {
        let d = std::env::temp_dir().join(format!("puzil-store-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        Self(d)
    }
    fn path(&self) -> std::path::PathBuf {
        self.0.join("acct.sqlite")
    }
}
impl Drop for TempDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn state() -> State {
    let r: Register = serde_json::from_value(json!({
        "queue_id": "q", "last_event_id": -1, "zulip_feature_level": 513, "user_id": 10,
        "realm_users": [{"user_id": 10, "full_name": "Me"}, {"user_id": 11, "full_name": "Ada"}],
        "subscriptions": [{"stream_id": 1, "name": "general", "color": "#abc"}],
        "user_topics": [{"stream_id": 1, "topic_name": "Hush", "visibility_policy": 1}],
        "muted_users": [{"id": 12}],
        "unread_msgs": {"streams": [{"stream_id": 1, "topic": "Hello", "unread_message_ids": [5]}],
                        "pms": [{"other_user_id": 11, "unread_message_ids": [6]}], "mentions": [6]}
    }))
    .unwrap();
    State::from_register(&r)
}

fn msg(id: u64, topic: &str, html: &str) -> serde_json::Value {
    json!({"type": "message", "flags": [], "message": {"id": id, "sender_id": 11, "type": "stream", "stream_id": 1,
        "subject": topic, "display_recipient": "general", "content": html, "timestamp": id}})
}

#[test]
fn conv_keys_roundtrip() {
    for c in [Conv::Topic(TopicKey::new(3, "a:b c")), Conv::Topic(TopicKey::new(3, "")), Conv::Dm(DmKey(vec![1, 2, 30]))] {
        assert_eq!(conv_from_str(&conv_to_str(&c)), Some(c));
    }
}

#[test]
fn html_to_text_keeps_tex_and_drops_katex_noise() {
    let html = r#"<p>Let <span class="katex"><span class="katex-mathml"><math xmlns="http://www.w3.org/1998/Math/MathML"><semantics><mrow><mi>x</mi></mrow><annotation encoding="application/x-tex">x^2</annotation></semantics></math></span><span class="katex-html" aria-hidden="true"><span class="base"><span class="mord">x</span></span></span></span> &amp; go</p>"#;
    let t = html_to_text(html);
    assert!(t.contains("Let") && t.contains("x^2") && t.contains("& go"), "{t:?}");
    assert_eq!(t.matches('x').count(), 1, "visible KaTeX and MathML are dropped: {t:?}");
}

#[test]
fn snapshot_roundtrips_through_reopen() {
    let db = TempDb::new("snap");
    let s = state();
    {
        let st = Store::open(&db.path()).unwrap();
        assert!(st.load_state().unwrap().is_none());
        st.write(writes_for(&s, &puzil_zulip::model::Changes { all: true, ..Default::default() }));
        st.flush();
    }
    let st = Store::open(&db.path()).unwrap();
    let l = st.load_state().unwrap().unwrap();
    assert_eq!(l.me, 10);
    assert_eq!(l.users.len(), 2);
    assert_eq!(l.subs[&1].color, "#abc");
    assert_eq!(l.unread, s.unread);
    assert_eq!(l.mentions, s.mentions);
    assert_eq!(l.muted_users, s.muted_users);
    assert_eq!(l.user_topics, s.user_topics);
    assert_eq!(l.topic_display(&TopicKey::new(1, "hello")), "Hello");
    assert!(!l.counts_unread(&Conv::Topic(TopicKey::new(1, "hush"))));
}

#[test]
fn write_behind_messages_moves_deletes_and_search() {
    let db = TempDb::new("msgs");
    let st = Store::open(&db.path()).unwrap();
    let mut s = state();
    st.write(writes_for(&s, &puzil_zulip::model::Changes { all: true, ..Default::default() }));
    let evs = parse_batch(&[
        msg(100, "Topic A", "<p>Café au lait with <strong>naïve</strong> résumé</p>"),
        msg(101, "Topic A", "<p>second message</p>"),
        msg(102, "Other", "<p>unrelated text</p>"),
    ]);
    let ch = s.apply_all(&evs);
    st.write(writes_for(&s, &ch));
    st.flush();
    let a = Conv::Topic(TopicKey::new(1, "topic a"));
    assert_eq!(st.messages(&a, None, 10).unwrap().iter().map(|m| m.id).collect::<Vec<_>>(), [100, 101]);
    assert_eq!(st.messages(&a, Some(101), 10).unwrap().len(), 1);
    // Diacritics are folded; the topic is indexed too.
    assert_eq!(st.search("cafe naive", 10).unwrap(), [100]);
    assert_eq!(st.search("topic:other", 10).unwrap(), [102]);
    // Move 101 to a new topic: it is re-filed and re-indexed.
    let ch = s.apply_all(&parse_batch(&[json!({"type": "update_message", "message_id": 101, "message_ids": [101],
        "propagate_mode": "change_one", "stream_id": 1, "orig_subject": "Topic A", "subject": "Split", "rendering_only": false, "flags": []})]));
    st.write(writes_for(&s, &ch));
    // Edit 100's content.
    let ch = s.apply_all(&parse_batch(&[json!({"type": "update_message", "message_id": 100, "message_ids": [100],
        "rendered_content": "<p>tea instead</p>", "edit_timestamp": 7, "rendering_only": false, "flags": [], "stream_id": 1})]));
    st.write(writes_for(&s, &ch));
    // Delete 102.
    let ch = s.apply_all(&parse_batch(&[json!({"type": "delete_message", "message_ids": [102], "message_type": "stream"})]));
    st.write(writes_for(&s, &ch));
    st.flush();
    assert_eq!(st.messages(&Conv::Topic(TopicKey::new(1, "split")), None, 10).unwrap()[0].id, 101);
    assert_eq!(st.messages(&a, None, 10).unwrap().iter().map(|m| m.id).collect::<Vec<_>>(), [100]);
    assert!(st.search("cafe", 10).unwrap().is_empty(), "old text left the index");
    assert_eq!(st.search("tea", 10).unwrap(), [100]);
    assert!(st.search("unrelated", 10).unwrap().is_empty());
    assert_eq!(st.message_count().unwrap(), 2);
}

#[test]
fn search_hits_snippets_filters_and_unsafe_input() {
    let db = TempDb::new("hits");
    let st = Store::open(&db.path()).unwrap();
    let mut s = state();
    st.write(writes_for(&s, &puzil_zulip::model::Changes { all: true, ..Default::default() }));
    let ch = s.apply_all(&parse_batch(&[
        msg(200, "Topic A", "<p>The natural latent argument goes through</p>"),
        msg(201, "Other", "<p>latents everywhere</p>"),
    ]));
    st.write(writes_for(&s, &ch));
    st.flush();
    let ids = |h: Vec<puzil_store::SearchHit>| h.iter().map(|h| h.message.id).collect::<Vec<_>>();
    // Prefix terms: "latent" finds "latents" too; newest first; snippets mark the match.
    let hits = st.search_hits(&puzil_store::fts_words("latent"), None, None, 10).unwrap();
    assert_eq!(ids(hits.clone()), [201, 200]);
    assert!(hits[1].snippet.contains("\u{2}latent\u{3}"), "{:?}", hits[1].snippet);
    // Narrowed to a topic's conversation key.
    let key = puzil_store::conv_to_str(&Conv::Topic(TopicKey::new(1, "topic a")));
    assert_eq!(ids(st.search_hits(&puzil_store::fts_words("latent"), None, Some(&key), 10).unwrap()), [200]);
    // FTS syntax in the input can't break the query.
    assert!(st.search_hits(&puzil_store::fts_words("\"latent AND ( NEAR"), None, None, 10).is_ok());
    // Filters alone.
    assert_eq!(ids(st.search_hits("", None, Some("t:1:"), 10).unwrap()), [201, 200]);
}

#[test]
fn fetched_ranges_merge_and_drafts() {
    let db = TempDb::new("ranges");
    let st = Store::open(&db.path()).unwrap();
    let c = Conv::Dm(DmKey(vec![10, 11]));
    st.write(vec![
        Write::FetchedRange { conv: c.clone(), lo: 10, hi: 20 },
        Write::FetchedRange { conv: c.clone(), lo: 30, hi: 40 },
        Write::FetchedRange { conv: c.clone(), lo: 20, hi: 30 },
        Write::FetchedRange { conv: c.clone(), lo: 50, hi: 60 },
        Write::Draft { conv: c.clone(), body: "half-written".into() },
    ]);
    st.flush();
    assert_eq!(st.fetched_ranges(&c).unwrap(), [(10, 40), (50, 60)]);
    assert_eq!(st.draft(&c).unwrap().as_deref(), Some("half-written"));
    st.write(vec![Write::Draft { conv: c.clone(), body: "  ".into() }]);
    st.flush();
    assert_eq!(st.draft(&c).unwrap(), None);
}

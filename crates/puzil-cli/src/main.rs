//! Headless Puzil: proves sync without a UI.
//!
//!     puzil unreads                 register once, print the unread tree
//!     puzil watch                   keep a queue open, print events and unread changes
//!     puzil record OUT MINUTES      save register + raw events + a final register (JSONL)
//!     puzil replay FILE             replay a recording and diff unreads against its final register
//!     puzil sync [N]                register, store the snapshot, cache the newest N messages
//!     puzil unreads --offline       print the unread tree from the store alone
//!     puzil search QUERY            full-text search of cached messages (FTS5 syntax)
//!     puzil bench [CORPUS] [REPLAY] time the offline pieces (store, search, render, math,
//!                                   replay) and print JSON; never touches the network
//!
//! The store lives in `PUZIL_DATA_DIR`, by default the gitignored `reference/data/`.
//! Credentials come from `PUZIL_ZULIPRC` or `~/.zuliprc` (Zulip's standard place).

use std::{io::Write, path::PathBuf, time::Duration};

use anyhow::{Context, Result};
use puzil_zulip::{
    Client, State,
    model::Conv,
    queue::{self, Update},
    replay::{Line, Recording, diff_unreads},
};

fn client() -> Result<Client> {
    let path = std::env::var_os("PUZIL_ZULIPRC")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".zuliprc")))
        .context("no zuliprc")?;
    Ok(Client::new(puzil_zulip::zuliprc::load(&path)?))
}

fn data_dir() -> PathBuf {
    std::env::var_os("PUZIL_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../reference/data"))
}

fn store_for(c: &Client, user_id: u64) -> Result<puzil_store::Store> {
    puzil_store::Store::open(&data_dir().join(puzil_store::account_file(&c.creds.realm, user_id)))
}

/// Finds the one account store in the data dir (the CLI is single-account for now).
fn only_store() -> Result<puzil_store::Store> {
    let mut found = std::fs::read_dir(data_dir())
        .context("no data dir; run `puzil sync` first")?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "sqlite"));
    let p = found.next().context("no store; run `puzil sync` first")?;
    puzil_store::Store::open(&p)
}

async fn sync(n: u32) -> Result<()> {
    let client = client()?;
    let t = std::time::Instant::now();
    let (reg, _) = queue::register(&client).await?;
    let mut s = State::from_register(&reg);
    let t_reg = t.elapsed();
    let store = store_for(&client, s.me)?;
    store.write(puzil_store::writes_for(&s, &puzil_zulip::model::Changes { all: true, ..Default::default() }));
    let (mut anchor, mut left, mut fetched) = (puzil_zulip::client::Anchor::Newest, n, 0usize);
    let t_fetch = std::time::Instant::now();
    while left > 0 {
        let page = client.messages(&serde_json::json!([]), anchor, left.min(1000), 0).await?;
        let oldest = page.messages.first().map(|m| m.id);
        let mut ch = puzil_zulip::model::Changes::default();
        for m in page.messages {
            ch.messages.insert(m.id);
            s.messages.insert(m.id, m);
        }
        fetched += ch.messages.len();
        store.write(puzil_store::writes_for(&s, &ch));
        s.messages.clear(); // the store holds them now
        match oldest {
            Some(o) if !page.found_oldest && o > 1 => {
                anchor = puzil_zulip::client::Anchor::Id(o - 1);
                left = left.saturating_sub(1000);
            }
            _ => break,
        }
    }
    store.flush();
    println!(
        "register {:.0?}; fetched and stored {fetched} messages in {:.1?}; store now holds {}",
        t_reg,
        t_fetch.elapsed(),
        store.message_count()?
    );
    Ok(())
}

fn print_tree(s: &State) {
    let by_conv = s.unread_by_conv();
    let mut streams: std::collections::BTreeMap<String, Vec<(String, usize, bool)>> = Default::default();
    let mut dms = vec![];
    for (c, ids) in &by_conv {
        match c {
            Conv::Topic(k) => {
                let name = s.streams.get(&k.stream_id).map(|st| st.name.clone()).unwrap_or_else(|| format!("stream {}", k.stream_id));
                streams.entry(name).or_default().push((s.topic_display(k), ids.len(), s.counts_unread(c)));
            }
            Conv::Dm(k) => dms.push((s.dm_display(k), ids.len(), s.counts_unread(c))),
        }
    }
    let counted: usize = by_conv.iter().filter(|(c, _)| s.counts_unread(c)).map(|(_, v)| v.len()).sum();
    println!(
        "{} unread ({} counted), {} mentions{}",
        s.unread.len(),
        counted,
        s.mentions.len(),
        if s.old_unreads_missing { ", older unreads not loaded" } else { "" }
    );
    let mark = |counted: bool| if counted { "" } else { "  (muted/unsubscribed)" };
    for (name, topics) in &streams {
        println!("#{name}  {}", topics.iter().map(|t| t.1).sum::<usize>());
        for (t, n, c) in topics {
            println!("    {t}  {n}{}", mark(*c));
        }
    }
    if !dms.is_empty() {
        println!("Direct messages");
        for (who, n, c) in &dms {
            println!("    {who}  {n}{}", mark(*c));
        }
    }
}

fn describe(e: &puzil_zulip::events::Event) -> String {
    use puzil_zulip::events::Event::*;
    match e {
        Message { message, flags } => format!("message {} from {} read={}", message.id, message.sender_full_name, flags.iter().any(|f| f == "read")),
        UpdateMessage(u) => format!("update_message {:?} move={}", u.message_id, u.is_move()),
        DeleteMessage { message_ids, .. } => format!("delete {message_ids:?}"),
        UpdateMessageFlags(f) => format!("flags {} {} on {} message(s){}", f.op, f.flag, f.messages.len(), if f.all { " (all)" } else { "" }),
        Reaction { add, message_id, reaction } => format!("reaction {} :{}: on {message_id}", if *add { "+" } else { "-" }, reaction.emoji_name),
        Other { kind, .. } => format!("(unmodeled) {kind}"),
        other => format!("{other:?}").chars().take(100).collect(),
    }
}

async fn watch(record: Option<(PathBuf, Duration)>) -> Result<()> {
    let client = client()?;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(queue::run(client.clone(), tx));
    let mut out = match &record {
        Some((p, _)) => Some(std::io::BufWriter::new(std::fs::File::create(p)?)),
        None => None,
    };
    let deadline = record.as_ref().map(|(_, d)| tokio::time::Instant::now() + *d);
    let mut state: Option<State> = None;
    // Plain `watch` persists through write-behind; `record` leaves the store alone.
    let mut store: Option<puzil_store::Store> = None;
    loop {
        let next = match deadline {
            Some(d) => match tokio::time::timeout_at(d, rx.recv()).await {
                Ok(u) => u,
                Err(_) => break,
            },
            None => rx.recv().await,
        };
        let Some(u) = next else { break };
        match u {
            Update::Registered { register, raw } => {
                if let Some(o) = &mut out {
                    // A re-register mid-recording invalidates replay; start a fresh file instead.
                    anyhow::ensure!(state.is_none(), "queue was lost mid-recording; recording is incomplete");
                    writeln!(o, "{}", serde_json::to_string(&Line::Register(raw))?)?;
                }
                let s = State::from_register(&register);
                if out.is_none() {
                    let st = store_for(&client, s.me)?;
                    st.write(puzil_store::writes_for(&s, &puzil_zulip::model::Changes { all: true, ..Default::default() }));
                    store = Some(st);
                }
                eprintln!("registered: feature level {}, {} users, {} subscriptions", s.feature_level, s.users.len(), s.subs.len());
                if out.is_none() {
                    print_tree(&s);
                }
                state = Some(s);
            }
            Update::Events { events, raw } => {
                if let Some(o) = &mut out {
                    writeln!(o, "{}", serde_json::to_string(&Line::Events(raw))?)?;
                    o.flush()?;
                }
                let s = state.as_mut().context("events before register")?;
                for e in &events {
                    eprintln!("  {}", describe(e));
                }
                let ch = s.apply_all(&events);
                if let Some(st) = &store {
                    st.write(puzil_store::writes_for(s, &ch));
                }
                if ch.unreads {
                    eprintln!("  → {} unread, {} mentions", s.unread.len(), s.mentions.len());
                }
            }
            Update::Retrying { error, after } => eprintln!("retrying in {after:.1?}: {error}"),
            Update::Unauthorized(m) => anyhow::bail!("unauthorized: {m}"),
        }
    }
    if let (Some(o), Some(_)) = (&mut out, &state) {
        let (_, raw) = queue::register(&client).await?;
        writeln!(o, "{}", serde_json::to_string(&Line::FinalRegister(raw))?)?;
        o.flush()?;
        eprintln!("recording complete");
    }
    Ok(())
}

fn replay(path: &str) -> Result<()> {
    let rec = Recording::parse(&std::fs::read_to_string(path)?)?;
    let s = rec.replay();
    println!("{} batches, {} events", rec.batches.len(), rec.event_count());
    if !s.unmodeled.is_empty() {
        println!("unmodeled: {:?}", s.unmodeled);
    }
    let Some(f) = &rec.final_register else {
        println!("no final register; nothing to compare");
        return Ok(());
    };
    let d = diff_unreads(&s, &State::from_register(f));
    if d.is_empty() {
        println!("unreads match the final register ({} unread)", s.unread.len());
        Ok(())
    } else {
        println!("{d:#?}");
        anyhow::bail!("unread mismatch")
    }
}

mod bench;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()?;
    match args.first().map(String::as_str) {
        Some("unreads") if args.get(1).map(String::as_str) == Some("--offline") => {
            let t = std::time::Instant::now();
            let s = only_store()?.load_state()?.context("store is empty")?;
            eprintln!("loaded from store in {:.1?}", t.elapsed());
            print_tree(&s);
            Ok(())
        }
        Some("sync") => rt.block_on(sync(args.get(1).map(|n| n.parse()).transpose()?.unwrap_or(5000))),
        Some("search") => {
            let store = only_store()?;
            let t = std::time::Instant::now();
            let ids = store.search(&args[1..].join(" "), 20)?;
            println!("{} hits in {:.1?}: {ids:?}", ids.len(), t.elapsed());
            Ok(())
        }
        Some("unreads") => rt.block_on(async {
            let (r, _) = queue::register(&client()?).await?;
            print_tree(&State::from_register(&r));
            Ok(())
        }),
        Some("watch") => rt.block_on(watch(None)),
        Some("record") => {
            let out = PathBuf::from(args.get(1).context("record OUT MINUTES")?);
            let mins: f64 = args.get(2).context("record OUT MINUTES")?.parse()?;
            rt.block_on(watch(Some((out, Duration::from_secs_f64(mins * 60.0)))))
        }
        Some("replay") => replay(args.get(1).context("replay FILE")?),
        Some("bench") => {
            let path = |i: usize| args.get(i).filter(|a| !a.is_empty()).map(PathBuf::from);
            bench::run(path(1), path(2))
        },
        _ => {
            eprintln!("usage: puzil unreads [--offline] | watch | record OUT MINUTES | replay FILE | sync [N] | search QUERY | bench [CORPUS] [REPLAY]");
            std::process::exit(2)
        }
    }
}

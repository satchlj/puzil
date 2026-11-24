//! `puzil bench`: the offline pieces under the app, timed. Reads the store in `PUZIL_DATA_DIR`
//! (point it at a copy: opening a store migrates and checkpoints it), message HTML from a
//! JSONL corpus (objects with `content`, or plain strings; default: the store's own
//! messages), and a recording for replay. Builds no network client.
//!
//! Each figure is the fastest of repeated runs (at least 7, for at least 300 ms): the least
//! disturbed by whatever else the machine is doing. Milliseconds unless named `_us`.

use std::{path::PathBuf, time::Instant};

use anyhow::{Context, Result};
use serde_json::{Value, json};

/// Words searched for: common ones, so every store has hits, and single letters (search runs
/// as you type).
const QUERIES: &[&str] = &["a", "t", "the", "think", "model", "agent", "proof", "thanks", "meeting", "probably", "a b", "zzzq"];
const MIN_REPS: usize = 7;
const MIN_TOTAL: std::time::Duration = std::time::Duration::from_millis(300);

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v.get(v.len() / 2).copied().unwrap_or(0.)
}

/// Runs `f` repeatedly; the fastest wall time in ms.
fn time<T>(mut f: impl FnMut() -> T) -> f64 {
    let start = Instant::now();
    let mut best = f64::INFINITY;
    let mut n = 0;
    while n < MIN_REPS || start.elapsed() < MIN_TOTAL {
        let t = Instant::now();
        std::hint::black_box(f());
        best = best.min(t.elapsed().as_secs_f64() * 1000.);
        n += 1;
    }
    best
}

fn corpus(path: Option<&PathBuf>, store: &puzil_store::Store) -> Result<Vec<String>> {
    if let Some(p) = path {
        let text = std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?;
        return Ok(text
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter_map(|v| v.as_str().map(String::from).or_else(|| v["content"].as_str().map(String::from)))
            .collect());
    }
    let mut out = Vec::new();
    for (conv, _) in store.busiest_convs(10_000)? {
        out.extend(store.messages(&conv, None, 10_000)?.into_iter().map(|m| m.content));
    }
    Ok(out)
}

pub fn run(corpus_path: Option<PathBuf>, replay_path: Option<PathBuf>) -> Result<()> {
    let store = super::only_store()?;
    let mut out = json!({ "version": env!("CARGO_PKG_VERSION") });

    // Store: the cached model the app shows at launch, and opening a conversation.
    out["load_state_ms"] = json!(time(|| store.load_state().ok().flatten()));
    let busiest = store.busiest_convs(1)?;
    if let Some((conv, n)) = busiest.first() {
        out["open_conv_query_ms"] = json!(time(|| store.messages(conv, None, 200).map(|v| v.len()).unwrap_or(0)));
        out["open_conv_messages"] = json!(n);
    }
    out["messages_cached"] = json!(store.message_count()?);

    // Search: each query's median, then the slowest.
    let per: Vec<f64> = QUERIES
        .iter()
        .map(|q| time(|| store.search_hits(&puzil_store::fts_words(q), None, None, 50).map(|h| h.len()).unwrap_or(0)) * 1000.)
        .collect();
    out["search_us"] = QUERIES.iter().zip(&per).map(|(q, us)| (q.to_string(), json!(us))).collect::<serde_json::Map<_, _>>().into();
    out["search_us_median"] = json!(median(per.clone()));
    out["search_us_max"] = json!(per.iter().copied().fold(0., f64::max));

    // Rendering message HTML into the AST and the text index the app lays out.
    let html = corpus(corpus_path.as_ref(), &store)?;
    let bytes: usize = html.iter().map(String::len).sum();
    let ast_ms = time(|| html.iter().map(|h| puzil_render::ast::MessageAst::from_html(h).blocks.len()).sum::<usize>());
    let index_ms = time(|| html.iter().map(|h| puzil_render::text_index::TextIndex::from_html(h).text.len()).sum::<usize>());
    out["render_messages"] = json!(html.len());
    out["render_ast_us_per_msg"] = json!(ast_ms * 1000. / html.len().max(1) as f64);
    out["render_index_us_per_msg"] = json!(index_ms * 1000. / html.len().max(1) as f64);
    out["render_mb_per_s"] = json!(bytes as f64 / 1e6 / (ast_ms / 1000.).max(1e-9));

    // Math: every distinct formula, typeset once by a fresh typesetter (its cold path).
    let mut sources = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for h in &html {
        for (tex, mathml, display) in puzil_render::ast::MessageAst::from_html(h).math() {
            let s = puzil_math::MathSource { tex: tex.into(), mathml: mathml.map(Into::into), display };
            if seen.insert(s.key()) {
                sources.push(s);
            }
        }
    }
    if !sources.is_empty() {
        let t = Instant::now();
        let mut ts = puzil_math::Typesetter::new();
        let ok = sources.iter().filter(|s| ts.render(s).is_ok()).count();
        let ms = t.elapsed().as_secs_f64() * 1000.;
        out["math_formulas"] = json!(sources.len());
        out["math_failed"] = json!(sources.len() - ok);
        out["math_us_per_formula"] = json!(ms * 1000. / sources.len() as f64);
    }

    // Replaying recorded events onto the model.
    if let Some(p) = replay_path {
        let rec = puzil_zulip::replay::Recording::parse(&std::fs::read_to_string(&p)?)?;
        let n = rec.event_count();
        let ms = time(|| rec.replay().messages.len());
        out["replay_events"] = json!(n);
        out["replay_us_per_event"] = json!(ms * 1000. / n.max(1) as f64);
    }

    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

use puzil_render::text_index::{digest, locate, TextIndex};

#[test]
fn digest_matches_overlay_js() {
    let vectors: Vec<(Vec<u16>, String)> =
        serde_json::from_str(include_str!("digest_vectors.json")).unwrap();
    for (units, hash) in vectors {
        assert_eq!(digest(&units), hash, "units {units:?}");
    }
}

#[test]
fn locate_finds_moved_passage() {
    let before: Vec<u16> = "the quick brown fox".encode_utf16().collect();
    let after: Vec<u16> = "oh, the quick brown fox".encode_utf16().collect();
    let hash = digest(&before[4..9]); // "quick"
    assert_eq!(locate(&before, 4, 9, &hash), Some(4));
    assert_eq!(locate(&after, 4, 9, &hash), Some(8));
    assert_eq!(locate(&after, 4, 9, "0000000000000000"), None);
}

#[test]
fn katex_is_one_atomic_piece() {
    let html = r#"<p>a <span class="katex"><span class="katex-mathml"><math><semantics><mrow><mi>x</mi></mrow><annotation encoding="application/x-tex">x^2</annotation></semantics></math></span><span class="katex-html">x2</span></span> b</p>"#;
    let ix = TextIndex::from_html(html);
    assert_eq!(ix.to_string_lossy(), "a x^2 b");
    assert!(ix.pieces[1].atomic);
}

/// Parity with Chrome's DOM over real messages. Needs the private corpus and Chrome dump
/// (tools/fetch_corpus.py, tools/textnodes_dump.py); skipped when absent.
#[test]
fn matches_chrome_on_corpus() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../reference");
    let (Ok(corpus), Ok(chrome)) = (
        std::fs::read_to_string(format!("{root}/corpus.jsonl")),
        std::fs::read_to_string(format!("{root}/chrome_textnodes.json")),
    ) else {
        eprintln!("corpus not present; skipping");
        return;
    };
    let chrome: std::collections::HashMap<String, String> = serde_json::from_str(&chrome).unwrap();
    let (mut total, mut bad) = (0, Vec::new());
    for line in corpus.lines() {
        let m: serde_json::Value = serde_json::from_str(line).unwrap();
        let id = m["id"].to_string();
        let ours = TextIndex::from_html(m["content"].as_str().unwrap());
        // Compare as UTF-16 so lone surrogates and all units count.
        let theirs: Vec<u16> = chrome[&id].encode_utf16().collect();
        total += 1;
        if ours.text != theirs {
            bad.push(id);
        }
    }
    eprintln!("{} / {total} messages match Chrome", total - bad.len());
    assert!(bad.is_empty(), "mismatched ids: {:?}", &bad[..bad.len().min(20)]);
}

/// Report (never fail) how Zulip web's live DOM differs from server HTML, using a dump
/// from tools/live_textnodes.js. Run with --nocapture to read the diffs.
#[test]
fn report_live_dom_divergence() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../reference");
    let (Ok(corpus), Ok(live)) = (
        std::fs::read_to_string(format!("{root}/corpus.jsonl")),
        std::fs::read_to_string(format!("{root}/live_textnodes.json")),
    ) else {
        return;
    };
    let live: std::collections::HashMap<String, String> = serde_json::from_str(&live).unwrap();
    let (mut same, mut diff) = (0, 0);
    for line in corpus.lines() {
        let m: serde_json::Value = serde_json::from_str(line).unwrap();
        let Some(theirs) = live.get(&m["id"].to_string()) else { continue };
        let ours = TextIndex::from_html(m["content"].as_str().unwrap()).to_string_lossy();
        if &ours == theirs {
            same += 1;
            continue;
        }
        diff += 1;
        let p = ours.chars().zip(theirs.chars()).take_while(|(a, b)| a == b).count();
        let ctx = |s: &str| s.chars().skip(p.saturating_sub(20)).take(60).collect::<String>();
        eprintln!("#{}: server {:?}\n{:>10}live   {:?}", m["id"], ctx(&ours), "", ctx(theirs));
    }
    eprintln!("live DOM: {same} identical, {diff} differ");
}

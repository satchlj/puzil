//! Parses a JSONL file of message HTML strings and reports timing and empty results.
fn main() {
    let path = std::env::args().nth(1).expect("FILE");
    let lines: Vec<String> = std::fs::read_to_string(path).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let t = std::time::Instant::now();
    let asts: Vec<_> = lines.iter().map(|h| puzil_render::MessageAst::from_html(h)).collect();
    let el = t.elapsed();
    let empty = asts.iter().zip(&lines).filter(|(a, h)| a.blocks.is_empty() && !h.is_empty()).count();
    println!("{} messages parsed in {:.1?} ({:.0} µs each); {empty} non-empty HTML gave no blocks", lines.len(), el, el.as_secs_f64() * 1e6 / lines.len() as f64);
    for (a, h) in asts.iter().zip(&lines).filter(|(a, _)| a.plain_text().trim().is_empty()).take(3) {
        println!("empty text: {} → {:?}", &h[..h.len().min(200)], a.blocks);
    }
}

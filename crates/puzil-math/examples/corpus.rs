//! Typesets every formula in a JSONL file of message HTML strings and reports which path
//! each took. `cargo run --release -p puzil-math --example corpus -- FILE`
use puzil_math::{MathSource, Typesetter, Via};
use puzil_render::ast::MessageAst;

fn main() {
    let path = std::env::args().nth(1).expect("FILE");
    let mut all = Vec::new();
    for l in std::fs::read_to_string(path).unwrap().lines() {
        let h: String = serde_json::from_str(l).unwrap();
        let ast = MessageAst::from_html(&h);
        all.extend(ast.math().into_iter().map(|(tex, mathml, display)| MathSource { tex: tex.into(), mathml: mathml.map(Into::into), display }));
    }
    let mut seen = std::collections::HashSet::new();
    all.retain(|s| seen.insert(s.key()));
    let mut ts = Typesetter::new();
    let (mut a, mut b, mut fail) = (0, 0, 0);
    let t = std::time::Instant::now();
    for s in &all {
        match ts.render(s) {
            Ok(r) if r.via == Via::MathMl => a += 1,
            Ok(_) => {
                b += 1;
                if b <= 3 {
                    eprintln!("mitex fallback: {}", s.tex);
                }
            }
            Err(e) => {
                fail += 1;
                eprintln!("FAIL {e}: {}", s.tex);
            }
        }
    }
    println!("{} unique formulas in {:.1?}: {a} via MathML, {b} via mitex, {fail} failed", all.len(), t.elapsed());
}

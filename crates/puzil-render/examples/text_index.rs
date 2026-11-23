//! Prints a message's Overlay text index, or the passage between two UTF-16 offsets:
//! `cargo run -p puzil-render --example text_index -- FILE.html [START END [HASH]]` (with a
//! hash: where overlay.js `locate` finds the passage)
fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let t = puzil_render::text_index::TextIndex::from_html(&std::fs::read_to_string(&a[0]).unwrap());
    match (a.get(1).and_then(|s| s.parse::<usize>().ok()), a.get(2).and_then(|s| s.parse::<usize>().ok())) {
        (Some(s), Some(e)) if a.len() > 3 => match puzil_render::text_index::locate(&t.text, s, e, &a[3]) {
            Some(at) => println!("located at {at}: {:?}", String::from_utf16_lossy(&t.text[at..at + (e - s)])),
            None => println!("not found"),
        },
        (Some(s), Some(e)) => println!("{:?} (digest {})", String::from_utf16_lossy(&t.text[s..e]), puzil_render::text_index::digest(&t.text[s..e])),
        _ => println!("{}", t.to_string_lossy()),
    }
}

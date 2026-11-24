//! Typeset math for Puzil (spike 3's winner).
//!
//! KaTeX's presentation MathML (shipped by Zulip with every macro expanded) is converted to
//! Typst math; if that fails, the TeX goes through mitex; if that fails too, the caller
//! shows the TeX as code. Output is monochrome SVG plus size and baseline in em, so the UI
//! can reserve the space and tint the glyphs at paint time.
//!
//! [`Engine`] compiles on one worker thread and caches by `blake3(source, display)`.

pub mod mathml;
mod world;

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, mpsc},
};

use typst::{
    layout::{Frame, FrameItem},
    syntax::Source,
};
use typst_layout::PagedDocument;
use world::{Base, MathWorld, file_id};

/// Text size formulas are compiled at; sizes are reported relative to it.
const SIZE_PT: f64 = 10.;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MathSource {
    pub tex: String,
    /// KaTeX's `<math>` element, when the server sent one.
    pub mathml: Option<String>,
    pub display: bool,
}

impl MathSource {
    pub fn key(&self) -> Key {
        let mut h = blake3::Hasher::new();
        h.update(&[self.display as u8]);
        h.update(self.mathml.as_deref().unwrap_or(&self.tex).as_bytes());
        Key(*h.finalize().as_bytes())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key(pub [u8; 32]);

impl Key {
    /// A short stable name, for sprite-atlas cache keys.
    pub fn hex(&self) -> String {
        self.0[..12].iter().map(|b| format!("{b:02x}")).collect()
    }
}

#[derive(Debug)]
pub struct Rendered {
    /// Monochrome SVG; glyphs are black paths meant to be used as an alpha mask.
    pub svg: Arc<[u8]>,
    /// Size in em of the surrounding text.
    pub width: f32,
    pub height: f32,
    /// Distance from the top to the baseline, in em.
    pub ascent: f32,
    /// Which path produced it.
    pub via: Via,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    MathMl,
    Mitex,
}

const PRELUDE: &str = r#"#import "/specs/mod.typ": mitex-scope
#set page(width: auto, height: auto, margin: 0pt, fill: none)
#set text(size: 10pt, top-edge: "bounds", bottom-edge: "bounds")
"#;

fn typst_str(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn wrap(body: String, display: bool) -> String {
    let body = if display { format!("display({body})") } else { body };
    format!("{PRELUDE}#box(${body}$)\n")
}

fn first_group(frame: &Frame) -> Option<(f64, &Frame)> {
    frame.items().find_map(|(p, item)| match item {
        FrameItem::Group(g) => Some((p.y.to_pt(), &g.frame)),
        _ => None,
    })
}

/// A typesetter. Not thread-safe; [`Engine`] owns one on its worker thread.
pub struct Typesetter {
    base: Base,
    compiles: usize,
}

impl Default for Typesetter {
    fn default() -> Self {
        Self::new()
    }
}

impl Typesetter {
    pub fn new() -> Self {
        Self { base: Base::new(), compiles: 0 }
    }

    fn compile(&mut self, src: String, via: Via) -> Result<Rendered, String> {
        self.compiles += 1;
        // comemo's memo cache grows without bound otherwise (spike 3: 324 MB after 10k).
        if self.compiles % 200 == 0 {
            typst::comemo::evict(4);
        }
        let world = MathWorld { base: &self.base, main: Source::new(file_id("/main.typ"), src) };
        let doc = typst::compile::<PagedDocument>(&world)
            .output
            .map_err(|errs| errs.iter().map(|e| e.message.to_string()).collect::<Vec<_>>().join("; "))?;
        let page = doc.pages().first().ok_or("no page")?;
        let size = page.frame.size();
        let baseline = match first_group(&page.frame) {
            Some((y, f)) => y + f.baseline().to_pt(),
            None => page.frame.baseline().to_pt(),
        };
        let svg = typst_svg::svg(page, &Default::default());
        Ok(Rendered {
            svg: Arc::from(svg.into_bytes().into_boxed_slice()),
            width: (size.x.to_pt() / SIZE_PT) as f32,
            height: (size.y.to_pt() / SIZE_PT) as f32,
            ascent: (baseline / SIZE_PT) as f32,
            via,
        })
    }

    /// MathML path first, then mitex.
    pub fn render(&mut self, s: &MathSource) -> Result<Rendered, String> {
        let mut err = String::new();
        if let Some(m) = &s.mathml {
            match mathml::convert(&m.replace("&nbsp;", "&#160;")) {
                Ok(body) => match self.compile(wrap(body, s.display), Via::MathMl) {
                    Ok(r) => return Ok(r),
                    Err(e) => err = e,
                },
                Err(e) => err = e,
            }
        }
        let converted = mitex::convert_math(&s.tex, None).map_err(|e| format!("{err}; mitex: {e}"))?;
        let inner = format!("#eval({}, mode: \"math\", scope: mitex-scope)", typst_str(&converted));
        self.compile(wrap(inner, s.display), Via::Mitex).map_err(|e| format!("{err}; mitex: {e}"))
    }
}

pub type Outcome = Result<Arc<Rendered>, String>;

/// How many finished formulas the engine keeps once idle (a few hundred bytes to ~20 KB each).
const DONE_CAP: usize = 2000;

/// How long the worker waits for more formulas before it frees the typesetter.
const IDLE: std::time::Duration = std::time::Duration::from_secs(3);

struct Shared {
    done: HashMap<Key, Outcome>,
    /// `done`'s keys, oldest first.
    order: std::collections::VecDeque<Key>,
    pending: HashSet<Key>,
}

/// Background typesetting with a cache. Results arrive through `on_done`, called on the
/// worker thread with the keys that just finished.
pub struct Engine {
    shared: Arc<Mutex<Shared>>,
    tx: mpsc::Sender<(Key, MathSource)>,
}

impl Engine {
    pub fn new(on_done: impl Fn(Key) + Send + 'static) -> Self {
        let shared = Arc::new(Mutex::new(Shared { done: HashMap::new(), order: Default::default(), pending: HashSet::new() }));
        let (tx, rx) = mpsc::channel::<(Key, MathSource)>();
        let sh = shared.clone();
        std::thread::Builder::new()
            .name("puzil-math".into())
            .spawn(move || {
                // Loading fonts and the typst library takes tens of ms: do it on the first
                // job, not at app start.
                let mut ts: Option<Typesetter> = None;
                loop {
                    let (key, src) = match rx.recv_timeout(IDLE) {
                        Ok(job) => job,
                        // Idle: drop the typesetter and typst's memo caches (fonts, the
                        // library, layouts: ~6 MB at rest after a few hundred formulas). The
                        // next formula loads them again, here, in tens of ms.
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            if ts.take().is_some() {
                                typst::comemo::evict(0);
                            }
                            // Finished formulas are kept for revisits, up to DONE_CAP: the
                            // oldest go first (views showing them hold their own copies; others
                            // are typeset again if asked for).
                            let mut s = sh.lock().unwrap();
                            while s.done.len() > DONE_CAP
                                && let Some(k) = s.order.pop_front()
                            {
                                s.done.remove(&k);
                            }
                            drop(s);
                            match rx.recv() {
                                Ok(job) => job,
                                Err(_) => break,
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    let out = ts.get_or_insert_with(Typesetter::new).render(&src).map(Arc::new);
                    if let Err(e) = &out {
                        tracing::debug!("math failed ({e}): {}", src.tex);
                    }
                    {
                        let mut s = sh.lock().unwrap();
                        s.pending.remove(&key);
                        if s.done.insert(key, out).is_none() {
                            s.order.push_back(key);
                        }
                    }
                    on_done(key);
                }
            })
            .expect("math thread");
        Self { shared, tx }
    }

    /// The finished result for `s`, or `None` (and a queued job) if it isn't ready.
    pub fn get_or_request(&self, s: &MathSource) -> Option<Outcome> {
        let key = s.key();
        let mut sh = self.shared.lock().unwrap();
        if let Some(o) = sh.done.get(&key) {
            return Some(o.clone());
        }
        if sh.pending.insert(key) {
            let _ = self.tx.send((key, s.clone()));
        }
        None
    }

    /// Queues `s` without waiting (prefetch).
    pub fn request(&self, s: &MathSource) {
        let _ = self.get_or_request(s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const X2: &str = r#"<math xmlns="http://www.w3.org/1998/Math/MathML"><semantics><mrow><msup><mi>x</mi><mn>2</mn></msup></mrow><annotation encoding="application/x-tex">x^2</annotation></semantics></math>"#;

    #[test]
    fn mathml_path_with_baseline() {
        let mut t = Typesetter::new();
        let r = t.render(&MathSource { tex: "x^2".into(), mathml: Some(X2.into()), display: false }).unwrap();
        assert_eq!(r.via, Via::MathMl);
        assert!(r.width > 0.5 && r.width < 3.0, "{r:?}");
        assert!(r.ascent > 0.3 && r.ascent < r.height, "{r:?}");
        assert!(std::str::from_utf8(&r.svg).unwrap().starts_with("<svg"));
    }

    #[test]
    fn falls_back_to_mitex_then_errors() {
        let mut t = Typesetter::new();
        let r = t.render(&MathSource { tex: r"\frac{a}{b}".into(), mathml: Some("<not xml".into()), display: true }).unwrap();
        assert_eq!(r.via, Via::Mitex);
        assert!(r.height > 1.0, "a display fraction is taller than a line: {r:?}");
        assert!(t.render(&MathSource { tex: r"\frac{".into(), mathml: None, display: false }).is_err());
    }

    #[test]
    fn engine_caches_and_notifies() {
        let (tx, rx) = mpsc::channel();
        let e = Engine::new(move |k| tx.send(k).unwrap());
        let s = MathSource { tex: "y".into(), mathml: None, display: false };
        assert!(e.get_or_request(&s).is_none());
        assert!(e.get_or_request(&s).is_none(), "second request is deduplicated");
        let k = rx.recv_timeout(std::time::Duration::from_secs(30)).unwrap();
        assert_eq!(k, s.key());
        assert!(e.get_or_request(&s).unwrap().is_ok());
        assert!(rx.try_recv().is_err(), "only one job ran");
    }
}

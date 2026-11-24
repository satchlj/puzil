//! A rich paragraph: one GPUI text layout plus inline atoms painted into reserved slots
//! (from spike 1).
//!
//! Atoms (typeset math, pill padding, custom emoji, inline-reaction badges) are runs of
//! `PuzilAtoms` spacer glyphs whose advances add up to an exact width; GPUI lays them out
//! and wraps them like letters, and we paint the real content at the laid-out position.
//!
//! Each paragraph also records its *index text*: what Iliad Overlay's text index has for
//! the same content (emoji as `:name:`, math as its TeX, nothing for pill padding), with a
//! map between display bytes and index-text bytes. That is how inline passage reactions
//! are placed and created.

use std::{cell::RefCell, ops::Range, rc::Rc, sync::Arc};

use gpui_kit::*;
use puzil_math::{Engine, Key};
use puzil_render::ast::{Inline, MentionKind, Style};

use crate::theme::Theme;

pub const ATOM_FAMILY: &str = "PuzilAtoms";
const ATOM_UPEM: f32 = 4096.;
pub const FONT_SIZE: f32 = 16.;
pub const LINE_HEIGHT: f32 = 22.;
/// Math is drawn larger than the text around it, as KaTeX does (`.katex { font-size: 1.21em }`).
const MATH_SCALE: f32 = 1.21;
pub const UI_FONT: &str = "Figtree";
/// Big headings (welcome, sign-in): wide and heavy, where Discord uses Ginto Nord.
pub const DISPLAY_FONT: &str = "Unbounded";
pub const MONO_FONT: &str = "JetBrains Mono";
const WJ: &str = "\u{2060}";

pub enum AtomKind {
    /// Padding (pill sides); paints nothing itself.
    Pad,
    /// A monochrome SVG tinted at paint time (typeset math). `key` is the atlas cache key.
    Svg { key: SharedString, data: Arc<[u8]>, tint: Hsla },
    /// A raster image (custom emoji).
    Image(Arc<RenderImage>),
    /// An inline-reaction badge after a passage; `id` is reported when it's clicked.
    Badge { label: SharedString, mine: bool, id: usize },
}

/// Looks up an image that is already in memory.
pub type ImageLookup<'a> = &'a dyn Fn(&str) -> Option<Arc<RenderImage>>;

/// Something painted into a reserved slot of the text.
pub struct Atom {
    pub range: Range<usize>,
    pub kind: AtomKind,
    pub width: f32,
    pub height: f32,
    /// Distance from the atom's top to its baseline.
    pub ascent: f32,
    /// What copy puts on the clipboard for this slot.
    pub copy: String,
}

/// A piece of the paragraph: `display` bytes of `text` and `index` bytes of `index_text`.
/// Identical strings map byte for byte; anything else (an emoji glyph shown for `:name:`,
/// a formula) maps only as a whole.
#[derive(Clone, Debug)]
pub struct Seg {
    pub display: Range<usize>,
    pub index: Range<usize>,
    pub identity: bool,
}

pub struct Paragraph {
    pub text: SharedString,
    pub runs: Vec<TextRun>,
    pub atoms: Vec<Atom>,
    pub pills: Vec<(Range<usize>, Hsla)>,
    pub links: Vec<(Range<usize>, Arc<str>)>,
    /// Inline-reaction passages.
    pub highlights: Vec<Range<usize>>,
    pub line_height: f32,
    /// Formulas still being typeset; shown as TeX until they are ready.
    pub pending: Vec<Key>,
    /// Images still loading (custom emoji); shown as `:name:` until they arrive.
    pub pending_images: Vec<String>,
    pub index_text: String,
    pub segs: Vec<Seg>,
}

/// Encodes a width in px as spacer code points (binary expansion in font units).
fn spacer(width_px: f32) -> String {
    let units = (width_px * ATOM_UPEM / FONT_SIZE).round().clamp(1., 65535.) as u32;
    (0..16).filter(|k| units & (1 << k) != 0).map(|k| char::from_u32(0xE000 + k).unwrap()).collect()
}

pub struct BuildCtx<'a> {
    pub theme: &'a Theme,
    pub me: u64,
    pub bold: bool,
    pub color: Option<Hsla>,
    pub math: Option<&'a Engine>,
    pub images: Option<ImageLookup<'a>>,
}

struct Builder {
    text: String,
    runs: Vec<TextRun>,
    atoms: Vec<Atom>,
    pills: Vec<(Range<usize>, Hsla)>,
    links: Vec<(Range<usize>, Arc<str>)>,
    pending: Vec<Key>,
    pending_images: Vec<String>,
    tallest: f32,
    index_text: String,
    segs: Vec<Seg>,
}

fn run(len: usize, font: Font, color: Hsla) -> TextRun {
    TextRun { len, font, color, background_color: None, underline: None, strikethrough: None }
}

impl Builder {
    fn new() -> Self {
        Self {
            text: String::new(),
            runs: vec![],
            atoms: vec![],
            pills: vec![],
            links: vec![],
            pending: vec![],
            pending_images: vec![],
            tallest: 0.,
            index_text: String::new(),
            segs: vec![],
        }
    }

    /// Text shown as is; the index has the same text.
    fn push(&mut self, s: &str, r: TextRun) {
        self.push_as(s, r, s);
    }

    /// Text shown as `s` whose index text is `index`.
    fn push_as(&mut self, s: &str, r: TextRun, index: &str) {
        if s.is_empty() && index.is_empty() {
            return;
        }
        let d0 = self.text.len();
        let i0 = self.index_text.len();
        if !s.is_empty() {
            self.text.push_str(s);
            self.runs.push(TextRun { len: s.len(), ..r });
        }
        self.index_text.push_str(index);
        self.segs.push(Seg { display: d0..self.text.len(), index: i0..self.index_text.len(), identity: s == index });
    }

    fn atom(&mut self, kind: AtomKind, width: f32, height: f32, ascent: f32, copy: String, index: &str, fg: Hsla) {
        let s = spacer(width);
        let start = self.text.len();
        self.push_as(&s, run(0, font(ATOM_FAMILY), fg), index);
        self.tallest = self.tallest.max(height);
        self.atoms.push(Atom { range: start..self.text.len(), kind, width, height, ascent, copy });
    }

    fn pad(&mut self, w: f32, fg: Hsla) {
        self.atom(AtomKind::Pad, w, 0., 0., String::new(), "", fg);
    }

    fn pill(&mut self, label: &str, index: &str, f: Font, fg: Hsla, bg: Hsla) {
        let start = self.text.len();
        self.pad(3., fg);
        self.push_as(WJ, run(0, font(ATOM_FAMILY), fg), "");
        self.push_as(label, run(0, f, fg), index);
        self.push_as(WJ, run(0, font(ATOM_FAMILY), fg), "");
        self.pad(3., fg);
        self.pills.push((start..self.text.len(), bg));
    }

    /// Typeset math if it's ready, else its TeX as code (and a note to rebuild later).
    fn math(&mut self, tex: &str, mathml: Option<&str>, display: bool, cx: &BuildCtx) {
        let fg = cx.color.unwrap_or(cx.theme.fg);
        let src = crate::math::source(tex, mathml, display);
        let key = src.key();
        match cx.math.map(|e| e.get_or_request(&src)) {
            Some(Some(Ok(r))) => {
                let em = FONT_SIZE * MATH_SCALE;
                self.atom(
                    AtomKind::Svg { key: format!("math://{}", key.hex()).into(), data: r.svg.clone(), tint: fg },
                    r.width * em,
                    r.height * em,
                    r.ascent * em,
                    format!("$${tex}$$"),
                    tex,
                    fg,
                );
            }
            outcome => {
                if matches!(outcome, Some(None)) {
                    self.pending.push(key);
                }
                let mut r = run(0, font(MONO_FONT), fg);
                r.background_color = Some(cx.theme.code_bg);
                self.push(tex, r);
            }
        }
    }

    fn finish(self, line_height: f32) -> Paragraph {
        // Tall inline atoms raise this paragraph's (uniform) line height.
        let line_height = line_height.max((self.tallest + 4.).round());
        Paragraph {
            text: self.text.into(),
            runs: self.runs,
            atoms: self.atoms,
            pills: self.pills,
            links: self.links,
            highlights: vec![],
            line_height,
            pending: self.pending,
            pending_images: self.pending_images,
            index_text: self.index_text,
            segs: self.segs,
        }
    }
}

fn styled_font(base: &str, s: &Style, bold: bool) -> Font {
    let mut f = font(if s.code { MONO_FONT } else { base });
    if s.bold || bold {
        f.weight = FontWeight::BOLD;
    }
    if s.italic {
        f.style = FontStyle::Italic;
    }
    f
}

pub fn build(inlines: &[Inline], cx: &BuildCtx) -> Paragraph {
    let t = cx.theme;
    let fg = cx.color.unwrap_or(t.fg);
    let mut b = Builder::new();
    for i in inlines {
        match i {
            Inline::Text { text, style } => {
                let start = b.text.len();
                let color = if style.link.is_some() { t.link } else { fg };
                let mut r = run(0, styled_font(UI_FONT, style, cx.bold), color);
                if style.code {
                    r.background_color = Some(t.code_bg);
                }
                if style.strike {
                    r.strikethrough = Some(StrikethroughStyle { thickness: px(1.), color: Some(color) });
                }
                b.push(text, r);
                if let Some(l) = &style.link {
                    b.links.push((start..b.text.len(), l.clone()));
                }
            }
            Inline::Break => b.push("\n", run(0, font(UI_FONT), fg)),
            Inline::Mention { text, kind, silent, user_id } => {
                let me = *kind == MentionKind::Wildcard || *user_id == Some(cx.me);
                let (pfg, pbg) = if me && !silent { (t.mention_me_fg, t.mention_me_bg) } else { (t.mention_fg, t.mention_bg) };
                let label = if text.starts_with('@') { text.clone() } else { format!("@{text}") };
                let mut f = font(UI_FONT);
                f.weight = FontWeight::MEDIUM;
                let start = b.text.len();
                b.pill(&label, text, f, pfg, pbg);
                // A person's mention opens their card (see `USER_LINK`).
                if let (Some(id), MentionKind::User) = (user_id, kind) {
                    b.links.push((start..b.text.len(), Arc::from(format!("{USER_LINK}{id}").as_str())));
                }
            }
            Inline::ChannelLink { text, href } => {
                let start = b.text.len();
                let mut f = font(UI_FONT);
                f.weight = FontWeight::MEDIUM;
                b.pill(text, text, f, t.mention_fg, t.mention_bg);
                b.links.push((start..b.text.len(), Arc::from(href.as_str())));
            }
            // Zulip's emoji span holds `:name:` as its (hidden) text.
            Inline::Emoji { glyph, name } => b.push_as(glyph, run(0, font(UI_FONT), fg), &format!(":{name}:")),
            Inline::CustomEmoji { name, src } => match cx.images.and_then(|f| f(src)) {
                Some(img) => {
                    let s = FONT_SIZE * 1.3;
                    b.atom(AtomKind::Image(img), s, s, s * 0.82, format!(":{name}:"), "", fg);
                }
                None => {
                    b.pending_images.push(src.clone());
                    b.push_as(&format!(":{name}:"), run(0, font(UI_FONT), t.muted), "");
                }
            },
            Inline::Image(m) => b.push_as(m.title.as_deref().unwrap_or("image"), run(0, font(UI_FONT), t.muted), ""),
            Inline::Math { tex, mathml } => b.math(tex, mathml.as_deref(), false, cx),
            Inline::Time { text, ts } => {
                let label = ts.map(crate::ui::fmt::datetime).unwrap_or_else(|| text.clone());
                let mut r = run(0, font(UI_FONT), fg);
                r.background_color = Some(t.code_bg);
                b.push_as(&label, r, text);
            }
        }
    }
    b.finish(LINE_HEIGHT)
}

/// Display math on a line of its own. The flag says whether it is typeset (else it is
/// the TeX, to be shown as code).
pub fn build_display_math(tex: &str, mathml: Option<&str>, cx: &BuildCtx) -> (Paragraph, bool) {
    let mut b = Builder::new();
    b.math(tex, mathml, true, cx);
    let typeset = !b.atoms.is_empty();
    (b.finish(if typeset { 0. } else { 20. }), typeset)
}

/// Monospace text with per-span colours (code blocks).
pub fn build_code(lines: &[Vec<(String, String)>], theme: &Theme) -> Paragraph {
    let mut b = Builder::new();
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            b.push("\n", run(0, font(MONO_FONT), theme.fg));
        }
        for (class, text) in line {
            b.push(text, run(0, font(MONO_FONT), theme.code_color(class)));
        }
    }
    if b.text.is_empty() {
        b.push_as(" ", run(0, font(MONO_FONT), theme.fg), "");
    }
    b.finish(20.)
}

// ---- Display ↔ index ----

impl Paragraph {
    /// Index-text byte for a display byte. Inside a piece that maps only as a whole (an
    /// emoji, a formula), `end` picks its end rather than its start.
    pub fn display_to_index(&self, d: usize, end: bool) -> usize {
        for s in &self.segs {
            if d <= s.display.start {
                return s.index.start;
            }
            if d < s.display.end {
                return if s.identity { s.index.start + (d - s.display.start) } else if end { s.index.end } else { s.index.start };
            }
        }
        self.index_text.len()
    }

    /// Display byte for an index-text byte.
    pub fn index_to_display(&self, i: usize, end: bool) -> usize {
        for s in &self.segs {
            if i <= s.index.start {
                return s.display.start;
            }
            if i < s.index.end {
                return if s.identity { s.display.start + (i - s.index.start) } else if end { s.display.end } else { s.display.start };
            }
        }
        self.text.len()
    }

    /// A copy with passages highlighted and, after each passage that has any, a row of
    /// reaction badges: `(display range, [(label, mine)], id of the first badge)`.
    pub fn with_marks(&self, marks: &[(Range<usize>, Vec<(SharedString, bool)>, usize)], theme: &Theme) -> Paragraph {
        let mut inserts: Vec<(usize, Vec<(SharedString, bool, usize)>)> = marks
            .iter()
            .filter(|(_, b, _)| !b.is_empty())
            .map(|(r, b, first)| (r.end, b.iter().enumerate().map(|(k, (l, m))| (l.clone(), *m, first + k)).collect()))
            .collect();
        inserts.sort_by_key(|(p, _)| *p);
        let mut ins = inserts.into_iter().peekable();
        let text = self.text.to_string();
        let mut out = Builder::new();
        let mut added: Vec<(usize, usize)> = Vec::new(); // (original position, bytes inserted)
        let insert = |out: &mut Builder, at: usize, badges: Vec<(SharedString, bool, usize)>, added: &mut Vec<(usize, usize)>| {
            let before = out.text.len();
            out.pad(4., theme.fg);
            for (label, mine, id) in badges {
                let digits = label.chars().filter(char::is_ascii_digit).count() as f32;
                out.atom(AtomKind::Badge { label, mine, id }, 30. + 8. * digits, 18., 14., String::new(), "", theme.fg);
                out.pad(2., theme.fg);
            }
            added.push((at, out.text.len() - before));
        };
        let mut pos = 0;
        for r in &self.runs {
            let end = pos + r.len;
            while let Some((p, _)) = ins.peek()
                && *p < end
            {
                let (p, badges) = ins.next().unwrap();
                let p = p.max(pos);
                if p > pos {
                    out.text.push_str(&text[pos..p]);
                    out.runs.push(TextRun { len: p - pos, ..r.clone() });
                    pos = p;
                }
                insert(&mut out, p, badges, &mut added);
            }
            if end > pos {
                out.text.push_str(&text[pos..end]);
                out.runs.push(TextRun { len: end - pos, ..r.clone() });
            }
            pos = end;
        }
        for (p, badges) in ins {
            insert(&mut out, p.min(pos), badges, &mut added);
        }
        // Positions at an insertion point: a range starting there moves past the badges,
        // one ending there stays before them.
        let shift = |p: usize, after: bool| -> usize {
            p + added.iter().filter(|(q, _)| if after { *q <= p } else { *q < p }).map(|(_, n)| n).sum::<usize>()
        };
        let range = |r: &Range<usize>| shift(r.start, true)..shift(r.end, false).max(shift(r.start, true));
        for a in &self.atoms {
            let kind = match &a.kind {
                AtomKind::Pad => AtomKind::Pad,
                AtomKind::Svg { key, data, tint } => AtomKind::Svg { key: key.clone(), data: data.clone(), tint: *tint },
                AtomKind::Image(i) => AtomKind::Image(i.clone()),
                AtomKind::Badge { label, mine, id } => AtomKind::Badge { label: label.clone(), mine: *mine, id: *id },
            };
            out.atoms.push(Atom { range: range(&a.range), kind, width: a.width, height: a.height, ascent: a.ascent, copy: a.copy.clone() });
        }
        out.atoms.sort_by_key(|a| a.range.start);
        out.pills = self.pills.iter().map(|(r, c)| (range(r), *c)).collect();
        out.links = self.links.iter().map(|(r, l)| (range(r), l.clone())).collect();
        out.pending = self.pending.clone();
        out.pending_images = self.pending_images.clone();
        out.index_text = self.index_text.clone();
        // Split pieces at insertion points first, so text after a badge keeps its mapping.
        let cuts: Vec<usize> = added.iter().map(|(p, _)| *p).collect();
        let mut pieces: Vec<Seg> = Vec::with_capacity(self.segs.len() + cuts.len());
        for seg in &self.segs {
            let mut cur = seg.clone();
            for &p in &cuts {
                if cur.identity && p > cur.display.start && p < cur.display.end {
                    let k = p - cur.display.start;
                    pieces.push(Seg { display: cur.display.start..p, index: cur.index.start..cur.index.start + k, identity: true });
                    cur = Seg { display: p..cur.display.end, index: cur.index.start + k..cur.index.end, identity: true };
                }
            }
            pieces.push(cur);
        }
        out.segs = pieces.iter().map(|s| Seg { display: range(&s.display), index: s.index.clone(), identity: s.identity }).collect();
        out.tallest = 0.;
        let mut p = out.finish(self.line_height);
        p.highlights = marks.iter().map(|(r, _, _)| range(r)).collect();
        p
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: that brings in gpui's own `test` attribute.
    use super::{AtomKind, BuildCtx, Inline, Paragraph, Style, Theme, build};

    fn para(inl: Vec<Inline>) -> Paragraph {
        let t = Theme::light();
        build(&inl, &BuildCtx { theme: &t, me: 1, bold: false, color: None, math: None, images: None })
    }

    #[test]
    fn index_text_and_mapping() {
        let p = para(vec![
            Inline::Text { text: "hi ".into(), style: Style::default() },
            Inline::Emoji { glyph: "🎉".into(), name: "tada".into() },
            Inline::Text { text: " ok".into(), style: Style::default() },
        ]);
        assert_eq!(p.index_text, "hi :tada: ok");
        assert_eq!(p.display_to_index(1, false), 1);
        // The emoji is atomic: its start maps to the start of `:tada:`, its end to the end.
        let e = p.text.find('🎉').unwrap();
        assert_eq!(p.display_to_index(e, false), 3);
        assert_eq!(p.display_to_index(e + '🎉'.len_utf8(), true), 9);
        assert_eq!(p.index_to_display(10, false), e + '🎉'.len_utf8() + 1);
    }

    #[test]
    fn marks_insert_badges_and_shift_ranges() {
        let p = para(vec![Inline::Text { text: "hello world".into(), style: Style::default() }]);
        let m = p.with_marks(&[(0..5, vec![("👍 2".into(), true)], 0)], &Theme::light());
        assert!(m.text.starts_with("hello"));
        assert!(m.text.ends_with(" world"));
        assert_eq!(m.highlights, vec![0..5]);
        assert!(m.atoms.iter().any(|a| matches!(a.kind, AtomKind::Badge { mine: true, id: 0, .. })));
        // Text after the badge keeps its index mapping.
        let w = m.text.find(" world").unwrap();
        assert_eq!(m.display_to_index(w + 1, false), 6);
    }
}

// ---- Selection ----

/// Position inside the whole list: (message id, paragraph index, byte offset).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Pos {
    pub msg: u64,
    pub para: usize,
    pub byte: usize,
}

#[derive(Default)]
pub struct Selection {
    pub anchor: Option<Pos>,
    pub head: Option<Pos>,
    pub dragging: bool,
}

impl Selection {
    pub fn ordered(&self) -> Option<(Pos, Pos)> {
        let (a, h) = (self.anchor?, self.head?);
        Some(if a <= h { (a, h) } else { (h, a) })
    }

    pub fn is_empty(&self) -> bool {
        self.ordered().is_none_or(|(a, b)| a == b)
    }

    /// The selected byte range within one paragraph, if any.
    pub fn range_in(&self, msg: u64, para: usize, len: usize) -> Option<Range<usize>> {
        let (s, e) = self.ordered()?;
        let here_start = Pos { msg, para, byte: 0 };
        let here_end = Pos { msg, para, byte: len };
        if e < here_start || s > here_end {
            return None;
        }
        let a = if s < here_start { 0 } else { s.byte };
        let b = if e > here_end { len } else { e.byte };
        (a < b).then_some(a..b)
    }
}

/// Link target of a user mention: this prefix, then the user id. Not a real URL; the link
/// handler opens the person's card for it.
pub const USER_LINK: &str = "puzil-user:";

pub type LinkHandler = Rc<dyn Fn(&str, &mut Window, &mut App)>;

pub struct RichText {
    para: Rc<Paragraph>,
    styled: StyledText,
    msg: u64,
    para_ix: usize,
    selection: Rc<RefCell<Selection>>,
    theme: Rc<Theme>,
    on_link: Option<LinkHandler>,
    on_badge: Option<BadgeHandler>,
}

/// Called with a badge's id when an inline-reaction badge is clicked.
pub type BadgeHandler = Rc<dyn Fn(usize, &mut Window, &mut App)>;

impl RichText {
    pub fn new(
        para: Rc<Paragraph>,
        msg: u64,
        para_ix: usize,
        selection: Rc<RefCell<Selection>>,
        theme: Rc<Theme>,
        on_link: Option<LinkHandler>,
    ) -> Self {
        let styled = StyledText::new(para.text.clone()).with_runs(para.runs.clone());
        Self { para, styled, msg, para_ix, selection, theme, on_link, on_badge: None }
    }

    pub fn on_badge(mut self, h: Option<BadgeHandler>) -> Self {
        self.on_badge = h;
        self
    }
}

impl IntoElement for RichText {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

/// Position of `ix` with downstream affinity: at a soft-wrap boundary GPUI reports the
/// end of the previous visual line, but content starting at `ix` is drawn at the start
/// of the next one.
fn pos_downstream(layout: &TextLayout, text: &str, bounds: Bounds<Pixels>, ix: usize) -> Option<Point<Pixels>> {
    let p = layout.position_for_index(ix)?;
    let next = text[ix..].chars().next().map(|c| ix + c.len_utf8())?;
    match layout.position_for_index(next) {
        Some(q) if q.y > p.y => Some(point(bounds.left(), q.y)),
        _ => Some(p),
    }
}

/// Rectangles covering `range` across wrapped visual lines.
fn range_rects(layout: &TextLayout, text: &str, bounds: Bounds<Pixels>, range: Range<usize>) -> Vec<Bounds<Pixels>> {
    let lh = layout.line_height();
    let (Some(p0), Some(p1)) = (pos_downstream(layout, text, bounds, range.start), layout.position_for_index(range.end)) else {
        return vec![];
    };
    if p0.y == p1.y {
        return vec![Bounds::from_corners(p0, point(p1.x, p1.y + lh))];
    }
    let mut out = vec![Bounds::from_corners(p0, point(bounds.right(), p0.y + lh))];
    let mut y = p0.y + lh;
    while y < p1.y {
        out.push(Bounds::from_corners(point(bounds.left(), y), point(bounds.right(), y + lh)));
        y += lh;
    }
    out.push(Bounds::from_corners(point(bounds.left(), p1.y), point(p1.x, p1.y + lh)));
    out
}

fn hit(layout: &TextLayout, p: Point<Pixels>) -> usize {
    match layout.index_for_position(p) {
        Ok(i) | Err(i) => i,
    }
}

fn link_at(para: &Paragraph, layout: &TextLayout, p: Point<Pixels>) -> Option<Arc<str>> {
    link_ix(para, layout, p).map(|i| para.links[i].1.clone())
}

/// Which of the paragraph's links is under `p`.
fn link_ix(para: &Paragraph, layout: &TextLayout, p: Point<Pixels>) -> Option<usize> {
    // Only an exact hit counts: `Err` means the point is past the end of a line.
    let i = layout.index_for_position(p).ok()?;
    para.links.iter().position(|(r, _)| r.contains(&i))
}

impl Element for RichText {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        self.styled.request_layout(id, inspector_id, window, cx)
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Hitbox {
        self.styled.prepaint(id, inspector_id, bounds, state, window, cx);
        window.insert_hitbox(bounds, HitboxBehavior::Normal)
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut (),
        hitbox: &mut Hitbox,
        window: &mut Window,
        cx: &mut App,
    ) {
        let layout = self.styled.layout().clone();
        let para = self.para.clone();

        for (r, bg) in &para.pills {
            for b in range_rects(&layout, &para.text, bounds, r.clone()) {
                let b = Bounds::from_corners(point(b.left(), b.top() + px(1.)), point(b.right(), b.bottom() - px(1.)));
                window.paint_quad(fill(b, *bg).corner_radii(px(4.)));
            }
        }
        for r in &para.highlights {
            for b in range_rects(&layout, &para.text, bounds, r.clone()) {
                window.paint_quad(fill(b, self.theme.passage).corner_radii(px(2.)));
            }
        }
        if let Some(sel) = self.selection.borrow().range_in(self.msg, self.para_ix, para.text.len()) {
            for b in range_rects(&layout, &para.text, bounds, sel) {
                window.paint_quad(fill(b, self.theme.selection));
            }
        }

        self.styled.paint(id, inspector_id, bounds, state, &mut (), window, cx);

        let lh = layout.line_height();
        let mut badges: Vec<(Bounds<Pixels>, usize)> = Vec::new();
        for atom in &para.atoms {
            if matches!(atom.kind, AtomKind::Pad) {
                continue;
            }
            let Some(pos) = pos_downstream(&layout, &para.text, bounds, atom.range.start) else { continue };
            let Some(line) = layout.line_layout_for_index(atom.range.start) else { continue };
            let l = &line.unwrapped_layout;
            let baseline_y = pos.y + (lh - l.ascent - l.descent) / 2. + l.ascent;
            let b = Bounds::new(point(pos.x, baseline_y - px(atom.ascent)), size(px(atom.width), px(atom.height)));
            match &atom.kind {
                AtomKind::Svg { key, data, tint } => {
                    window.paint_svg(b, key.clone(), Some(data), TransformationMatrix::unit(), *tint, cx).ok();
                }
                AtomKind::Image(img) => {
                    window.paint_image(b, b, Corners::default(), img.clone(), 0, false).ok();
                }
                AtomKind::Badge { label, mine, id } => {
                    let t = &self.theme;
                    let border = if *mine { t.reaction_mine_border } else { t.border };
                    let bg = if *mine { t.reaction_mine_bg } else { t.reaction_bg };
                    window.paint_quad(quad(b, px(atom.height / 2.), bg, px(1.), border, BorderStyle::Solid));
                    let run = TextRun { len: label.len(), font: font(UI_FONT), color: t.fg, background_color: None, underline: None, strikethrough: None };
                    let shaped = window.text_system().shape_line(label.clone(), px(12.), &[run], None);
                    let origin = point(b.left() + (b.size.width - shaped.width) / 2., b.top());
                    shaped.paint(origin, b.size.height, TextAlign::Left, None, window, cx).ok();
                    badges.push((b, *id));
                }
                AtomKind::Pad => {}
            }
        }

        let hovered = if hitbox.is_hovered(window) { link_ix(&para, &layout, window.mouse_position()) } else { None };
        let over_link = hovered.is_some();
        if hitbox.is_hovered(window) {
            window.set_cursor_style(if over_link { CursorStyle::PointingHand } else { CursorStyle::IBeam }, hitbox);
        }
        // The link under the pointer is underlined (not mentions: they're pills). Under its
        // glyphs on each line it covers, in the link colour.
        if let Some((r, target)) = hovered.and_then(|i| para.links.get(i))
            && !target.starts_with(USER_LINK)
        {
            for b in range_rects(&layout, &para.text, bounds, r.clone()) {
                let Some(line) = layout.line_layout_for_index(hit(&layout, b.center())) else { continue };
                let l = &line.unwrapped_layout;
                let baseline = b.top() + (lh - l.ascent - l.descent) / 2. + l.ascent;
                let y = baseline + (l.descent * 0.45).max(px(1.));
                window.paint_quad(fill(Bounds::new(point(b.left(), y), size(b.size.width, px(1.))), self.theme.link));
            }
        }
        // The cursor and underline are chosen at paint, and moving within the paragraph doesn't
        // repaint: ask for a frame when the pointer crosses onto, off or between links.
        if !para.links.is_empty() {
            let (layout, para, hitbox) = (layout.clone(), para.clone(), hitbox.clone());
            window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, _| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                let now = if hitbox.is_hovered(window) { link_ix(&para, &layout, e.position) } else { None };
                if now != hovered {
                    window.refresh();
                }
            });
        }

        // Selection anchors live in the shared model, so they survive this element unmounting.
        let (msg, para_ix) = (self.msg, self.para_ix);
        {
            let selection = self.selection.clone();
            let layout = layout.clone();
            let hitbox = hitbox.clone();
            window.on_mouse_event(move |e: &MouseDownEvent, phase, window, _| {
                if phase == DispatchPhase::Bubble && e.button == MouseButton::Left && hitbox.is_hovered(window) {
                    let p = Pos { msg, para: para_ix, byte: hit(&layout, e.position) };
                    let mut s = selection.borrow_mut();
                    if !e.modifiers.shift || s.anchor.is_none() {
                        s.anchor = Some(p);
                    }
                    s.head = Some(p);
                    s.dragging = true;
                    window.refresh();
                }
            });
        }
        {
            let selection = self.selection.clone();
            let layout = layout.clone();
            window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, _| {
                if phase != DispatchPhase::Bubble || !selection.borrow().dragging {
                    return;
                }
                // Accept moves anywhere in this paragraph's horizontal band, so dragging
                // through gutters and gaps still extends the selection.
                if e.position.y >= bounds.top() && e.position.y < bounds.bottom() {
                    let p = Pos { msg, para: para_ix, byte: hit(&layout, e.position) };
                    selection.borrow_mut().head = Some(p);
                    window.refresh();
                }
            });
        }
        {
            let selection = self.selection.clone();
            let hitbox = hitbox.clone();
            let on_link = self.on_link.clone();
            let on_badge = self.on_badge.clone();
            window.on_mouse_event(move |e: &MouseUpEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble || e.button != MouseButton::Left {
                    return;
                }
                let clicked = {
                    let mut s = selection.borrow_mut();
                    s.dragging = false;
                    hitbox.is_hovered(window) && s.is_empty()
                };
                if !clicked {
                    return;
                }
                if let (Some(h), Some((_, id))) = (&on_badge, badges.iter().find(|(b, _)| b.contains(&e.position))) {
                    h(*id, window, cx);
                } else if let (Some(h), Some(l)) = (&on_link, link_at(&para, &layout, e.position)) {
                    h(&l, window, cx);
                }
            });
        }
    }
}

/// Text copied for `range` of a paragraph, with atoms replaced by their copy text.
pub fn copy_text(para: &Paragraph, range: Range<usize>) -> String {
    let mut out = String::new();
    let mut i = range.start;
    for atom in &para.atoms {
        if atom.range.end <= i || atom.range.start >= range.end {
            continue;
        }
        out.push_str(&para.text[i..atom.range.start.max(i)]);
        out.push_str(&atom.copy);
        i = atom.range.end;
    }
    if i < range.end {
        out.push_str(&para.text[i..range.end]);
    }
    out.retain(|c| c != '\u{2060}');
    out
}

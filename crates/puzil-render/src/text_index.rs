//! The text Iliad Overlay anchors inline reactions to, computed from server HTML.
//!
//! Mirrors `textNodes()` in overlay.js: every text node in document order, except that a
//! `.katex` element contributes its TeX annotation as one atomic piece and `[data-zo]`
//! elements (Overlay's own additions) are skipped. Offsets are UTF-16 code units, as in
//! JavaScript. See docs/spikes/02-overlay-parity.md for where Zulip web's live DOM
//! differs from server HTML.

use scraper::{ElementRef, Html, Node, Selector};
use std::sync::LazyLock;

static TEX: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse(r#"annotation[encoding="application/x-tex"]"#).unwrap());

/// One contributed piece: a text node, or a whole formula.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Piece {
    pub start: usize,
    pub len: usize,
    pub atomic: bool,
}

#[derive(Debug, Clone, Default)]
pub struct TextIndex {
    /// Concatenated text as UTF-16 code units.
    pub text: Vec<u16>,
    pub pieces: Vec<Piece>,
}

impl TextIndex {
    pub fn from_html(html: &str) -> Self {
        let doc = Html::parse_fragment(html);
        let mut index = TextIndex::default();
        index.walk(doc.root_element());
        index
    }

    fn add(&mut self, s: &str, atomic: bool) {
        let start = self.text.len();
        self.text.extend(s.encode_utf16());
        self.pieces.push(Piece { start, len: self.text.len() - start, atomic });
    }

    fn walk(&mut self, el: ElementRef) {
        let e = el.value();
        if e.attr("data-zo").is_some() {
            return;
        }
        if e.has_class("katex", scraper::CaseSensitivity::CaseSensitive) {
            let tex = match el.select(&TEX).next() {
                Some(a) => a.text().collect::<String>(),
                None => el.text().collect::<String>(),
            };
            self.add(&tex, true);
            return;
        }
        for child in el.children() {
            match child.value() {
                Node::Text(t) => self.add(t, false),
                Node::Element(_) => self.walk(ElementRef::wrap(child).unwrap()),
                _ => {}
            }
        }
    }

    /// Lossy UTF-8 view, for debugging and tests.
    pub fn to_string_lossy(&self) -> String {
        String::from_utf16_lossy(&self.text)
    }
}

/// Code points of a UTF-16 slice the way JavaScript's `for (const c of s)` yields them:
/// surrogate pairs combine, lone surrogates come through as their raw unit value.
fn js_code_points(units: &[u16]) -> impl Iterator<Item = u32> + '_ {
    char::decode_utf16(units.iter().copied()).map(|r| match r {
        Ok(c) => c as u32,
        Err(e) => e.unpaired_surrogate() as u32,
    })
}

/// Overlay's passage digest: two 32-bit FNV-1a-style passes over code points.
pub fn digest(units: &[u16]) -> String {
    let (mut a, mut b) = (0x811c_9dc5u32, 0x0100_0193u32);
    for c in js_code_points(units) {
        a = (a ^ c).wrapping_mul(0x0100_0193);
        b = (b ^ c).wrapping_mul(0x811c_9dc5).wrapping_add(0x9e37);
    }
    format!("{a:08x}{b:08x}")
}

/// Where a stored passage is now: its recorded start if the digest still matches there,
/// else the first same-length window with that digest, else `None` (overlay.js `locate`).
pub fn locate(text: &[u16], start: usize, end: usize, hash: &str) -> Option<usize> {
    let len = end.checked_sub(start)?;
    if end <= text.len() && digest(&text[start..end]) == hash {
        return Some(start);
    }
    (0..=text.len().checked_sub(len)?).find(|&s| digest(&text[s..s + len]) == hash)
}

impl TextIndex {
    /// Finds `needle` in the index at or after unit `from`, treating any run of whitespace
    /// in the needle as matching any run (possibly empty) of whitespace in the index, and
    /// leading whitespace in the needle as matching nothing. Returns `(needle byte, index
    /// unit)` for every char boundary of the needle, ending with `needle.len()`.
    ///
    /// Used to map text as Puzil displays it (whitespace collapsed, per paragraph) onto
    /// the Overlay text index (every text node, verbatim).
    pub fn align(&self, needle: &str, from: usize) -> Option<Vec<(usize, usize)>> {
        let idx: Vec<(usize, char)> = {
            let mut v = Vec::new();
            let mut unit = 0;
            for r in char::decode_utf16(self.text.iter().copied()) {
                let c = r.unwrap_or('\u{FFFD}');
                v.push((unit, c));
                unit += c.len_utf16();
            }
            v
        };
        let total = self.text.len();
        let nd: Vec<(usize, char)> = needle.char_indices().collect();
        let first = nd.iter().position(|(_, c)| !c.is_whitespace())?;
        let starts = idx.iter().enumerate().filter(|(_, (u, c))| *u >= from && *c == nd[first].1).map(|(k, _)| k);
        'outer: for k0 in starts {
            let mut map = Vec::with_capacity(nd.len() + 1);
            for &(b, _) in &nd[..first] {
                map.push((b, idx[k0].0));
            }
            let (mut ni, mut ki) = (first, k0);
            while ni < nd.len() {
                let (nb, nc) = nd[ni];
                let unit_here = idx.get(ki).map(|x| x.0).unwrap_or(total);
                if nc.is_whitespace() {
                    while ni < nd.len() && nd[ni].1.is_whitespace() {
                        map.push((nd[ni].0, unit_here));
                        ni += 1;
                    }
                    while ki < idx.len() && idx[ki].1.is_whitespace() {
                        ki += 1;
                    }
                    continue;
                }
                match idx.get(ki) {
                    Some(&(u, c)) if c == nc => {
                        map.push((nb, u));
                        ni += 1;
                        ki += 1;
                    }
                    _ => continue 'outer,
                }
            }
            map.push((needle.len(), idx.get(ki).map(|x| x.0).unwrap_or(total)));
            return Some(map);
        }
        None
    }
}

#[cfg(test)]
mod align_tests {
    use super::TextIndex;

    #[test]
    fn aligns_across_whitespace_and_paragraphs() {
        let t = TextIndex::from_html("<p>Hello <strong>big</strong>\nworld</p>\n<p>Second  para</p>");
        // Index text: "Hello big\nworld\nSecond  para"
        let m = t.align("Hello big world", 0).unwrap();
        assert_eq!(m.first(), Some(&(0, 0)));
        assert_eq!(m.last(), Some(&(15, 15)));
        let m = t.align("Second para", 10).unwrap();
        assert_eq!(m[0], (0, 16));
        assert_eq!(*m.last().unwrap(), (11, 28), "the double space in the index is one space in the needle");
        assert!(t.align("missing", 0).is_none());
    }

    #[test]
    fn math_is_its_tex() {
        let t = TextIndex::from_html(r#"<p>a <span class="katex"><annotation encoding="application/x-tex">x^2</annotation></span> b</p>"#);
        let m = t.align("a x^2 b", 0).unwrap();
        assert_eq!(m.last(), Some(&(7, 7)));
    }
}

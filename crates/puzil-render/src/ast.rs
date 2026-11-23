//! Server HTML → `MessageAst`, the block/inline tree the UI lays out.
//!
//! The input is Zulip's rendered `content` (`apply_markdown=true`). Parsing is total: any
//! element we don't know is descended into for its text, so nothing silently disappears.

use std::sync::{Arc, LazyLock};

use scraper::{CaseSensitivity::CaseSensitive, ElementRef, Html, Node, Selector};

static TEX: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse(r#"annotation[encoding="application/x-tex"]"#).unwrap());
static MATHML: LazyLock<Selector> = LazyLock::new(|| Selector::parse("math").unwrap());

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MessageAst {
    pub blocks: Vec<Block>,
    /// An Overlay reply or thread header, taken out of `blocks` (the UI draws a chip).
    pub reply: Option<ReplyMeta>,
}

/// The first paragraph of a reply or a thread's first message: a mention of the parent's
/// author and a link to the parent (overlay.js `replyHeader`). Zulip's own quote-and-reply
/// has the same shape followed by a quote.
#[derive(Debug, Clone, PartialEq)]
pub struct ReplyMeta {
    /// Link text `thread from this message`: the first message of a thread.
    pub is_thread: bool,
    pub author: String,
    pub author_id: Option<u64>,
    pub href: String,
    pub stream_id: u64,
    /// The topic as encoded in the link.
    pub topic_encoded: String,
    pub parent_id: u64,
    /// `blocks[0]` is the quote of the parent (shown collapsed).
    pub quote: bool,
}

/// Parses `…/(channel|stream)/<id>[-name]/topic/<enc>/near/<id>`.
fn parse_near(href: &str) -> Option<(u64, String, u64)> {
    let i = href.find("/channel/").map(|i| i + 9).or_else(|| href.find("/stream/").map(|i| i + 8))?;
    let rest = &href[i..];
    let (chan, rest) = rest.split_once('/')?;
    let sid = chan.split('-').next()?.parse().ok()?;
    let rest = rest.strip_prefix("topic/")?;
    let (topic, rest) = rest.split_once('/')?;
    let rest = rest.strip_prefix("near/")?;
    let id: String = rest.chars().take_while(char::is_ascii_digit).collect();
    Some((sid, topic.to_string(), id.parse().ok()?))
}

fn reply_meta(blocks: &[Block]) -> Option<ReplyMeta> {
    let Some(Block::Para(v)) = blocks.first() else { return None };
    let Some(Inline::Mention { text, user_id, .. }) = v.first() else { return None };
    let (link_text, href) = v.iter().find_map(|i| match i {
        Inline::Text { text, style } => style.link.as_deref().filter(|l| l.contains("/near/")).map(|l| (text.clone(), l.to_string())),
        _ => None,
    })?;
    let mut rest = String::new();
    for i in &v[1..] {
        match i {
            Inline::Text { text, style } if style.link.is_none() => rest.push_str(text),
            Inline::Text { .. } => {}
            _ => return None,
        }
    }
    let rest = rest.trim();
    if !(rest.is_empty() || rest == ":") {
        return None;
    }
    let (stream_id, topic_encoded, parent_id) = parse_near(&href)?;
    Some(ReplyMeta {
        is_thread: link_text == "thread from this message",
        author: text.trim_start_matches('@').to_string(),
        author_id: *user_id,
        href,
        stream_id,
        topic_encoded,
        parent_id,
        quote: matches!(blocks.get(1), Some(Block::Quote(_))),
    })
}

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Para(Vec<Inline>),
    Heading(u8, Vec<Inline>),
    Quote(Vec<Block>),
    /// Highlighted code: lines of `(pygments class, text)` spans. The class is empty for
    /// unhighlighted text.
    Code { lang: Option<String>, lines: Vec<Vec<(String, String)>> },
    /// Display math: the TeX and KaTeX's `<math>` element (for typesetting).
    Math { tex: String, mathml: Option<String> },
    List { ordered: bool, start: u32, items: Vec<Vec<Block>> },
    Table { rows: Vec<Row> },
    /// A spoiler. Iliad marks AI-written text with a coloured circle at the start of
    /// the header (⚫🔴🟡🟢); `tint` is that circle.
    Spoiler { header: Vec<Block>, body: Vec<Block>, tint: Option<char> },
    Media(Media),
    Rule,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub header: bool,
    pub cells: Vec<Vec<Inline>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MediaKind {
    Image,
    Video,
    YouTube,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Media {
    pub kind: MediaKind,
    /// Where clicking goes (the full upload or the video page).
    pub href: Option<String>,
    /// The thumbnail or image source.
    pub src: Option<String>,
    pub title: Option<String>,
    /// From `data-original-dimensions`, so the list can reserve the height up front.
    pub dims: Option<(u32, u32)>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
    pub link: Option<Arc<str>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MentionKind {
    User,
    Group,
    /// `@all`, `@everyone`, `@channel`, `@topic`.
    Wildcard,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Inline {
    Text { text: String, style: Style },
    Break,
    Mention { text: String, kind: MentionKind, silent: bool, user_id: Option<u64> },
    /// `#channel` or `#channel > topic`.
    ChannelLink { text: String, href: String },
    /// Unicode emoji: the glyph to draw and its Zulip name.
    Emoji { glyph: String, name: String },
    /// Realm (custom) emoji.
    CustomEmoji { name: String, src: String },
    Math { tex: String, mathml: Option<String> },
    /// `<time datetime=…>`: a Unix timestamp, and the server's fallback text.
    Time { ts: Option<i64>, text: String },
    /// An image placed in running text (`img.inline-image`). Paragraphs split these out
    /// into [`Block::Media`]; they remain inline only inside headings and table cells.
    Image(Media),
}

impl MessageAst {
    pub fn from_html(html: &str) -> Self {
        let doc = Html::parse_fragment(html);
        let mut p = Parser::default();
        let mut blocks = p.blocks(doc.root_element());
        let reply = reply_meta(&blocks);
        if reply.is_some() {
            blocks.remove(0);
        }
        MessageAst { blocks, reply }
    }

    /// Every formula in the message: `(tex, mathml, display)`, in document order.
    pub fn math(&self) -> Vec<(&str, Option<&str>, bool)> {
        fn inl<'a>(v: &'a [Inline], out: &mut Vec<(&'a str, Option<&'a str>, bool)>) {
            for i in v {
                if let Inline::Math { tex, mathml } = i {
                    out.push((tex, mathml.as_deref(), false));
                }
            }
        }
        fn walk<'a>(bs: &'a [Block], out: &mut Vec<(&'a str, Option<&'a str>, bool)>) {
            for b in bs {
                match b {
                    Block::Math { tex, mathml } => out.push((tex, mathml.as_deref(), true)),
                    Block::Para(v) | Block::Heading(_, v) => inl(v, out),
                    Block::Quote(bs) => walk(bs, out),
                    Block::List { items, .. } => items.iter().for_each(|i| walk(i, out)),
                    Block::Spoiler { header, body, .. } => {
                        walk(header, out);
                        walk(body, out);
                    }
                    Block::Table { rows } => rows.iter().flat_map(|r| &r.cells).for_each(|c| inl(c, out)),
                    Block::Code { .. } | Block::Media(_) | Block::Rule => {}
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.blocks, &mut out);
        out
    }

    /// Links to uploaded files that aren't shown as images (PDFs, archives, …): `(name, href)`.
    pub fn attachments(&self) -> Vec<(String, String)> {
        fn inl(v: &[Inline], out: &mut Vec<(String, String)>) {
            for i in v {
                if let Inline::Text { text, style } = i
                    && let Some(l) = &style.link
                    && l.contains("/user_uploads/")
                    && !out.iter().any(|(_, h)| h.as_str() == &**l)
                {
                    out.push((text.clone(), l.to_string()));
                }
            }
        }
        fn walk(bs: &[Block], out: &mut Vec<(String, String)>) {
            for b in bs {
                match b {
                    Block::Para(v) | Block::Heading(_, v) => inl(v, out),
                    Block::Quote(bs) => walk(bs, out),
                    Block::List { items, .. } => items.iter().for_each(|i| walk(i, out)),
                    Block::Spoiler { body, .. } => walk(body, out),
                    _ => {}
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.blocks, &mut out);
        let media: Vec<&str> = self.media().iter().filter_map(|m| m.href.as_deref()).collect();
        out.retain(|(_, h)| !media.contains(&h.as_str()));
        out
    }

    /// Every media block, in document order.
    pub fn media(&self) -> Vec<&Media> {
        fn walk<'a>(bs: &'a [Block], out: &mut Vec<&'a Media>) {
            for b in bs {
                match b {
                    Block::Media(m) => out.push(m),
                    Block::Quote(bs) => walk(bs, out),
                    Block::List { items, .. } => items.iter().for_each(|i| walk(i, out)),
                    Block::Spoiler { header, body, .. } => {
                        walk(header, out);
                        walk(body, out);
                    }
                    _ => {}
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.blocks, &mut out);
        out
    }

    /// Plain text of the whole message, for previews and notifications.
    pub fn plain_text(&self) -> String {
        let mut out = String::new();
        for b in &self.blocks {
            block_text(b, &mut out);
            if !out.ends_with('\n') {
                out.push('\n');
            }
        }
        out.trim_end().to_string()
    }
}

fn inlines_text(v: &[Inline], out: &mut String) {
    for i in v {
        match i {
            Inline::Text { text, .. } | Inline::Mention { text, .. } | Inline::ChannelLink { text, .. } => out.push_str(text),
            Inline::Time { text, .. } => out.push_str(text),
            Inline::Image(m) => out.push_str(m.title.as_deref().unwrap_or("(image)")),
            Inline::Break => out.push('\n'),
            Inline::Emoji { glyph, .. } => out.push_str(glyph),
            Inline::CustomEmoji { name, .. } => {
                out.push(':');
                out.push_str(name);
                out.push(':');
            }
            Inline::Math { tex, .. } => {
                out.push_str("$$");
                out.push_str(tex);
                out.push_str("$$");
            }
        }
    }
}

fn block_text(b: &Block, out: &mut String) {
    match b {
        Block::Para(v) | Block::Heading(_, v) => inlines_text(v, out),
        Block::Quote(bs) => bs.iter().for_each(|b| block_text(b, out)),
        Block::Code { lines, .. } => {
            for l in lines {
                l.iter().for_each(|(_, t)| out.push_str(t));
                out.push('\n');
            }
        }
        Block::Math { tex, .. } => {
            out.push_str("$$");
            out.push_str(tex);
            out.push_str("$$");
        }
        Block::List { items, .. } => {
            for it in items {
                it.iter().for_each(|b| block_text(b, out));
                out.push('\n');
            }
        }
        Block::Table { rows } => {
            for r in rows {
                for (i, c) in r.cells.iter().enumerate() {
                    if i > 0 {
                        out.push_str(" | ");
                    }
                    inlines_text(c, out);
                }
                out.push('\n');
            }
        }
        Block::Spoiler { header, .. } => header.iter().for_each(|b| block_text(b, out)),
        Block::Media(m) => out.push_str(m.title.as_deref().unwrap_or("(media)")),
        Block::Rule => out.push_str("---"),
    }
}

fn has(el: &ElementRef, class: &str) -> bool {
    el.value().has_class(class, CaseSensitive)
}

/// Unicode glyph for a Zulip emoji code such as `1f469-200d-1f4bb`.
pub fn emoji_glyph(code: &str) -> Option<String> {
    let mut s: String = code.split('-').map(|h| u32::from_str_radix(h, 16).ok().and_then(char::from_u32)).collect::<Option<_>>()?;
    // Older symbols (❤ U+2764, ☺, ✔…) default to text presentation and draw as black glyphs
    // unless asked for the emoji form; Zulip's codes leave the selector off.
    let mut chars = s.chars();
    if let (Some(c), None) = (chars.next(), chars.next())
        && (c as u32) < 0x1F000
    {
        s.push('\u{FE0F}');
    }
    Some(s)
}

const BLOCK_TAGS: &[&str] = &[
    "p", "h1", "h2", "h3", "h4", "h5", "h6", "blockquote", "ul", "ol", "pre", "table", "hr", "div",
];

fn is_block(el: &ElementRef) -> bool {
    let name = el.value().name();
    BLOCK_TAGS.contains(&name) || (name == "span" && has(el, "katex-display"))
}

#[derive(Default)]
struct Parser;

impl Parser {
    /// Block children of `el`. Loose inline content between blocks becomes a paragraph.
    fn blocks(&mut self, el: ElementRef) -> Vec<Block> {
        let mut out = Vec::new();
        let mut loose = Vec::new();
        let flush = |loose: &mut Vec<Inline>, out: &mut Vec<Block>| {
            paragraphs(tidy(std::mem::take(loose)), out);
        };
        for child in el.children() {
            match child.value() {
                Node::Text(t) => push_text(&mut loose, t, &Style::default()),
                Node::Element(_) => {
                    let c = ElementRef::wrap(child).unwrap();
                    if is_block(&c) {
                        flush(&mut loose, &mut out);
                        self.block(c, &mut out);
                    } else {
                        self.inline(c, &Style::default(), &mut loose);
                    }
                }
                _ => {}
            }
        }
        flush(&mut loose, &mut out);
        out
    }

    fn block(&mut self, el: ElementRef, out: &mut Vec<Block>) {
        let name = el.value().name();
        match name {
            "p" => {
                // Zulip wraps ```math blocks in a paragraph of their own.
                let mut kids = el.children().filter(|c| !matches!(c.value(), Node::Text(t) if t.trim().is_empty()));
                if let (Some(only), None) = (kids.next(), kids.next())
                    && let Some(span) = ElementRef::wrap(only)
                    && span.value().name() == "span"
                    && has(&span, "katex-display")
                {
                    return out.push(Block::Math { tex: tex_of(span), mathml: mathml_of(span) });
                }
                let v = self.inlines(el);
                paragraphs(v, out);
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                out.push(Block::Heading(name[1..].parse().unwrap_or(1), self.inlines(el)))
            }
            "blockquote" => out.push(Block::Quote(self.blocks(el))),
            "hr" => out.push(Block::Rule),
            "ul" | "ol" => {
                let start = el.value().attr("start").and_then(|s| s.parse().ok()).unwrap_or(1);
                let items = el
                    .children()
                    .filter_map(ElementRef::wrap)
                    .filter(|c| c.value().name() == "li")
                    .map(|li| self.blocks(li))
                    .collect();
                out.push(Block::List { ordered: name == "ol", start, items });
            }
            "pre" => out.push(code_block(el, None)),
            "table" => out.push(Block::Table { rows: self.rows(el) }),
            "span" => out.push(Block::Math { tex: tex_of(el), mathml: mathml_of(el) }),
            "div" if has(&el, "codehilite") => {
                let lang = el.value().attr("data-code-language").map(str::to_string);
                match el.children().filter_map(ElementRef::wrap).find(|c| c.value().name() == "pre") {
                    Some(pre) => out.push(code_block(pre, lang)),
                    None => out.extend(self.blocks(el)),
                }
            }
            "div" if has(&el, "spoiler-block") => {
                let part = |cls: &str| el.children().filter_map(ElementRef::wrap).find(|c| has(c, cls));
                let header = part("spoiler-header").map(|h| self.blocks(h)).unwrap_or_default();
                let body = part("spoiler-content").map(|b| self.blocks(b)).unwrap_or_default();
                let tint = first_char(&header).filter(|c| matches!(c, '⚫' | '🔴' | '🟡' | '🟢'));
                out.push(Block::Spoiler { header, body, tint });
            }
            "div" if has(&el, "message_inline_image") || has(&el, "youtube-video") => out.push(Block::Media(media(el))),
            _ => out.extend(self.blocks(el)),
        }
    }

    fn rows(&mut self, table: ElementRef) -> Vec<Row> {
        let mut rows = Vec::new();
        for tr in table.descendants().filter_map(ElementRef::wrap).filter(|e| e.value().name() == "tr") {
            let mut header = false;
            let cells = tr
                .children()
                .filter_map(ElementRef::wrap)
                .filter(|c| matches!(c.value().name(), "td" | "th"))
                .map(|c| {
                    header |= c.value().name() == "th";
                    self.inlines(c)
                })
                .collect();
            rows.push(Row { header, cells });
        }
        rows
    }

    fn inlines(&mut self, el: ElementRef) -> Vec<Inline> {
        let mut v = Vec::new();
        self.inline_children(el, &Style::default(), &mut v);
        tidy(v)
    }

    fn inline_children(&mut self, el: ElementRef, style: &Style, out: &mut Vec<Inline>) {
        for child in el.children() {
            match child.value() {
                Node::Text(t) => push_text(out, t, style),
                Node::Element(_) => self.inline(ElementRef::wrap(child).unwrap(), style, out),
                _ => {}
            }
        }
    }

    fn inline(&mut self, el: ElementRef, style: &Style, out: &mut Vec<Inline>) {
        let e = el.value();
        let mut s = style.clone();
        match e.name() {
            "br" => return out.push(Inline::Break),
            "strong" | "b" => s.bold = true,
            "em" | "i" => s.italic = true,
            "del" | "s" => s.strike = true,
            "code" => s.code = true,
            "time" => {
                let ts = e.attr("datetime").and_then(parse_iso_ts);
                return out.push(Inline::Time { ts, text: el.text().collect() });
            }
            "img" if has(&el, "emoji") => {
                let name = e.attr("title").or(e.attr("alt")).unwrap_or("").trim_matches(':').to_string();
                return out.push(Inline::CustomEmoji { name, src: e.attr("src").unwrap_or("").to_string() });
            }
            "img" if has(&el, "inline-image") => {
                let dims = e.attr("data-original-dimensions").and_then(parse_dims);
                return out.push(Inline::Image(Media {
                    kind: MediaKind::Image,
                    href: e.attr("data-original-src").or(e.attr("src")).map(str::to_string),
                    src: e.attr("src").map(str::to_string),
                    title: e.attr("alt").map(str::to_string),
                    dims,
                }));
            }
            "img" => {
                let alt = e.attr("alt").unwrap_or("image");
                return push_text(out, alt, &s);
            }
            "a" if has(&el, "stream") || has(&el, "stream-topic") => {
                return out.push(Inline::ChannelLink {
                    text: el.text().collect(),
                    href: e.attr("href").unwrap_or("").to_string(),
                });
            }
            "a" => s.link = e.attr("href").map(Arc::from),
            "span" if has(&el, "katex") || has(&el, "katex-display") => {
                return out.push(Inline::Math { tex: tex_of(el), mathml: mathml_of(el) });
            }
            "span" if has(&el, "tex-error") => s.code = true,
            "span" if has(&el, "emoji") => {
                let code = e.classes().find_map(|c| c.strip_prefix("emoji-")).unwrap_or("");
                let name = e.attr("title").unwrap_or("").replace(' ', "_");
                return match emoji_glyph(code) {
                    Some(glyph) => out.push(Inline::Emoji { glyph, name }),
                    None => push_text(out, &el.text().collect::<String>(), &s),
                };
            }
            "span" if has(&el, "user-mention") || has(&el, "user-group-mention") => {
                let kind = if has(&el, "user-group-mention") {
                    MentionKind::Group
                } else if e.attr("data-user-id") == Some("*") || e.classes().any(|c| c.ends_with("wildcard-mention")) {
                    MentionKind::Wildcard
                } else {
                    MentionKind::User
                };
                return out.push(Inline::Mention {
                    text: el.text().collect(),
                    kind,
                    silent: has(&el, "silent"),
                    user_id: e.attr("data-user-id").and_then(|u| u.parse().ok()),
                });
            }
            _ => {}
        }
        self.inline_children(el, &s, out);
    }
}

fn first_char(blocks: &[Block]) -> Option<char> {
    match blocks.first()? {
        Block::Para(v) => match v.first()? {
            Inline::Text { text, .. } => text.trim_start().chars().next(),
            Inline::Emoji { glyph, .. } => glyph.chars().next(),
            _ => None,
        },
        _ => None,
    }
}

fn tex_of(el: ElementRef) -> String {
    match el.select(&TEX).next() {
        Some(a) => a.text().collect(),
        None => el.text().collect(),
    }
}

fn mathml_of(el: ElementRef) -> Option<String> {
    el.select(&MATHML).next().map(|m| m.html())
}

fn code_block(pre: ElementRef, lang: Option<String>) -> Block {
    let mut spans: Vec<(String, String)> = Vec::new();
    fn walk(el: ElementRef, class: &str, spans: &mut Vec<(String, String)>) {
        for child in el.children() {
            match child.value() {
                Node::Text(t) => spans.push((class.to_string(), t.to_string())),
                Node::Element(e) => {
                    let c = e.attr("class").unwrap_or(class);
                    walk(ElementRef::wrap(child).unwrap(), c, spans)
                }
                _ => {}
            }
        }
    }
    walk(pre, "", &mut spans);
    let mut lines = vec![Vec::new()];
    for (class, text) in spans {
        for (i, part) in text.split('\n').enumerate() {
            if i > 0 {
                lines.push(Vec::new());
            }
            if !part.is_empty() {
                lines.last_mut().unwrap().push((class.clone(), part.to_string()));
            }
        }
    }
    while lines.last().is_some_and(|l| l.is_empty()) && lines.len() > 1 {
        lines.pop();
    }
    Block::Code { lang, lines }
}

/// A paragraph's inlines as blocks: inline images become media blocks between the
/// surrounding text.
fn paragraphs(v: Vec<Inline>, out: &mut Vec<Block>) {
    let mut cur = Vec::new();
    for i in v {
        match i {
            Inline::Image(m) => {
                let text = tidy(std::mem::take(&mut cur));
                if !text.is_empty() {
                    out.push(Block::Para(text));
                }
                out.push(Block::Media(m));
            }
            i => cur.push(i),
        }
    }
    let text = tidy(cur);
    if !text.is_empty() {
        out.push(Block::Para(text));
    }
}

fn parse_dims(d: &str) -> Option<(u32, u32)> {
    let (w, h) = d.split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

fn media(div: ElementRef) -> Media {
    let kind = if has(&div, "youtube-video") {
        MediaKind::YouTube
    } else if has(&div, "message_inline_video") {
        MediaKind::Video
    } else {
        MediaKind::Image
    };
    let a = div.descendants().filter_map(ElementRef::wrap).find(|e| e.value().name() == "a");
    let img = div.descendants().filter_map(ElementRef::wrap).find(|e| matches!(e.value().name(), "img" | "video"));
    let dims = img.and_then(|i| i.value().attr("data-original-dimensions")).and_then(parse_dims);
    Media {
        kind,
        href: a.and_then(|a| a.value().attr("href")).map(str::to_string),
        src: img.and_then(|i| i.value().attr("data-src-fullsize").or(i.value().attr("src"))).map(str::to_string),
        title: a.and_then(|a| a.value().attr("title")).map(str::to_string),
        dims,
    }
}

/// HTML whitespace inside inline content: newlines are spaces, runs collapse.
fn push_text(out: &mut Vec<Inline>, t: &str, style: &Style) {
    let mut s = String::with_capacity(t.len());
    let mut prev_space = matches!(out.last(), Some(Inline::Text { text, .. }) if text.ends_with(' '));
    for c in t.chars() {
        let c = if c == '\n' || c == '\t' || c == '\r' { ' ' } else { c };
        if c == ' ' && prev_space && !style.code {
            continue;
        }
        prev_space = c == ' ';
        s.push(c);
    }
    if s.is_empty() {
        return;
    }
    if let Some(Inline::Text { text, style: st }) = out.last_mut()
        && st == style
    {
        text.push_str(&s);
        return;
    }
    out.push(Inline::Text { text: s, style: style.clone() });
}

/// Trim whitespace at the start and end of a paragraph and around line breaks.
fn tidy(mut v: Vec<Inline>) -> Vec<Inline> {
    let n = v.len();
    for i in 0..n {
        let after_break = i == 0 || matches!(v[i - 1], Inline::Break);
        let before_break = i + 1 == n || matches!(v[i + 1], Inline::Break);
        if let Inline::Text { text, style } = &mut v[i]
            && !style.code
        {
            if after_break {
                *text = text.trim_start().to_string();
            }
            if before_break {
                *text = text.trim_end().to_string();
            }
        }
    }
    v.retain(|i| !matches!(i, Inline::Text { text, .. } if text.is_empty()));
    while matches!(v.last(), Some(Inline::Break)) {
        v.pop();
    }
    let lead = v.iter().take_while(|i| matches!(i, Inline::Break)).count();
    v.drain(..lead);
    v
}

/// `2024-05-01T12:00:00Z` → Unix seconds. Only the forms Zulip emits.
fn parse_iso_ts(s: &str) -> Option<i64> {
    let (date, rest) = s.split_once('T')?;
    let mut d = date.split('-').map(|x| x.parse::<i64>());
    let (y, m, day) = (d.next()?.ok()?, d.next()?.ok()?, d.next()?.ok()?);
    let (time, offset) = match rest.find(['Z', '+', '-']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "Z"),
    };
    let mut t = time.split(':').map(|x| x.split('.').next().unwrap_or("0").parse::<i64>());
    let (hh, mm, ss) = (t.next()?.ok()?, t.next().unwrap_or(Ok(0)).ok()?, t.next().unwrap_or(Ok(0)).ok()?);
    let off = match offset.as_bytes().first() {
        Some(b'+') | Some(b'-') => {
            let sign = if offset.starts_with('-') { -1 } else { 1 };
            let o = offset[1..].replace(':', "");
            let (oh, om) = (o.get(..2)?.parse::<i64>().ok()?, o.get(2..4).unwrap_or("0").parse::<i64>().ok()?);
            sign * (oh * 3600 + om * 60)
        }
        _ => 0,
    };
    // Days from civil (Howard Hinnant).
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + hh * 3600 + mm * 60 + ss - off)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Inline {
        Inline::Text { text: s.into(), style: Style::default() }
    }

    #[test]
    fn paragraphs_breaks_and_styles() {
        let a = MessageAst::from_html("<p>Hello <strong>bold</strong> and <em>it</em><br>\nnext</p>\n<p>two</p>");
        assert_eq!(a.blocks.len(), 2);
        let Block::Para(v) = &a.blocks[0] else { panic!() };
        assert_eq!(v[0], text("Hello "));
        assert!(matches!(&v[1], Inline::Text { style, .. } if style.bold));
        assert_eq!(v[4], Inline::Break);
        assert_eq!(v[5], text("next"));
    }

    #[test]
    fn mentions_emoji_math_links() {
        let a = MessageAst::from_html(concat!(
            r#"<p><span class="user-mention silent" data-user-id="7">Ada</span> "#,
            r#"<span aria-label="tada" class="emoji emoji-1f389" role="img" title="tada">:tada:</span> "#,
            r#"<span class="katex"><span class="katex-mathml"><math><semantics><mi>x</mi><annotation encoding="application/x-tex">x^2</annotation></semantics></math></span><span class="katex-html">x</span></span> "#,
            r##"<a href="https://e.com">link</a> <a class="stream-topic" href="#narrow/channel/1-a/topic/b">#a &gt; b</a></p>"##
        ));
        let Block::Para(v) = &a.blocks[0] else { panic!() };
        assert_eq!(v[0], Inline::Mention { text: "Ada".into(), kind: MentionKind::User, silent: true, user_id: Some(7) });
        assert_eq!(v[2], Inline::Emoji { glyph: "🎉".into(), name: "tada".into() });
        assert!(matches!(&v[4], Inline::Math { tex, mathml: Some(m) } if tex == "x^2" && m.starts_with("<math") && m.contains("<mi>x</mi>")));
        assert!(matches!(&v[6], Inline::Text { style, .. } if style.link.as_deref() == Some("https://e.com")));
        assert_eq!(v[8], Inline::ChannelLink { text: "#a > b".into(), href: "#narrow/channel/1-a/topic/b".into() });
    }

    #[test]
    fn blocks_code_lists_spoilers_media() {
        let a = MessageAst::from_html(concat!(
            "<div class=\"codehilite\" data-code-language=\"Python\"><pre><span></span><code><span class=\"k\">def</span> f():\n    <span class=\"k\">pass</span>\n</code></pre></div>",
            "<ul>\n<li>one</li>\n<li><p>two</p></li>\n</ul>",
            r#"<div class="spoiler-block"><div class="spoiler-header"><p>⚫ Claude</p></div><div class="spoiler-content" aria-hidden="true"><p>body</p></div></div>"#,
            r#"<div class="message_inline_image"><a href="/user_uploads/x.png" title="x.png"><img data-original-dimensions="640x480" src="/thumb.webp"></a></div>"#,
            r#"<span class="katex-display"><span class="katex"><annotation encoding="application/x-tex">\int</annotation></span></span>"#,
        ));
        let Block::Code { lang, lines } = &a.blocks[0] else { panic!("{:?}", a.blocks[0]) };
        assert_eq!(lang.as_deref(), Some("Python"));
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0][0], ("k".into(), "def".into()));
        let Block::List { items, ordered: false, .. } = &a.blocks[1] else { panic!() };
        assert_eq!(items.len(), 2);
        let Block::Spoiler { tint, body, .. } = &a.blocks[2] else { panic!() };
        assert_eq!(*tint, Some('⚫'));
        assert_eq!(body.len(), 1);
        let Block::Media(m) = &a.blocks[3] else { panic!() };
        assert_eq!(m.dims, Some((640, 480)));
        assert!(matches!(&a.blocks[4], Block::Math { tex, mathml: None } if tex == "\\int"));
    }

    #[test]
    fn paragraph_of_display_math_is_a_block() {
        let a = MessageAst::from_html(
            r#"<p><span class="katex-display"><span class="katex"><span class="katex-mathml"><math display="block"><mi>y</mi></math></span></span></span></p>"#,
        );
        assert!(matches!(&a.blocks[..], [Block::Math { mathml: Some(m), .. }] if m.contains("<mi>y</mi>")), "{:?}", a.blocks);
    }

    #[test]
    fn inline_images_split_paragraphs_and_attachments() {
        let a = MessageAst::from_html(concat!(
            r#"<p>before <img alt="image.png" class="inline-image" data-original-dimensions="2800x1080" "#,
            r#"data-original-src="/user_uploads/1/a/image.png" src="/user_uploads/thumbnail/1/a/image.png/840x560.webp"> after</p>"#,
            r#"<p>See <a href="/user_uploads/1/b/paper.pdf">paper.pdf</a> and <a href="/user_uploads/1/a/image.png">again</a></p>"#,
        ));
        assert!(matches!(&a.blocks[0], Block::Para(v) if v == &[text("before")]));
        let Block::Media(m) = &a.blocks[1] else { panic!("{:?}", a.blocks) };
        assert_eq!((m.dims, m.href.as_deref()), (Some((2800, 1080)), Some("/user_uploads/1/a/image.png")));
        assert!(matches!(&a.blocks[2], Block::Para(v) if v == &[text("after")]));
        assert_eq!(a.attachments(), [("paper.pdf".to_string(), "/user_uploads/1/b/paper.pdf".to_string())]);
    }

    #[test]
    fn reply_and_thread_headers() {
        let a = MessageAst::from_html(concat!(
            r#"<p><span class="user-mention silent" data-user-id="7">Ada</span> <a href="https://r/#narrow/channel/12-gen/topic/My.20topic/near/99">↩</a></p>"#,
            "
<blockquote>
<p>quoted</p>
</blockquote>
<p>my answer</p>"
        ));
        let r = a.reply.as_ref().unwrap();
        assert_eq!((r.is_thread, r.author.as_str(), r.author_id, r.stream_id, r.parent_id, r.quote), (false, "Ada", Some(7), 12, 99, true));
        assert_eq!(r.topic_encoded, "My.20topic");
        assert!(matches!(a.blocks[0], Block::Quote(_)));
        let t = MessageAst::from_html(
            r#"<p><span class="user-mention silent" data-user-id="8">Bo</span> <a href="/#narrow/channel/3/topic/T/near/5">thread from this message</a></p><p>x</p>"#,
        );
        assert!(t.reply.as_ref().unwrap().is_thread);
        // Text beyond the mention and link: an ordinary message.
        let n = MessageAst::from_html(
            r#"<p><span class="user-mention" data-user-id="8">@Bo</span> see <a href="/#narrow/channel/3/topic/T/near/5">this</a></p>"#,
        );
        assert!(n.reply.is_none());
    }

    #[test]
    fn iso_time() {
        assert_eq!(parse_iso_ts("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso_ts("2024-03-01T10:30:00+01:00"), Some(1709285400));
    }
}

//! `MessageAst` → laid-out blocks. Building happens once per message version; rendering
//! a built message is cheap.

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::Arc,
};

use gpui_kit::{prelude::FluentBuilder as _, *};
use puzil_render::ast::{Block, Media, MediaKind, MessageAst};

use super::rich::{self, BuildCtx, ImageLookup, LinkHandler, Paragraph, RichText, Selection};
use crate::{media::fit, theme::Theme};

/// The box inline images are fitted into.
pub const MEDIA_MAX: (f32, f32) = (480., 320.);
/// Placeholder size for an image whose dimensions the server didn't send.
const MEDIA_UNKNOWN: (f32, f32) = (240., 160.);

pub enum BuiltBlock {
    Para(usize),
    Heading(u8, usize),
    Quote(Vec<BuiltBlock>),
    Code(usize),
    /// Display math; `true` once typeset (else its TeX in a code box).
    Math(usize, bool),
    List {
        ordered: bool,
        start: u32,
        items: Vec<Vec<BuiltBlock>>,
    },
    Table(Vec<(bool, Vec<usize>)>),
    Spoiler {
        key: usize,
        header: Vec<BuiltBlock>,
        body: Vec<BuiltBlock>,
        tint: Option<char>,
    },
    Media(Media),
    Rule,
}

/// A message ready to render. Paragraphs are numbered in document order; selection
/// positions refer to these numbers.
pub struct BuiltMsg {
    pub blocks: Vec<BuiltBlock>,
    pub paras: Vec<Rc<Paragraph>>,
    /// Formulas still being typeset; rebuild when any of them is ready.
    pub pending: HashSet<puzil_math::Key>,
    /// Images this message shows (media thumbnails and custom emoji). Emoji not yet loaded
    /// need a rebuild when they arrive; media only need a repaint unless their size was
    /// unknown.
    pub images: HashSet<String>,
    /// Images whose arrival changes the layout.
    pub relayout_images: HashSet<String>,
    /// Uploaded files linked from the text: `(name, href)`.
    pub attachments: Vec<(String, String)>,
    /// An Overlay reply or thread header (drawn as a chip above the message).
    pub reply: Option<puzil_render::ast::ReplyMeta>,
    /// Overlay text index of the message and, per paragraph, how its index text aligns
    /// with it (`(index-text byte, index unit)`), when the message can carry inline
    /// reactions.
    pub index: Option<(Vec<u16>, Vec<Option<Vec<(usize, usize)>>>)>,
    /// The inline reaction behind each badge id.
    pub badges: Vec<puzil_overlay::InlineReaction>,
}

/// Toggle key for a reply's collapsed quote (spoilers use small numbers).
pub const QUOTE_KEY: usize = usize::MAX;

struct Ctx<'a> {
    theme: &'a Theme,
    me: u64,
    math: Option<&'a puzil_math::Engine>,
    images: Option<ImageLookup<'a>>,
    paras: Vec<Rc<Paragraph>>,
    spoilers: usize,
}

impl Ctx<'_> {
    fn add(&mut self, p: Paragraph) -> usize {
        self.paras.push(Rc::new(p));
        self.paras.len() - 1
    }

    fn inl(&mut self, v: &[puzil_render::ast::Inline], bold: bool) -> usize {
        let p = rich::build(v, &self.bcx(bold));
        self.add(p)
    }

    fn bcx(&self, bold: bool) -> BuildCtx<'_> {
        BuildCtx { theme: self.theme, me: self.me, bold, color: None, math: self.math, images: self.images }
    }

    fn blocks(&mut self, bs: &[Block]) -> Vec<BuiltBlock> {
        bs.iter().map(|b| self.block(b)).collect()
    }

    fn block(&mut self, b: &Block) -> BuiltBlock {
        match b {
            Block::Para(v) => BuiltBlock::Para(self.inl(v, false)),
            Block::Heading(l, v) => BuiltBlock::Heading(*l, self.inl(v, true)),
            Block::Quote(bs) => BuiltBlock::Quote(self.blocks(bs)),
            Block::Code { lines, .. } => {
                let p = rich::build_code(lines, self.theme);
                BuiltBlock::Code(self.add(p))
            }
            Block::Math { tex, mathml } => {
                let (p, typeset) = rich::build_display_math(tex, mathml.as_deref(), &self.bcx(false));
                BuiltBlock::Math(self.add(p), typeset)
            }
            Block::List { ordered, start, items } => {
                BuiltBlock::List { ordered: *ordered, start: *start, items: items.iter().map(|it| self.blocks(it)).collect() }
            }
            Block::Table { rows } => BuiltBlock::Table(
                rows.iter().map(|r| (r.header, r.cells.iter().map(|c| self.inl(c, r.header)).collect())).collect(),
            ),
            Block::Spoiler { header, body, tint } => {
                let key = self.spoilers;
                self.spoilers += 1;
                BuiltBlock::Spoiler { key, header: self.blocks(header), body: self.blocks(body), tint: *tint }
            }
            Block::Media(m) => BuiltBlock::Media(m.clone()),
            Block::Rule => BuiltBlock::Rule,
        }
    }
}

/// Inline reactions to draw, with the message's HTML (for its Overlay text index).
pub struct Inline<'a> {
    pub html: &'a str,
    pub reactions: &'a [puzil_overlay::InlineReaction],
}

pub fn build(
    ast: &MessageAst,
    theme: &Theme,
    me: u64,
    math: Option<&puzil_math::Engine>,
    images: Option<ImageLookup<'_>>,
    inline: Option<Inline<'_>>,
) -> BuiltMsg {
    let mut cx = Ctx { theme, me, math, images, paras: vec![], spoilers: 0 };
    // A paragraph that only links the uploaded files would repeat their cards: drop it.
    let attachments = ast.attachments();
    let shown: Vec<Block> = ast.blocks.iter().filter_map(|b| without_file_links(b, &attachments)).collect();
    let blocks = cx.blocks(&shown);
    let pending = cx.paras.iter().flat_map(|p| p.pending.iter().copied()).collect();
    let mut relayout: HashSet<String> = cx.paras.iter().flat_map(|p| p.pending_images.iter().cloned()).collect();
    let mut all = relayout.clone();
    for m in ast.media() {
        if let Some(src) = &m.src {
            all.insert(src.clone());
            if m.dims.is_none() {
                relayout.insert(src.clone());
            }
        }
    }
    let mut paras = cx.paras;
    let (index, badges) = match inline {
        Some(i) => place_inline(&mut paras, i, theme),
        None => (None, vec![]),
    };
    BuiltMsg {
        blocks,
        paras,
        pending,
        images: all,
        relayout_images: relayout,
        attachments,
        reply: ast.reply.clone(),
        index,
        badges,
    }
}

type IndexMaps = (Vec<u16>, Vec<Option<Vec<(usize, usize)>>>);

/// Aligns every paragraph with the Overlay text index, finds each reacted passage
/// (overlay.js `locate`: its recorded place if the digest still matches, else the first
/// same-length window that does), and marks it with a highlight and badges.
fn place_inline(
    paras: &mut [Rc<Paragraph>],
    inline: Inline<'_>,
    theme: &Theme,
) -> (Option<IndexMaps>, Vec<puzil_overlay::InlineReaction>) {
    use puzil_render::text_index::{TextIndex, locate};
    let ti = TextIndex::from_html(inline.html);
    let mut maps = Vec::with_capacity(paras.len());
    let mut cursor = 0;
    for p in paras.iter() {
        let m = if p.index_text.trim().is_empty() { None } else { ti.align(&p.index_text, cursor) };
        if let Some(m) = &m {
            cursor = m.last().map(|x| x.1).unwrap_or(cursor);
        }
        maps.push(m);
    }
    // Group reactions by passage, in order of first appearance.
    let mut groups: Vec<((usize, usize, String), Vec<puzil_overlay::InlineReaction>)> = Vec::new();
    for r in inline.reactions {
        let k = (r.start, r.end, r.hash.clone());
        match groups.iter_mut().find(|(g, _)| *g == k) {
            Some((_, v)) => v.push(r.clone()),
            None => groups.push((k, vec![r.clone()])),
        }
    }
    let mut marks: Vec<Vec<(std::ops::Range<usize>, Vec<(SharedString, bool)>, usize)>> = vec![vec![]; paras.len()];
    let mut badges = Vec::new();
    for ((start, end, hash), rs) in groups {
        let Some(at) = locate(&ti.text, start, end, &hash) else {
            tracing::debug!("inline passage {start}..{end} not found");
            continue;
        };
        let (a, b) = (at, at + (end - start));
        // Display ranges in every paragraph the passage touches.
        let mut touched = Vec::new();
        for (pi, m) in maps.iter().enumerate() {
            let Some(m) = m else { continue };
            let (lo, hi) = (m.first().map(|x| x.1).unwrap_or(0), m.last().map(|x| x.1).unwrap_or(0));
            if hi <= a || lo >= b {
                continue;
            }
            let ib_start = m.iter().find(|(_, u)| *u >= a).map(|x| x.0).unwrap_or(0);
            let ib_end = m.iter().rev().find(|(_, u)| *u <= b).map(|x| x.0).unwrap_or(paras[pi].index_text.len());
            let p = &paras[pi];
            let (ds, de) = (p.index_to_display(ib_start, false), p.index_to_display(ib_end, true));
            if de > ds {
                touched.push((pi, ds..de));
            }
        }
        let Some(last) = touched.len().checked_sub(1) else {
            continue;
        };
        for (k, (pi, r)) in touched.into_iter().enumerate() {
            let labels: Vec<(SharedString, bool)> = if k == last {
                rs.iter()
                    .map(|x| {
                        let g = puzil_overlay::palette_glyph(&x.emoji)
                            .map(str::to_string)
                            .unwrap_or_else(|| format!(":{}:", x.emoji));
                        (SharedString::from(format!("{g} {}", x.user_ids.len())), x.mine)
                    })
                    .collect()
            } else {
                vec![]
            };
            let first = badges.len();
            if k == last {
                badges.extend(rs.iter().cloned());
            }
            marks[pi].push((r, labels, first));
        }
    }
    for (pi, m) in marks.into_iter().enumerate() {
        if !m.is_empty() {
            paras[pi] = Rc::new(paras[pi].with_marks(&m, theme));
        }
    }
    (Some((ti.text, maps)), badges)
}

/// A selected passage as Overlay addresses it: `(start, end, hash)` in index units.
pub struct Passage {
    pub start: usize,
    pub end: usize,
    pub hash: String,
}

/// Everything a render pass of one message needs.
pub struct RenderCtx {
    pub msg: u64,
    pub theme: Rc<Theme>,
    pub selection: Rc<RefCell<Selection>>,
    pub on_link: LinkHandler,
    pub expanded: Rc<HashSet<(u64, usize)>>,
    pub on_toggle: Rc<dyn Fn(u64, usize, &mut Window, &mut App)>,
    /// Loaded images among this message's [`BuiltMsg::images`].
    pub images: HashMap<String, Arc<RenderImage>>,
    pub on_media: Rc<dyn Fn(&Media, &mut Window, &mut App)>,
    pub on_attachment: Rc<dyn Fn(&str, &str, &mut Window, &mut App)>,
    /// Attachments being downloaded.
    pub downloading: Rc<HashSet<String>>,
    pub on_badge: Option<Rc<dyn Fn(u64, usize, &mut Window, &mut App)>>,
}

impl BuiltMsg {
    /// The selection as an Overlay passage, if it lies within this message. A formula cut
    /// by the selection's edge is taken whole, as in overlay.js.
    pub fn passage(&self, msg: u64, sel: &Selection) -> Option<Passage> {
        let (a, b) = sel.ordered()?;
        if a.msg != msg || b.msg != msg || a == b {
            return None;
        }
        let (units, maps) = self.index.as_ref()?;
        let unit = |pos: rich::Pos, end: bool| -> Option<usize> {
            let p = self.paras.get(pos.para)?;
            let m = maps.get(pos.para)?.as_ref()?;
            let ib = p.display_to_index(pos.byte, end);
            m.iter().find(|(b, _)| *b >= ib).map(|x| x.1)
        };
        let (start, end) = (unit(a, false)?, unit(b, true)?);
        if start >= end || end > units.len() || String::from_utf16_lossy(&units[start..end]).trim().is_empty() {
            return None;
        }
        Some(Passage { start, end, hash: puzil_render::text_index::digest(&units[start..end]) })
    }

    pub fn render(&self, rc: &RenderCtx) -> Div {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .w_full()
            .children(self.blocks.iter().enumerate().map(|(i, b)| {
                if i == 0 && self.reply.as_ref().is_some_and(|r| r.quote) && !rc.expanded.contains(&(rc.msg, QUOTE_KEY)) {
                    self.collapsed_quote(b, rc)
                } else {
                    self.block(b, rc)
                }
            }))
            .when(!self.attachments.is_empty(), |d| {
                d.child(
                    div()
                        .flex()
                        .flex_row()
                        .flex_wrap()
                        .gap_2()
                        .children(self.attachments.iter().map(|(name, href)| attachment_card(name, href, rc))),
                )
            })
    }

    /// A reply's quote of its parent, folded to one line; click to show it.
    fn collapsed_quote(&self, b: &BuiltBlock, rc: &RenderCtx) -> AnyElement {
        let t = &rc.theme;
        let first = first_para(b).map(|i| self.paras[i].text.to_string()).unwrap_or_default();
        let text: String = first.chars().filter(|c| !('\u{E000}'..='\u{E00F}').contains(c) && *c != '\u{2060}').collect();
        let (msg, toggle) = (rc.msg, rc.on_toggle.clone());
        div()
            .id(ElementId::named_usize(format!("quote-{msg}"), 0))
            .flex()
            .flex_row()
            .gap_2()
            .cursor_pointer()
            .text_sm()
            .text_color(t.muted)
            .child(div().w(px(3.)).flex_none().rounded_sm().bg(t.quote_bar))
            .child(
                div()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(format!("▸ {}", text.split_whitespace().collect::<Vec<_>>().join(" "))),
            )
            .on_click(move |_, w, cx| toggle(msg, QUOTE_KEY, w, cx))
            .into_any_element()
    }

    fn para(&self, ix: usize, rc: &RenderCtx) -> Div {
        let p = self.paras[ix].clone();
        let lh = p.line_height;
        div().w_full().min_w_0().line_height(px(lh)).child(
            RichText::new(p, rc.msg, ix, rc.selection.clone(), rc.theme.clone(), Some(rc.on_link.clone())).on_badge(
                rc.on_badge.clone().map(|h| {
                    let msg = rc.msg;
                    Rc::new(move |id, w: &mut Window, cx: &mut App| h(msg, id, w, cx)) as rich::BadgeHandler
                }),
            ),
        )
    }

    fn block(&self, b: &BuiltBlock, rc: &RenderCtx) -> AnyElement {
        let t = &rc.theme;
        match b {
            BuiltBlock::Para(i) => self.para(*i, rc).into_any_element(),
            BuiltBlock::Heading(l, i) => {
                let size = match l {
                    1 => 22.,
                    2 => 19.,
                    3 => 17.,
                    _ => 15.,
                };
                self.para(*i, rc).text_size(px(size)).line_height(px(size * 1.4)).pt_1().into_any_element()
            }
            BuiltBlock::Quote(bs) => div()
                .flex()
                .flex_row()
                .gap_2()
                .child(div().w(px(4.)).flex_none().rounded_sm().bg(t.quote_bar))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .flex_1()
                        .flex_basis(px(0.))
                        .min_w_0()
                        .children(bs.iter().map(|b| self.block(b, rc))),
                )
                .into_any_element(),
            BuiltBlock::Math(i, true) => {
                div().w_full().py_1().pl_4().overflow_hidden().child(self.para(*i, rc)).into_any_element()
            }
            BuiltBlock::Code(i) | BuiltBlock::Math(i, false) => div()
                .w_full()
                .rounded(px(4.))
                .bg(t.code_bg)
                .border_1()
                .border_color(t.border)
                .px_3()
                .py_2()
                .text_size(px(13.))
                .child(self.para(*i, rc).line_height(px(19.)))
                .into_any_element(),
            BuiltBlock::List { ordered, start, items } => div()
                .flex()
                .flex_col()
                .gap_0p5()
                .children(items.iter().enumerate().map(|(n, it)| {
                    let marker = if *ordered { format!("{}.", *start as usize + n) } else { "•".into() };
                    div()
                        .flex()
                        .flex_row()
                        .gap_1()
                        .child(
                            div()
                                .w(px(22.))
                                .flex_none()
                                .text_right()
                                .text_color(t.muted)
                                .line_height(px(rich::LINE_HEIGHT))
                                .child(marker),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_0p5()
                                .flex_1()
                                .flex_basis(px(0.))
                                .min_w_0()
                                .children(it.iter().map(|b| self.block(b, rc))),
                        )
                }))
                .into_any_element(),
            BuiltBlock::Table(rows) => div()
                .flex()
                .flex_col()
                .border_1()
                .border_color(t.border)
                .rounded_md()
                .children(rows.iter().map(|(header, cells)| {
                    div().flex().flex_row().border_b_1().border_color(t.border).when(*header, |d| d.bg(t.code_bg)).children(
                        cells
                            .iter()
                            .map(|c| div().flex_1().flex_basis(px(0.)).min_w_0().px_2().py_0p5().child(self.para(*c, rc))),
                    )
                }))
                .into_any_element(),
            BuiltBlock::Spoiler { key, header, body, tint } => {
                let open = rc.expanded.contains(&(rc.msg, *key));
                let (msg, key) = (rc.msg, *key);
                let toggle = rc.on_toggle.clone();
                let bar = match tint {
                    Some('🔴') => hsla(0.0, 0.7, 0.55, 1.),
                    Some('🟡') => hsla(0.13, 0.8, 0.5, 1.),
                    Some('🟢') => hsla(0.36, 0.6, 0.45, 1.),
                    Some(_) => t.muted,
                    None => t.quote_bar,
                };
                // Discord's embed: a tinted bar down the left of a raised card.
                div()
                    .flex()
                    .flex_col()
                    .max_w(px(720.))
                    .rounded(px(4.))
                    .bg(t.code_bg)
                    .border_l_4()
                    .border_color(bar)
                    .overflow_hidden()
                    .child(
                        div()
                            .id(ElementId::named_usize(format!("spoiler-{msg}"), key))
                            .flex()
                            .flex_row()
                            .gap_1p5()
                            .px_3()
                            .py_1p5()
                            .cursor_pointer()
                            .hover(|s| s.bg(t.bg_selected.opacity(0.35)))
                            .on_click(move |_, w, cx| toggle(msg, key, w, cx))
                            .child(div().h(px(rich::LINE_HEIGHT)).flex().items_center().child(crate::icons::icon(
                                if open { "chevron-down" } else { "chevron-right" },
                                16.,
                                t.muted,
                            )))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .flex_basis(px(0.))
                                    .min_w_0()
                                    .children(header.iter().map(|b| self.block(b, rc))),
                            ),
                    )
                    .when(open, |d| {
                        d.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .pl(px(34.))
                                .pr_3()
                                .pb_2()
                                .children(body.iter().map(|b| self.block(b, rc))),
                        )
                    })
                    .into_any_element()
            }
            BuiltBlock::Media(m) => media_block(m, rc),
            BuiltBlock::Rule => div().h(px(1.)).w_full().bg(t.border).my_1().into_any_element(),
        }
    }

    /// Copy text of the selected part of this message.
    pub fn copy(&self, msg: u64, sel: &Selection) -> String {
        let mut out = Vec::new();
        for (i, p) in self.paras.iter().enumerate() {
            if let Some(r) = sel.range_in(msg, i, p.text.len()) {
                out.push(rich::copy_text(p, r));
            }
        }
        out.join("\n")
    }
}

/// An image, video or YouTube thumbnail, sized before it loads so the list doesn't jump.
fn media_block(m: &Media, rc: &RenderCtx) -> AnyElement {
    let t = &rc.theme;
    let img = m.src.as_ref().and_then(|s| rc.images.get(s)).cloned();
    let natural = m.dims.or_else(|| {
        img.as_ref().map(|i| {
            let s = i.size(0);
            (s.width.0 as u32, s.height.0 as u32)
        })
    });
    let (w, h) = match natural {
        Some(d) => fit(d, MEDIA_MAX.0, MEDIA_MAX.1),
        None => MEDIA_UNKNOWN,
    };
    let video = m.kind != MediaKind::Image;
    let on_media = rc.on_media.clone();
    let media = m.clone();
    div()
        .id(ElementId::named_usize(format!("media-{}", rc.msg), m as *const Media as usize))
        .relative()
        .w(px(w))
        .h(px(h))
        .flex_none()
        .rounded_md()
        .overflow_hidden()
        .bg(t.code_bg)
        .border_1()
        .border_color(t.border)
        .cursor_pointer()
        .on_click(move |_, w, cx| on_media(&media, w, cx))
        .child(match img {
            Some(i) if crate::media::just_arrived(&i) => {
                let id = ElementId::Integer(Arc::as_ptr(&i) as u64);
                super::motion::arrive(id, img_el(i).size_full(), 0.).into_any_element()
            }
            Some(i) => img_el(i).size_full().into_any_element(),
            None => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(t.faint)
                .child(m.title.clone().unwrap_or_else(|| "image".into()))
                .into_any_element(),
        })
        .when(video, |d| {
            d.child(
                div().absolute().inset_0().flex().items_center().justify_center().child(
                    div()
                        .size(px(44.))
                        .rounded_full()
                        .bg(hsla(0., 0., 0., 0.6))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(white())
                        .child("▶"),
                ),
            )
        })
        .into_any_element()
}

fn img_el(i: Arc<RenderImage>) -> Img {
    img(ImageSource::Render(i)).object_fit(ObjectFit::Contain)
}

/// A card for an uploaded file: click to download it and open it with the system.
fn attachment_card(name: &str, href: &str, rc: &RenderCtx) -> AnyElement {
    let t = &rc.theme;
    let ext = href.rsplit('.').next().filter(|e| e.len() <= 5).unwrap_or("").to_ascii_uppercase();
    let busy = rc.downloading.contains(href);
    let on = rc.on_attachment.clone();
    let (n, h) = (name.to_string(), href.to_string());
    let tint = match ext.as_str() {
        "PDF" => hsla(0.0, 0.62, 0.55, 1.),
        "ZIP" | "TAR" | "GZ" => hsla(0.1, 0.7, 0.5, 1.),
        "PNG" | "JPG" | "JPEG" | "GIF" | "SVG" | "WEBP" => hsla(0.38, 0.5, 0.45, 1.),
        _ => t.accent,
    };
    div()
        .id(ElementId::named_usize(format!("file-{}", rc.msg), href.len() ^ name.len()))
        .flex()
        .flex_row()
        .items_center()
        .gap_3()
        .p_4()
        .w(px(400.))
        .max_w_full()
        .rounded(px(8.))
        .bg(t.sidebar)
        .border_1()
        .border_color(t.border)
        .cursor_pointer()
        .hover(|s| s.border_color(t.faint))
        .on_click(move |_, w, cx| on(&n, &h, w, cx))
        .child(
            div()
                .relative()
                .w(px(30.))
                .h(px(40.))
                .flex_none()
                .child(crate::icons::icon("file", 40., tint).absolute().left(px(-5.)).top_0())
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom(px(7.))
                        .text_center()
                        .text_size(px(7.))
                        .font_weight(FontWeight::BOLD)
                        .text_color(tint)
                        .child(ext.clone()),
                ),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .text_color(t.link)
                        .font_weight(FontWeight::MEDIUM)
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(name.to_string()),
                )
                .when(busy, |d| d.child(div().text_size(px(12.)).text_color(t.faint).child("Downloading…"))),
        )
        .child(crate::icons::icon("external-link", 20., t.icon))
        .into_any_element()
}

/// `b` without the trailing lines that only link uploaded files (Zulip puts each upload's
/// link on its own line), or `None` when nothing else is left.
fn without_file_links(b: &Block, files: &[(String, String)]) -> Option<Block> {
    use puzil_render::ast::Inline as I;
    let Block::Para(v) = b else {
        return Some(b.clone());
    };
    let is_file =
        |i: &I| matches!(i, I::Text { style, .. } if style.link.as_deref().is_some_and(|l| files.iter().any(|(_, h)| h == l)));
    let blank =
        |i: &I| matches!(i, I::Break) || matches!(i, I::Text { text, style } if style.link.is_none() && text.trim().is_empty());
    let mut end = v.len();
    let mut cut = end;
    // Walk back over blank runs and file links; stop at the first real content.
    while end > 0 {
        let i = &v[end - 1];
        if is_file(i) {
            end -= 1;
            cut = end;
        } else if blank(i) {
            end -= 1;
        } else {
            break;
        }
    }
    if cut == v.len() {
        return Some(b.clone());
    }
    let mut kept: Vec<I> = v[..cut].to_vec();
    while kept.last().is_some_and(blank) {
        kept.pop();
    }
    (!kept.is_empty()).then_some(Block::Para(kept))
}

fn first_para(b: &BuiltBlock) -> Option<usize> {
    match b {
        BuiltBlock::Para(i) | BuiltBlock::Heading(_, i) | BuiltBlock::Code(i) | BuiltBlock::Math(i, _) => Some(*i),
        BuiltBlock::Quote(bs) => bs.iter().find_map(first_para),
        BuiltBlock::List { items, .. } => items.iter().flatten().find_map(first_para),
        BuiltBlock::Spoiler { header, .. } => header.iter().find_map(first_para),
        BuiltBlock::Table(rows) => rows.first().and_then(|(_, c)| c.first().copied()),
        BuiltBlock::Media(_) | BuiltBlock::Rule => None,
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: that brings in gpui's own `test` attribute.
    use super::{Inline, MessageAst, Selection, Theme, build, rich::Pos};
    use puzil_render::text_index::{TextIndex, digest};

    #[test]
    fn inline_reactions_place_and_selections_map_back() {
        let html = concat!(
            "<p>Hello <strong>big</strong> world</p>\n",
            r#"<p>Hi <span class="emoji emoji-1f389" title="tada">:tada:</span> there</p>"#
        );
        let ti = TextIndex::from_html(html);
        let text = ti.to_string_lossy();
        let s = text.find("big world").unwrap();
        let r = puzil_overlay::InlineReaction {
            start: s,
            end: s + 9,
            hash: digest(&ti.text[s..s + 9]),
            emoji: "tada".into(),
            user_ids: vec![1, 2],
            mine: true,
        };
        let ast = MessageAst::from_html(html);
        let b = build(&ast, &Theme::light(), 1, None, None, Some(Inline { html, reactions: std::slice::from_ref(&r) }));
        let p = &b.paras[0];
        assert_eq!(&p.text[p.highlights[0].clone()], "big world");
        assert_eq!(b.badges, vec![r]);

        // Select "Hi 🎉" in the second paragraph: the emoji is `:tada:` in the index.
        let p1 = &b.paras[1];
        let end = p1.text.find(" there").unwrap();
        let sel = Selection {
            anchor: Some(Pos { msg: 1, para: 1, byte: 0 }),
            head: Some(Pos { msg: 1, para: 1, byte: end }),
            dragging: false,
        };
        let pg = b.passage(1, &sel).unwrap();
        assert_eq!(String::from_utf16(&ti.text[pg.start..pg.end]).unwrap(), "Hi :tada:");
        assert_eq!(pg.hash, digest(&ti.text[pg.start..pg.end]));
    }
}

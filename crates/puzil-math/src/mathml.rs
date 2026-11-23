//! KaTeX presentation MathML → Typst math source.
//!
//! KaTeX has already expanded every macro, so this only maps a small, closed set of
//! presentation elements. Leaves are escaped character by character so nothing in the
//! source is ever read as a Typst identifier, shorthand, or argument separator.

use roxmltree::{Document, Node};

pub fn convert(mathml: &str) -> Result<String, String> {
    let doc = Document::parse(mathml).map_err(|e| e.to_string())?;
    let mut out = String::new();
    node(doc.root_element(), &mut out)?;
    Ok(out)
}

/// Element children, minus invisible operators: KaTeX sometimes emits `<mo>\u{2061}</mo>`
/// (function application) as an extra child of msub/msubsup, which would shift arguments.
fn elems<'a, 'i>(n: Node<'a, 'i>) -> impl Iterator<Item = Node<'a, 'i>> {
    n.children().filter(|c| {
        c.is_element()
            && !(c.tag_name().name() == "mo"
                && text_of(*c).chars().all(|ch| ('\u{2061}'..='\u{2064}').contains(&ch)))
    })
}

fn text_of(n: Node) -> String {
    n.descendants().filter(|d| d.is_text()).filter_map(|d| d.text()).collect()
}

/// Escape one leaf so Typst reads it literally.
fn esc(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '\u{2061}' | '\u{2062}' | '\u{2063}' => {} // invisible function application/times
            // \| outside \left…\right comes through as <mi>∥</mi>: a norm bar, not "parallel".
            // Typst spaces a bare ‖ like a relation; KaTeX treats it as an ordinary symbol.
            '∥' | '‖' => out.push_str("class(\"normal\", \\‖)"),
            // Delimiters auto-pair in Typst math; escape them so pairing follows the MathML tree.
            '⟨' | '⟩' | '⌈' | '⌉' | '⌊' | '⌋' | '∣' | '⟦' | '⟧' => {
                out.push('\\');
                out.push(c);
            }
            c if c.is_alphanumeric() => out.push(c),
            ' ' | '\u{a0}' => out.push_str("space "),
            '\u{2009}' => out.push_str("thin "),
            '\u{2005}' => out.push_str("med "),
            '\u{2004}' => out.push_str("thick "),
            '\u{200a}' => out.push_str("#h(0.06em) "),
            c if c.is_ascii() => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
        out.push(' ');
    }
}

fn quote(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(if c == '\u{a0}' { ' ' } else { c });
    }
    out.push_str("\" ");
}

fn row(n: Node, out: &mut String) -> Result<(), String> {
    let kids: Vec<_> = elems(n).collect();
    // \left( … \right) and friends: a row bracketed by stretchy fences.
    let fence = |k: &Node| k.tag_name().name() == "mo" && k.attribute("fence") == Some("true");
    if kids.len() >= 2 && fence(&kids[0]) && fence(kids.last().unwrap()) {
        out.push_str("lr(");
        for k in &kids {
            node(*k, out)?;
        }
        out.push_str(") ");
        return Ok(());
    }
    for k in kids {
        node(k, out)?;
    }
    Ok(())
}

fn variant(v: Option<&str>) -> &'static [&'static str] {
    match v {
        Some("double-struck") => &["bb"],
        Some("script") => &["cal"],
        Some("fraktur") => &["frak"],
        Some("bold") => &["upright", "bold"],
        Some("bold-italic") => &["bold"],
        Some("sans-serif") => &["upright", "sans"],
        Some("monospace") => &["mono"],
        Some("normal") => &["upright"],
        Some("italic") => &["italic"],
        _ => &[],
    }
}

fn styled(v: Option<&str>, out: &mut String, inner: impl FnOnce(&mut String)) {
    let fs = variant(v);
    for f in fs {
        out.push_str(f);
        out.push('(');
    }
    inner(out);
    for _ in fs {
        out.push(')');
    }
    out.push(' ');
}

fn accent_fn(c: &str) -> Option<&'static str> {
    Some(match c {
        "^" | "ˆ" | "\u{302}" => "hat",
        "~" | "˜" | "\u{303}" => "tilde",
        "ˉ" | "¯" | "\u{304}" => "macron",
        "‾" | "\u{305}" => "overline",
        "→" | "\u{20d7}" => "arrow",
        "˙" | "\u{307}" => "dot",
        "¨" | "\u{308}" => "dot.double",
        "ˇ" | "\u{30c}" => "caron",
        "˘" | "\u{306}" => "breve",
        "´" | "\u{301}" => "acute",
        "`" | "\u{300}" => "grave",
        "⏞" => "overbrace",
        "⏟" => "underbrace",
        "⎴" => "overbracket",
        "⎵" => "underbracket",
        _ => return None,
    })
}

fn em(len: &str) -> Option<f32> {
    let len = len.trim();
    if let Some(v) = len.strip_suffix("em") {
        v.parse().ok()
    } else if let Some(v) = len.strip_suffix("ex") {
        v.parse::<f32>().ok().map(|x| x * 0.43)
    } else if let Some(v) = len.strip_suffix("mu") {
        v.parse::<f32>().ok().map(|x| x / 18.)
    } else {
        len.parse().ok()
    }
}

fn node(n: Node, out: &mut String) -> Result<(), String> {
    let kids: Vec<_> = elems(n).collect();
    let k = |i: usize| kids.get(i).copied().ok_or_else(|| format!("<{}> missing child {i}", n.tag_name().name()));
    match n.tag_name().name() {
        "math" | "mrow" | "mpadded" => row(n, out)?,
        "semantics" => node(k(0)?, out)?,
        "annotation" | "annotation-xml" => {}
        "mstyle" => {
            let mut inner = String::new();
            row(n, &mut inner)?;
            if inner.trim().is_empty() {
                return Ok(());
            }
            let level = n.attribute("scriptlevel");
            let display = n.attribute("displaystyle");
            let wrap = match (display, level) {
                (Some("true"), _) => Some("display"),
                (Some("false"), Some("0") | None) => Some("inline"),
                (_, Some("1")) => Some("script"),
                (_, Some("2")) => Some("sscript"),
                _ => None,
            };
            let color = n.attribute("mathcolor");
            if let Some(c) = color {
                let fill = if c.starts_with('#') {
                    format!("rgb(\"{c}\")")
                } else {
                    c.chars().filter(|c| c.is_ascii_alphabetic()).collect::<String>().to_lowercase()
                };
                out.push_str(&format!("#text(fill: {fill}, $"));
            }
            if let Some(w) = wrap {
                out.push_str(w);
                out.push('(');
            }
            out.push_str(&inner);
            if wrap.is_some() {
                out.push_str(") ");
            }
            if color.is_some() {
                out.push_str("$) ");
            }
        }
        "mi" => {
            let t = text_of(n);
            let v = n.attribute("mathvariant");
            if t.chars().count() > 1 && v.is_none() {
                out.push_str("op(");
                quote(&t, out);
                out.push_str(") ");
            } else {
                styled(v, out, |o| esc(&t, o));
            }
        }
        "mn" => styled(n.attribute("mathvariant"), out, |o| esc(&text_of(n), o)),
        "mo" => {
            let t = text_of(n);
            match t.as_str() {
                // A fence-attributed ∥ is a norm bar (\|), not the relation "parallel".
                "∑" | "∏" | "∫" | "∮" | "⋃" | "⋂" | "⨁" | "⨂" | "∐" | "⋁" | "⋀" => out.push_str(&format!("{t} ")),
                _ => esc(&t, out),
            }
        }
        "mtext" => {
            let t = text_of(n);
            if t.trim().is_empty() {
                for _ in t.chars() {
                    out.push_str("space ");
                }
            } else {
                styled(n.attribute("mathvariant"), out, |o| quote(&t, o));
            }
        }
        "ms" => quote(&text_of(n), out),
        "mspace" => {
            if n.attribute("linebreak") == Some("newline") {
                out.push_str("\\ ");
            } else if let Some(w) = n.attribute("width").and_then(em) {
                out.push_str(&format!("#h({w}em) "));
            }
        }
        "mfrac" => {
            if n.attribute("linethickness").and_then(em) == Some(0.) || n.attribute("linethickness") == Some("0px") {
                out.push_str("mat(delim: #none, ");
                node(k(0)?, out)?;
                out.push_str("; ");
                node(k(1)?, out)?;
                out.push_str(") ");
            } else {
                out.push_str("frac(");
                node(k(0)?, out)?;
                out.push_str(", ");
                node(k(1)?, out)?;
                out.push_str(") ");
            }
        }
        "msqrt" => {
            out.push_str("sqrt(");
            row(n, out)?;
            out.push_str(") ");
        }
        "mroot" => {
            out.push_str("root(");
            node(k(1)?, out)?;
            out.push_str(", ");
            node(k(0)?, out)?;
            out.push_str(") ");
        }
        "msub" | "msup" | "msubsup" => {
            let name = n.tag_name().name();
            out.push_str("attach(");
            node(k(0)?, out)?;
            if name != "msup" {
                out.push_str(", b: ");
                node(k(1)?, out)?;
            }
            if name != "msub" {
                out.push_str(", t: ");
                node(k(if name == "msup" { 1 } else { 2 })?, out)?;
            }
            out.push_str(") ");
        }
        "munder" | "mover" => {
            let over = n.tag_name().name() == "mover";
            let script = k(1)?;
            let st = text_of(script);
            let is_accent = n.attribute(if over { "accent" } else { "accentunder" }) == Some("true")
                || (script.tag_name().name() == "mo" && accent_fn(st.trim()).is_some());
            if is_accent && let Some(f) = accent_fn(st.trim()) {
                let f = match (f, over) {
                    ("overline", false) | ("macron", false) => "underline",
                    (f, _) => f,
                };
                out.push_str(f);
                out.push('(');
                node(k(0)?, out)?;
                out.push_str(") ");
            } else if is_accent {
                out.push_str("accent(");
                node(k(0)?, out)?;
                out.push_str(", \"");
                out.push_str(&st.replace('"', ""));
                out.push_str("\") ");
            } else {
                // \underbrace{x}_{label} nests: munder(munder(x, ⏟), label).
                let base = k(0)?;
                let brace = (base.tag_name().name() == "munder" || base.tag_name().name() == "mover")
                    && elems(base).nth(1).map(text_of).and_then(|t| accent_fn(t.trim())).is_some_and(|f| f.ends_with("brace") || f.ends_with("bracket"));
                if brace {
                    let f = accent_fn(text_of(elems(base).nth(1).unwrap()).trim()).unwrap();
                    out.push_str(f);
                    out.push('(');
                    node(elems(base).next().unwrap(), out)?;
                    out.push_str(", ");
                    node(script, out)?;
                    out.push_str(") ");
                } else {
                    out.push_str("attach(limits(");
                    node(base, out)?;
                    out.push_str(if over { "), t: " } else { "), b: " });
                    node(script, out)?;
                    out.push_str(") ");
                }
            }
        }
        "munderover" => {
            out.push_str("attach(limits(");
            node(k(0)?, out)?;
            out.push_str("), b: ");
            node(k(1)?, out)?;
            out.push_str(", t: ");
            node(k(2)?, out)?;
            out.push_str(") ");
        }
        "menclose" => {
            out.push_str("#box(stroke: 0.6pt, inset: (x: 3pt, y: 3pt), $");
            row(n, out)?;
            out.push_str("$) ");
        }
        "mphantom" => {
            out.push_str("#hide($");
            row(n, out)?;
            out.push_str("$) ");
        }
        "mtable" => table(n, out)?,
        "mtr" | "mtd" | "mlabeledtr" => row(n, out)?,
        "merror" => return Err("merror".into()),
        other => return Err(format!("unsupported <{other}>")),
    }
    Ok(())
}

fn table(n: Node, out: &mut String) -> Result<(), String> {
    let rows: Vec<_> = elems(n).filter(|r| r.tag_name().name().ends_with("tr")).collect();
    let align = n.attribute("columnalign").unwrap_or("");
    // aligned/align*: alternating right/left columns map onto Typst's own `&` alignment.
    if align.starts_with("right left") {
        for (ri, r) in rows.iter().enumerate() {
            if ri > 0 {
                out.push_str(" \\ ");
            }
            for (ci, c) in elems(*r).enumerate() {
                if ci > 0 {
                    out.push_str(" & ");
                }
                node(c, out)?;
            }
        }
        return Ok(());
    }
    let col_align = match align.split_whitespace().next() {
        Some("left") => ", align: #left",
        Some("right") => ", align: #right",
        _ => "",
    };
    out.push_str(&format!("mat(delim: #none{col_align}, "));
    for (ri, r) in rows.iter().enumerate() {
        if ri > 0 {
            out.push_str("; ");
        }
        for (ci, c) in elems(*r).enumerate() {
            if ci > 0 {
                out.push_str(", ");
            }
            node(c, out)?;
        }
    }
    out.push_str(") ");
    Ok(())
}

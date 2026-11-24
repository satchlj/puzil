//! Spelling: which words of a message are misspelled, suggestions for one, and the
//! correction autocorrect would make.
//!
//! On macOS this is the system spellchecker (its languages, its learned words, and its
//! "Correct spelling automatically" setting). Elsewhere it's a Hunspell dictionary: the
//! system's for the locale (`/usr/share/hunspell`) when there is one, else the bundled
//! en_US; learned words are kept in Puzil's data folder, and autocorrect makes only very
//! safe fixes (Settings can turn it off).
//!
//! Everything here runs on the main thread (the macOS checker wants that); the checker is
//! made on first use.

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    ops::Range,
};

thread_local! {
    static SPELLER: RefCell<Option<Speller>> = const { RefCell::new(None) };
    static AUTOCORRECT: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

/// Whether autocorrect is on where Puzil decides (not macOS, which has a system setting).
pub fn set_autocorrect(on: bool) {
    AUTOCORRECT.with(|a| a.set(on));
}

struct Speller {
    backend: Backend,
    /// Whether each word checked so far is spelled right.
    known: HashMap<String, bool>,
    /// "Ignore": right for the rest of this run.
    ignored: HashSet<String>,
}

enum Backend {
    #[cfg(target_os = "macos")]
    Mac { tag: isize },
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    Hunspell(Box<hun::Hun>),
}

fn with<R>(f: impl FnOnce(&mut Speller) -> R) -> R {
    SPELLER.with(|s| {
        let mut s = s.borrow_mut();
        let s = s.get_or_insert_with(|| Speller { backend: Backend::new(), known: HashMap::new(), ignored: HashSet::new() });
        f(s)
    })
}

impl Backend {
    #[cfg(target_os = "macos")]
    fn new() -> Self {
        Backend::Mac { tag: objc2_app_kit::NSSpellChecker::uniqueSpellDocumentTag() }
    }

    #[cfg(not(target_os = "macos"))]
    fn new() -> Self {
        Backend::Hunspell(Box::new(hun::Hun::load()))
    }

    fn check(&self, word: &str) -> bool {
        match self {
            #[cfg(target_os = "macos")]
            Backend::Mac { tag } => mac::check(word, *tag),
            Backend::Hunspell(h) => h.check(word),
        }
    }

    fn suggest(&self, word: &str) -> Vec<String> {
        match self {
            #[cfg(target_os = "macos")]
            Backend::Mac { tag } => mac::guesses(word, *tag),
            Backend::Hunspell(h) => h.suggest(word),
        }
    }

    fn correction(&self, word: &str) -> Option<String> {
        match self {
            #[cfg(target_os = "macos")]
            Backend::Mac { tag } => mac::correction(word, *tag),
            Backend::Hunspell(h) => {
                if !AUTOCORRECT.with(|a| a.get()) {
                    return None;
                }
                h.suggest(word).into_iter().next().filter(|s| safe_fix(word, s))
            }
        }
    }

    fn learn(&mut self, word: &str) {
        match self {
            #[cfg(target_os = "macos")]
            Backend::Mac { .. } => mac::learn(word),
            Backend::Hunspell(h) => h.learn(word),
        }
    }
}

/// Whether `word` is spelled right (or isn't checked: see [`words`]).
pub fn ok(word: &str) -> bool {
    with(|s| {
        if s.ignored.contains(word) {
            return true;
        }
        if let Some(k) = s.known.get(word) {
            return *k;
        }
        let k = s.backend.check(word);
        s.known.insert(word.to_string(), k);
        k
    })
}

/// The misspelled words of a message, as byte ranges.
pub fn mistakes(text: &str) -> Vec<Range<usize>> {
    words(text).into_iter().filter(|r| !ok(&text[r.clone()])).collect()
}

/// A few likely spellings, best first.
pub fn suggestions(word: &str) -> Vec<String> {
    let mut v = with(|s| s.backend.suggest(word));
    v.retain(|w| w != word);
    v.truncate(5);
    v
}

/// What autocorrect would change `word` to, if it's sure enough (and autocorrect is on).
pub fn correction(word: &str) -> Option<String> {
    if ok(word) {
        return None;
    }
    with(|s| s.backend.correction(word)).filter(|c| c != word)
}

/// Add to the dictionary for good.
pub fn learn(word: &str) {
    with(|s| {
        s.backend.learn(word);
        s.known.insert(word.to_string(), true);
    });
}

/// Treat as right until Puzil quits.
pub fn ignore(word: &str) {
    with(|s| {
        s.ignored.insert(word.to_string());
    });
}

/// A fix safe enough to make unasked: a lowercase word of four letters or more, one
/// letter added, dropped, changed, or two swapped.
fn safe_fix(word: &str, fix: &str) -> bool {
    let lower = |w: &str| w.chars().all(|c| !c.is_uppercase());
    word.chars().count() >= 4 && lower(word) && lower(fix) && !fix.contains(' ') && one_edit(word, fix)
}

fn one_edit(a: &str, b: &str) -> bool {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let pre = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suf = a[pre..].iter().rev().zip(b[pre..].iter().rev()).take_while(|(x, y)| x == y).count();
    let (ra, rb) = (&a[pre..a.len() - suf], &b[pre..b.len() - suf]);
    match (ra.len(), rb.len()) {
        (1, 1) | (0, 1) | (1, 0) => true,
        (2, 2) => ra[0] == rb[1] && ra[1] == rb[0],
        _ => false,
    }
}

/// The words worth checking, as byte ranges: runs of letters (with apostrophes inside),
/// leaving out code, math, links, email addresses, @mentions and #channel links, :emoji:,
/// single letters, ALLCAPS and camelCase, and anything touching a digit or underscore.
pub fn words(text: &str) -> Vec<Range<usize>> {
    let skip = skipped(text);
    let b = text.as_bytes();
    let mut out = Vec::new();
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut i = 0;
    let mut si = 0;
    while i < chars.len() {
        let (start, c) = chars[i];
        while si < skip.len() && skip[si].end <= start {
            si += 1;
        }
        if si < skip.len() && skip[si].start <= start {
            // Jump past the skipped region.
            let end = skip[si].end;
            while i < chars.len() && chars[i].0 < end {
                i += 1;
            }
            continue;
        }
        if !c.is_alphabetic() {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < chars.len() {
            let (_, c) = chars[j];
            let apostrophe = (c == '\'' || c == '’') && j + 1 < chars.len() && chars[j + 1].1.is_alphabetic() && j > i;
            if c.is_alphabetic() || apostrophe {
                j += 1;
            } else {
                break;
            }
        }
        let end = if j < chars.len() { chars[j].0 } else { text.len() };
        let before = start.checked_sub(1).map(|p| b[p]);
        let after = b.get(end).copied();
        let touches = |x: Option<u8>| x.is_some_and(|x| x.is_ascii_digit() || x == b'_');
        let w = &text[start..end];
        let n = j - i;
        let caps = w.chars().filter(|c| c.is_uppercase()).count();
        let camel = w.chars().skip(1).any(|c| c.is_uppercase());
        if n > 1 && !touches(before) && !touches(after) && !(caps == n && n > 1) && !camel {
            out.push(start..end);
        }
        i = j;
    }
    out
}

/// Regions of `text` that aren't prose, sorted.
fn skipped(text: &str) -> Vec<Range<usize>> {
    let mut v: Vec<Range<usize>> = Vec::new();
    // Fenced code blocks: from a ``` (or ~~~) line to the next.
    let mut off = 0;
    let mut fence: Option<usize> = None;
    for line in text.split_inclusive('\n') {
        let t = line.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            match fence.take() {
                Some(s) => v.push(s..off + line.len()),
                None => fence = Some(off),
            }
        }
        off += line.len();
    }
    if let Some(s) = fence {
        v.push(s..text.len());
    }
    // Inline code and math: `…`, $$…$$.
    for (open, close) in [("`", "`"), ("$$", "$$")] {
        let mut from = 0;
        while let Some(a) = text[from..].find(open).map(|p| p + from) {
            let Some(b) = text[a + open.len()..].find(close).map(|p| p + a + open.len()) else { break };
            v.push(a..b + close.len());
            from = b + close.len();
        }
    }
    // Mentions and channel links: @**…**, @_**…**, #**…**, @*group*.
    for open in ["@**", "@_**", "#**", "@*", "@_*"] {
        let close = if open.ends_with("**") { "**" } else { "*" };
        let mut from = 0;
        while let Some(a) = text[from..].find(open).map(|p| p + from) {
            let body = a + open.len();
            let b = text[body..].find(close).map(|p| p + body + close.len()).unwrap_or(text.len());
            v.push(a..b);
            from = b.max(body);
        }
    }
    // Whitespace-separated tokens that are links, email addresses or :emoji:.
    let mut start = None;
    for (i, c) in text.char_indices().chain([(text.len(), ' ')]) {
        if c.is_whitespace() {
            if let Some(s) = start.take() {
                let tok: &str = &text[s..i];
                let link = tok.contains("://") || tok.starts_with("www.") || tok.contains("](");
                let email = tok.contains('@') && tok.contains('.');
                let emoji = tok.len() > 2 && tok.starts_with(':') && tok.trim_end_matches(|c: char| c.is_ascii_punctuation() && c != ':').ends_with(':');
                if link || email || emoji {
                    // For a [text](url) link, only the (url) part.
                    let from = tok.find("](").map(|p| s + p + 1).filter(|_| !tok.contains("://") || tok.find("](") < tok.find("://")).unwrap_or(s);
                    v.push(from..i);
                }
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    v.sort_by_key(|r| (r.start, r.end));
    // Merge overlaps.
    let mut out: Vec<Range<usize>> = Vec::new();
    for r in v {
        match out.last_mut() {
            Some(l) if r.start <= l.end => l.end = l.end.max(r.end),
            _ => out.push(r),
        }
    }
    out
}

#[cfg(target_os = "macos")]
mod mac {
    use objc2_app_kit::NSSpellChecker;
    use objc2_foundation::{NSRange, NSString};

    fn range(s: &str) -> NSRange {
        NSRange::new(0, s.encode_utf16().count())
    }

    pub fn check(word: &str, tag: isize) -> bool {
        let sc = NSSpellChecker::sharedSpellChecker();
        let s = NSString::from_str(word);
        // SAFETY: a null word count is allowed.
        let r = unsafe { sc.checkSpellingOfString_startingAt_language_wrap_inSpellDocumentWithTag_wordCount(&s, 0, None, false, tag, std::ptr::null_mut()) };
        r.length == 0
    }

    pub fn guesses(word: &str, tag: isize) -> Vec<String> {
        let sc = NSSpellChecker::sharedSpellChecker();
        let s = NSString::from_str(word);
        sc.guessesForWordRange_inString_language_inSpellDocumentWithTag(range(word), &s, None, tag)
            .map(|a| a.iter().map(|w| w.to_string()).collect())
            .unwrap_or_default()
    }

    pub fn correction(word: &str, tag: isize) -> Option<String> {
        if !NSSpellChecker::isAutomaticSpellingCorrectionEnabled() {
            return None;
        }
        let sc = NSSpellChecker::sharedSpellChecker();
        let s = NSString::from_str(word);
        let lang = sc.language();
        sc.correctionForWordRange_inString_language_inSpellDocumentWithTag(range(word), &s, &lang, tag).map(|c| c.to_string())
    }

    pub fn learn(word: &str) {
        NSSpellChecker::sharedSpellChecker().learnWord(&NSString::from_str(word));
    }
}

/// Hunspell, for systems without a spellchecker Puzil can ask (Linux).
#[cfg_attr(target_os = "macos", allow(dead_code))]
mod hun {
    use std::path::PathBuf;

    pub struct Hun {
        dict: spellbook::Dictionary,
        learned: PathBuf,
    }

    pub(super) const AFF: &str = include_str!("../../../assets/dictionaries/en_US.aff");
    pub(super) const DIC: &str = include_str!("../../../assets/dictionaries/en_US.dic");

    impl Hun {
        pub fn load() -> Self {
            let dict = system_dictionary()
                .and_then(|(aff, dic)| spellbook::Dictionary::new(&aff, &dic).ok())
                .unwrap_or_else(|| spellbook::Dictionary::new(AFF, DIC).expect("bundled dictionary"));
            Self::with(dict, crate::accounts::data_dir().join("spelling-words.txt"))
        }

        pub fn with(mut dict: spellbook::Dictionary, learned: PathBuf) -> Self {
            for w in std::fs::read_to_string(&learned).unwrap_or_default().lines().filter(|l| !l.trim().is_empty()) {
                let _ = dict.add(w.trim());
            }
            Self { dict, learned }
        }

        pub fn check(&self, word: &str) -> bool {
            self.dict.check(&word.replace('’', "'"))
        }

        pub fn suggest(&self, word: &str) -> Vec<String> {
            let mut out = Vec::new();
            self.dict.suggest(&word.replace('’', "'"), &mut out);
            out
        }

        pub fn learn(&mut self, word: &str) {
            let _ = self.dict.add(word);
            let mut all = std::fs::read_to_string(&self.learned).unwrap_or_default();
            all.push_str(word);
            all.push('\n');
            if let Some(d) = self.learned.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let _ = std::fs::write(&self.learned, all);
        }
    }

    /// The installed Hunspell dictionary for the locale ($LC_ALL, $LC_MESSAGES, $LANG:
    /// `de_DE.UTF-8` tries de_DE, then any de_*).
    fn system_dictionary() -> Option<(String, String)> {
        let locale = ["LC_ALL", "LC_MESSAGES", "LANG"].iter().find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty() && v != "C" && v != "POSIX"))?;
        let name = locale.split(['.', '@']).next()?.to_string();
        let lang = name.split('_').next()?.to_string();
        let dirs = ["/usr/share/hunspell", "/usr/share/myspell/dicts", "/usr/share/myspell", "/app/share/hunspell"];
        for d in dirs {
            let read = |stem: &str| -> Option<(String, String)> {
                Some((std::fs::read_to_string(format!("{d}/{stem}.aff")).ok()?, std::fs::read_to_string(format!("{d}/{stem}.dic")).ok()?))
            };
            if let Some(x) = read(&name) {
                return Some(x);
            }
            let mut others: Vec<String> = std::fs::read_dir(d)
                .ok()?
                .filter_map(|e| e.ok()?.file_name().into_string().ok())
                .filter_map(|f| f.strip_suffix(".dic").map(str::to_string))
                .filter(|s| s.split('_').next() == Some(lang.as_str()))
                .collect();
            others.sort();
            if let Some(x) = others.first().and_then(|s| read(s)) {
                return Some(x);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws(t: &str) -> Vec<&str> {
        words(t).into_iter().map(|r| &t[r]).collect()
    }

    #[test]
    fn picks_prose_words() {
        assert_eq!(ws("I don’t think so"), ["don’t", "think", "so"]);
        assert_eq!(ws("see `code here` and $$x^2$$ ok"), ["see", "and", "ok"]);
        assert_eq!(ws("hi @**Ann Lee** in #**general>topic** now"), ["hi", "in", "now"]);
        assert_eq!(ws("go to https://example.com/foo or a@b.com :tada: yes"), ["go", "to", "or", "yes"]);
        assert_eq!(ws("NASA and camelCase and v2 and foo_bar"), ["and", "and", "and"]);
        assert_eq!(ws("[the link](http://x.org) done"), ["the", "link", "done"]);
        assert_eq!(ws("a\n```\nfn mian() {}\n```\nend"), ["end"]);
    }

    #[test]
    fn safe_fixes_only() {
        assert!(safe_fix("teh", "the") == false); // too short
        assert!(safe_fix("recieve", "receive"));
        assert!(safe_fix("mesage", "message"));
        assert!(!safe_fix("Smith", "Smit"));
        assert!(!safe_fix("abcdef", "abxyef"));
    }

    #[test]
    fn hunspell_checks_and_suggests() {
        let dict = spellbook::Dictionary::new(hun::AFF, hun::DIC).unwrap();
        let mut h = hun::Hun::with(dict, std::env::temp_dir().join("puzil-spell-test-none.txt"));
        assert!(h.check("received"));
        assert!(h.check("don’t"));
        assert!(!h.check("recieved"));
        assert_eq!(h.suggest("recieved").first().map(String::as_str), Some("received"));
        let _ = std::fs::remove_file(std::env::temp_dir().join("puzil-spell-test-none.txt"));
        h.learn("Puzil");
        assert!(h.check("Puzil"));
        let _ = std::fs::remove_file(std::env::temp_dir().join("puzil-spell-test-none.txt"));
    }
}

#[cfg(all(test, target_os = "macos"))]
mod mac_tests {
    #[test]
    fn system_checker_answers() {
        let tag = objc2_app_kit::NSSpellChecker::uniqueSpellDocumentTag();
        assert!(super::mac::check("received", tag));
        assert!(!super::mac::check("recieved", tag));
        assert!(super::mac::guesses("recieved", tag).iter().any(|g| g == "received"));
    }
}

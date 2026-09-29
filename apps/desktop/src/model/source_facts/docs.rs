//! What a module says about itself, and what each public name says: the
//! first paragraph of its own `//!` docs (or of the `///` on its `mod x;`
//! declaration in its parent), reduced to a sentence; and for every public
//! item its first source line and the first sentence of its `///`. Ports of
//! the board's `extract_more.py` (module docs) and `extract_trust.py`
//! (`scan_items_detail`), with the same rules.

use super::scan::{Literals, mask};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

const LEDE_MAX: usize = 240;
const DOC_FULL_MAX: usize = 400;
const SIG_MAX: usize = 140;
const DOC_MAX: usize = 200;
const ABBR: [&str; 9] = ["e.g", "i.e", "vs", "etc", "approx", "cf", "resp", "no", "inc"];
const STOP_HEADINGS: [&str; 34] = [
    "install", "installation", "installing", "usage", "getting started", "quick start", "quickstart", "license", "licence", "licensing",
    "contributing", "contribution", "contributions", "examples", "example", "minimum supported rust version", "msrv", "changelog",
    "documentation", "docs", "usage example", "usage examples", "basic usage", "example usage", "sponsors", "contributors",
    "acknowledgements", "acknowledgments", "development", "testing", "benchmarks", "errors", "panics", "safety",
];

/// A module's words: its first sentence and its first paragraph.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Doc {
    /// The first sentence.
    pub sentence: String,
    /// The first paragraph, cut at a sentence end near 400 characters.
    pub paragraph: String,
}

/// One public name read from source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Item {
    /// Its name.
    pub name: String,
    /// The keyword that declares it (`fn`, `struct`, `trait`...).
    pub keyword: &'static str,
    /// Its first source line, trimmed and cut.
    pub signature: String,
    /// Its line in its file, from one.
    pub line: usize,
    /// Its whole declaration up to the body (`{` or `;`), on one line and cut.
    pub declaration: String,
    /// The first sentence of its `///`.
    pub doc: Option<String>,
    /// The module (by key) that defines it, when it is public here only by
    /// a `pub use` (the usual shape of a library that keeps its modules private).
    pub from: Option<String>,
}

/// Whether a module can be reached by a path from outside its crate.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum Access {
    /// It can.
    #[default]
    Public,
    /// Declared without `pub` somewhere on the way down.
    Private,
}

/// One module's public names, and whether the module itself is private.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Module {
    /// Its path (`sync::mpsc`, `lib`).
    pub path: String,
    /// Whether it is declared without `pub` somewhere on the way down.
    pub access: Access,
    /// Its public names.
    pub items: Vec<Item>,
}

/// Markdown inline read as plain words: links as their text, images gone,
/// code spans kept, emphasis marks dropped.
#[must_use]
pub fn clean_inline(text: &str) -> String {
    // Links reduced to their text, code spans kept, emphasis marks dropped.
    let mut out = String::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '`' {
            let end = chars[i + 1..].iter().position(|x| *x == '`').map(|p| i + 1 + p);
            if let Some(end) = end {
                out.extend(&chars[i..=end]);
                i = end + 1;
                continue;
            }
        }
        if c == '!' && chars.get(i + 1) == Some(&'[') {
            // An image is no words: `![alt](src)` is gone.
            if let Some(close) = chars[i + 2..].iter().position(|x| *x == ']').map(|p| i + 2 + p) {
                let mut end = close + 1;
                if chars.get(end) == Some(&'(')
                    && let Some(p) = chars[end..].iter().position(|x| *x == ')')
                {
                    end += p + 1;
                }
                i = end;
                continue;
            }
        }
        if c == '[' {
            // [text](url) and [text][ref] read as text; [text] as text.
            if let Some(close) = chars[i + 1..].iter().position(|x| *x == ']').map(|p| i + 1 + p) {
                let label: String = chars[i + 1..close].iter().collect();
                let mut end = close + 1;
                if chars.get(end) == Some(&'(') {
                    if let Some(p) = chars[end..].iter().position(|x| *x == ')') {
                        end += p + 1;
                    }
                } else if chars.get(end) == Some(&'[')
                    && let Some(p) = chars[end..].iter().position(|x| *x == ']')
                {
                    end += p + 1;
                }
                out.push_str(&label);
                i = end;
                continue;
            }
        }
        if c == '*' {
            i += 1;
            continue;
        }
        if c == '<' {
            // Inline HTML tags and autolinks.
            if let Some(p) = chars[i..].iter().position(|x| *x == '>')
                && chars.get(i + 1).is_some_and(|n| n.is_alphabetic() || *n == '/')
            {
                i += p + 1;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn letters(text: &str) -> usize {
    text.chars().filter(|c| c.is_ascii_alphabetic()).count()
}

fn list_start(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with('|')
        || t.starts_with("- ")
        || t.starts_with("* ")
        || t.starts_with("+ ")
        || t.split_once(['.', ')']).is_some_and(|(n, rest)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) && rest.starts_with(' '))
        || (t.starts_with('[') && t.contains("]:"))
}

/// A row of links with no words of its own (badges, a nav bar).
#[must_use]
pub fn nav_row(block: &str) -> bool {
    let links = block.matches("](").count() + block.matches("][").count();
    let chars: Vec<char> = block.chars().collect();
    let mut words = String::new();
    let mut depth = 0;
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '[' => depth += 1,
            ']' if depth > 0 => {
                depth -= 1;
                // The `(target)` of a link is never a word: a badge inside a
                // link (`[![alt](img)](href)`) has two of them.
                if chars.get(i + 1) == Some(&'(')
                    && let Some(close) = chars[i + 1..].iter().position(|c| *c == ')')
                {
                    i += close + 1;
                }
            }
            c if depth == 0 => words.push(c),
            _ => {}
        }
        i += 1;
    }
    links >= 2 && letters(&words) < 3
}

/// The cleaned first prose paragraph of a markdown text.
#[must_use]
pub fn first_paragraph(text: &str) -> Option<String> {
    let mut blocks: Vec<(bool, String)> = Vec::new(); // (heading, text)
    let mut current: Vec<String> = Vec::new();
    let mut fence: Option<String> = None;
    let flush = |current: &mut Vec<String>, blocks: &mut Vec<(bool, String)>| {
        if !current.is_empty() {
            blocks.push((false, current.join(" ")));
            current.clear();
        }
    };
    for line in text.lines() {
        let st = line.trim();
        if let Some(mark) = &fence {
            if st.starts_with(mark.as_str()) {
                fence = None;
            }
            continue;
        }
        if st.starts_with("```") || st.starts_with("~~~") {
            flush(&mut current, &mut blocks);
            fence = Some(st.chars().take(3).collect());
            continue;
        }
        if st.is_empty() {
            flush(&mut current, &mut blocks);
            continue;
        }
        if st.starts_with('#') && st.trim_start_matches('#').starts_with(' ') || st == "#" {
            flush(&mut current, &mut blocks);
            blocks.push((true, st.trim_start_matches('#').trim().to_owned()));
            continue;
        }
        if st.len() >= 3 && st.chars().all(|c| c == '=' || c == '-') && !current.is_empty() {
            blocks.push((true, current.join(" ")));
            current.clear();
            continue;
        }
        if line.starts_with("    ") && current.is_empty() {
            continue;
        }
        if !current.is_empty() && list_start(st) {
            flush(&mut current, &mut blocks);
        }
        current.push(st.to_owned());
    }
    flush(&mut current, &mut blocks);
    let mut title: Option<String> = None;
    for (heading, raw) in blocks {
        if heading {
            let cleaned = clean_inline(&raw);
            let key = cleaned.to_lowercase();
            if STOP_HEADINGS.contains(&key.trim_matches([' ', ':', '#']).trim()) {
                return title;
            }
            if title.is_none() && letters(&cleaned) >= 2 {
                title = Some(cleaned);
            }
            continue;
        }
        if list_start(&raw) || nav_row(&raw) {
            continue;
        }
        let cleaned = clean_inline(&raw);
        if letters(&cleaned) >= 2 {
            return Some(cleaned);
        }
    }
    title
}

/// The first sentence of `text`, links and backticks dropped.
#[must_use]
pub fn first_sentence(text: &str) -> Option<String> {
    let cleaned: String = {
        // `[text](url)` to `text`, backticks off.
        let no_links = clean_inline(text);
        no_links.replace('`', "")
    };
    let t = cleaned.trim();
    if t.is_empty() {
        return None;
    }
    let bytes = t.as_bytes();
    let mut end = t.len();
    for (at, c) in t.char_indices() {
        if !matches!(c, '.' | '!' | '?') {
            continue;
        }
        let after = bytes.get(at + 1);
        if after.is_some_and(|b| !b.is_ascii_whitespace()) {
            continue;
        }
        let last_word = t[..at].rsplit(' ').next().unwrap_or("").to_lowercase();
        let last_word = last_word.trim_start_matches('(');
        if c == '.' && (ABBR.contains(&last_word) || (last_word.len() == 1 && last_word.chars().all(char::is_alphabetic))) {
            continue;
        }
        let rest = t[at + 1..].trim_start();
        if let Some(next) = rest.chars().next()
            && !(next.is_uppercase() || next.is_ascii_digit() || "\"'([*_".contains(next))
        {
            continue;
        }
        end = if c == '.' { at } else { at + 1 };
        break;
    }
    let mut out = t[..end].trim().to_owned();
    if end == t.len() && out.ends_with('.') && !out.ends_with("..") {
        out.pop();
    }
    if out.chars().count() > LEDE_MAX {
        let cut: String = out.chars().take(LEDE_MAX - 1).collect();
        out = format!("{}…", cut.rsplit_once(' ').map_or(cut.as_str(), |(head, _)| head).trim_end_matches([',', ';', ':']));
    }
    (!out.is_empty()).then_some(out)
}

/// The paragraph cut to about `cap` characters at a sentence or word end.
fn paragraph_cut(para: &str, cap: usize) -> String {
    if para.chars().count() <= cap {
        return para.to_owned();
    }
    let cut: String = para.chars().take(cap).collect();
    let stop = [". ", "! ", "? "].iter().filter_map(|m| cut.rfind(m)).max();
    match stop {
        Some(at) if at * 10 > cap * 4 => cut[..=at].to_owned(),
        _ => format!("{}…", cut.rsplit_once(' ').map_or(cut.as_str(), |(head, _)| head).trim_end_matches([',', ';', ':'])),
    }
}

fn doc_of(paragraph: Option<String>) -> Option<Doc> {
    let paragraph = paragraph?;
    Some(Doc { sentence: first_sentence(&paragraph)?, paragraph: paragraph_cut(&paragraph, DOC_FULL_MAX) })
}

/// The `//!` lines at the top of a file (and `#![doc = "…"]`, and
/// `#![doc = include_str!("…")]`, in source order), as one markdown text.
fn inner_doc(text: &str, dir: &Path) -> Option<String> {
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        let s = line.trim();
        if let Some(body) = s.strip_prefix("//!") {
            out.push(body.strip_prefix(' ').unwrap_or(body).to_owned());
        } else if s.starts_with("#![doc") {
            if let Some(start) = s.find("include_str!(\"") {
                let rest = &s[start + 14..];
                if let Some(end) = rest.find('"')
                    && let Ok(included) = fs::read_to_string(dir.join(&rest[..end]))
                {
                    out.push(included);
                }
            } else if let Some(q) = s.find('"')
                && let Some(end) = s.rfind('"')
                && end > q
            {
                out.push(s[q + 1..end].replace("\\n", "\n"));
            }
        } else if s.is_empty() || s.starts_with("//") && !s.starts_with("///") || s.starts_with("#![") {
            continue;
        } else {
            break;
        }
    }
    (!out.is_empty()).then(|| out.join("\n"))
}

/// The `///` lines directly above line `at` (attributes may sit between),
/// in source order.
fn outer_doc(lines: &[&str], at: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = at;
    while i > 0 {
        let s = lines[i - 1].trim();
        if s.starts_with("///") && !s.starts_with("////") {
            let body = &s[3..];
            out.push(body.strip_prefix(' ').unwrap_or(body).to_owned());
            i -= 1;
        } else if s.starts_with("#[") || s.starts_with("#![") {
            i -= 1;
        } else if s.ends_with(']') && !s.starts_with("//") {
            // The tail of a multi-line attribute.
            let mut j = i - 1;
            while j > 0 && i - j <= 20 && !lines[j - 1].trim().starts_with("#[") {
                j -= 1;
            }
            if j > 0 && lines[j - 1].trim().starts_with("#[") {
                i = j - 1;
            } else {
                break;
            }
        } else {
            break;
        }
    }
    out.reverse();
    out
}

/// The first sentence of the `///` above line `at`.
fn doc_sentence(lines: &[&str], at: usize) -> Option<String> {
    let doc = outer_doc(lines, at);
    let mut paragraphs: Vec<Vec<&str>> = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for line in &doc {
        if line.trim().is_empty() {
            if !current.is_empty() {
                paragraphs.push(std::mem::take(&mut current));
            }
        } else {
            current.push(line.trim());
        }
    }
    if !current.is_empty() {
        paragraphs.push(current);
    }
    for paragraph in paragraphs {
        let first = paragraph[0];
        if first.starts_with('#') || first.starts_with("```") || first.starts_with("~~~") {
            continue;
        }
        let mut sentence = first_sentence(&paragraph.join(" "))?;
        if sentence.chars().count() > DOC_MAX {
            let cut: String = sentence.chars().take(DOC_MAX - 1).collect();
            sentence = format!("{}…", cut.rsplit_once(' ').map_or(cut.as_str(), |(head, _)| head).trim_end_matches([',', ';', ':']));
        }
        return Some(sentence);
    }
    None
}

const TESTISH: [&str; 11] = ["tests", "test", "benches", "bench", "examples", "example", "fixtures", "fixture", "testdata", "corpus", "target"];

fn is_test_file(stem: &str) -> bool {
    stem == "tests" || stem == "test" || stem.ends_with("_tests") || stem.ends_with("_test")
}

/// The module key of a file relative to the library root: `lib`, or its
/// first two path segments (`sync::mpsc`), a `mod.rs` standing for its folder.
fn module_name(rel: &[String]) -> String {
    let mut parts: Vec<String> = rel.to_vec();
    if let Some(last) = parts.last_mut() {
        *last = last.strip_suffix(".rs").unwrap_or(last).to_owned();
    }
    if parts.len() > 1 && parts.last().is_some_and(|p| p == "mod") {
        parts.pop();
    }
    if parts == ["lib"] {
        return "lib".to_owned();
    }
    parts.iter().take(2).cloned().collect::<Vec<_>>().join("::")
}

/// The module path of a file as its full segments; empty for the crate root.
fn full_module_path(rel: &[String]) -> Vec<String> {
    let mut parts: Vec<String> = rel.to_vec();
    if let Some(last) = parts.last_mut() {
        *last = last.strip_suffix(".rs").unwrap_or(last).to_owned();
    }
    if parts.len() > 1 && parts.last().is_some_and(|p| p == "mod") {
        parts.pop();
    }
    if parts == ["lib"] || parts == ["main"] {
        return Vec::new();
    }
    parts
}

fn ident_at(code: &[u8], mut i: usize) -> (usize, &str) {
    let start = i;
    while i < code.len() && (code[i].is_ascii_alphanumeric() || code[i] == b'_') {
        i += 1;
    }
    (i, std::str::from_utf8(&code[start..i]).unwrap_or(""))
}

fn skip_blank(code: &[u8], mut i: usize) -> usize {
    while i < code.len() && (code[i] == b' ' || code[i] == b'\t') {
        i += 1;
    }
    i
}

const KEYWORDS: [(&str, &str); 10] = [
    ("fn", "fn"),
    ("struct", "struct"),
    ("enum", "enum"),
    ("trait", "trait"),
    ("type", "type"),
    ("const", "const"),
    ("static", "static"),
    ("union", "union"),
    ("mod", "mod"),
    ("macro_rules!", "macro_rules!"),
];

/// Matches `pub QUAL* KEYWORD [mut] [r#]name?` at the start of a line;
/// returns (keyword, name, end).
fn item_at(code: &[u8], at: usize) -> Option<(&'static str, String, usize)> {
    let mut i = skip_blank(code, at);
    if !code[i..].starts_with(b"pub") {
        return None;
    }
    i += 3;
    let after_pub = skip_blank(code, i);
    if after_pub == i {
        return None;
    }
    i = after_pub;
    // Qualifiers, greedy with backtracking to the keyword.
    let mut stops = vec![i];
    let mut j = i;
    loop {
        let word_end = ident_at(code, j).0;
        let word = std::str::from_utf8(&code[j..word_end]).unwrap_or("");
        if matches!(word, "async" | "unsafe" | "const" | "safe" | "default") {
            let next = skip_blank(code, word_end);
            if next == word_end {
                break;
            }
            j = next;
            stops.push(j);
        } else if word == "extern" {
            let mut next = skip_blank(code, word_end);
            if next == word_end {
                break;
            }
            if code.get(next) == Some(&b'"') {
                let close = code[next + 1..].iter().position(|c| *c == b'"').map(|p| next + 1 + p + 1)?;
                next = skip_blank(code, close);
                if next == close {
                    break;
                }
            }
            j = next;
            stops.push(j);
        } else {
            break;
        }
    }
    for start in stops.into_iter().rev() {
        for (text, keyword) in KEYWORDS {
            if !code[start..].starts_with(text.as_bytes()) {
                continue;
            }
            let end = start + text.len();
            if code.get(end).is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_') {
                continue;
            }
            let mut k = skip_blank(code, end);
            if code[k..].starts_with(b"mut") && code.get(k + 3).is_some_and(|b| *b == b' ' || *b == b'\t') {
                k = skip_blank(code, k + 3);
            }
            if code[k..].starts_with(b"r#") {
                k += 2;
            }
            let (name_end, name) = ident_at(code, k);
            let name = if name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') { name.to_owned() } else { String::new() };
            let end = if name.is_empty() { k } else { name_end };
            return Some((keyword, name, end));
        }
    }
    None
}

/// Every `mod x;` declaration in a file: `(name, public, line)`.
fn mod_decls(code: &str) -> Vec<(String, bool, usize)> {
    let mut out = Vec::new();
    for (line_no, line) in code.lines().enumerate() {
        let mut t = line.trim_start();
        let mut public = false;
        if let Some(rest) = t.strip_prefix("pub") {
            let rest = rest.trim_start();
            public = true;
            t = if rest.starts_with('(') { rest.find(')').map_or(rest, |c| rest[c + 1..].trim_start()) } else { rest };
        } else if t.starts_with("#[") {
            // Attributes on the same line: skip them.
            let mut rest = t;
            while rest.starts_with("#[") {
                match rest.find(']') {
                    Some(c) => rest = rest[c + 1..].trim_start(),
                    None => break,
                }
            }
            t = rest;
            if let Some(rest) = t.strip_prefix("pub") {
                let rest = rest.trim_start();
                public = true;
                t = if rest.starts_with('(') { rest.find(')').map_or(rest, |c| rest[c + 1..].trim_start()) } else { rest };
            }
        }
        let t = t.strip_prefix("unsafe ").unwrap_or(t);
        if let Some(rest) = t.strip_prefix("mod ") {
            let rest = rest.trim_start().trim_start_matches("r#");
            let (end, name) = ident_at(rest.as_bytes(), 0);
            if rest[end..].trim_start().starts_with(';') && !name.is_empty() {
                out.push((name.to_owned(), public, line_no));
            }
        }
    }
    out
}

fn walk(dir: &Path, root: &Path, skip_nested: bool, out: &mut Vec<(PathBuf, Vec<String>)>) {
    let Ok(read) = fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = read.filter_map(Result::ok).collect();
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            if name.starts_with('.') || TESTISH.contains(&name.to_lowercase().as_str()) || (skip_nested && path.join("Cargo.toml").exists()) || name == "node_modules" {
                continue;
            }
            walk(&path, root, skip_nested, out);
        } else if kind.is_file() && name.ends_with(".rs") && name != "build.rs" {
            let stem = name.trim_end_matches(".rs");
            if is_test_file(stem) {
                continue;
            }
            let rel = path.strip_prefix(root).ok().map(|r| r.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect()).unwrap_or_default();
            out.push((path, rel));
        }
    }
}

/// The public names of the crate whose library root directory is `root`,
/// by module, largest first.
#[must_use]
pub fn items(root: &Path, crate_dir: &Path) -> Vec<Module> {
    if !root.is_dir() {
        return Vec::new();
    }
    let mut files = Vec::new();
    walk(root, root, root == crate_dir, &mut files);
    let mut modules: Vec<Module> = Vec::new();
    let mut seen: Vec<(String, &'static str, String)> = Vec::new();
    let mut declared: HashMap<Vec<String>, HashMap<String, bool>> = HashMap::new();
    let mut contributed: Vec<(String, Vec<String>)> = Vec::new();
    let mut uses: Vec<(String, Vec<String>, Vec<(Vec<String>, String)>)> = Vec::new();
    for (path, rel) in files {
        let Ok(bytes) = fs::read(&path) else { continue };
        let text = String::from_utf8_lossy(&bytes).into_owned();
        if !text.contains("pub") && !text.contains("macro_export") {
            continue;
        }
        let code = mask(&text, Literals::Blank);
        let module = module_name(&rel);
        let full = full_module_path(&rel);
        for (name, public, _) in mod_decls(&code) {
            let entry = declared.entry(full.clone()).or_default().entry(name).or_insert(false);
            *entry = *entry || public;
        }
        let file_uses = pub_uses(&code);
        if !file_uses.is_empty() {
            uses.push((module.clone(), full.clone(), file_uses));
        }
        let before = seen.len();
        let lines: Vec<&str> = text.split('\n').collect();
        let bytes = code.as_bytes();
        // Newline offsets of the masked text (same offsets as the source).
        let newlines: Vec<usize> = bytes.iter().enumerate().filter(|(_, b)| **b == b'\n').map(|(i, _)| i).collect();
        let mut stack: Vec<&str> = Vec::new();
        let mut bad = 0usize;
        let mut boundary: Option<usize> = None;
        let mut found: Vec<Item> = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            if matches!(b, b'{' | b'}' | b';') {
                if b == b'{' {
                    let kind = if bad > 0 {
                        "other"
                    } else {
                        let raw = &code[boundary.map_or(0, |at| at + 1)..i];
                        let header = strip_attributes(raw);
                        if let Some(name) = mod_header(header.trim()) {
                            if raw.replace(' ', "").contains("cfg(test)") || name == "tests" || name == "test" { "test" } else { "mod" }
                        } else if extern_header(header.trim()) {
                            "extern"
                        } else {
                            "other"
                        }
                    };
                    stack.push(kind);
                    if matches!(kind, "other" | "test") {
                        bad += 1;
                    }
                } else if b == b'}'
                    && let Some(kind) = stack.pop()
                    && matches!(kind, "other" | "test")
                {
                    bad -= 1;
                }
                boundary = Some(i);
                i += 1;
                continue;
            }
            if (i == 0 || bytes[i - 1] == b'\n') && let Some((keyword, name, end)) = item_at(bytes, i) {
                if bad == 0 && keyword != "mod" && !name.is_empty() {
                    let key = (module.clone(), keyword, name.clone());
                    if !seen.contains(&key) {
                        seen.push(key);
                        let line = newlines.partition_point(|n| *n < i);
                        found.push(Item {
                            name,
                            keyword,
                            signature: signature(lines.get(line).copied().unwrap_or("")),
                            line: line + 1,
                            declaration: declaration(bytes, i),
                            doc: doc_sentence(&lines, line),
                            from: None,
                        });
                    }
                }
                i = end.max(i + 1);
                continue;
            }
            i += 1;
        }
        // `#[macro_export] macro_rules! name` anywhere in the file.
        let mut from = 0;
        while let Some(at) = code[from..].find("#[macro_export") {
            let start = from + at;
            let rest = &code[start..];
            if let Some(m) = rest.find("macro_rules!") {
                let between = &rest[..m];
                if between.matches("#[").count() >= 1 && !between.contains(';') && !between.contains('{') {
                    let (_, name) = ident_at(rest.as_bytes(), skip_blank(rest.as_bytes(), m + 12).max(m + 12));
                    let name_at = rest[m + 12..].trim_start();
                    let (_, name2) = ident_at(name_at.as_bytes(), 0);
                    let name = if name2.is_empty() { name } else { name2 };
                    let key = (module.clone(), "macro_rules!", name.to_owned());
                    if !name.is_empty() && !seen.contains(&key) {
                        seen.push(key);
                        let line = newlines.partition_point(|n| *n < start + m);
                        found.push(Item {
                            name: name.to_owned(),
                            keyword: "macro_rules!",
                            signature: signature(lines.get(line).copied().unwrap_or("")),
                            line: line + 1,
                            declaration: signature(lines.get(line).copied().unwrap_or("")),
                            doc: doc_sentence(&lines, line),
                            from: None,
                        });
                    }
                }
            }
            from = start + 14;
        }
        if !found.is_empty() {
            match modules.iter_mut().find(|m| m.path == module) {
                Some(m) => m.items.extend(found),
                None => modules.push(Module { path: module.clone(), access: Access::Public, items: found }),
            }
        }
        if seen.len() > before {
            contributed.push((module.clone(), full));
        }
    }
    let private = |path: &[String]| -> bool {
        (1..=path.len()).any(|i| declared.get(&path[..i - 1]).and_then(|d| d.get(&path[i - 1])).is_some_and(|public| !public))
    };
    // What a `pub use` makes public: a name defined in a module nobody can
    // reach by path is public where it is re-exported.
    let mut reexporting: Vec<(String, Vec<String>)> = Vec::new();
    for (module, full, leaves) in &uses {
        if private(full) {
            continue;
        }
        let mut added: Vec<Item> = Vec::new();
        for (path, exported) in leaves {
            let Some((target, name)) = path.split_last().map(|(name, rest)| (rest.to_vec(), name.clone())) else { continue };
            let Some(target) = resolve_module(&target, full, &declared) else { continue };
            if !private(&target) {
                continue;
            }
            let key = module_key(&target);
            let Some(defined) = modules.iter().find(|m| m.path == key) else { continue };
            let pick: Vec<&Item> = if name == "*" { defined.items.iter().collect() } else { defined.items.iter().filter(|i| i.name == name).collect() };
            for item in pick {
                let shown = if name == "*" { item.name.clone() } else { exported.clone() };
                if shown == "_" {
                    continue;
                }
                added.push(Item { name: shown, from: Some(key.clone()), ..item.clone() });
            }
        }
        if added.is_empty() {
            continue;
        }
        let at = match modules.iter().position(|m| m.path == *module) {
            Some(at) => at,
            None => {
                modules.push(Module { path: module.clone(), access: Access::Public, items: Vec::new() });
                modules.len() - 1
            }
        };
        for item in added {
            if !modules[at].items.iter().any(|have| have.name == item.name) {
                modules[at].items.push(item);
            }
        }
        reexporting.push((module.clone(), full.clone()));
    }
    for module in &mut modules {
        let hidden = contributed.iter().chain(reexporting.iter()).filter(|(m, _)| *m == module.path).all(|(_, full)| private(full));
        module.access = if hidden { Access::Private } else { Access::Public };
    }
    modules.sort_by(|a, b| b.items.len().cmp(&a.items.len()).then_with(|| a.path.cmp(&b.path)));
    modules
}

/// The key of a module by its full path: `lib` for the root, else its first
/// two segments (`sync::mpsc`), as [`module_name`] keys a file.
fn module_key(path: &[String]) -> String {
    if path.is_empty() { "lib".to_owned() } else { path.iter().take(2).cloned().collect::<Vec<_>>().join("::") }
}

/// The module a `pub use` path names, as full segments, when it is one of
/// this crate's: `crate::a::b`, `self::a`, `super::a`, or a child of the
/// module the statement sits in (`a::b`). Paths into other crates are `None`.
fn resolve_module(path: &[String], here: &[String], declared: &HashMap<Vec<String>, HashMap<String, bool>>) -> Option<Vec<String>> {
    let mut out: Vec<String>;
    let mut rest = path;
    match path.first().map(String::as_str) {
        Some("crate") => {
            out = Vec::new();
            rest = &path[1..];
        }
        Some("self") => {
            out = here.to_vec();
            rest = &path[1..];
        }
        Some("super") => {
            out = here.to_vec();
            while rest.first().is_some_and(|s| s == "super") {
                out.pop();
                rest = &rest[1..];
            }
        }
        Some(first) => {
            // A child of this module, or (edition 2015) of the root.
            if declared.get(here).is_some_and(|d| d.contains_key(first)) {
                out = here.to_vec();
            } else if declared.get(&Vec::new()).is_some_and(|d| d.contains_key(first)) && here.is_empty() {
                out = Vec::new();
            } else {
                return None;
            }
        }
        None => return Some(here.to_vec()),
    }
    out.extend(rest.iter().cloned());
    Some(out)
}

/// Every `pub use` in a file (masked code), flattened to leaves: the path
/// to the name (or to `*`) and the name it is exported as.
fn pub_uses(code: &str) -> Vec<(Vec<String>, String)> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = code[from..].find("pub use ") {
        let start = from + at;
        let line_start = code[..start].rfind('\n').map_or(0, |n| n + 1);
        if code[line_start..start].trim().is_empty()
            && let Some(end) = code[start..].find(';')
        {
            use_tree(&code[start + 8..start + end], &[], &mut out);
            from = start + end;
            continue;
        }
        from = start + 8;
    }
    out
}

fn use_tree(tree: &str, prefix: &[String], out: &mut Vec<(Vec<String>, String)>) {
    let tree = tree.trim();
    let segments = |text: &str| -> Vec<String> { text.split("::").map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned).collect() };
    if let Some(open) = tree.find('{') {
        let close = tree.rfind('}').unwrap_or(tree.len());
        let mut path = prefix.to_vec();
        path.extend(segments(&tree[..open]));
        let inner = &tree[open + 1..close.max(open + 1)];
        let (mut depth, mut from) = (0_i32, 0);
        for (at, c) in inner.char_indices() {
            match c {
                '{' => depth += 1,
                '}' => depth -= 1,
                ',' if depth == 0 => {
                    use_tree(&inner[from..at], &path, out);
                    from = at + 1;
                }
                _ => {}
            }
        }
        use_tree(&inner[from..], &path, out);
        return;
    }
    let (text, alias) = match tree.split_once(" as ") {
        Some((path, alias)) => (path, Some(alias.trim())),
        None => (tree, None),
    };
    let mut path = prefix.to_vec();
    path.extend(segments(text));
    match path.last().map(String::as_str) {
        // `a::{self}`: the module itself, not a name.
        Some("self") | None => {}
        Some(name) => {
            let exported = alias.map_or_else(|| name.to_owned(), str::to_owned);
            out.push((path, exported));
        }
    }
}

/// The declaration that starts at `at` in the masked text: everything up
/// to the body's `{` or the closing `;`, outside parentheses and brackets,
/// on one line and at most 480 characters.
fn declaration(code: &[u8], at: usize) -> String {
    let mut depth = 0_i32;
    let mut end = at;
    let mut previous = 0u8;
    while end < code.len() && end - at < 640 {
        let b = code[end];
        match b {
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            b'<' => depth += 1,
            b'>' if previous != b'-' && previous != b'=' => depth -= 1,
            b'{' | b';' if depth <= 0 => break,
            _ => {}
        }
        previous = b;
        end += 1;
    }
    let text = String::from_utf8_lossy(&code[at..end]);
    let one: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let one = one.trim().trim_end_matches(',').to_owned();
    if one.chars().count() > 480 { one.chars().take(480).collect() } else { one }
}

fn signature(line: &str) -> String {
    let s = line.trim();
    if s.chars().count() <= SIG_MAX {
        s.to_owned()
    } else {
        format!("{}…", s.chars().take(SIG_MAX - 1).collect::<String>().trim_end())
    }
}

fn strip_attributes(raw: &str) -> String {
    let mut out = String::new();
    let mut rest = raw;
    while let Some(at) = rest.find("#[").or_else(|| rest.find("#![")) {
        out.push_str(&rest[..at]);
        out.push(' ');
        let mut depth = 0;
        let mut end = rest.len();
        for (i, c) in rest[at..].char_indices() {
            match c {
                '[' => depth += 1,
                ']' => {
                    depth -= 1;
                    if depth == 0 {
                        end = at + i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// `mod name` at the end of a header (`pub mod x`, `mod x`).
fn mod_header(header: &str) -> Option<&str> {
    let words: Vec<&str> = header.split_whitespace().collect();
    let n = words.len();
    (n >= 2 && words[n - 2] == "mod").then(|| words[n - 1])
}

fn extern_header(header: &str) -> bool {
    let mut words = header.split_whitespace().rev();
    match words.next() {
        Some("extern") => true,
        Some(w) if w.starts_with('"') => words.next() == Some("extern"),
        _ => false,
    }
}

/// The `Doc` of module `name` (a key of [`items`]): its own `//!` or
/// included README, else the `///` on its `mod x;` in its parent.
#[must_use]
pub fn module_doc(root: &Path, root_file: &Path, name: &str) -> Option<Doc> {
    let read = |p: &Path| fs::read_to_string(p).ok();
    let file_of = |segs: &[&str]| -> Option<PathBuf> {
        let base = segs.iter().fold(root.to_path_buf(), |dir, seg| dir.join(seg));
        let file = base.with_file_name(format!("{}.rs", base.file_name()?.to_string_lossy()));
        [file, base.join("mod.rs")].into_iter().find(|f| f.is_file())
    };
    if name == "lib" {
        let file = if root.join("lib.rs").is_file() { root.join("lib.rs") } else { root_file.to_path_buf() };
        let text = read(&file)?;
        return doc_of(first_paragraph(&inner_doc(&text, file.parent().unwrap_or(root))?));
    }
    let segs: Vec<&str> = name.split("::").collect();
    if let Some(own) = file_of(&segs)
        && let Some(text) = read(&own)
        && let Some(doc) = inner_doc(&text, own.parent().unwrap_or(root)).and_then(|d| first_paragraph(&d)).and_then(|p| doc_of(Some(p)))
    {
        return Some(doc);
    }
    let parent = if segs.len() == 1 { Some(root_file.to_path_buf()) } else { file_of(&segs[..segs.len() - 1]) }?;
    let text = read(&parent)?;
    let code = mask(&text, Literals::Blank);
    let lines: Vec<&str> = text.split('\n').collect();
    let last = *segs.last()?;
    for (decl, _, line) in mod_decls(&code) {
        if decl == last {
            let docs = outer_doc(&lines, line);
            if !docs.is_empty()
                && let Some(doc) = doc_of(first_paragraph(&docs.join("\n")))
            {
                return Some(doc);
            }
        }
    }
    None
}

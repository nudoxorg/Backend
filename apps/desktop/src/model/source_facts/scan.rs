//! Reading a Rust crate's source for what it does: lines of code, unsafe
//! blocks, and where it names the network, files, programs, the
//! environment and C. A port of the design board's extractor
//! (`Nudox-Design-System/v6/moments/extract_trust.py`, `trust_scan`), with
//! the same rules, so the two agree number for number: comments are blanked
//! (strings are kept, the way the board keeps them), test-ish directories do
//! not count for capabilities, nested crates are not this crate.

use facet::folio::state::{Build, Unsafe};
use std::fs;
use std::path::{Path, PathBuf};

/// Directories whose files are examples, tests or fixtures, not the crate.
const TESTISH: [&str; 11] = [
    "tests", "test", "benches", "bench", "examples", "example", "fixtures", "fixture", "testdata",
    "corpus", "target",
];

/// Example lines kept per capability.
const EXAMPLES: usize = 3;
/// Hits kept per capability before the examples are picked.
const HITS: usize = 40;
/// Characters of a line kept as evidence.
const EVIDENCE: usize = 110;

/// One place a source file shows something: file, one-based line, the line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Evidence {
    /// The file, relative to the crate root.
    pub file: String,
    /// The line, from one.
    pub line: usize,
    /// The line, trimmed and cut.
    pub text: String,
}

/// One capability: how many lines name it and a few of them.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Capability {
    /// Lines that name it.
    pub count: usize,
    /// Up to three of them, one per file first.
    pub examples: Vec<Evidence>,
}

/// What a crate's source says it can do.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Scan {
    /// Lines of code in `src`.
    pub sloc: usize,
    /// `unsafe` blocks, functions, impls and traits.
    pub unsafe_count: usize,
    /// Whether the crate root forbids (or denies) `unsafe_code`.
    pub unsafe_code: Unsafe,
    /// The network.
    pub net: Capability,
    /// Files.
    pub fs: Capability,
    /// Other programs.
    pub process: Capability,
    /// The environment.
    pub env: Capability,
    /// C.
    pub ffi: Capability,
    /// Whether the crate has tests.
    pub suite: Suite,
    /// Files under `examples`.
    pub examples: usize,
    /// Whether `build.rs` exists at the root.
    pub build: Build,
}

/// Whether a crate has tests.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum Suite {
    /// None found.
    #[default]
    Untested,
    /// A `tests` directory or a `#[test]`.
    Tested,
}

/// What `mask` keeps of string and char literals.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Literals {
    /// Literals stay (a path in a string is still a path).
    Keep,
    /// Literals are blanked like comments.
    Blank,
}

const fn ident(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Blanks comments (and, when `literals` is [`Literals::Blank`], string and char literals)
/// with spaces, keeping newlines and byte offsets, so a match's offset is a
/// line in the original.
#[must_use]
pub fn mask(src: &str, literals: Literals) -> String {
    let strings = literals == Literals::Keep;
    let bytes = src.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let blank = |out: &mut Vec<u8>, from: usize, to: usize| {
        for &b in &bytes[from..to] {
            out.push(if b == b'\n' { b'\n' } else { b' ' });
        }
    };
    let keep = |out: &mut Vec<u8>, from: usize, to: usize| out.extend_from_slice(&bytes[from..to]);
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        let next = bytes.get(i + 1).copied();
        if b == b'/' && next == Some(b'/') {
            let end = memchr(b'\n', bytes, i).unwrap_or(bytes.len());
            blank(&mut out, i, end);
            i = end;
        } else if b == b'/' && next == Some(b'*') {
            let mut depth = 1;
            let mut j = i + 2;
            while j < bytes.len() && depth > 0 {
                if bytes[j] == b'/' && bytes.get(j + 1) == Some(&b'*') {
                    depth += 1;
                    j += 2;
                } else if bytes[j] == b'*' && bytes.get(j + 1) == Some(&b'/') {
                    depth -= 1;
                    j += 2;
                } else {
                    j += 1;
                }
            }
            blank(&mut out, i, j);
            i = j;
        } else if b == b'\'' {
            // A char literal or a lifetime.
            let end = char_literal(bytes, i);
            if let Some(end) = end {
                if strings {
                    keep(&mut out, i, end)
                } else {
                    blank(&mut out, i, end)
                }
                i = end;
            } else {
                out.push(b'\'');
                i += 1;
            }
        } else if b == b'"' {
            let end = string_end(bytes, i + 1);
            if strings {
                keep(&mut out, i, end)
            } else {
                blank(&mut out, i, end)
            }
            i = end;
        } else if (b == b'r' || (b == b'b' && next == Some(b'r')))
            && (i == 0 || !ident(bytes[i - 1]))
        {
            // A raw string: r#"…"#, br"…".
            let start = if b == b'b' { i + 2 } else { i + 1 };
            let hashes = bytes[start..].iter().take_while(|c| **c == b'#').count();
            if bytes.get(start + hashes) == Some(&b'"') {
                let body = start + hashes + 1;
                let close: Vec<u8> = std::iter::once(b'"')
                    .chain(std::iter::repeat_n(b'#', hashes))
                    .collect();
                let end = find(bytes, &close, body).map_or(bytes.len(), |at| at + close.len());
                if strings {
                    keep(&mut out, i, end)
                } else {
                    blank(&mut out, i, end)
                }
                i = end;
            } else {
                out.push(b);
                i += 1;
            }
        } else {
            out.push(b);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_default()
}

fn memchr(needle: u8, hay: &[u8], from: usize) -> Option<usize> {
    hay[from..]
        .iter()
        .position(|b| *b == needle)
        .map(|at| from + at)
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from >= hay.len() {
        return None;
    }
    hay[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|at| from + at)
}

/// The end of a string whose body starts at `from`.
fn string_end(bytes: &[u8], from: usize) -> usize {
    let mut j = from;
    while j < bytes.len() {
        match bytes[j] {
            b'\\' => j += 2,
            b'"' => return j + 1,
            _ => j += 1,
        }
    }
    bytes.len()
}

/// The end of a char literal starting at `at` (`'x'`, `'\n'`, `'\u{1F600}'`), if it is one.
fn char_literal(bytes: &[u8], at: usize) -> Option<usize> {
    let rest = &bytes[at + 1..];
    let first = *rest.first()?;
    if first == b'\\' {
        let second = *rest.get(1)?;
        let mut end = 2;
        match second {
            b'x' => end = 4,
            b'u' if rest.get(2) == Some(&b'{') => {
                end = 3 + rest[3..].iter().position(|c| *c == b'}')? + 1
            }
            b'\n' => return None,
            _ => {}
        }
        (rest.get(end) == Some(&b'\'')).then_some(at + 1 + end + 1)
    } else if first == b'\'' || first == b'\n' {
        None
    } else {
        // One (possibly multi-byte) char, then a quote.
        let len = match first {
            0..=0x7f => 1,
            0xc0..=0xdf => 2,
            0xe0..=0xef => 3,
            _ => 4,
        };
        (rest.get(len) == Some(&b'\'')).then_some(at + 1 + len + 1)
    }
}

/// The one-based line of byte offset `at`, given the newline offsets.
fn line_of(newlines: &[usize], at: usize) -> usize {
    newlines.partition_point(|n| *n < at) + 1
}

fn newlines(text: &str) -> Vec<usize> {
    text.bytes()
        .enumerate()
        .filter(|(_, b)| *b == b'\n')
        .map(|(i, _)| i)
        .collect()
}

/// One literal to find, and whether an identifier character may touch it.
struct Pattern {
    text: &'static str,
    /// No identifier character before it.
    lead: bool,
    /// No identifier character after it.
    tail: bool,
}

const fn p(text: &'static str, lead: bool, tail: bool) -> Pattern {
    Pattern { text, lead, tail }
}

const NET: [Pattern; 8] = [
    p("std::net", true, true),
    p("TcpStream", true, true),
    p("TcpListener", true, true),
    p("UdpSocket", true, true),
    p("tokio::net", true, true),
    p("hyper::", true, false),
    p("reqwest::", true, false),
    p("ureq::", true, false),
];
const FS: [Pattern; 6] = [
    p("std::fs", true, true),
    p("File::open", true, true),
    p("File::create", true, true),
    p("OpenOptions", true, true),
    p("tokio::fs", true, true),
    p("read_dir", true, true),
];
const PROCESS: [Pattern; 2] = [
    p("std::process::Command", true, false),
    p("Command::new(", true, false),
];
const ENV: [Pattern; 4] = [
    p("std::env::var", true, false),
    p("env::var(", true, false),
    p("env::vars(", true, false),
    p("env::set_var", true, false),
];
const FFI: [Pattern; 3] = [
    p("#[link(", false, false),
    p("dlopen", false, false),
    p("libloading", false, false),
];

/// Byte offsets where `pattern` occurs in `code`.
fn occurrences(code: &str, pattern: &Pattern) -> Vec<usize> {
    let bytes = code.as_bytes();
    code.match_indices(pattern.text)
        .map(|(at, _)| at)
        .filter(|at| {
            let end = at + pattern.text.len();
            (!pattern.lead || *at == 0 || !ident(bytes[at - 1]))
                && (!pattern.tail || end >= bytes.len() || !ident(bytes[end]))
        })
        .collect()
}

/// `extern "C"` (any whitespace between).
fn extern_c(code: &str) -> Vec<usize> {
    let bytes = code.as_bytes();
    code.match_indices("extern")
        .map(|(at, _)| at)
        .filter(|at| {
            let mut j = at + 6;
            let start = j;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            j > start && bytes[j..].starts_with(b"\"C\"")
        })
        .collect()
}

/// Whether `unsafe` at `at` opens a block, a function, an impl or a trait.
fn unsafe_at(bytes: &[u8], at: usize) -> bool {
    let mut j = at + 6;
    let skip = |mut j: usize| {
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        j
    };
    if j < bytes.len() && ident(bytes[j]) {
        return false;
    }
    j = skip(j);
    let word = |j: usize, w: &str| {
        bytes[j..].starts_with(w.as_bytes()) && bytes.get(j + w.len()).is_none_or(|b| !ident(*b))
    };
    if bytes.get(j) == Some(&b'{') {
        return true;
    }
    if word(j, "impl") || word(j, "trait") || word(j, "fn") {
        return true;
    }
    if word(j, "extern") {
        j = skip(j + 6);
        if bytes.get(j) == Some(&b'"') {
            j = string_end(bytes, j + 1);
            j = skip(j);
        }
        return word(j, "fn");
    }
    false
}

fn count_unsafe(code: &str) -> usize {
    let bytes = code.as_bytes();
    code.match_indices("unsafe")
        .filter(|(at, _)| (*at == 0 || !ident(bytes[at - 1])) && unsafe_at(bytes, *at))
        .count()
}

fn process_imported(code: &str) -> bool {
    let mut from = 0;
    while let Some(at) = code[from..].find("process::") {
        let rest = code[from + at + 9..].trim_start();
        if rest.starts_with("Command") || rest.starts_with('*') {
            return true;
        }
        if let Some(group) = rest.strip_prefix('{')
            && let Some(close) = group.find('}')
            && group[..close]
                .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                .any(|w| w == "Command")
        {
            return true;
        }
        from += at + 9;
    }
    false
}

fn forbids_unsafe(code: &str) -> bool {
    let mut from = 0;
    while let Some(at) = code[from..].find("#![") {
        let attr = &code[from + at + 3..];
        let Some(close) = attr.find(']') else { break };
        let inner = attr[..close].trim();
        if let Some(rest) = inner
            .strip_prefix("forbid")
            .or_else(|| inner.strip_prefix("deny"))
            && let Some(list) = rest.trim_start().strip_prefix('(')
            && list
                .split(')')
                .next()
                .unwrap_or("")
                .split(',')
                .any(|x| x.trim() == "unsafe_code")
        {
            return true;
        }
        from += at + 3;
    }
    false
}

/// Up to `k` examples: one per distinct file first (in path order), then
/// the rest, sorted by file and line.
fn first_examples(hits: &[Evidence], k: usize) -> Vec<Evidence> {
    let mut picked: Vec<&Evidence> = Vec::new();
    for hit in hits {
        if !picked.iter().any(|p| p.file == hit.file) {
            picked.push(hit);
        }
    }
    if picked.len() < k {
        for hit in hits {
            if picked.len() >= k {
                break;
            }
            if !picked.iter().any(|p| std::ptr::eq(*p, hit)) {
                picked.push(hit);
            }
        }
    }
    let mut out: Vec<Evidence> = picked.into_iter().take(k).cloned().collect();
    out.sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line)));
    out
}

/// The `.rs` files of a crate, in path order, skipping hidden and vendored
/// directories and nested crates.
fn rust_files(root: &Path) -> Vec<(PathBuf, Vec<String>)> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<(PathBuf, Vec<String>)>) {
        let Ok(read) = fs::read_dir(dir) else { return };
        let mut entries: Vec<_> = read.filter_map(Result::ok).collect();
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if name.starts_with('.')
                    || name == "target"
                    || name == "node_modules"
                    || path.join("Cargo.toml").exists()
                {
                    continue;
                }
                walk(&path, root, out);
            } else if kind.is_file() && name.ends_with(".rs") {
                let rel: Vec<String> = path
                    .strip_prefix(root)
                    .ok()
                    .map(|r| {
                        r.components()
                            .map(|c| c.as_os_str().to_string_lossy().into_owned())
                            .collect()
                    })
                    .unwrap_or_default();
                out.push((path, rel));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out
}

/// Reads the crate at `root`; `lib` is the crate's library file, relative
/// to `root` (`src/lib.rs`).
#[must_use]
pub fn scan(root: &Path, lib: &str) -> Scan {
    let lib_parts: Vec<&str> = Path::new(lib)
        .parent()
        .map(|p| {
            p.components()
                .filter_map(|c| c.as_os_str().to_str())
                .collect()
        })
        .unwrap_or_default();
    let mut out = Scan::default();
    let (mut net, mut fs_hits, mut process, mut env, mut ffi) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut counts = [0usize; 5];
    for (path, rel) in rust_files(root) {
        let top = if rel.len() > 1 {
            rel[0].to_lowercase()
        } else {
            String::new()
        };
        if top == "examples" || top == "example" {
            out.examples += 1;
        }
        let dirs = &rel[..rel.len().saturating_sub(1)];
        let cap_ok = !dirs
            .iter()
            .any(|part| TESTISH.contains(&part.to_lowercase().as_str()));
        // `src/**`, or a library root elsewhere (`[lib] path = "rust/lib.rs"`).
        let in_src = top == "src"
            || (rel != ["build.rs".to_owned()]
                && rel.len() > lib_parts.len()
                && rel[..lib_parts.len()]
                    .iter()
                    .map(String::as_str)
                    .eq(lib_parts.iter().copied())
                && lib_parts != ["src"]
                && !rel[lib_parts.len()..rel.len() - 1]
                    .iter()
                    .any(|part| TESTISH.contains(&part.to_lowercase().as_str())));
        if !(in_src || cap_ok) {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else { continue };
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let code = mask(&text, Literals::Keep);
        if in_src {
            out.sloc += text
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty() && !line.starts_with("//"))
                .count();
            out.unsafe_count += count_unsafe(&code);
            if out.suite == Suite::Untested && code.contains("#[test]") {
                out.suite = Suite::Tested;
            }
        }
        if cap_ok {
            let nl = newlines(&text);
            let lines: Vec<&str> = text.split('\n').collect();
            let file = rel.join("/");
            let proc_ok = process_imported(&code);
            let mut hit = |list: &mut Vec<Evidence>,
                           count: &mut usize,
                           offsets: Vec<usize>,
                           skip: &dyn Fn(usize) -> bool| {
                let mut offsets = offsets;
                offsets.sort_unstable();
                let mut seen: Vec<usize> = Vec::new();
                for at in offsets {
                    if skip(at) {
                        continue;
                    }
                    let line = line_of(&nl, at);
                    if seen.contains(&line) {
                        continue;
                    }
                    seen.push(line);
                    *count += 1;
                    if list.len() < HITS {
                        let raw = lines.get(line - 1).copied().unwrap_or("").trim();
                        list.push(Evidence {
                            file: file.clone(),
                            line,
                            text: raw.chars().take(EVIDENCE).collect(),
                        });
                    }
                }
            };
            let of = |patterns: &[Pattern]| -> Vec<usize> {
                patterns
                    .iter()
                    .flat_map(|pattern| occurrences(&code, pattern))
                    .collect()
            };
            hit(&mut net, &mut counts[0], of(&NET), &|_| false);
            hit(&mut fs_hits, &mut counts[1], of(&FS), &|_| false);
            // A bare `Command::new(` may be clap's, not `std::process`'s.
            hit(&mut process, &mut counts[2], of(&PROCESS), &|at| {
                code[at..].starts_with("Command::new(") && !proc_ok
            });
            hit(&mut env, &mut counts[3], of(&ENV), &|_| false);
            let mut ffi_at = of(&FFI);
            ffi_at.extend(extern_c(&code));
            hit(&mut ffi, &mut counts[4], ffi_at, &|_| false);
        }
    }
    let lib_file = root.join(lib);
    let lib_file = if lib_file.is_file() {
        lib_file
    } else {
        root.join("src").join("main.rs")
    };
    if let Ok(bytes) = fs::read(&lib_file) {
        out.unsafe_code = if forbids_unsafe(&mask(&String::from_utf8_lossy(&bytes), Literals::Keep))
        {
            Unsafe::Forbidden
        } else {
            Unsafe::Allowed
        };
    }
    if root.join("tests").is_dir() {
        out.suite = Suite::Tested;
    }
    out.build = if root.join("build.rs").is_file() {
        Build::Script
    } else {
        Build::Plain
    };
    out.net = Capability {
        count: counts[0],
        examples: first_examples(&net, EXAMPLES),
    };
    out.fs = Capability {
        count: counts[1],
        examples: first_examples(&fs_hits, EXAMPLES),
    };
    out.process = Capability {
        count: counts[2],
        examples: first_examples(&process, EXAMPLES),
    };
    out.env = Capability {
        count: counts[3],
        examples: first_examples(&env, EXAMPLES),
    };
    out.ffi = Capability {
        count: counts[4],
        examples: first_examples(&ffi, EXAMPLES),
    };
    out
}

/// Lines of code in a crate's `src`: the same count as [`scan`]'s `sloc`,
/// without the rest (what a dependency weighs).
#[must_use]
pub fn sloc(root: &Path, lib: &str) -> usize {
    let lib_parts: Vec<String> = Path::new(lib)
        .parent()
        .map(|p| {
            p.components()
                .filter_map(|c| c.as_os_str().to_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    rust_files(root)
        .into_iter()
        .filter(|(_, rel)| {
            let top = if rel.len() > 1 {
                rel[0].to_lowercase()
            } else {
                String::new()
            };
            top == "src"
                || (rel != &["build.rs".to_owned()]
                    && rel.len() > lib_parts.len()
                    && rel[..lib_parts.len()] == lib_parts[..]
                    && lib_parts != ["src".to_owned()]
                    && !rel[lib_parts.len()..rel.len() - 1]
                        .iter()
                        .any(|part| TESTISH.contains(&part.to_lowercase().as_str())))
        })
        .map(|(path, _)| {
            fs::read(&path).map_or(0, |bytes| {
                String::from_utf8_lossy(&bytes)
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty() && !line.starts_with("//"))
                    .count()
            })
        })
        .sum()
}

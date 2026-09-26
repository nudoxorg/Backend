//! Code highlighting: tree-sitter captures (through `gpui_component`'s
//! highlighter) mapped onto [`Syntax`](crate::tokens::Syntax) roles.
//!
//! [`highlight`] is what views call: a cache hit returns the runs at once; a
//! miss starts one background parse (deduplicated while in flight), returns
//! `None`, and notifies the asking view when the runs land. Runs carry
//! [`Role`]s, not colours, so the cache survives an appearance change;
//! [`Highlighted::styles`] resolves them against a palette for
//! `StyledText::with_highlights`.
//!
//! ```ignore
//! let text: SharedString = source.into();
//! let styled = match code::highlight(Lang::Rust, text.clone(), window, cx) {
//!     Some(runs) => StyledText::new(text).with_highlights(runs.styles(cx.palette())),
//!     None => StyledText::new(text), // plain for the frame or two it takes
//! };
//! ```

use crate::tokens::{Palette, Syntax};
use gpui::{App, EntityId, FontStyle, Global, HighlightStyle, Hsla, SharedString, Task, Window};
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::sync::Arc;

/// The languages of the seven ecosystems.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Lang {
    /// cargo.
    Rust,
    /// npm (TypeScript).
    TypeScript,
    /// npm (JavaScript: highlighted with the TypeScript grammar, a syntactic
    /// superset, so no second grammar ships).
    JavaScript,
    /// PyPI.
    Python,
    /// Go modules.
    Go,
    /// Maven.
    Java,
    /// NuGet.
    CSharp,
    /// Conan.
    Cpp,
}

impl Lang {
    /// From a file extension or a language name (`rs`, `rust`, `tsx`, `c++`…).
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name.trim_start_matches('.').to_ascii_lowercase().as_str() {
            "rs" | "rust" => Self::Rust,
            "ts" | "tsx" | "mts" | "cts" | "typescript" => Self::TypeScript,
            "js" | "jsx" | "mjs" | "cjs" | "javascript" => Self::JavaScript,
            "py" | "pyi" | "python" => Self::Python,
            "go" | "golang" => Self::Go,
            "java" => Self::Java,
            "cs" | "csharp" | "c#" => Self::CSharp,
            "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" | "h" | "c++" => Self::Cpp,
            _ => return None,
        })
    }

    /// The highlighter's language name.
    #[must_use]
    pub const fn grammar(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::TypeScript | Self::JavaScript => "typescript",
            Self::Python => "python",
            Self::Go => "go",
            Self::Java => "java",
            Self::CSharp => "csharp",
            Self::Cpp => "cpp",
        }
    }
}

/// What a span of code is, in the palette's terms.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Role {
    /// Keywords and storage modifiers.
    Keyword,
    /// Type names, modules and namespaces.
    Type,
    /// Traits and interfaces.
    Contract,
    /// Functions and methods.
    Function,
    /// Macros and lifetimes.
    Macro,
    /// Strings, characters, escapes.
    String,
    /// Numbers.
    Number,
    /// Constants, booleans, enum members.
    Constant,
    /// Comments (italic).
    Comment,
    /// Punctuation and operators.
    Punctuation,
    /// Attributes, decorators, annotations, labels.
    Attribute,
    /// Parameters.
    Parameter,
}

impl Role {
    /// The role a tree-sitter capture name maps to, or `Some(None)` for a
    /// capture that is deliberately plain (variables, properties), or `None`
    /// for a name this table does not know (it styles nothing).
    #[must_use]
    pub fn of_capture(name: &str) -> Option<Option<Self>> {
        let mut parts = name.split('.');
        let head = parts.next().unwrap_or_default();
        let next = parts.next();
        Some(match (head, next) {
            ("comment", _) => Some(Self::Comment),
            ("string" | "character" | "escape", _) => Some(Self::String),
            ("number" | "float", _) => Some(Self::Number),
            ("boolean", _) | ("constant", _) => Some(Self::Constant),
            ("variable", Some("parameter")) | ("parameter", _) => Some(Self::Parameter),
            ("variable", Some("builtin")) => Some(Self::Keyword),
            ("variable" | "property" | "field" | "tag" | "embedded" | "none", _) => None,
            ("type", Some("interface" | "trait")) | ("interface" | "trait", _) => {
                Some(Self::Contract)
            }
            ("type" | "constructor" | "namespace" | "module", _) => Some(Self::Type),
            // Rust tags lifetimes (and loop labels, which are lifetimes) `@label`.
            ("function" | "method", Some("macro")) | ("macro" | "lifetime" | "label", _) => {
                Some(Self::Macro)
            }
            ("function" | "method", _) => Some(Self::Function),
            ("keyword" | "include" | "conditional" | "repeat" | "exception" | "storageclass"
            | "modifier", _) => Some(Self::Keyword),
            ("punctuation" | "operator" | "delimiter", _) => Some(Self::Punctuation),
            ("attribute" | "decorator" | "annotation", _) => Some(Self::Attribute),
            _ => return None,
        })
    }

    /// The colour this role paints with.
    #[must_use]
    pub fn tone(self, syntax: &Syntax) -> Hsla {
        match self {
            Self::Keyword => syntax.keyword,
            Self::Type => syntax.type_name,
            Self::Contract => syntax.contract,
            Self::Function => syntax.function,
            Self::Macro => syntax.macro_name,
            Self::String => syntax.string,
            Self::Number => syntax.number,
            Self::Constant => syntax.constant,
            Self::Comment => syntax.comment,
            Self::Punctuation => syntax.punctuation,
            Self::Attribute => syntax.attribute,
            Self::Parameter => syntax.parameter,
        }
        .into()
    }
}

/// Highlighted source: sorted, non-overlapping byte runs with a role (bytes
/// between runs are plain).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Highlighted {
    /// The language it was parsed as.
    pub lang: Lang,
    /// The source.
    pub source: SharedString,
    /// Role runs, ascending, disjoint, on char boundaries.
    pub runs: Vec<(Range<usize>, Role)>,
}

impl Highlighted {
    /// Highlight styles for `StyledText::with_highlights`, coloured from
    /// `palette.syntax` (comments italic).
    #[must_use]
    pub fn styles(&self, palette: &Palette) -> Vec<(Range<usize>, HighlightStyle)> {
        self.runs
            .iter()
            .map(|(range, role)| {
                (
                    range.clone(),
                    HighlightStyle {
                        color: Some(role.tone(&palette.syntax)),
                        font_style: (*role == Role::Comment).then_some(FontStyle::Italic),
                        ..HighlightStyle::default()
                    },
                )
            })
            .collect()
    }

    /// The role at byte `offset`, if any.
    #[must_use]
    pub fn role_at(&self, offset: usize) -> Option<Role> {
        let ix = self.runs.partition_point(|(range, _)| range.end <= offset);
        self.runs
            .get(ix)
            .filter(|(range, _)| range.start <= offset)
            .map(|(_, role)| *role)
    }
}

/// Parses and maps `source` now, on this thread (the background task's body;
/// tests and tools call it directly).
#[must_use]
pub fn highlight_now(lang: Lang, source: SharedString) -> Highlighted {
    let runs = runs(lang, &source);
    Highlighted { lang, source, runs }
}

#[cfg(feature = "highlight")]
fn runs(lang: Lang, source: &str) -> Vec<(Range<usize>, Role)> {
    use gpui_component::Rope;
    use gpui_component::highlighter::SyntaxHighlighter;
    let mut highlighter = SyntaxHighlighter::new(lang.grammar());
    let rope = Rope::from_str(source);
    highlighter.update(None, &rope, None);
    // Later (inner) captures override earlier (outer) ones, byte by byte;
    // `Some(None)` paints plain over an outer role (a variable inside a
    // string interpolation), unknown names leave what is there.
    let mut paint: Vec<Option<Role>> = vec![None; source.len()];
    for (range, name) in highlighter.captures(0..source.len()) {
        if let Some(role) = Role::of_capture(&name) {
            let end = range.end.min(paint.len());
            for byte in &mut paint[range.start.min(end)..end] {
                *byte = role;
            }
        }
    }
    let mut runs: Vec<(Range<usize>, Role)> = Vec::new();
    let mut start = 0;
    for offset in 1..=paint.len() {
        let boundary = offset == paint.len() || paint[offset] != paint[start];
        if boundary {
            if let Some(role) = paint[start] {
                runs.push((start..offset, role));
            }
            start = offset;
        }
    }
    runs
}

#[cfg(not(feature = "highlight"))]
fn runs(_lang: Lang, _source: &str) -> Vec<(Range<usize>, Role)> {
    Vec::new()
}

/// Cache capacity in source bytes (the LRU evicts past it).
const CACHE_BYTES: usize = 8 << 20;

type Key = (Lang, u64, usize);

#[derive(Default)]
struct Cache {
    entries: HashMap<Key, (Arc<Highlighted>, u64)>,
    bytes: usize,
    tick: u64,
    in_flight: HashMap<Key, (HashSet<EntityId>, Task<()>)>,
    parses: u64,
}

impl Global for Cache {}

fn key(lang: Lang, source: &str) -> Key {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut hasher);
    (lang, hasher.finish(), source.len())
}

impl Cache {
    fn get(&mut self, key: &Key, source: &str) -> Option<Arc<Highlighted>> {
        self.tick += 1;
        let tick = self.tick;
        let (entry, used) = self.entries.get_mut(key)?;
        if entry.source.as_ref() != source {
            return None;
        }
        *used = tick;
        Some(Arc::clone(entry))
    }

    fn insert(&mut self, key: Key, highlighted: Arc<Highlighted>) {
        self.tick += 1;
        self.bytes += highlighted.source.len();
        if let Some((old, _)) = self.entries.insert(key, (highlighted, self.tick)) {
            self.bytes -= old.source.len();
        }
        while self.bytes > CACHE_BYTES && self.entries.len() > 1 {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(key, _)| *key)
            else {
                break;
            };
            if let Some((evicted, _)) = self.entries.remove(&oldest) {
                self.bytes -= evicted.source.len();
            }
        }
    }
}

/// The highlighted runs for `source`, or `None` while a background parse
/// runs; the view being rendered is notified when they land. Identical
/// requests share one parse.
pub fn highlight(
    lang: Lang,
    source: impl Into<SharedString>,
    window: &mut Window,
    cx: &mut App,
) -> Option<Arc<Highlighted>> {
    let source = source.into();
    let key = key(lang, &source);
    let view = window.current_view();
    let cache = cx.default_global::<Cache>();
    if let Some(hit) = cache.get(&key, &source) {
        return Some(hit);
    }
    if let Some((waiting, _)) = cache.in_flight.get_mut(&key) {
        waiting.insert(view);
        return None;
    }
    cache.parses += 1;
    let parse = cx
        .background_executor()
        .spawn(async move { highlight_now(lang, source) });
    let task = cx.spawn(async move |cx| {
        let highlighted = parse.await;
        cx.update(|cx| {
            let cache = cx.default_global::<Cache>();
            let waiting = cache
                .in_flight
                .remove(&key)
                .map(|(waiting, _)| waiting)
                .unwrap_or_default();
            cache.insert(key, Arc::new(highlighted));
            for view in waiting {
                cx.notify(view);
            }
        });
    });
    cx.default_global::<Cache>()
        .in_flight
        .insert(key, (HashSet::from([view]), task));
    None
}

/// Parses started since the app began (a cache hit or a shared in-flight
/// parse does not count).
#[must_use]
pub fn parses(cx: &App) -> u64 {
    cx.try_global::<Cache>().map_or(0, |cache| cache.parses)
}

#[cfg(all(test, feature = "highlight"))]
mod tests {
    use super::{Lang, Role, highlight_now};

    /// The role of the first occurrence of `needle` in the highlighted source.
    fn role(lang: Lang, source: &'static str, needle: &str) -> Option<Role> {
        let highlighted = highlight_now(lang, source.into());
        let at = source.find(needle).expect("needle in source");
        let role = highlighted.role_at(at);
        // The whole needle carries that role.
        for offset in at..at + needle.len() {
            assert_eq!(highlighted.role_at(offset), role, "{needle} at {offset}");
        }
        role
    }

    #[test]
    fn each_language_marks_keywords_strings_comments_and_functions() {
        let cases: [(Lang, &'static str, [(&str, Option<Role>); 4]); 8] = [
            (
                Lang::Rust,
                "// note\npub fn parse(input: &str) -> u32 { let s = \"hi\"; 42 }",
                [("pub", Some(Role::Keyword)), ("\"hi\"", Some(Role::String)), ("// note", Some(Role::Comment)), ("parse", Some(Role::Function))],
            ),
            (
                Lang::TypeScript,
                "// note\nexport function parse(input: string): number { const s = \"hi\"; return 42; }",
                [("export", Some(Role::Keyword)), ("\"hi\"", Some(Role::String)), ("// note", Some(Role::Comment)), ("parse", Some(Role::Function))],
            ),
            (
                Lang::JavaScript,
                "// note\nfunction parse(input) { const s = 'hi'; return 42; }",
                [("function", Some(Role::Keyword)), ("'hi'", Some(Role::String)), ("// note", Some(Role::Comment)), ("parse", Some(Role::Function))],
            ),
            (
                Lang::Python,
                "# note\ndef parse(value):\n    s = \"hi\"\n    return 42\n",
                [("def", Some(Role::Keyword)), ("\"hi\"", Some(Role::String)), ("# note", Some(Role::Comment)), ("parse", Some(Role::Function))],
            ),
            (
                Lang::Go,
                "// note\nfunc Parse(input string) int { s := \"hi\"; return 42 }",
                [("func", Some(Role::Keyword)), ("\"hi\"", Some(Role::String)), ("// note", Some(Role::Comment)), ("Parse", Some(Role::Function))],
            ),
            (
                Lang::Java,
                "// note\nclass A { int parse(String input) { String s = \"hi\"; return 42; } }",
                [("class", Some(Role::Keyword)), ("\"hi\"", Some(Role::String)), ("// note", Some(Role::Comment)), ("parse", Some(Role::Function))],
            ),
            (
                Lang::CSharp,
                "// note\nclass A { int Parse(string input) { var s = \"hi\"; return 42; } }",
                [("class", Some(Role::Keyword)), ("\"hi\"", Some(Role::String)), ("// note", Some(Role::Comment)), ("Parse", Some(Role::Function))],
            ),
            (
                Lang::Cpp,
                "// note\nint parse(const char* input) { auto s = \"hi\"; return 42; }",
                [("return", Some(Role::Keyword)), ("\"hi\"", Some(Role::String)), ("// note", Some(Role::Comment)), ("parse", Some(Role::Function))],
            ),
        ];
        let mut misses = Vec::new();
        for (lang, source, expectations) in cases {
            for (needle, expected) in expectations {
                let got = role(lang, source, needle);
                if got != expected {
                    misses.push(format!("{lang:?} `{needle}`: {got:?}, expected {expected:?}"));
                }
            }
        }
        assert!(misses.is_empty(), "{misses:#?}");
    }

    #[test]
    fn runs_are_sorted_disjoint_and_on_char_boundaries() {
        let source = "fn ünïcödé() -> &'static str { \"ß→ä\" } // ok ☃";
        let highlighted = highlight_now(Lang::Rust, source.into());
        assert!(!highlighted.runs.is_empty());
        for pair in highlighted.runs.windows(2) {
            assert!(pair[0].0.end <= pair[1].0.start, "{pair:?}");
        }
        for (range, _) in &highlighted.runs {
            assert!(source.is_char_boundary(range.start) && source.is_char_boundary(range.end));
        }
        assert_eq!(highlighted.role_at(source.find("'static").expect("lifetime")), Some(Role::Macro));
    }

    mod async_cache {
        use crate::code::{Lang, highlight, parses};
        use gpui::{Context, IntoElement, Render, TestAppContext, Window, div};
        use std::cell::RefCell;
        use std::rc::Rc;

        const SOURCE: &str = "fn main() { let answer = 42; }";

        struct Source {
            history: Rc<RefCell<Vec<bool>>>,
        }

        impl Render for Source {
            fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let first = highlight(Lang::Rust, SOURCE, window, cx);
                // A second, identical request in the same frame shares the parse.
                let second = highlight(Lang::Rust, SOURCE, window, cx);
                assert_eq!(first.is_some(), second.is_some());
                self.history
                    .borrow_mut()
                    .push(first.is_some_and(|runs| !runs.runs.is_empty()));
                div()
            }
        }

        #[gpui::test]
        fn a_miss_parses_once_in_the_background_and_wakes_the_view(cx: &mut TestAppContext) {
            let history = Rc::new(RefCell::new(Vec::new()));
            let (_view, cx) = cx.add_window_view({
                let history = Rc::clone(&history);
                |_, _| Source { history }
            });
            cx.run_until_parked();
            cx.update(|window, cx| {
                window.draw(cx).clear(cx);
            });
            let history = history.borrow().clone();
            assert_eq!(history.first(), Some(&false), "the first frame is plain: {history:?}");
            assert_eq!(history.last(), Some(&true), "woken and highlighted: {history:?}");
            assert_eq!(cx.update(|_, cx| parses(cx)), 1, "identical requests share one parse");
        }
    }
}

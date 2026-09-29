//! Places your workspace names the symbol, read into verbs.
//!
//! The index gives a use as a relation and a span; the shell reads the line
//! at the span from the local file. This module reads what the line does:
//! it calls it, makes one, reads it, changes it, uses it up, matches on it,
//! holds one, derives it, implements it, asks for it (a bound), names it or
//! imports it; which member of the symbol it reaches; and, for a generic
//! function, what the generic is at this call. Nothing here guesses a line:
//! a use with no line text is not read at all.

use super::super::view::{Ctx, Do, Kind, Use, Uses, Verb};
use super::text::last_segment;

/// The relation the index gave a use.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Rel {
    /// A direct call.
    Calls,
    /// A method call.
    MethodCall,
    /// A use as a type.
    TypeReference,
    /// A read.
    Reads,
    /// A write.
    Writes,
    /// An import.
    Imports,
    /// An implementation.
    Implements,
    /// An override.
    Overrides,
    /// A re-export.
    Reexports,
    /// An inheritance.
    Inherits,
    /// A documentation reference.
    Documents,
}

/// One use as the shell read it.
#[derive(Clone, Debug)]
pub struct Site {
    /// The package it is in.
    pub package: String,
    /// The file, relative to the package.
    pub file: String,
    /// The file's absolute path.
    pub path: String,
    /// The line, one-based.
    pub line: u32,
    /// The line's text, trimmed.
    pub text: String,
    /// What the index says the relation is.
    pub rel: Rel,
    /// The index resolved it (rather than matched a name).
    pub exact: bool,
}

/// What the page's symbol is, for reading the lines that name it.
#[derive(Clone, Debug)]
pub struct Reader {
    /// The symbol's name (`Value`, `from_str`).
    pub name: String,
    /// Its kind.
    pub kind: Kind,
    /// Its members and what each does with the value.
    pub members: Vec<(String, Do)>,
    /// Its cases (an enum's variants), matched on and built.
    pub cases: Vec<String>,
    /// It has a generic parameter whose choice is worth reading.
    pub generic: bool,
}

/// Whether a file is a test's.
#[must_use]
pub fn is_test(file: &str) -> bool {
    let lower = file.to_ascii_lowercase();
    lower.split('/').any(|part| matches!(part, "tests" | "test" | "__tests__" | "spec" | "e2e"))
        || lower.ends_with("tests.rs")
        || lower.ends_with("_test.rs")
        || lower.ends_with("_tests.rs")
        || lower.ends_with("_test.go")
        || last_segment_of_path(&lower).starts_with("test_")
        || lower.contains(".test.")
        || lower.contains(".spec.")
}

fn last_segment_of_path(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The byte range of `name` as a whole word in `text`.
#[must_use]
pub fn mark_of(text: &str, name: &str) -> Option<(usize, usize)> {
    let name = last_segment(name);
    if name.is_empty() {
        return None;
    }
    text.match_indices(name).find_map(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + name.len()..].chars().next();
        let boundary = |c: Option<char>| !c.is_some_and(|c| c.is_alphanumeric() || c == '_');
        (boundary(before) && boundary(after)).then_some((at, at + name.len()))
    })
}

/// Reads one site.
#[must_use]
pub fn read(site: &Site, reader: &Reader) -> Use {
    let text = site.text.as_str();
    let mark = mark_of(text, &reader.name);
    let (verb, member) = verb_of(site, reader);
    let fill = if reader.generic && matches!(reader.kind, Kind::Function | Kind::Method) { fill_of(text, &reader.name) } else { None };
    Use {
        package: site.package.clone(),
        file: site.file.clone(),
        path: site.path.clone(),
        line: site.line,
        text: text.to_owned(),
        mark,
        verb,
        member,
        ctx: if is_test(&site.file) { Ctx::Test } else { Ctx::Code },
        fill,
        approx: !site.exact,
    }
}

/// Reads every site into the page's uses.
#[must_use]
pub fn read_all(sites: &[Site], reader: &Reader) -> Uses {
    Uses { all: sites.iter().map(|site| read(site, reader)).collect(), elsewhere: None }
}

fn starts_import(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with("use ")
        || t.starts_with("pub use ")
        || t.starts_with("pub(crate) use ")
        || t.starts_with("import ")
        || t.starts_with("from ") && t.contains(" import ")
        || t.starts_with("export {")
        || t.starts_with("export * from")
        || t.starts_with("const ") && t.contains("require(")
}

fn verb_of(site: &Site, reader: &Reader) -> (Verb, Option<String>) {
    let text = site.text.as_str();
    let name = last_segment(&reader.name);
    if matches!(site.rel, Rel::Imports | Rel::Reexports) || starts_import(text) {
        return (Verb::Imports, None);
    }
    if text.contains("derive(") && contains_word(text, name) {
        return (Verb::Derives, None);
    }
    if matches!(site.rel, Rel::Implements | Rel::Inherits | Rel::Overrides) || is_impl(text, name) {
        return (Verb::Implements, None);
    }
    if matches!(reader.kind, Kind::Function | Kind::Method) {
        return (Verb::Calls, None);
    }
    if matches!(reader.kind, Kind::Trait) && asks_for(text, name) {
        return (Verb::AsksFor, None);
    }
    // A member reached: `Name::member` (exact) or `.member(` (by name).
    if let Some((member, doing, qualified)) = member_of(text, name, reader) {
        let verb = match doing {
            Do::Makes => Verb::Makes,
            Do::Reads => Verb::Reads,
            Do::Changes => Verb::Changes,
            Do::UsesUp => Verb::UsesUp,
        };
        let _ = qualified;
        return (verb, Some(member));
    }
    if let Some(case) = case_of(text, name, reader) {
        let matching = is_match_line(text);
        return (if matching { Verb::Matches } else { Verb::Makes }, Some(case));
    }
    if is_construction(text, name) {
        return (Verb::Makes, None);
    }
    match site.rel {
        Rel::Calls | Rel::MethodCall => (Verb::Calls, None),
        Rel::Reads => (Verb::Reads, None),
        Rel::Writes => (Verb::Changes, None),
        Rel::Documents => (Verb::Names, None),
        _ => (type_verb(text, name), None),
    }
}

fn contains_word(text: &str, word: &str) -> bool {
    mark_of(text, word).is_some()
}

fn is_impl(text: &str, name: &str) -> bool {
    let t = text.trim_start();
    (t.starts_with("impl") && contains_word(t, name) && t.contains(" for ")) || (t.starts_with("class ") && t.contains(name) && (t.contains("extends") || t.contains("implements") || t.contains('(')))
}

/// `T: Serialize`, `+ Serialize`, `impl Serialize`, `where S: Serializer`.
fn asks_for(text: &str, name: &str) -> bool {
    let Some((at, _)) = mark_of(text, name) else { return false };
    let before = text[..at].trim_end();
    before.ends_with(':') && !before.ends_with("::") || before.ends_with('+') || before.ends_with("impl") || before.ends_with("dyn") || before.ends_with("where")
}

fn is_match_line(text: &str) -> bool {
    text.contains("=>") || text.contains("if let ") || text.contains("matches!(") || text.contains("while let ") || text.trim_start().starts_with("match ") || text.contains("instanceof") || text.trim_start().starts_with("case ")
}

fn is_construction(text: &str, name: &str) -> bool {
    text.contains(&format!("{name}::new(")) || text.contains(&format!("{name}::default(")) || text.contains(&format!("{name} {{")) && !text.trim_start().starts_with("struct ") && !text.contains("impl") || text.contains(&format!("new {name}("))
}

fn member_of(text: &str, name: &str, reader: &Reader) -> Option<(String, Do, bool)> {
    for (member, doing) in &reader.members {
        if text.contains(&format!("{name}::{member}(")) || text.contains(&format!("{name}::{member})")) || text.contains(&format!("{name}::{member},")) {
            return Some((member.clone(), *doing, true));
        }
    }
    for (member, doing) in &reader.members {
        if *doing != Do::Makes && contains_word(text, name) && (text.contains(&format!(".{member}(")) || text.contains(&format!(".{member}\n")) || text.ends_with(&format!(".{member}"))) {
            return Some((member.clone(), *doing, false));
        }
    }
    None
}

fn case_of(text: &str, name: &str, reader: &Reader) -> Option<String> {
    reader.cases.iter().find(|case| text.contains(&format!("{name}::{case}")) || text.contains(&format!("{name}.{case}"))).cloned()
}

/// What a line that names a type does with it, from the line itself.
fn type_verb(text: &str, name: &str) -> Verb {
    let Some((at, end)) = mark_of(text, name) else { return Verb::Names };
    let before = text[..at].trim_end();
    let after = text[end..].trim_start();
    if before.ends_with("&mut") || before.ends_with("&mut ") {
        return Verb::Changes;
    }
    if before.ends_with('&') {
        return Verb::Reads;
    }
    if text.contains("->") && text.find("->").is_some_and(|arrow| arrow < at) {
        return Verb::Makes;
    }
    if text.trim_start().starts_with("let ") && text[..at].contains(':') && !text[..at].contains('=') {
        return Verb::Makes;
    }
    if before.ends_with('<') || before.ends_with(',') && text[..at].contains('<') {
        return Verb::Holds;
    }
    let field = text.trim_start();
    let field = field.strip_prefix("pub ").unwrap_or(field);
    if before.ends_with(':') && (after.starts_with(',') || after.is_empty()) && !field.starts_with("let ") && !field.starts_with("fn ") {
        return Verb::Holds;
    }
    Verb::Names
}

// ------------------------------------------------------------------ fills

/// What a generic function's parameter is at this call: the turbofish
/// (`from_str::<Metadata>(`) or the annotation it is bound to
/// (`let m: Metadata = …from_str(…)`).
#[must_use]
pub fn fill_of(text: &str, function: &str) -> Option<String> {
    let name = last_segment(function);
    if let Some((at, end)) = mark_of(text, name) {
        let after = &text[end..];
        if let Some(rest) = after.strip_prefix("::<")
            && let Some((inner, _)) = super::text::balanced(&format!("<{rest}"), 0)
        {
            let tidy = tidy_type(inner);
            return (!tidy.is_empty() && tidy != "_").then_some(tidy);
        }
        // `let x: T = … name(`
        let before = &text[..at];
        if let Some(eq) = before.rfind('=')
            && !before[eq..].contains("==")
            && let Some(colon) = first_colon(&before[..eq])
            && before[..colon].trim_start().starts_with("let ")
        {
            let ty = before[colon + 1..eq].trim();
            let tidy = tidy_type(ty);
            return (!tidy.is_empty() && tidy != "_").then_some(tidy);
        }
    }
    None
}

/// The first `:` that is not half of a `::`.
fn first_colon(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    (0..bytes.len()).find(|&at| bytes[at] == b':' && bytes.get(at + 1) != Some(&b':') && (at == 0 || bytes[at - 1] != b':'))
}

/// A type with its module paths dropped: `serde_json::Value` → `Value`,
/// `Vec<crate::x::Entry>` → `Vec<Entry>`.
#[must_use]
pub fn tidy_type(text: &str) -> String {
    let mut out = String::new();
    let mut segment = String::new();
    let mut chars = text.trim().chars().peekable();
    while let Some(ch) = chars.next() {
        if ch.is_alphanumeric() || ch == '_' || ch == '$' {
            segment.push(ch);
        } else if ch == ':' && chars.peek() == Some(&':') {
            chars.next();
            segment.clear();
        } else {
            out.push_str(&segment);
            segment.clear();
            out.push(ch);
        }
    }
    out.push_str(&segment);
    out.split_whitespace().collect::<Vec<_>>().join(" ").replace("& ", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site(file: &str, text: &str, rel: Rel) -> Site {
        Site { package: "engine".into(), file: file.into(), path: format!("/w/{file}"), line: 10, text: text.into(), rel, exact: true }
    }

    fn value_reader() -> Reader {
        Reader {
            name: "Value".into(),
            kind: Kind::Enum,
            members: vec![("as_str".into(), Do::Reads), ("as_array_mut".into(), Do::Changes), ("try_into".into(), Do::UsesUp), ("from".into(), Do::Makes)],
            cases: vec!["Null".into(), "Array".into(), "String".into()],
            generic: false,
        }
    }

    #[test]
    fn a_line_says_what_it_does_with_a_type() {
        let r = value_reader();
        let v = |text: &str, rel: Rel| read(&site("src/a.rs", text, rel), &r);
        assert_eq!(v("use serde_json::Value;", Rel::TypeReference).verb, Verb::Imports);
        assert_eq!(v(".and_then(serde_json::Value::as_str)", Rel::MethodCall).verb, Verb::Reads);
        assert_eq!(v(".and_then(serde_json::Value::as_str)", Rel::MethodCall).member.as_deref(), Some("as_str"));
        assert_eq!(v("match v { Value::Null => 0, _ => 1 }", Rel::TypeReference).verb, Verb::Matches);
        assert_eq!(v("let v = Value::Array(items);", Rel::TypeReference).verb, Verb::Makes);
        assert_eq!(v("fn f(v: &mut Value) {", Rel::TypeReference).verb, Verb::Changes);
        assert_eq!(v("fn f(v: &Value) -> usize {", Rel::TypeReference).verb, Verb::Reads);
        assert_eq!(v("fn make() -> Value {", Rel::TypeReference).verb, Verb::Makes);
        assert_eq!(v("    pub root: Value,", Rel::TypeReference).verb, Verb::Holds);
        assert_eq!(v("let items: Vec<Value> = Vec::new();", Rel::TypeReference).verb, Verb::Makes);
        assert_eq!(v("row.get(\"lane\").and_then(Value::as_str)", Rel::TypeReference).verb, Verb::Reads);
        assert_eq!(v("#[derive(Clone, Debug, Value)]", Rel::TypeReference).verb, Verb::Derives);
        assert_eq!(v("x.as_str()", Rel::Reads).verb, Verb::Reads);
    }

    #[test]
    fn traits_are_derived_implemented_or_asked_for() {
        let r = Reader { name: "Serialize".into(), kind: Kind::Trait, members: Vec::new(), cases: Vec::new(), generic: false };
        let v = |text: &str, rel: Rel| read(&site("src/a.rs", text, rel), &r).verb;
        assert_eq!(v("#[derive(Debug, Serialize)]", Rel::TypeReference), Verb::Derives);
        assert_eq!(v("impl Serialize for Event {", Rel::Implements), Verb::Implements);
        assert_eq!(v("fn write<T: Serialize>(value: &T)", Rel::TypeReference), Verb::AsksFor);
        assert_eq!(v("where T: Clone + Serialize,", Rel::TypeReference), Verb::AsksFor);
        assert_eq!(v("use serde::Serialize;", Rel::Imports), Verb::Imports);
    }

    #[test]
    fn a_generic_call_says_what_it_chose() {
        assert_eq!(fill_of("let command = serde_json::from_str::<SurfaceCommand>(encoded).map_err(|e| {", "from_str").as_deref(), Some("SurfaceCommand"));
        assert_eq!(fill_of("let metadata: Metadata = match serde_json::from_str(json) {", "from_str").as_deref(), Some("Metadata"));
        assert_eq!(fill_of("let mut forged: serde_json::Value = serde_json::from_str(&encoded).expect(\"value\");", "from_str").as_deref(), Some("Value"));
        assert_eq!(fill_of("let x: Vec<crate::raw::RawEntry> = serde_json::from_str(s)?;", "from_str").as_deref(), Some("Vec<RawEntry>"));
        assert_eq!(fill_of("assert!(serde_json::from_str::<EventDto>(&encoded).is_err());", "from_str").as_deref(), Some("EventDto"));
        assert_eq!(fill_of("serde_json::from_str(&text)", "from_str"), None);
    }

    #[test]
    fn tests_are_told_from_code() {
        assert!(is_test("src/wire/tests.rs") && is_test("tests/live_registry.rs") && is_test("pkg/foo_test.go") && is_test("a/test_x.py") && is_test("a/b.test.ts"));
        assert!(!is_test("src/registry/discovery.rs") && !is_test("lib/util/resolveCommand.js"));
    }

    #[test]
    fn the_name_is_marked_where_it_stands() {
        let text = "let v = serde_json::Value::as_str;";
        let (a, b) = mark_of(text, "Value").expect("marked");
        assert_eq!(&text[a..b], "Value");
        assert_eq!(mark_of("ValueError(x)", "Value"), None);
    }
}

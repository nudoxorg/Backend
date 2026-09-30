//! Places your workspace names the symbol, read into verbs.
//!
//! The index gives a use as a relation and a span; the shell reads the line
//! at the span from the local file. This module reads what the line does:
//! it calls it, makes one, reads it, changes it, uses it up, matches on it,
//! holds one, derives it, implements it, asks for it (a bound), names it or
//! imports it; which member of the symbol it reaches; and, for a generic
//! function, what the generic is at this call. Nothing here guesses a line:
//! a use with no line text is not read at all.

use super::super::view::{Ctx, Do, Kind, Use, Uses, Verb, View};
use std::ops::Range;
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
    /// Where the reference is in `text`, as the index's span says; none when
    /// the place was matched by name and only the line is known.
    pub mark: Option<Range<usize>>,
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
    /// Its public fields, read as `.field`.
    pub fields: Vec<String>,
    /// It has a generic parameter whose choice is worth reading.
    pub generic: bool,
}

impl Reader {
    /// The reader of a page's lines: what its symbol is, its members and cases.
    #[must_use]
    pub fn of(view: &View) -> Self {
        use super::super::view::Shape;
        let mut members = Vec::new();
        for group in &view.verbs {
            members.extend(group.rows.iter().map(|row| (row.name.clone(), group.verb)));
        }
        let (mut cases, mut fields) = (Vec::new(), Vec::new());
        match &view.shape {
            Some(Shape::OneOf(shape)) => cases.extend(shape.iter().map(|case| case.name.clone())),
            Some(Shape::Holds { fields: shape, .. }) => fields.extend(shape.iter().map(|field| field.name.clone())),
            _ => {}
        }
        Self { name: view.head.name.clone(), kind: view.head.kind, members, cases, fields, generic: !view.generics.is_empty() }
    }
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

/// The byte ranges of `name` as a whole word in `text`, in order.
fn marks_of<'a>(text: &'a str, name: &'a str) -> impl Iterator<Item = Range<usize>> + 'a {
    let name = last_segment(name);
    text.match_indices(name).filter_map(move |(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + name.len()..].chars().next();
        let boundary = |c: Option<char>| !c.is_some_and(|c| c.is_alphanumeric() || c == '_');
        (!name.is_empty() && boundary(before) && boundary(after)).then_some(at..at + name.len())
    })
}

/// The byte range of the first `name` as a whole word in `text`.
#[must_use]
pub fn mark_of(text: &str, name: &str) -> Option<Range<usize>> {
    marks_of(text, name).next()
}

/// Reads one site.
#[must_use]
pub fn read(site: &Site, reader: &Reader) -> Use {
    let text = site.text.as_str();
    let mark = site.mark.clone().or_else(|| mark_of(text, &reader.name));
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

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// A reference and what stands on either side of it: the classifier reads
/// the token where the index put it, not the first place its name appears.
struct Around<'a> {
    /// The whole line.
    line: &'a str,
    /// Everything before the token.
    before: &'a str,
    /// The token.
    token: &'a str,
    /// Everything after it.
    after: &'a str,
}

impl<'a> Around<'a> {
    fn of(line: &'a str, mark: &Range<usize>) -> Option<Self> {
        (mark.start < mark.end && mark.end <= line.len() && line.is_char_boundary(mark.start) && line.is_char_boundary(mark.end))
            .then(|| Self { line, before: &line[..mark.start], token: &line[mark.clone()], after: &line[mark.end..] })
    }

    /// What is before the token, with the module path in front of it dropped.
    fn lead(&self) -> &'a str {
        before_path(self.before)
    }

    /// The name after `::` or `.` that follows the token.
    fn next(&self) -> Option<&'a str> {
        let rest = self.after.strip_prefix("::").or_else(|| self.after.strip_prefix('.'))?;
        let end = rest.find(|c: char| !is_word_char(c)).unwrap_or(rest.len());
        (end > 0).then(|| &rest[..end])
    }

    /// Whether the token stands where a value is taken apart rather than made.
    fn in_pattern(&self) -> bool {
        let head = self.before.trim_start();
        let lets = ["let ", "if let ", "while let ", "else if let ", "} else if let "].iter().any(|lead| head.starts_with(lead));
        (self.after.contains("=>") && !self.before.contains("=>"))
            || (lets && !self.before.contains('=') && self.after.contains('='))
            || self.before.contains("matches!(")
            || head.starts_with("case ")
            || self.lead().ends_with("instanceof")
            || self.before.contains("isinstance(")
    }

    /// Whether the token is inside a `derive(...)` list.
    fn in_derive(&self) -> bool {
        self.before.rfind("derive(").is_some_and(|open| !self.before[open..].contains(')'))
    }

    /// Whether what follows assigns to it: `x.count = 1`, `x.count += 1`.
    fn assigned(&self) -> bool {
        let after = self.after.trim_start();
        (after.starts_with('=') && !after.starts_with("==") && !after.starts_with("=>"))
            || ["+=", "-=", "*=", "/=", "|=", "&=", "^=", "%=", "<<=", ">>="].iter().any(|op| after.starts_with(op))
    }
}

/// What the symbol is when a token names it: itself, one of its members, one
/// of its cases or one of its fields.
enum Role<'a> {
    Itself,
    Member(&'a str, Do),
    Case(&'a str),
    Field(&'a str),
}

impl Reader {
    fn role(&self, token: &str) -> Role<'_> {
        if last_segment(&self.name) == token {
            return Role::Itself;
        }
        if let Some((member, doing)) = self.members.iter().find(|(member, _)| member == token) {
            return Role::Member(member, *doing);
        }
        if let Some(case) = self.cases.iter().find(|case| *case == token) {
            return Role::Case(case);
        }
        if let Some(field) = self.fields.iter().find(|field| *field == token) {
            return Role::Field(field);
        }
        Role::Itself
    }

    /// Where the reference is on a line the index gave no span for: the
    /// symbol's own name, or else the first of its members, cases or fields
    /// reached through a path or a dot (a bare word is only a last resort).
    fn find(&self, text: &str) -> Option<Range<usize>> {
        if let Some(mark) = mark_of(text, &self.name) {
            return Some(mark);
        }
        let names = self.members.iter().map(|(name, _)| name).chain(&self.cases).chain(&self.fields);
        names
            .flat_map(|name| marks_of(text, name))
            .map(|mark| {
                let reached = text[..mark.start].ends_with('.') || text[..mark.start].ends_with("::");
                (!reached, mark.start, mark)
            })
            .min_by_key(|(bare, start, _)| (*bare, *start))
            .map(|(_, _, mark)| mark)
    }
}

fn verb_of(site: &Site, reader: &Reader) -> (Verb, Option<String>) {
    let text = site.text.as_str();
    let name = last_segment(&reader.name);
    let mark = site.mark.clone().or_else(|| reader.find(text));
    let around = mark.as_ref().and_then(|mark| Around::of(text, mark));
    if matches!(site.rel, Rel::Imports | Rel::Reexports) || starts_import(text) {
        return (Verb::Imports, None);
    }
    if around.as_ref().map_or_else(|| text.contains("derive(") && contains_word(text, name), Around::in_derive) {
        return (Verb::Derives, None);
    }
    if matches!(site.rel, Rel::Implements | Rel::Inherits | Rel::Overrides) || is_impl(text, name) {
        return (Verb::Implements, None);
    }
    if matches!(reader.kind, Kind::Function | Kind::Method) {
        return (Verb::Calls, None);
    }
    let Some(around) = around else { return (from_relation(site.rel).unwrap_or(Verb::Names), None) };
    if matches!(reader.kind, Kind::Trait) && asks_for(&around) {
        return (Verb::AsksFor, None);
    }
    match reader.role(around.token) {
        Role::Member(member, doing) => {
            // A member reached by its bare name is only this type's when the
            // index resolved it or the type is on the line.
            let path = around.before.trim_end().ends_with("::") || around.before.trim_end().ends_with('.');
            if path && (site.exact || contains_word(text, name)) {
                return (verb_of_do(doing), Some(member.to_owned()));
            }
            (from_relation(site.rel).unwrap_or(Verb::Names), None)
        }
        Role::Field(field) => {
            let verb = if around.before.trim_end().ends_with('.') {
                if around.assigned() { Verb::Changes } else { Verb::Reads }
            } else if around.after.trim_start().starts_with(':') {
                Verb::Makes
            } else {
                Verb::Reads
            };
            (verb, Some(field.to_owned()))
        }
        Role::Case(case) => (if around.in_pattern() { Verb::Matches } else { Verb::Makes }, Some(case.to_owned())),
        Role::Itself => itself(&around, reader, site),
    }
}

/// The type's own name: what stands after it says what the place does.
fn itself(around: &Around<'_>, reader: &Reader, site: &Site) -> (Verb, Option<String>) {
    if let Some(next) = around.next() {
        if let Some(case) = reader.cases.iter().find(|case| *case == next) {
            return (if around.in_pattern() { Verb::Matches } else { Verb::Makes }, Some(case.clone()));
        }
        if let Some((member, doing)) = reader.members.iter().find(|(member, _)| member == next) {
            return (verb_of_do(*doing), Some(member.clone()));
        }
        if matches!(next, "new" | "default" | "with_capacity" | "empty") {
            return (Verb::Makes, None);
        }
    }
    let after = around.after.trim_start();
    if after.starts_with('{') && !declares(around.lead()) || after.starts_with('(') {
        return (if around.in_pattern() { Verb::Matches } else { Verb::Makes }, None);
    }
    if around.in_pattern() && after.starts_with("=>") {
        return (Verb::Matches, None);
    }
    if builds_from_text(around.line) {
        return (Verb::Makes, None);
    }
    (from_relation(site.rel).unwrap_or_else(|| type_verb(around)), None)
}

/// What the index's relation alone says a place does, when it says.
const fn from_relation(rel: Rel) -> Option<Verb> {
    match rel {
        Rel::Calls | Rel::MethodCall => Some(Verb::Calls),
        Rel::Reads => Some(Verb::Reads),
        Rel::Writes => Some(Verb::Changes),
        Rel::Documents => Some(Verb::Names),
        _ => None,
    }
}

fn verb_of_do(doing: Do) -> Verb {
    match doing {
        Do::Makes => Verb::Makes,
        Do::Reads => Verb::Reads,
        Do::Changes => Verb::Changes,
        Do::UsesUp => Verb::UsesUp,
    }
}

fn contains_word(text: &str, word: &str) -> bool {
    mark_of(text, word).is_some()
}

fn is_impl(text: &str, name: &str) -> bool {
    let t = text.trim_start();
    (t.starts_with("impl") && contains_word(t, name) && t.contains(" for ")) || (t.starts_with("class ") && t.contains(name) && (t.contains("extends") || t.contains("implements") || t.contains('(')))
}

/// Whether what precedes a name declares it (`struct`, `enum`, `impl`, `class`).
fn declares(lead: &str) -> bool {
    ["struct", "enum", "impl", "trait", "class", "interface", "type", "union"].iter().any(|word| lead.ends_with(word))
}

/// The text before a name with the module path in front of it dropped:
/// `fn f(v: &serde_json::` → `fn f(v: &`.
fn before_path(before: &str) -> &str {
    let mut text = before.trim_end();
    while let Some(rest) = text.strip_suffix("::") {
        text = rest.trim_end_matches(is_word_char).trim_end();
    }
    text
}

/// `T: Serialize`, `+ Serialize`, `impl Serialize`, `where S: Serializer`.
fn asks_for(around: &Around<'_>) -> bool {
    let before = around.lead();
    before.ends_with(':') && !before.ends_with("::") || before.ends_with('+') || before.ends_with("impl") || before.ends_with("dyn") || before.ends_with("where")
}

/// `from_str::<Value>(…)`, `from_slice`, `to_value(`, `json!(`: making one from something else.
fn builds_from_text(text: &str) -> bool {
    ["from_str", "from_slice", "from_reader", "from_value", "to_value", "json!"].iter().any(|word| text.contains(word))
}

/// What a line that names a type does with it, from what stands around it.
fn type_verb(around: &Around<'_>) -> Verb {
    let before = around.lead();
    let after = around.after.trim_start();
    if before.ends_with("&mut") {
        return Verb::Changes;
    }
    if before.ends_with('&') {
        return Verb::Reads;
    }
    let head = around.line.trim_start();
    if around.before.contains("->") {
        return Verb::Makes;
    }
    if head.starts_with("let ") && around.before.contains(':') && !around.before.contains('=') {
        return Verb::Makes;
    }
    // `Vec<Value>` after a `let` is the same thing made; elsewhere it is held.
    if before.ends_with('<') || before.ends_with(',') && around.before.contains('<') {
        return if head.starts_with("let ") { Verb::Makes } else { Verb::Holds };
    }
    let field = head.strip_prefix("pub ").unwrap_or(head);
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
    if let Some(Range { start: at, end }) = mark_of(text, name) {
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
        Site { package: "engine".into(), file: file.into(), path: format!("/w/{file}"), line: 10, text: text.into(), mark: None, rel, exact: true }
    }

    fn value_reader() -> Reader {
        Reader {
            name: "Value".into(),
            kind: Kind::Enum,
            members: vec![("as_str".into(), Do::Reads), ("as_array_mut".into(), Do::Changes), ("try_into".into(), Do::UsesUp), ("from".into(), Do::Makes)],
            cases: vec!["Null".into(), "Array".into(), "String".into()],
            fields: Vec::new(),
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

    /// The place at the `nth` whole-word `token` on the line, as the index would span it.
    fn at(text: &str, token: &str, nth: usize, rel: Rel) -> Site {
        let mark = marks_of(text, token).nth(nth).expect("the token is on the line");
        Site { mark: Some(mark), ..site("src/a.rs", text, rel) }
    }

    #[test]
    fn the_token_where_the_index_put_it_says_what_the_place_does() {
        let value = value_reader();
        let verb = |text: &str, token: &str, nth: usize| {
            let read = read(&at(text, token, nth, Rel::TypeReference), &value);
            (read.verb, read.member)
        };
        // A pattern is left of the arrow; the same case right of it is made.
        assert_eq!(verb("Some(serde_json::Value::Null) => Absent,", "Value", 0), (Verb::Matches, Some("Null".into())));
        assert_eq!(verb("other => serde_json::Value::Null,", "Value", 0), (Verb::Makes, Some("Null".into())));
        assert_eq!(verb("if let Value::Array(items) = v {", "Value", 0), (Verb::Matches, Some("Array".into())));
        assert_eq!(verb("assert!(matches!(v, Value::Array(_)));", "Value", 0), (Verb::Matches, Some("Array".into())));
        assert_eq!(verb("let v = Value::Array(items);", "Value", 0), (Verb::Makes, Some("Array".into())));
        // One line, two references: each is read where it stands.
        assert_eq!(verb("fn wrap(v: &Value) -> Value {", "Value", 0).0, Verb::Reads);
        assert_eq!(verb("fn wrap(v: &Value) -> Value {", "Value", 1).0, Verb::Makes);
        // A member reached through a path or a dot, by the member's own token.
        assert_eq!(verb("row.get(\"a\").and_then(Value::as_str)", "as_str", 0), (Verb::Reads, Some("as_str".into())));
        assert_eq!(verb("map.as_array_mut().unwrap().clear();", "as_array_mut", 0), (Verb::Changes, Some("as_array_mut".into())));
    }

    #[test]
    fn a_field_is_read_or_changed_where_its_token_stands() {
        let info = Reader { name: "AllocationInfo".into(), kind: Kind::Struct, members: Vec::new(), cases: Vec::new(), fields: vec!["count_total".into()], generic: false };
        let verb = |text: &str, nth: usize| {
            let read = read(&at(text, "count_total", nth, Rel::Reads), &info);
            (read.verb, read.member)
        };
        assert_eq!(verb("assert_eq!(info.count_total, 3);", 0), (Verb::Reads, Some("count_total".into())));
        assert_eq!(verb("count_total: info.count_total,", 1), (Verb::Reads, Some("count_total".into())), "the second one is the read");
        assert_eq!(verb("count_total: info.count_total,", 0), (Verb::Makes, Some("count_total".into())), "the first one names a field of the struct being made");
        assert_eq!(verb("stats.count_total += 1;", 0), (Verb::Changes, Some("count_total".into())));
    }

    #[test]
    fn traits_are_derived_implemented_or_asked_for() {
        let r = Reader { name: "Serialize".into(), kind: Kind::Trait, members: Vec::new(), cases: Vec::new(), fields: Vec::new(), generic: false };
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
        let mark = mark_of(text, "Value").expect("marked");
        assert_eq!(&text[mark], "Value");
        assert_eq!(mark_of("ValueError(x)", "Value"), None);
    }
}

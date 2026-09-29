//! The simple symbol page through the real shell, on pages pinned to what
//! the sources write: serde_json's `from_str` (`de.rs:2709`), `Value`
//! (`value/mod.rs`) and `Value::as_str`, allocation-counter's
//! `AllocationInfo`, serde's `Serialize`, Python's `re.match`, zod's
//! `ZodType.parse`, pflag's `FlagSet.Parse`. Every assertion reads what was
//! painted (the probe ledger): the words, never a count.

use crate::model::pages::{
    Arrival, DeclRef, DocFragment, DocSection, DocSections, Excerpt, Gap, GapReason, Known, LineSpan, Member, Members, MethodGroup, OutlinePosition, PageValue, Provenance, ReadFailure, Receiver, ReferenceScope,
    ReferenceSite, Relation, RelationKind, Rose, SectionKind, SignatureText, SourceLocation, SourceSite, SymbolPage,
};
use crate::model::pages::{ByteSpan, FileSpan};
use crate::navigation::{Coordinate, Route, SymbolRoute, View};
use crate::runtime::reads::{PageReader, ReadContext, ReadPool, ReadRequest};
use crate::shell::tests::{Fixture, PACKAGE, Rig, rig_with_reads};
use backend_library::DeclarationKind;
use facet::probe::{Ledger, TextSample};
use gpui::TestAppContext;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

pub(super) fn label(file: &str, line: u32, name: &str) -> String {
    format!("{PACKAGE}::{file}:{line}::{name}")
}

pub(super) fn decl(file: &str, line: u32, name: &str, kind: DeclarationKind) -> DeclRef {
    DeclRef::from_label(&label(file, line, name), None, Some(kind), None).expect("decl")
}

fn signature(text: &str) -> Known<SignatureText> {
    Known::Known(SignatureText { text: Arc::from(text), tokens: Arc::from([]) })
}

fn text(doc: &str) -> Arc<[DocFragment]> {
    Arc::from([DocFragment::Text(Arc::from(doc))])
}

pub(super) fn member(file: &str, line: u32, name: &str, kind: DeclarationKind, sig: &str, summary: Option<&str>) -> Member {
    Member { decl: decl(file, line, name, kind), signature: signature(sig), summary: summary.map(Arc::from), docs: summary.map_or_else(|| Arc::from([]), text), sections: DocSections::default() }
}

fn gap() -> Gap {
    Gap::new(GapReason::NoSemanticPublication, "")
}

pub(super) fn page(identity: DeclRef, sig: &str, doc: Option<&str>, made_of: Vec<Member>, does: Vec<(Receiver, Vec<Member>)>, siblings: Vec<DeclRef>) -> SymbolPage {
    let (path, line) = (identity.path.clone().unwrap_or_else(|| Arc::from("")), identity.line.unwrap_or(1));
    let outline = if siblings.is_empty() {
        Known::Unknown(Gap::new(GapReason::NotServed, ""))
    } else {
        Known::Known(OutlinePosition { ancestors: Arc::from([]), siblings: Arc::from(siblings), index: Some(0) })
    };
    SymbolPage {
        package: Known::Known(crate::model::pages::PackageRef::parse(PACKAGE).expect("package")),
        signature: signature(sig),
        docs: doc.map_or_else(|| Arc::from([]), text),
        sections: DocSections::default(),
        site: SourceSite {
            location: Known::Known(SourceLocation { path, line }),
            excerpt: Known::Known(Excerpt { text: Arc::from(sig), lines: Some(LineSpan { first: line, last: line }), complete: true }),
        },
        members: Known::Known(Members {
            made_of: Arc::from(made_of),
            does: Arc::from(does.into_iter().map(|(receiver, members)| MethodGroup { receiver, members: Arc::from(members) }).collect::<Vec<_>>()),
            other: Arc::from([]),
        }),
        rose: Rose { up: Known::Known(Arc::from([])), down: Known::Known(Arc::from([])), left: Known::Unknown(gap()), right: Known::Known(Arc::from([])), implemented_by: Known::Known(Arc::from([])) },
        references: Known::Known(Arc::from([])),
        outline,
        identity,
    }
}

/// serde_json 1.0.151 `from_str` (`de.rs:2709`), with its docs' lede, an
/// example and its `# Errors` section as the source writes them.
fn from_str() -> SymbolPage {
    let mut page = page(
        decl("de.rs", 2709, "from_str", DeclarationKind::Function),
        "pub fn from_str<'a, T>(s: &'a str) -> Result<T>\nwhere\n    T: de::Deserialize<'a>,",
        Some("Deserialize an instance of type `T` from a string of JSON text."),
        Vec::new(),
        Vec::new(),
        vec![decl("de.rs", 2600, "from_slice", DeclarationKind::Function), decl("de.rs", 2709, "from_str", DeclarationKind::Function), decl("de.rs", 2650, "from_reader", DeclarationKind::Function), decl("ser.rs", 2000, "to_string", DeclarationKind::Function)],
    );
    let errors = "This conversion can fail if the structure of the input does not match the structure expected by `T`, for example if `T` is a struct type but the input contains something other than a JSON map. It can also fail if the structure is correct but `T`'s implementation of `Deserialize` decides that something is wrong with the data, for example required struct fields are missing from the JSON map or some number is too big to fit in the expected primitive type.";
    page.sections = DocSections {
        lead: Arc::from([DocFragment::Text(Arc::from("Deserialize an instance of type `T` from a string of JSON text.")), DocFragment::Break, DocFragment::Break, DocFragment::Code(Arc::from("let u: User = serde_json::from_str(j).unwrap();\nprintln!(\"{:#?}\", u);"))]),
        sections: Arc::from([DocSection { kind: SectionKind::Errors, title: Arc::from("Errors"), body: text(errors), entries: Arc::from([]) }]),
    };
    page
}

fn value() -> SymbolPage {
    let v = |name: &str, sig: &str, doc: &str| member("mod.rs", 116, name, DeclarationKind::Variant, sig, Some(doc));
    let f = |name: &str, sig: &str| member("mod.rs", 300, name, DeclarationKind::Method, sig, None);
    let mut page = page(
        decl("mod.rs", 116, "Value", DeclarationKind::Enum),
        "pub enum Value",
        Some("Represents any valid JSON value."),
        vec![
            v("Null", "Null", "Represents a JSON null value."),
            v("Bool", "Bool(bool)", "Represents a JSON boolean."),
            v("Number", "Number(Number)", "Represents a JSON number, whether integer or floating point."),
            v("String", "String(String)", "Represents a JSON string."),
            v("Array", "Array(Vec<Value>)", "Represents a JSON array."),
            v("Object", "Object(Map<String, Value>)", "Represents a JSON object."),
        ],
        vec![
            (Receiver::Changes, vec![f("as_object_mut", "pub fn as_object_mut(&mut self) -> Option<&mut Map<String, Value>>"), f("take", "pub fn take(&mut self) -> Value")]),
            (Receiver::Reads, vec![f("as_str", "pub fn as_str(&self) -> Option<&str>"), f("as_array", "pub fn as_array(&self) -> Option<&Vec<Value>>"), f("is_null", "pub fn is_null(&self) -> bool")]),
            (Receiver::Makes, vec![f("from", "fn from(f: bool) -> Self"), f("from", "fn from(f: &str) -> Self"), f("from", "fn from(f: f32) -> Self"), f("from_str", "fn from_str(s: &str) -> Result<Value, Self::Err>")]),
        ],
        Vec::new(),
    );
    page.rose.up = Known::Known(Arc::from([relation("Clone", DeclarationKind::Trait), relation("PartialEq", DeclarationKind::Trait), relation("Serialize", DeclarationKind::Trait), relation("Eq", DeclarationKind::Trait)]));
    page
}

fn relation(name: &str, kind: DeclarationKind) -> Relation {
    Relation { decl: decl("lib.rs", 1, name, kind), kind: RelationKind::Semantic(backend_library::SemanticLinkKind::Implements), provenance: Provenance::Structural, arrival: Arrival::NotReported, via: None }
}

fn as_str() -> SymbolPage {
    page(
        decl("mod.rs", 492, "as_str", DeclarationKind::Method),
        "pub fn as_str(&self) -> Option<&str>",
        Some("If the `Value` is a String, returns the associated str. Returns None otherwise."),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
}

fn allocation_info() -> SymbolPage {
    let field = |name: &str, ty: &str, doc: &str| member("lib.rs", 84, name, DeclarationKind::Field, &format!("pub {name}: {ty}"), Some(doc));
    page(
        decl("lib.rs", 84, "AllocationInfo", DeclarationKind::Struct),
        "pub struct AllocationInfo",
        Some("The allocation information obtained by a `measure()` call."),
        vec![
            field("count_total", "u64", "The total number of allocations made during a measure() call."),
            field("count_current", "i64", "The current (net result) number of allocations during a measure() call."),
            field("bytes_total", "u64", "The total amount of bytes allocated during a measure() call."),
        ],
        Vec::new(),
        Vec::new(),
    )
}

fn serialize() -> SymbolPage {
    let mut required = member("ser.rs", 257, "serialize", DeclarationKind::Method, "fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>\n    where\n        S: Serializer;", Some("Serialize this value into the given Serde serializer."));
    required.decl.facts.obligation = Known::Known(Some(backend_library::Obligation::Required));
    page(decl("ser.rs", 256, "Serialize", DeclarationKind::Trait), "pub trait Serialize", Some("A data structure that can be serialized into any data format supported by Serde."), Vec::new(), vec![(Receiver::Reads, vec![required])], Vec::new())
}

fn py_match() -> SymbolPage {
    let mut page = page(
        decl("__init__.py", 164, "match", DeclarationKind::Function),
        "def match(pattern, string, flags=0):",
        Some("Try to apply the pattern at the start of the string, returning a Match object, or None if no match was found."),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    page.sections = DocSections {
        lead: text("Try to apply the pattern at the start of the string, returning a Match object, or None if no match was found."),
        sections: Arc::from([DocSection {
            kind: SectionKind::Errors,
            title: Arc::from("Raises"),
            body: Arc::from([]),
            entries: Arc::from([crate::model::pages::DocEntry { subject: Arc::from("re.error"), body: text("If the pattern itself is invalid.") }]),
        }]),
    };
    page
}

fn zod_parse() -> SymbolPage {
    page(
        decl("schemas.ts", 169, "ZodType::parse", DeclarationKind::Method),
        "parse(data: unknown, params?: core.ParseContext<core.$ZodIssue>): core.output<this>;",
        Some("Parse the data, throwing if it does not fit."),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
}

/// Serves the pinned pages; everything else from the shell's fixture.
struct Pinned;

impl PageReader for Pinned {
    fn read(&mut self, request: &ReadRequest, context: &ReadContext<'_>) -> Result<PageValue, ReadFailure> {
        if let ReadRequest::Symbol(symbol) = request {
            let pinned = match symbol.identity().name() {
                "from_str" => Some(from_str()),
                "Value" => Some(value()),
                "as_str" => Some(as_str()),
                "AllocationInfo" => Some(allocation_info()),
                "Serialize" => Some(serialize()),
                "match" => Some(py_match()),
                "parse" => Some(zod_parse()),
                _ => None,
            };
            if let Some(page) = pinned {
                return Ok(PageValue::Symbol(page));
            }
        }
        Fixture.read(request, context)
    }
}

pub(super) fn route(file: &str, line: u32, name: &str) -> Route {
    Route::Symbol(SymbolRoute {
        project: None,
        package: crate::core::PackageId::new(PACKAGE).expect("package id"),
        id: Coordinate::new(&label(file, line, name)).expect("coordinate"),
        at: None,
        view: View::Page,
        line: None,
        selected: None,
    })
}

fn open(cx: &mut TestAppContext, file: &str, line: u32, name: &str, width: f32) -> (Rig, Ledger) {
    let pool = ReadPool::start(2, |_| Pinned).expect("pinned pool");
    let mut rig = rig_with_reads(cx, Some(route(file, line, name)), width, 900.0, pool);
    rig.cx.update(|_, cx| facet::probe::enable(cx));
    rig.settle();
    rig.repaint();
    let ledger = rig.cx.update(|_, cx| facet::probe::take(cx));
    (rig, ledger)
}

fn texts<'a>(ledger: &'a Ledger, prefix: &str) -> Vec<&'a TextSample> {
    ledger.texts.iter().filter(|text| text.key.starts_with(prefix)).collect()
}

fn says(ledger: &Ledger, key: &str) -> Option<String> {
    ledger.texts.iter().find(|text| text.key == key).map(|text| text.content.clone())
}

fn all(ledger: &Ledger, prefix: &str, suffix: &str) -> Vec<String> {
    ledger.texts.iter().filter(|text| text.key.starts_with(prefix) && text.key.ends_with(suffix)).map(|text| text.content.clone()).collect()
}

#[gpui::test]
fn from_str_is_a_call_with_a_generic_and_a_failure(cx: &mut TestAppContext) {
    let (_rig, ledger) = open(cx, "de.rs", 2709, "from_str", 1440.0);
    // The header.
    assert_eq!(says(&ledger, "s6-kind").as_deref(), Some("FUNCTION"));
    assert_eq!(says(&ledger, "s6-lang").as_deref(), Some("Rust"));
    // The call: one port in plain words, what it gives (a generic you choose), how it fails.
    assert_eq!(says(&ledger, "s6-port-0-name").as_deref(), Some("s"));
    assert_eq!(says(&ledger, "s6-port-0-type-word").as_deref(), Some("text"));
    assert_eq!(says(&ledger, "s6-port-0-type-written").as_deref(), Some("&'a str"));
    assert_eq!(says(&ledger, "s6-gives-label").as_deref(), Some("GIVES"));
    assert_eq!(says(&ledger, "s6-gives-role").as_deref(), Some("you choose it"));
    assert_eq!(says(&ledger, "s6-fail-label").as_deref(), Some("OR FAILS"));
    assert_eq!(says(&ledger, "s6-fail-type-word").as_deref(), Some("Error"));
    assert_eq!(says(&ledger, "s6-fail-when").as_deref(), Some("if the structure of the input does not match the structure expected by T"));
    // The errors section is the section below, not repeated in the docs.
    assert!(ledger.texts.iter().all(|t| !t.key.starts_with("s6-doc-") || !t.content.starts_with("This conversion can fail")), "the errors are not in the docs");
    assert_eq!(says(&ledger, "s6-lede").as_deref(), Some("Deserialize an instance of type T from a string of JSON text."));
}

#[gpui::test]
fn value_is_a_fork_whose_cases_hold_things_in_words(cx: &mut TestAppContext) {
    let (_rig, ledger) = open(cx, "mod.rs", 116, "Value", 1440.0);
    assert_eq!(says(&ledger, "s6-kind").as_deref(), Some("ENUM"));
    assert_eq!(all(&ledger, "s6-case-", "-name"), ["Null", "Bool", "Number", "String", "Array", "Object"]);
    assert_eq!(says(&ledger, "s6-case-1-holds-0-word").as_deref(), Some("yes or no"));
    assert_eq!(says(&ledger, "s6-case-4-holds-0-word").as_deref(), Some("a list of Value"));
    assert_eq!(says(&ledger, "s6-case-5-holds-0-word").as_deref(), Some("a map of text to Value"));
    assert_eq!(says(&ledger, "s6-case-0-nothing").as_deref(), Some("nothing inside"));
    assert_eq!(says(&ledger, "s6-shape-head-count").as_deref(), Some("one of 6"));
    assert_eq!(says(&ledger, "s6-shape-aside-loop").as_deref(), Some("holds more of itself"));
    // The methods, in groups by what they do with it.
    assert_eq!(all(&ledger, "s6-group-", "-head"), ["Makes one", "Reads it", "Changes it"]);
    // `From` is one row that lists what it converts from.
    assert_eq!(says(&ledger, "s6-group-0-method-0-name").as_deref(), Some("from"));
    assert!(says(&ledger, "s6-group-0-method-0-sig").is_some_and(|sig| sig.contains("yes or no") && sig.contains("text")));
    assert_eq!(says(&ledger, "s6-group-0-method-1-name").as_deref(), Some("parse"));
    // What it can do: the traits it implements, as chips.
    assert_eq!(all(&ledger, "s6-cap-", ""), ["copies", "compares with ==", "serde can write it"]);
}

#[gpui::test]
fn a_method_that_may_give_nothing_says_when(cx: &mut TestAppContext) {
    let (_rig, ledger) = open(cx, "mod.rs", 492, "as_str", 1440.0);
    assert_eq!(says(&ledger, "s6-kind").as_deref(), Some("METHOD"));
    assert_eq!(says(&ledger, "s6-recv-type-word").as_deref(), Some("Value"));
    assert_eq!(says(&ledger, "s6-recv-says").as_deref(), Some("reads it"));
    assert_eq!(says(&ledger, "s6-gives-type-word").as_deref(), Some("text"));
    assert_eq!(says(&ledger, "s6-none-label").as_deref(), Some("OR NOTHING"));
    assert_eq!(says(&ledger, "s6-none-when").as_deref(), Some("if it isn't a String"));
    assert!(says(&ledger, "s6-fail-label").is_none(), "as_str cannot fail");
}

#[gpui::test]
fn a_struct_is_a_bracket_of_its_fields(cx: &mut TestAppContext) {
    let (_rig, ledger) = open(cx, "lib.rs", 84, "AllocationInfo", 1440.0);
    assert_eq!(says(&ledger, "s6-kind").as_deref(), Some("STRUCT"));
    assert_eq!(all(&ledger, "s6-field-", "-name"), ["count_total", "count_current", "bytes_total"]);
    assert_eq!(says(&ledger, "s6-field-0-type-word").as_deref(), Some("a count"));
    assert_eq!(says(&ledger, "s6-field-0-type-written").as_deref(), Some("u64"));
    assert_eq!(says(&ledger, "s6-field-1-type-word").as_deref(), Some("an integer"));
    assert_eq!(says(&ledger, "s6-shape-head-count").as_deref(), Some("holds 3"));
}

#[gpui::test]
fn a_trait_is_what_you_write(cx: &mut TestAppContext) {
    let (_rig, ledger) = open(cx, "ser.rs", 256, "Serialize", 1440.0);
    assert_eq!(says(&ledger, "s6-kind").as_deref(), Some("TRAIT"));
    assert_eq!(says(&ledger, "s6-shape-head-title").as_deref(), Some("WHAT YOU WRITE"));
    assert_eq!(says(&ledger, "s6-owed-0-name").as_deref(), Some("serialize"));
    assert_eq!(says(&ledger, "s6-owed-0-takes-word").as_deref(), Some("a serializer"));
}

#[gpui::test]
fn python_without_hints_is_dotted_and_says_how_we_know(cx: &mut TestAppContext) {
    let (_rig, ledger) = open(cx, "__init__.py", 164, "match", 1440.0);
    assert_eq!(says(&ledger, "s6-lang").as_deref(), Some("Python"));
    assert_eq!(all(&ledger, "s6-port-", "-name"), ["pattern", "string", "flags"]);
    assert_eq!(says(&ledger, "s6-port-2-default").as_deref(), Some("= 0"));
    assert_eq!(says(&ledger, "s6-fail-label").as_deref(), Some("OR RAISES"));
    assert_eq!(says(&ledger, "s6-none-when").as_deref(), Some("if no match was found"));
    assert!(says(&ledger, "s6-block-how").is_some_and(|how| how.contains("read from its docs")), "one amber line says where the types came from");
}

#[gpui::test]
fn typescript_keeps_its_declared_types(cx: &mut TestAppContext) {
    let (_rig, ledger) = open(cx, "schemas.ts", 169, "ZodType::parse", 1440.0);
    assert_eq!(says(&ledger, "s6-port-0-type-word").as_deref(), Some("anything"));
    assert_eq!(says(&ledger, "s6-port-1-optional").as_deref(), Some("optional"));
    assert!(says(&ledger, "s6-block-how").is_none(), "declared types need no note");
}

/// Real lines, read from the local files at the index's spans; picking a
/// place opens it. (The workspace list.)
#[allow(dead_code)]
fn workspace_files() -> HashMap<PathBuf, Arc<str>> {
    HashMap::new()
}

#[allow(dead_code)]
fn site(name: &str, file: &str, at: u32, relation: backend_library::SemanticLinkKind) -> ReferenceSite {
    ReferenceSite {
        site: decl(file, 1, name, DeclarationKind::Function),
        relation,
        confidence: backend_library::SemanticConfidence::Compiler,
        span: Known::Known(FileSpan { file: Arc::from(file), bytes: ByteSpan::new(at, at + 8).expect("span") }),
        scope: ReferenceScope::Local,
    }
}

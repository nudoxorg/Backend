//! The simple symbol page through the real shell, on pages pinned to what
//! the sources write: serde_json's `from_str` (`de.rs:2709`), `Value`
//! (`value/mod.rs`) and `Value::as_str`, allocation-counter's
//! `AllocationInfo`, serde's `Serialize`, Python's `re.match`, zod's
//! `ZodType.parse`, pflag's `FlagSet.Parse`. Every assertion reads what was
//! painted (the probe ledger): the words, never a count.

use crate::model::pages::{
    Arrival, DeclRef, DocFragment, DocSection, DocSections, Excerpt, Gap, GapReason, Known, LineSpan, Member, Members, MethodGroup, OutlinePosition, PageValue, Provenance, ReadFailure, Receiver, Relation, RelationKind, Resolution, Rose, SectionKind, SignatureText, SourceLocation, SourceSite, SymbolPage, UseLine,
};
use crate::navigation::{Coordinate, Route, SymbolRoute, View};
use crate::runtime::reads::{PageReader, ReadContext, ReadPool, ReadRequest};
use crate::shell::anatomy_tests::painted;
use crate::shell::tests::{Fixture, PACKAGE, Rig, rig_with_reads};
use backend_library::{DeclarationKind, SemanticLinkKind};
use facet::probe::{Ledger, TextSample};
use gpui::TestAppContext;
use std::rc::Rc;
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
        workspace: Arc::from([]),
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
    page.workspace = workspace();
    page
}

/// One place of your workspace: the line as the read worker put it on the
/// page, with the token the index's span covers marked.
fn place(package: &str, file: &str, line: u32, text: &str, token: &str, relation: SemanticLinkKind) -> UseLine {
    let at = u32::try_from(text.find(token).expect("the token is on the line")).expect("a short line");
    UseLine {
        package: Arc::from(package),
        file: Arc::from(file),
        path: Arc::from(format!("/work/{package}/{file}")),
        line,
        text: Arc::from(text),
        mark: Some(at..at + u32::try_from(token.len()).expect("a short token")),
        relation,
        resolution: Resolution::Resolved,
    }
}

/// Who calls `from_str` in the workspace: two places in `engine`, one in
/// `gui-harness`, and one test in `gui-harness`.
fn workspace() -> Arc<[UseLine]> {
    Arc::from([
        place("engine", "src/a.rs", 12, "let m: Metadata = serde_json::from_str(json)?;", "from_str", SemanticLinkKind::Calls),
        place("engine", "src/b.rs", 40, "let c = serde_json::from_str::<Command>(text)?;", "from_str", SemanticLinkKind::Calls),
        place("gui-harness", "src/c.rs", 7, "let v: Value = serde_json::from_str(&s)?;", "from_str", SemanticLinkKind::Calls),
        place("gui-harness", "tests/t.rs", 9, "assert!(serde_json::from_str::<Command>(bad).is_err());", "from_str", SemanticLinkKind::Calls),
    ])
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
        decl("mod.rs", 492, "Value::as_str", DeclarationKind::Method),
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
pub(super) struct Pinned;

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
    open_in(cx, file, line, name, width, 900.0)
}

/// [`open`] in a window `height` tall (a long page all on screen, so a click can reach any part of it).
fn open_in(cx: &mut TestAppContext, file: &str, line: u32, name: &str, width: f32, height: f32) -> (Rig, Ledger) {
    let pool = ReadPool::start(2, |_| Pinned).expect("pinned pool");
    let mut rig = rig_with_reads(cx, Some(route(file, line, name)), width, height, pool);
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
    let (_rig, ledger) = open(cx, "mod.rs", 492, "Value::as_str", 1440.0);
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

/// Rests the pointer on `key`'s text (its own box) and lets the card rise.
fn rest_on(rig: &mut Rig, ledger: &Ledger, key: &str) -> Ledger {
    let at = texts(ledger, key).first().map(|t| (t.bounds.x + t.bounds.width / 2.0, t.bounds.y + t.bounds.height / 2.0)).unwrap_or_else(|| panic!("`{key}` is not painted"));
    rig.cx.update(|_, cx| facet::probe::enable(cx));
    rig.cx.simulate_mouse_move(gpui::point(gpui::px(at.0), gpui::px(at.1)), None, gpui::Modifiers::default());
    for _ in 0..3 {
        rig.frame(120);
    }
    rig.repaint();
    rig.cx.update(|_, cx| facet::probe::take(cx))
}

/// The generic's pill is a door into progressive disclosure: resting on it
/// says its role in words and what it must be.
#[gpui::test]
fn resting_on_a_generic_says_what_it_must_be(cx: &mut TestAppContext) {
    let (mut rig, ledger) = open(cx, "de.rs", 2709, "from_str", 1440.0);
    assert!(texts(&ledger, "s6-card-").is_empty(), "no card until you rest on something");
    let card = rest_on(&mut rig, &ledger, "s6-gives-type-pill-name");
    assert_eq!(says(&card, "s6-card-gen-title").as_deref(), Some("you choose it"));
    assert_eq!(says(&card, "s6-card-gen-says").as_deref(), Some("You choose it: whatever you read the input into."));
    assert_eq!(says(&card, "s6-card-gen-must-title").is_some() || says(&card, "s6-card-gen-must").is_some(), true);
    assert_eq!(says(&card, "s6-card-gen-must").as_deref(), Some("IT MUST BE"));
    assert_eq!(says(&card, "s6-card-gen-bound-0-name").as_deref(), Some("Deserialize"));
    assert_eq!(says(&card, "s6-card-gen-bound-0-means").as_deref(), Some("can be read by serde (any format)"));
}

/// The error type opens its kinds.
#[gpui::test]
fn resting_on_the_error_says_what_it_is(cx: &mut TestAppContext) {
    let (mut rig, ledger) = open(cx, "de.rs", 2709, "from_str", 1440.0);
    let card = rest_on(&mut rig, &ledger, "s6-fail-type-word");
    let keys: Vec<&str> = card.texts.iter().filter(|t| t.key.contains("card") || t.key.contains("fail")).map(|t| t.key.as_str()).collect();
    assert_eq!(says(&card, "s6-card-err-title").as_deref(), Some("Error"), "{keys:?}");
    assert!(says(&card, "s6-card-err-when").is_some_and(|when| when.starts_with("This conversion can fail if the structure")), "the docs' own words");
}


// ------------------------------------------------------------------ in your workspace


/// The centre of the target `key` (a stop in the reader's walk, a click's place).
fn centre(ledger: &Ledger, key: &str) -> gpui::Point<gpui::Pixels> {
    let target = ledger.targets.iter().find(|target| target.key == key).unwrap_or_else(|| panic!("`{key}` is not a target: {:?}", ledger.targets.iter().map(|t| t.key.as_str()).collect::<Vec<_>>()));
    gpui::point(gpui::px(target.bounds.x + target.bounds.width / 2.0), gpui::px(target.bounds.y + target.bounds.height / 2.0))
}

/// What is painted now, with the ledger recording.
fn now(rig: &mut Rig) -> Ledger {
    painted(rig)
}

#[gpui::test]
fn your_workspace_lists_the_real_lines_by_package_most_places_first(cx: &mut TestAppContext) {
    let (_rig, ledger) = open(cx, "de.rs", 2709, "from_str", 1440.0);
    // Tests are left out until you include them: three places, two packages.
    assert_eq!(all(&ledger, "s6-pkg-", "-name"), ["engine", "gui-harness"]);
    assert_eq!(all(&ledger, "s6-pkg-", "-count"), ["2", "1"]);
    assert_eq!(says(&ledger, "s6-pkg-0-place-0-code").as_deref(), Some("let m: Metadata = serde_json::from_str(json)?;"));
    assert_eq!(says(&ledger, "s6-pkg-0-place-0-place").as_deref(), Some("src/a.rs:12"));
    assert_eq!(says(&ledger, "s6-pkg-1-place-0-code").as_deref(), Some("let v: Value = serde_json::from_str(&s)?;"));
    // What each place does with it: the chips.
    assert_eq!(all(&ledger, "s6-chip-", "-word"), ["calls", "include tests"]);
    // A generic call says what it chose, from the turbofish or the annotation.
    assert_eq!(all(&ledger, "s6-pkg-", "-fill"), ["T = Metadata", "T = Command", "T = Value"]);
}

#[gpui::test]
fn a_click_on_a_place_opens_that_file_at_that_line_in_the_editor(cx: &mut TestAppContext) {
    use crate::host::editor::{Command, Launch};
    use std::cell::RefCell;
    struct Recorder(Rc<RefCell<Vec<Command>>>);
    impl Launch for Recorder {
        fn run(&self, command: &Command) -> std::io::Result<()> {
            self.0.borrow_mut().push(command.clone());
            Ok(())
        }
    }
    let ran = Rc::new(RefCell::new(Vec::new()));
    let (mut rig, _) = open_in(cx, "de.rs", 2709, "from_str", 1440.0, 2600.0);
    rig.cx.update(|_, cx| crate::host::editor::install(Rc::new(Recorder(Rc::clone(&ran))), cx));
    let ledger = now(&mut rig);
    let at = centre(&ledger, "s6-pkg-1-place-0");
    rig.cx.simulate_click(at, gpui::Modifiers::default());
    rig.settle();
    assert_eq!(*ran.borrow(), [Command { program: "code".into(), args: vec!["-g".into(), "/work/gui-harness/src/c.rs:7".into()] }], "the file the place is in, at its line");
}

#[gpui::test]
fn picking_a_package_from_the_menu_narrows_the_list_and_escape_closes_the_menu(cx: &mut TestAppContext) {
    let (mut rig, _) = open(cx, "de.rs", 2709, "from_str", 1440.0);
    let ledger = now(&mut rig);
    rig.cx.simulate_click(centre(&ledger, "s6-picker"), gpui::Modifiers::default());
    rig.settle();
    let menu_key: gpui::ElementId = "s6-menu".to_owned().into();
    assert!(rig.cx.update(|window, cx| facet::overlay::float::is_open(&menu_key, window, cx)), "the chip opens the menu");
    rig.keys("down");
    assert!(rig.cx.update(|window, cx| facet::overlay::float::is_open(&menu_key, window, cx)), "an arrow walks the menu, it does not close it");
    rig.keys("escape");
    assert!(!rig.cx.update(|window, cx| facet::overlay::float::is_open(&menu_key, window, cx)), "Escape closes it");
    let ledger = now(&mut rig);
    rig.cx.simulate_click(centre(&ledger, "s6-picker"), gpui::Modifiers::default());
    rig.settle();
    // The menu opens on the first row (all packages): engine, then gui-harness, two rows down.
    assert!(rig.cx.update(|window, cx| facet::overlay::float::is_open(&menu_key, window, cx)), "the menu is open again");
    rig.keys("down down enter");
    assert!(!rig.cx.update(|window, cx| facet::overlay::float::is_open(&menu_key, window, cx)), "choosing a row closes the menu");
    let ledger = now(&mut rig);
    assert_eq!(says(&ledger, "s6-picker-name").as_deref(), Some("gui-harness"), "the chip names the package chosen");
    assert_eq!(all(&ledger, "s6-pkg-", "-name"), ["gui-harness"], "only its places are listed");
}

#[gpui::test]
fn clicking_a_chip_filters_to_that_verb_and_a_test_place_shows_when_tests_are_included(cx: &mut TestAppContext) {
    let (mut rig, _) = open(cx, "de.rs", 2709, "from_str", 1440.0);
    let ledger = now(&mut rig);
    let tests = ledger.targets.iter().find(|target| target.key.starts_with("s6-chip-tests") || target.key == "s6-chip-tests").map(|target| target.key.clone()).expect("include tests is a chip");
    rig.cx.simulate_click(centre(&ledger, &tests), gpui::Modifiers::default());
    rig.settle();
    let ledger = now(&mut rig);
    assert_eq!(all(&ledger, "s6-pkg-", "-count"), ["2", "2"], "gui-harness now counts its test place too");
}

// ------------------------------------------------------------------ fit

use crate::shell::fit_tests::{findings, resize};

/// What the page painted, by the words of each text and where it sits: what a
/// fresh window and a window that was resized about must agree on.
fn layout_of(ledger: &Ledger) -> std::collections::BTreeMap<String, (String, i32, i32, i32)> {
    ledger
        .texts
        .iter()
        .filter(|text| text.key.starts_with("s6-"))
        .map(|text| (text.key.clone(), (text.content.clone(), text.bounds.x.round() as i32, text.bounds.y.round() as i32, text.bounds.width.round() as i32)))
        .collect()
}

/// A phone shows the page whole: nothing of it is cut mid-glyph, hangs past
/// the window's edge, lies where nothing shows it, or sits over other text.
#[gpui::test]
fn on_a_phone_the_page_fits_the_window_with_nothing_cut_or_hanging_past_the_edge(cx: &mut TestAppContext) {
    let (mut rig, _) = open(cx, "de.rs", 2709, "from_str", 1440.0);
    let mut wrong = Vec::new();
    for (width, height) in [(320.0, 568.0), (360.0, 640.0), (390.0, 844.0), (430.0, 932.0), (800.0, 900.0)] {
        resize(&mut rig, width, height);
        let ledger = painted(&mut rig);
        assert!(ledger.texts.iter().any(|text| text.key == "s6-pkg-0-name"), "{width}: the workspace is on the page");
        let found = findings(&ledger, width, height);
        wrong.extend(found.iter().filter(|finding| finding.what.contains("[s6-")).map(|finding| format!("{width:.0} px: {finding}")));
    }
    assert!(wrong.is_empty(), "{} findings:\n{}", wrong.len(), wrong.join("\n"));
}

/// The page after a storm of resizes is the page a fresh window of that size
/// paints: modes are a function of the room, not a memory of the drag.
#[gpui::test]
fn after_a_resize_storm_the_page_is_what_a_fresh_window_paints(cx: &mut TestAppContext) {
    let (mut stormed, _) = open(cx, "de.rs", 2709, "from_str", 1440.0);
    for width in [1300.0, 1000.0, 720.0, 320.0, 900.0, 1500.0, 640.0, 360.0, 1240.0, 800.0] {
        resize(&mut stormed, width, 900.0);
    }
    let after = layout_of(&painted(&mut stormed));
    drop(stormed);
    let (mut fresh, _) = open(cx, "de.rs", 2709, "from_str", 800.0);
    let fresh = layout_of(&painted(&mut fresh));
    assert_eq!(after.keys().collect::<Vec<_>>(), fresh.keys().collect::<Vec<_>>(), "the same things are on the page");
    let moved: Vec<String> = after.iter().filter(|(key, at)| fresh.get(*key) != Some(at)).map(|(key, at)| format!("{key}: {at:?} vs {:?}", fresh.get(key))).collect();
    assert!(moved.is_empty(), "{} places differ from a fresh window:\n{}", moved.len(), moved.join("\n"));
}

/// Every action on the page is a stop in the keyboard walk, and every stop is
/// at least 24 px square: nothing a mouse can do is out of a keyboard's reach.
#[gpui::test]
fn every_action_on_the_page_is_a_stop_in_the_walk_and_at_least_24_px(cx: &mut TestAppContext) {
    let (mut rig, _) = open_in(cx, "de.rs", 2709, "from_str", 1440.0, 2600.0);
    let ledger = now(&mut rig);
    let stops: Vec<&str> = ledger.targets.iter().filter(|target| target.key.starts_with("s6-")).map(|target| target.key.as_str()).collect();
    for wanted in [
        "s6-example-0-line",       // a fold
        "s6-gives-type-pill",      // a generic's card
        "s6-fail-type-card",       // an error's card
        "s6-picker",               // the package menu
        "s6-chip-0",               // a verb filter
        "s6-chip-tests",           // include tests
        "s6-pkg-0-place-0",        // a place opens its file
        "s6-block-source",         // the source link
    ] {
        assert!(stops.contains(&wanted), "`{wanted}` is not a stop in the walk: {stops:?}");
    }
    let small: Vec<String> = ledger
        .targets
        .iter()
        .filter(|target| target.key.starts_with("s6-") && (target.bounds.height < 23.5 || target.bounds.width < 23.5))
        .map(|target| format!("{} {:.0}x{:.0}", target.key, target.bounds.width, target.bounds.height))
        .collect();
    assert!(small.is_empty(), "stops under 24 px: {small:?}");
}

// ------------------------------------------------------------------ the hover card's clock

/// Steps virtual time `step` ms at a time until `wanted` holds of what is painted; the ms it took.
fn until(rig: &mut Rig, step: u64, limit: u64, wanted: impl Fn(&Ledger) -> bool) -> Option<u64> {
    let mut at = 0;
    while at <= limit {
        if wanted(&painted(rig)) {
            return Some(at);
        }
        rig.frame(step);
        at += step;
    }
    None
}

/// The card is up soon after the pointer comes to rest on its word, and gone soon
/// after it leaves (measured in virtual time: the clock the motion runs on).
#[gpui::test]
fn the_generic_card_rises_after_the_rest_and_is_gone_after_the_pointer_leaves(cx: &mut TestAppContext) {
    let (mut rig, ledger) = open(cx, "de.rs", 2709, "from_str", 1440.0);
    let word = texts(&ledger, "s6-gives-type-pill-name").first().map(|t| (t.bounds.x + t.bounds.width / 2.0, t.bounds.y + t.bounds.height / 2.0)).expect("the pill is painted");
    rig.cx.simulate_mouse_move(gpui::point(gpui::px(word.0), gpui::px(word.1)), None, gpui::Modifiers::default());
    let up = until(&mut rig, 8, 400, |ledger| ledger.texts.iter().any(|t| t.key == "s6-card-gen-title")).expect("the card never came up");
    eprintln!("HOVER-CARD: painted {up} ms after the pointer came to rest");
    assert!((100..=150).contains(&up), "the card is up between the 120 ms rest and 150 ms: {up} ms");
    // Let it finish arriving, then leave.
    for _ in 0..4 {
        rig.frame(120);
    }
    rig.cx.simulate_mouse_move(gpui::point(gpui::px(4.0), gpui::px(880.0)), None, gpui::Modifiers::default());
    let gone = until(&mut rig, 8, 1200, |ledger| !ledger.texts.iter().any(|t| t.key.starts_with("s6-card-gen-"))).expect("the card never went");
    eprintln!("HOVER-CARD: gone {gone} ms after the pointer left");
    assert!(gone <= 600, "a card the pointer left goes: {gone} ms");
}

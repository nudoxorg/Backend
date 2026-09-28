//! The drawn page through the real shell, on pages pinned to what the
//! harness fixture index returns for them (`NUDOX_PAGE_DUMP`, 2026-09-28,
//! copied verbatim): toml's `Value` and present's `RelationLabel` (Rust),
//! pflag's `ErrorHandling` and its constants (Go), zod's `$ZodStringFormats`
//! (TypeScript). Every assertion reads what was painted (the probe ledger).
//!
//! Law 1 (the lead, wave 6): at 1440×900 the hero, the specimen and the
//! first in-section are wholly on the first screen.

use crate::model::pages::{
    DeclRef, DocFragment, DocSections, Excerpt, Gap, GapReason, Known, LineSpan, Member, Members, MethodGroup,
    OutlinePosition, PageValue, ReadFailure, Receiver, Rose, SignatureText, SourceLocation, SourceSite, SymbolPage,
};
use crate::navigation::{Coordinate, Route, SymbolRoute, View};
use crate::runtime::reads::{PageReader, ReadContext, ReadPool, ReadRequest};
use crate::shell::tests::{Fixture, PACKAGE, Rig, rig_with_reads};
use backend_library::DeclarationKind;
use facet::probe::{Ledger, TextSample};
use gpui::TestAppContext;
use std::sync::Arc;

fn label(file: &str, line: u32, name: &str) -> String {
    format!("{PACKAGE}::{file}:{line}::{name}")
}

fn decl(file: &str, line: u32, name: &str, kind: DeclarationKind) -> DeclRef {
    DeclRef::from_label(&label(file, line, name), None, Some(kind), None).expect("decl")
}

fn signature(text: &str) -> Known<SignatureText> {
    Known::Known(SignatureText { text: Arc::from(text), tokens: Arc::from([]) })
}

fn member(file: &str, line: u32, name: &str, kind: DeclarationKind, sig: &str, summary: Option<&str>) -> Member {
    Member {
        decl: decl(file, line, name, kind),
        signature: signature(sig),
        summary: summary.map(Arc::from),
        docs: summary.map_or_else(|| Arc::from([]), |summary| Arc::from([DocFragment::Text(Arc::from(summary))])),
        sections: DocSections::default(),
    }
}

fn gap() -> Gap {
    Gap::new(GapReason::NoSemanticPublication, "")
}

fn page(identity: DeclRef, sig: &str, doc: Option<&str>, made_of: Vec<Member>, does: Vec<(Receiver, Vec<Member>)>, siblings: Vec<DeclRef>) -> SymbolPage {
    let (path, line) = (identity.path.clone().unwrap_or_else(|| Arc::from("")), identity.line.unwrap_or(1));
    let outline = if siblings.is_empty() {
        Known::Unknown(Gap::new(GapReason::NotServed, ""))
    } else {
        Known::Known(OutlinePosition { ancestors: Arc::from([]), siblings: Arc::from(siblings), index: Some(0) })
    };
    SymbolPage {
        package: Known::Known(crate::model::pages::PackageRef::parse(PACKAGE).expect("package")),
        signature: signature(sig),
        docs: doc.map_or_else(|| Arc::from([]), |doc| Arc::from([DocFragment::Text(Arc::from(doc))])),
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
        rose: Rose {
            up: Known::Known(Arc::from([])),
            down: Known::Known(Arc::from([])),
            left: Known::Unknown(gap()),
            right: Known::Known(Arc::from([])),
            implemented_by: Known::Known(Arc::from([])),
        },
        references: Known::Known(Arc::from([])),
        outline,
        identity,
    }
}

fn value() -> SymbolPage {
    let v = |name: &str, sig: &str, doc: &str| member("value.rs", 25, name, DeclarationKind::Variant, sig, Some(doc));
    let f = |name: &str, sig: &str| member("value.rs", 200, name, DeclarationKind::Method, sig, None);
    page(
        decl("value.rs", 25, "Value", DeclarationKind::Enum),
        "pub enum Value",
        Some("Representation of a TOML value."),
        vec![
            v("String", "String(String)", "Represents a TOML string"),
            v("Integer", "Integer(i64)", "Represents a TOML integer"),
            v("Float", "Float(f64)", "Represents a TOML float"),
            v("Boolean", "Boolean(bool)", "Represents a TOML boolean"),
            v("Datetime", "Datetime(Datetime)", "Represents a TOML datetime"),
            v("Array", "Array(Array)", "Represents a TOML array"),
            v("Table", "Table(Table)", "Represents a TOML table"),
        ],
        vec![
            (Receiver::Changes, vec![
                f("as_array_mut", "pub fn as_array_mut(&mut self) -> Option<&mut Vec<Value>>"),
                f("as_table_mut", "pub fn as_table_mut(&mut self) -> Option<&mut Table>"),
                f("get_mut", "pub fn get_mut<I: Index>(&mut self, index: I) -> Option<&mut Value>"),
                f("index_mut", "fn index_mut(&mut self, index: I) -> &mut Value"),
            ]),
            (Receiver::Reads, vec![
                f("as_array", "pub fn as_array(&self) -> Option<&Vec<Value>>"),
                f("as_bool", "pub fn as_bool(&self) -> Option<bool>"),
                f("as_datetime", "pub fn as_datetime(&self) -> Option<&Datetime>"),
                f("as_float", "pub fn as_float(&self) -> Option<f64>"),
                f("as_integer", "pub fn as_integer(&self) -> Option<i64>"),
                f("as_str", "pub fn as_str(&self) -> Option<&str>"),
                f("as_table", "pub fn as_table(&self) -> Option<&Table>"),
                f("fmt", "fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result"),
                f("get", "pub fn get<I: Index>(&self, index: I) -> Option<&Value>"),
                f("index", "fn index(&self, index: I) -> &Value"),
                f("is_array", "pub fn is_array(&self) -> bool"),
                f("is_bool", "pub fn is_bool(&self) -> bool"),
                f("is_datetime", "pub fn is_datetime(&self) -> bool"),
                f("is_float", "pub fn is_float(&self) -> bool"),
                f("is_integer", "pub fn is_integer(&self) -> bool"),
                f("is_str", "pub fn is_str(&self) -> bool"),
                f("is_table", "pub fn is_table(&self) -> bool"),
                f("same_type", "pub fn same_type(&self, other: &Value) -> bool"),
                f("type_str", "pub fn type_str(&self) -> &'static str"),
            ]),
            (Receiver::Consumes, vec![
                f("try_into", "pub fn try_into<'de, T>(self) -> Result<T, crate::de::Error>\n    where\n        T: de::Deserialize<'de>,"),
            ]),
            (Receiver::Makes, vec![
                f("from", "fn from(val: &'a str) -> Value"),
                f("from", "fn from(val: Vec<V>) -> Value"),
                f("from", "fn from(val: BTreeMap<S, V>) -> Value"),
                f("from", "fn from(val: HashMap<S, V>) -> Value"),
                f("from_str", "fn from_str(s: &str) -> Result<Value, Self::Err>"),
                f("try_from", "pub fn try_from<T>(value: T) -> Result<Value, crate::ser::Error>\n    where\n        T: ser::Serialize,"),
            ]),
        ],
        Vec::new(),
    )
}

fn relation_label() -> SymbolPage {
    let v = |name: &str, sig: &str, doc: &str| member("glyph.rs", 139, name, DeclarationKind::Variant, sig, Some(doc));
    page(
        decl("glyph.rs", 138, "RelationLabel", DeclarationKind::Enum),
        "pub enum RelationLabel",
        Some("The readable label of one relation group."),
        vec![
            v("Typed", "Typed(SemanticLinkKind, RelationDirection)", "A relation whose compiler kind and direction are both known."),
            v("Neighbourhood", "Neighbourhood", "A bounded neighbourhood whose per-edge kind the reply did not carry."),
            v("Related", "Related", "Incoming and outgoing neighbours whose per-edge kind is not carried."),
        ],
        vec![
            (Receiver::Reads, vec![member("glyph.rs", 150, "fmt", DeclarationKind::Method, "fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result", None)]),
            (Receiver::Consumes, vec![member("glyph.rs", 160, "as_str", DeclarationKind::Method, "pub const fn as_str(self) -> &'static str", None)]),
        ],
        Vec::new(),
    )
}

/// pflag's `ErrorHandling` and the constants after it in `flag.go`.
const GO_CONSTANTS: [(&str, u32, &str); 3] = [
    ("ContinueOnError", 133, "ContinueOnError ErrorHandling = iota"),
    ("ExitOnError", 135, "ExitOnError"),
    ("PanicOnError", 137, "PanicOnError"),
];

fn error_handling() -> SymbolPage {
    let mut siblings = vec![decl("flag.go", 129, "ErrorHandling", DeclarationKind::Type)];
    siblings.extend(GO_CONSTANTS.iter().map(|(name, line, _)| decl("flag.go", *line, name, DeclarationKind::Constant)));
    siblings.push(decl("flag.go", 148, "ParseErrorsWhitelist", DeclarationKind::Type));
    page(decl("flag.go", 129, "ErrorHandling", DeclarationKind::Type), "ErrorHandling int", Some("ErrorHandling defines how to handle flag parsing errors."), Vec::new(), Vec::new(), siblings)
}

fn go_constant(name: &str) -> Option<SymbolPage> {
    let (name, line, sig) = GO_CONSTANTS.iter().find(|(constant, ..)| *constant == name)?;
    Some(page(decl("flag.go", *line, name, DeclarationKind::Constant), sig, None, Vec::new(), Vec::new(), Vec::new()))
}

fn string_formats() -> SymbolPage {
    page(
        decl("stubs.ts", 3, "$ZodStringFormats", DeclarationKind::Type),
        "export type $ZodStringFormats = \"email\" | \"url\" | \"uuid\" | \"regex\" | \"jwt\" | \"starts_with\" | \"ends_with\" | \"includes\";",
        None,
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
                "Value" => Some(value()),
                "RelationLabel" => Some(relation_label()),
                "ErrorHandling" => Some(error_handling()),
                "$ZodStringFormats" => Some(string_formats()),
                other => go_constant(other),
            };
            if let Some(page) = pinned {
                return Ok(PageValue::Symbol(page));
            }
        }
        Fixture.read(request, context)
    }
}

fn route(file: &str, line: u32, name: &str) -> Route {
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

fn bottom(text: &TextSample) -> f32 {
    text.bounds.y + text.bounds.height
}

/// The first screen holds the hero, the specimen and the first in-section,
/// whole: every painted text of theirs ends above y = 900.
fn first_screen(ledger: &Ledger, name: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let title = texts(ledger, "name:0:");
    assert!(!title.is_empty(), "{name}: no title painted");
    parts.push(("title", title));
    parts.push(("specimen", texts(ledger, "page-case-")));
    let getting = texts(ledger, "page-section-Getting");
    let has_getting = !getting.is_empty();
    parts.push(("Getting one", getting));
    parts.push(("rails", texts(ledger, "page-rail-")));
    let mut out = Vec::new();
    for (part, samples) in parts {
        for sample in samples {
            assert!(
                bottom(sample) <= 900.0,
                "{name}: {part} `{}` ends at y {} (below the first 1440×900 screen)",
                sample.content,
                bottom(sample)
            );
        }
    }
    if has_getting {
        out.push("Getting one".to_owned());
    }
    out
}

#[gpui::test]
fn a_rust_enum_page_is_hero_fork_and_getting_one_on_the_first_screen(cx: &mut TestAppContext) {
    let (mut rig, ledger) = open(cx, "value.rs", 25, "Value", 1440.0);
    assert_eq!(first_screen(&ledger, "Value"), ["Getting one"], "Value's first in-section is drawn");
    // One tine per case, 24 px apart, each with the accessors that read it.
    let names = texts(&ledger, "page-case-").into_iter().filter(|text| text.key.ends_with("-name")).collect::<Vec<_>>();
    assert_eq!(names.iter().map(|text| text.content.as_str()).collect::<Vec<_>>(), ["String", "Integer", "Float", "Boolean", "Datetime", "Array", "Table"]);
    for pair in names.windows(2) {
        assert!((pair[1].bounds.y - pair[0].bounds.y - 24.0).abs() < 0.5, "one row per case at a 24 px pitch: {} → {}", pair[0].content, pair[1].content);
    }
    let accessors = texts(&ledger, "page-case-0-acc-").into_iter().map(|text| text.content.as_str()).collect::<Vec<_>>();
    assert_eq!(accessors, ["as_str", "is_str"], "String's tine carries what reads it");
    // Getting one, best first: the parse from text leads.
    let verbs = texts(&ledger, "page-rail-").into_iter().filter(|text| text.key.ends_with("-verb")).map(|text| text.content.as_str()).collect::<Vec<_>>();
    assert_eq!(verbs.first().copied(), Some("parse"), "rails: {verbs:?}");
    // Nothing the chrome already says, and no narration.
    let said = rig.said();
    for gone in ["Reference", "Relations", "Usage", "History", "Source", "enum", "value.rs:25", "Value has 7 variants."] {
        assert!(!said.iter().any(|line| line == gone || line.starts_with("Value has ")), "`{gone}` is still said: {said:#?}");
    }
    // The docs of a case ride its hover, never the row at rest.
    assert!(!said.iter().any(|line| line == "Represents a TOML string"), "a case's doc is at rest: {said:#?}");
}

#[gpui::test]
fn relation_label_is_hero_and_fork_on_the_first_screen(cx: &mut TestAppContext) {
    let (_rig, ledger) = open(cx, "glyph.rs", 138, "RelationLabel", 1440.0);
    first_screen(&ledger, "RelationLabel");
    let names = texts(&ledger, "page-case-").into_iter().filter(|text| text.key.ends_with("-name")).map(|text| text.content.as_str()).collect::<Vec<_>>();
    assert_eq!(names, ["Typed", "Neighbourhood", "Related"]);
    let carries = texts(&ledger, "page-case-0-carries-").into_iter().filter(|text| text.key.contains("-tok-")).map(|text| text.content.as_str()).collect::<Vec<_>>();
    assert_eq!(carries, ["SemanticLinkKind", "RelationDirection"], "Typed carries both, in words");
}

#[gpui::test]
fn a_go_named_type_forks_into_its_constants_with_their_values(cx: &mut TestAppContext) {
    let (_rig, ledger) = open(cx, "flag.go", 129, "ErrorHandling", 1440.0);
    first_screen(&ledger, "ErrorHandling");
    let rows = texts(&ledger, "page-case-")
        .into_iter()
        .filter(|text| text.key.ends_with("-name") || text.key.ends_with("-value"))
        .map(|text| text.content.as_str())
        .collect::<Vec<_>>();
    assert_eq!(rows, ["ContinueOnError", "0", "ExitOnError", "1", "PanicOnError", "2"], "iota, printed");
    assert_eq!(texts(&ledger, "page-open").first().map(|text| text.content.as_str()), Some("or any int"), "Go can't close it");
}

#[gpui::test]
fn a_typescript_literal_union_folds_past_seven(cx: &mut TestAppContext) {
    let (_rig, ledger) = open(cx, "stubs.ts", 3, "$ZodStringFormats", 1440.0);
    first_screen(&ledger, "$ZodStringFormats");
    let names = texts(&ledger, "page-case-").into_iter().filter(|text| text.key.ends_with("-name")).map(|text| text.content.as_str()).collect::<Vec<_>>();
    assert_eq!(names, ["\"email\"", "\"url\"", "\"uuid\"", "\"regex\"", "\"jwt\"", "\"starts_with\"", "\"ends_with\""]);
    assert_eq!(texts(&ledger, "page-cases-more-words").first().map(|text| text.content.as_str()), Some("and 1 more"));
}

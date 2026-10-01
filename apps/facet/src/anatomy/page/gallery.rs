//! The drawn page in the gallery: the reader region of the desktop at 1440
//! and at 760 (the window less the shelf and the titlebar), with pages
//! compiled from what the harness fixture index returns for them
//! (`NUDOX_PAGE_DUMP`, 2026-09-28, copied verbatim): toml's `Value` and
//! present's `RelationLabel` (Rust), pflag's `ErrorHandling` and its
//! constants (Go), zod's `$ZodStringFormats` (TypeScript). The Go
//! constants' texts are their declaration lines in `flag.go:133-137`, not yet
//! a dump (the fixture index could not be rebuilt on 2026-09-28).
//!
//! No world, no shell: the page is what the index alone gives it. The doors
//! are the shell's; here the page is still (hover is the gallery's own).

use super::{Anchors, Capability, DoesGroup, DoesRow, Geometry, Still, does, page, said};
use crate::anatomy::plan::{
    DeclKind, Doers, DropVerb, Effect, Lang, Owes, PagePlan, SectionFacts, SectionId, Site, Source, SourceFailure, SourceMember, Tier, Uses, VerbRow,
    compile, spelled,
};
use crate::overlay::float;
use crate::semantics::recorded::{Language, callable};
use crate::theme::ActiveFacet;
use crate::tokens::scale;
use gpui::{AnyView, App, AppContext, Context, IntoElement, ParentElement, Render, Styled, Window, div, px};

/// The desktop's reader at a 1440 × 900 window (the shelf is 264, the
/// titlebar 50), and at 760 (the shelf folds to its 42 px spine).
pub(crate) const WIDE: (u32, u32) = (1176, 850);
pub(crate) const NARROW: (u32, u32) = (718, 850);

/// Which pinned page a scene draws.
#[derive(Clone, Copy)]
pub(crate) enum PageOf {
    Value,
    RelationLabel,
    ErrorHandling,
    StringFormats,
    FromStr,
    Parse,
    ZodParse,
    Serialize,
    GoValue,
    Collection,
}

fn member(name: &str, kind: DeclKind, signature: &str, summary: Option<&str>, effect: Effect) -> SourceMember {
    SourceMember { name: name.to_owned(), kind, signature: Some(signature.to_owned()), summary: summary.map(ToOwned::to_owned), deprecated: None, effect, link: None, owes: Default::default() }
}

fn source(name: &str, kind: DeclKind, lang: Lang, signature: &str, lede: Option<&str>, made_of: Vec<SourceMember>, does: Vec<SourceMember>) -> Source {
    Source {
        name: name.to_owned(),
        owner: None,
        kind,
        lang,
        package: String::new(),
        version: None,
        module: String::new(),
        signature: Some(signature.to_owned()),
        lede: lede.map(ToOwned::to_owned),
        deprecated: None,
        made_of,
        extends: Vec::new(),
        sections: Vec::new(),
        does,
        rails: Vec::new(),
        since: None,
        yours: false,
        failures: Vec::new(),
        uses: Default::default(),
        doers: Default::default(),
        reach: Default::default(),
        caps: Vec::new(),
        siblings: Vec::new(),
        scope: String::new(),
        history: Default::default(),
    }
}

/// The sections the index gives a page facts for: Getting one when it has
/// makers, What it does when it has operations of its own.
fn sections(source: &mut Source) {
    let makers = source.does.iter().filter(|m| m.effect == Effect::Makes).count();
    let own = source.does.iter().filter(|m| m.effect != Effect::Makes).count();
    if makers > 0 {
        source.sections.push(SectionFacts { id: SectionId::Getting, count: u32::try_from(makers).ok(), yours: None, tier: Tier::Compiler });
    }
    if own > 0 {
        source.sections.push(SectionFacts { id: SectionId::Does, count: u32::try_from(own).ok(), yours: None, tier: Tier::Compiler });
    }
    if !source.failures.is_empty() {
        source.sections.push(SectionFacts { id: SectionId::Fails, count: None, yours: None, tier: Tier::Compiler });
    }
}

fn value() -> (Source, crate::icons::Kind) {
    let v = |name: &str, sig: &str, doc: &str| member(name, DeclKind::Variant, sig, Some(doc), Effect::None);
    let f = |name: &str, effect: Effect, sig: &str| member(name, DeclKind::Method, sig, None, effect);
    let mut s = source("Value", DeclKind::Enum, Lang::Rust, "pub enum Value", Some("Representation of a TOML value."), vec![
        v("String", "String(String)", "Represents a TOML string"),
        v("Integer", "Integer(i64)", "Represents a TOML integer"),
        v("Float", "Float(f64)", "Represents a TOML float"),
        v("Boolean", "Boolean(bool)", "Represents a TOML boolean"),
        v("Datetime", "Datetime(Datetime)", "Represents a TOML datetime"),
        v("Array", "Array(Array)", "Represents a TOML array"),
        v("Table", "Table(Table)", "Represents a TOML table"),
    ], vec![
        f("as_array_mut", Effect::Changes, "pub fn as_array_mut(&mut self) -> Option<&mut Vec<Value>>"),
        f("as_table_mut", Effect::Changes, "pub fn as_table_mut(&mut self) -> Option<&mut Table>"),
        f("get_mut", Effect::Changes, "pub fn get_mut<I: Index>(&mut self, index: I) -> Option<&mut Value>"),
        f("index_mut", Effect::Changes, "fn index_mut(&mut self, index: I) -> &mut Value"),
        f("as_array", Effect::Reads, "pub fn as_array(&self) -> Option<&Vec<Value>>"),
        f("as_bool", Effect::Reads, "pub fn as_bool(&self) -> Option<bool>"),
        f("as_datetime", Effect::Reads, "pub fn as_datetime(&self) -> Option<&Datetime>"),
        f("as_float", Effect::Reads, "pub fn as_float(&self) -> Option<f64>"),
        f("as_integer", Effect::Reads, "pub fn as_integer(&self) -> Option<i64>"),
        f("as_str", Effect::Reads, "pub fn as_str(&self) -> Option<&str>"),
        f("as_table", Effect::Reads, "pub fn as_table(&self) -> Option<&Table>"),
        f("fmt", Effect::Reads, "fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result"),
        f("get", Effect::Reads, "pub fn get<I: Index>(&self, index: I) -> Option<&Value>"),
        f("index", Effect::Reads, "fn index(&self, index: I) -> &Value"),
        f("is_array", Effect::Reads, "pub fn is_array(&self) -> bool"),
        f("is_bool", Effect::Reads, "pub fn is_bool(&self) -> bool"),
        f("is_datetime", Effect::Reads, "pub fn is_datetime(&self) -> bool"),
        f("is_float", Effect::Reads, "pub fn is_float(&self) -> bool"),
        f("is_integer", Effect::Reads, "pub fn is_integer(&self) -> bool"),
        f("is_str", Effect::Reads, "pub fn is_str(&self) -> bool"),
        f("is_table", Effect::Reads, "pub fn is_table(&self) -> bool"),
        f("same_type", Effect::Reads, "pub fn same_type(&self, other: &Value) -> bool"),
        f("serialize", Effect::Reads, "fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>\n    where\n        S: ser::Serializer,"),
        f("type_str", Effect::Reads, "pub fn type_str(&self) -> &'static str"),
        f("deserialize_any", Effect::UsesUp, "fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, crate::de::Error>\n    where\n        V: de::Visitor<'de>,"),
        f("into_deserializer", Effect::UsesUp, "fn into_deserializer(self) -> Self"),
        f("try_into", Effect::UsesUp, "pub fn try_into<'de, T>(self) -> Result<T, crate::de::Error>\n    where\n        T: de::Deserialize<'de>,"),
        f("deserialize", Effect::Makes, "fn deserialize<D>(deserializer: D) -> Result<Value, D::Error>\n    where\n        D: de::Deserializer<'de>,"),
        f("from", Effect::Makes, "fn from(val: &'a str) -> Value"),
        f("from", Effect::Makes, "fn from(val: Vec<V>) -> Value"),
        f("from", Effect::Makes, "fn from(val: BTreeMap<S, V>) -> Value"),
        f("from", Effect::Makes, "fn from(val: HashMap<S, V>) -> Value"),
        f("from_str", Effect::Makes, "fn from_str(s: &str) -> Result<Value, Self::Err>"),
        f("try_from", Effect::Makes, "pub fn try_from<T>(value: T) -> Result<Value, crate::ser::Error>\n    where\n        T: ser::Serialize,"),
    ]);
    sections(&mut s);
    // Who uses it, as the world counts it (DESIGN's rs-value, extracted
    // from this repository's own code).
    s.uses = Uses {
        sites: vec![
            site("inherits", "desktop · manifest.rs:404", "        .and_then(toml::Value::as_bool)", "Value", true),
            site("package_facts", "desktop · manifest.rs:116", "let field = |select: fn(&ManifestPackage) -> Option<&toml::Value>| {", "Value", true),
            site("read_cargo_package_table", "local-service · local_manifest.rs:21", "let root = toml::from_str::<toml::Value>(", "Value", false),
        ],
        rows: vec![
            verb("taken by", 10, 4, 4, &[("inherits", true), ("requirement", true), ("inherited_string", true)]),
            verb("held by", 17, 13, 2, &[("ManifestPackage.version", true), ("ManifestPackage.description", true), ("ManifestPackage.license", true)]),
            verb("called on by", 14, 0, 2, &[("Table::deserialize_any", false), ("Table::deserialize_enum", false), ("Table::deserialize_option", false)]),
            verb("used by", 3, 1, 3, &[("package_facts", true), ("Map::deserialize", false), ("parse_rustsec", false)]),
        ],
    };
    s.sections.push(SectionFacts { id: SectionId::Uses, count: Some(59), yours: Some(7), tier: Tier::Compiler });
    (s, crate::icons::Kind::Enum)
}

fn site(caller: &str, place: &str, code: &str, name: &str, yours: bool) -> Site {
    let mark = code.find(name).and_then(|at| u32::try_from(at).ok().zip(u32::try_from(at + name.len()).ok()));
    Site { caller: caller.to_owned(), place: place.to_owned(), code: Some(code.to_owned()), mark, yours, link: None }
}

fn verb(verb: &str, count: u32, yours: u32, packages: u32, names: &[(&str, bool)]) -> VerbRow {
    VerbRow { verb: verb.to_owned(), count, yours, names: names.iter().map(|(name, yours)| ((*name).to_owned(), None, *yours)).collect(), packages }
}

/// serde_json 1.0.151 `de.rs:2709`, its docs' `# Errors`, and who calls it
/// (DESIGN's rs-from_str).
fn from_str() -> (Source, crate::icons::Kind) {
    let mut s = source("from_str", DeclKind::Function, Lang::Rust, "pub fn from_str<'a, T>(s: &'a str) -> Result<T>\nwhere\n    T: de::Deserialize<'a>,",
        Some("Deserialize an instance of type `T` from a string of JSON text."), Vec::new(), Vec::new());
    s.failures = vec![SourceFailure {
        verb: DropVerb::FailsWith,
        member: None,
        words: "This conversion can fail if the structure of the input does not match the structure expected by T, for example if T is a struct type but the input contains something other than a JSON map. It can also fail if the structure is correct but T's implementation of Deserialize decides that something is wrong with the data.".to_owned(),
    }];
    s.uses = Uses {
        sites: vec![
            site("decode", "extension-qdrant · response.rs:30", "    serde_json::from_str(body).map_err(|source| QdrantError::Decode { phase, source })", "from_str", false),
            site("lower_surface_json", "present · call.rs:314", "let command = serde_json::from_str::<SurfaceCommand>(encoded).map_err(|error| {", "from_str", false),
            site("CompileCommands::load", "frontend-clang · compile_commands.rs:90", "let entries: Vec<RawEntry> = serde_json::from_str(&text).ok()?;", "from_str", false),
        ],
        rows: vec![verb("called by", 11, 0, 5, &[("Value::deserialize", false), ("Value::from_str", false), ("Map::from_str", false)])],
    };
    sections(&mut s);
    s.sections.push(SectionFacts { id: SectionId::Uses, count: Some(11), yours: None, tier: Tier::Compiler });
    (s, crate::icons::Kind::Function)
}

/// pflag `flag.go:1164`, and who calls it (DESIGN's go-parse, from cobra).
fn parse() -> (Source, crate::icons::Kind) {
    let mut s = source("Parse", DeclKind::Method, Lang::Go, "func (f *FlagSet) Parse(arguments []string) error",
        Some("Parse parses flag definitions from the argument list, which should not include the command name."), Vec::new(), Vec::new());
    s.owner = Some("FlagSet".to_owned());
    s.uses = Uses {
        sites: vec![
            site("Command.ParseFlags", "cobra · command.go:1882", "err := c.Flags().Parse(args)", "Parse", false),
            site("Parse", "pflag · flag.go:1238", "CommandLine.Parse(os.Args[1:])", "Parse", false),
        ],
        rows: Vec::new(),
    };
    s.sections.push(SectionFacts { id: SectionId::Uses, count: Some(2), yours: None, tier: Tier::Compiler });
    (s, crate::icons::Kind::Method)
}

/// zod 4.1.8 `classic/schemas.ts`: `ZodType.parse`, which has no doc
/// comment (so no lede), and its one caller in zod (DESIGN's ts-parse).
fn zod_parse() -> (Source, crate::icons::Kind) {
    let mut s = source("parse", DeclKind::Method, Lang::TypeScript, "parse(data: unknown, params?: core.ParseContext<core.$ZodIssue>): core.output<this>;",
        None, Vec::new(), Vec::new());
    s.owner = Some("ZodType".to_owned());
    s.uses = Uses {
        sites: vec![site("ZodType.decode", "zod · classic/schemas.ts", "inst.decode = (data, params) => parse.decode(inst, data, params);", "decode", false)],
        rows: Vec::new(),
    };
    s.sections.push(SectionFacts { id: SectionId::Uses, count: Some(2), yours: None, tier: Tier::Name });
    (s, crate::icons::Kind::Method)
}

fn owed(name: &str, signature: &str, summary: Option<&str>, owes: Owes) -> SourceMember {
    SourceMember { owes, ..member(name, DeclKind::Method, signature, summary, Effect::None) }
}

fn doers(count: u32, yours: u32, computed: bool, names: &[(&str, bool)]) -> Doers {
    Doers { count, yours, names: names.iter().map(|(name, yours)| ((*name).to_owned(), None, *yours)).collect(), computed, tier: Tier::Compiler }
}

/// serde_core 1.0.229 `ser/mod.rs`: `Serialize`, its one required method,
/// and who does it (DESIGN's rs-serialize, counted by the world).
fn serialize() -> (Source, crate::icons::Kind) {
    let mut s = source("Serialize", DeclKind::Trait, Lang::Rust, "pub trait Serialize",
        Some("A data structure that can be serialized into any data format supported by Serde."), Vec::new(),
        vec![owed("serialize", "fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>\n    where\n        S: Serializer;", Some("Serialize this value into the given Serde serializer."), Owes::Required)]);
    s.doers = doers(486, 17, false, &[("PersistedProjectPhase", true), ("PersistedPackageLane", true), ("PersistedDesktopState", true), ("PersistedRoute", true), ("PersistedAppearance", true)]);
    (s, crate::icons::Kind::Trait)
}

/// pflag `flag.go:208`: `Value`, three methods a flag value writes, and the
/// 41 types the type checker finds satisfy it (DESIGN's go-value).
fn go_value() -> (Source, crate::icons::Kind) {
    let mut s = source("Value", DeclKind::Interface, Lang::Go, "Value interface",
        Some("Value is the interface to the dynamic value stored in a flag."), Vec::new(),
        vec![
            owed("String", "String() string", None, Owes::Unknown),
            owed("Set", "Set(string) error", None, Owes::Unknown),
            owed("Type", "Type() string", None, Owes::Unknown),
        ]);
    s.doers = doers(41, 0, true, &[("int16Value", false), ("int32Value", false), ("stringValue", false), ("boolValue", false), ("float64Value", false)]);
    (s, crate::icons::Kind::Interface)
}

/// yaml 2.9.0 `nodes/Collection.d.ts`: an abstract class whose five
/// abstract members an implementor writes and whose path helpers it gets
/// (DESIGN's ts-collection).
fn collection() -> (Source, crate::icons::Kind) {
    let r = |name: &str, sig: &str, say: &str| owed(name, sig, Some(say), Owes::Required);
    let p = |name: &str, sig: &str| owed(name, sig, None, Owes::Provided);
    let mut s = source("Collection", DeclKind::Class, Lang::TypeScript, "export declare abstract class Collection extends NodeBase",
        None, Vec::new(),
        vec![
            r("add", "abstract add(value: unknown): void;", "Adds a value to the collection."),
            r("delete", "abstract delete(key: unknown): boolean;", "Removes a value from the collection."),
            r("get", "abstract get(key: unknown, keepScalar?: boolean): unknown;", "Returns item at `key`, or `undefined` if not found."),
            r("has", "abstract has(key: unknown): boolean;", "Checks if the collection includes a value with the key `key`."),
            r("set", "abstract set(key: unknown, value: unknown): void;", "Sets a value in this collection."),
            p("clone", "clone(schema?: Schema): Collection;"),
            p("addIn", "addIn(path: Iterable<unknown>, value: unknown): void;"),
            p("deleteIn", "deleteIn(path: Iterable<unknown>): boolean;"),
            p("getIn", "getIn(path: Iterable<unknown>, keepScalar?: boolean): unknown;"),
            p("hasIn", "hasIn(path: Iterable<unknown>): boolean;"),
            p("setIn", "setIn(path: Iterable<unknown>, value: unknown): void;"),
        ]);
    s.extends = vec!["NodeBase".to_owned()];
    s.doers = doers(4, 0, false, &[("YAMLSeq", false), ("YAMLMap", false), ("YAMLOMap", false), ("YAMLSet", false)]);
    (s, crate::icons::Kind::Class)
}

fn relation_label() -> (Source, crate::icons::Kind) {
    let v = |name: &str, sig: &str, doc: &str| member(name, DeclKind::Variant, sig, Some(doc), Effect::None);
    let mut s = source("RelationLabel", DeclKind::Enum, Lang::Rust, "pub enum RelationLabel", Some("The readable label of one relation group."), vec![
        v("Typed", "Typed(SemanticLinkKind, RelationDirection)", "A relation whose compiler kind and direction are both known."),
        v("Neighbourhood", "Neighbourhood", "A bounded neighbourhood whose per-edge kind the reply did not carry."),
        v("Related", "Related", "Incoming and outgoing neighbours whose per-edge kind is not carried."),
    ], vec![
        member("fmt", DeclKind::Method, "fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result", None, Effect::Reads),
        member("as_str", DeclKind::Method, "pub const fn as_str(self) -> &'static str", None, Effect::UsesUp),
    ]);
    sections(&mut s);
    (s, crate::icons::Kind::Enum)
}

fn error_handling() -> (Source, crate::icons::Kind) {
    let c = |name: &str, sig: &str, doc: &str| member(name, DeclKind::Constant, sig, Some(doc), Effect::None);
    let s = source("ErrorHandling", DeclKind::Alias, Lang::Go, "ErrorHandling int", Some("ErrorHandling defines how to handle flag parsing errors."), vec![
        c("ContinueOnError", "ContinueOnError ErrorHandling = iota", "ContinueOnError will return an err from Parse() if an error is found"),
        c("ExitOnError", "ExitOnError", "ExitOnError will call os.Exit(2) if an error is found when parsing"),
        c("PanicOnError", "PanicOnError", "PanicOnError will panic() if an error is found when parsing flags"),
    ], Vec::new());
    (s, crate::icons::Kind::Type)
}

fn string_formats() -> (Source, crate::icons::Kind) {
    let s = source("$ZodStringFormats", DeclKind::Alias, Lang::TypeScript,
        "export type $ZodStringFormats = \"email\" | \"url\" | \"uuid\" | \"regex\" | \"jwt\" | \"starts_with\" | \"ends_with\" | \"includes\";",
        None, Vec::new(), Vec::new());
    (s, crate::icons::Kind::Type)
}

struct PageScene {
    source: Source,
    plan: PagePlan,
    kind: crate::icons::Kind,
    /// Doors the gallery lends the page (every link opens nothing, but hovers
    /// and rings): `Still` when there are none.
    doors: Option<GalleryDoors>,
}

/// Doors for a still page: each link is a door that lights on hover and
/// opens nothing; the link named `from` is the declaration the page was
/// reached from.
pub(crate) struct GalleryDoors {
    pub(crate) from: Option<String>,
    /// What a click on a link does (the hop film swaps pages).
    pub(crate) open: Option<std::rc::Rc<dyn Fn(&str, &mut Window, &mut App)>>,
    /// Marks are shared elements (the hop film).
    pub(crate) marks: bool,
}

impl super::Doors for GalleryDoors {
    fn door(&self, link: &str) -> Option<super::Door> {
        let open = self.open.clone().map(|open| {
            let link = link.to_owned();
            std::rc::Rc::new(move |window: &mut Window, cx: &mut App| open(&link, window, cx)) as std::rc::Rc<dyn Fn(&mut Window, &mut App)>
        });
        Some(super::Door { subject: crate::hover::Subject::new(link.to_owned()), peek: None, open, from: self.from.as_deref() == Some(link) })
    }
    fn mark(&self, link: &str) -> Option<gpui::ElementId> {
        self.marks.then(|| gpui::ElementId::Name(gpui::SharedString::from(format!("mark:{link}"))))
    }
    fn fold(&self, _: &'static str) -> Option<super::Fold> {
        None
    }
    fn track(&self, _: gpui::SharedString, _: gpui::SharedString, _: Option<&super::Door>, element: gpui::AnyElement) -> gpui::AnyElement {
        element
    }
    fn say(&self, _: &str) {}
}

/// The pinned source for `which`, and the mark its gem wears.
pub(crate) fn pinned(which: PageOf) -> (Source, crate::icons::Kind) {
    match which {
        PageOf::Value => value(),
        PageOf::RelationLabel => relation_label(),
        PageOf::ErrorHandling => error_handling(),
        PageOf::StringFormats => string_formats(),
        PageOf::FromStr => from_str(),
        PageOf::Parse => parse(),
        PageOf::ZodParse => zod_parse(),
        PageOf::Serialize => serialize(),
        PageOf::GoValue => go_value(),
        PageOf::Collection => collection(),
    }
}

pub(crate) fn view(which: PageOf, cx: &mut App) -> AnyView {
    let (source, kind) = pinned(which);
    view_of(source, kind, cx)
}

/// A page drawn from `source`.
pub(crate) fn view_of(source: Source, kind: crate::icons::Kind, cx: &mut App) -> AnyView {
    let plan = compile(&source);
    cx.new(|_: &mut Context<PageScene>| PageScene { source, plan, kind, doors: None }).into()
}

/// A page drawn from `source` with doors on every link, `from` ringed.
pub(crate) fn view_with_doors(source: Source, kind: crate::icons::Kind, from: Option<&str>, cx: &mut App) -> AnyView {
    let plan = compile(&source);
    let doors = Some(GalleryDoors { from: from.map(ToOwned::to_owned), open: None, marks: false });
    cx.new(|_: &mut Context<PageScene>| PageScene { source, plan, kind, doors }).into()
}

/// What it does, from the source's own operations (accessors on tines and
/// makers in Getting one stay out).
fn does_groups(source: &Source, plan: &PagePlan) -> Vec<DoesGroup> {
    let on_tines = match &plan.spec {
        crate::anatomy::plan::Spec::Choice(choice) => choice.cases.iter().flat_map(|case| case.accessors.iter().map(|a| a.name.clone())).collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    [(Effect::Reads, "reads it", crate::icons::Mod::Reads, "page-does-reads"), (Effect::Changes, "changes it", crate::icons::Mod::Changes, "page-does-changes"), (Effect::UsesUp, "uses it up", crate::icons::Mod::Consumes, "page-does-uses-up")]
        .into_iter()
        .map(|(effect, words, mark, fold)| DoesGroup {
            words,
            mark,
            fold,
            rows: source
                .does
                .iter()
                .filter(|m| m.effect == effect && !on_tines.contains(&m.name))
                .map(|m| {
                    let pipe = m.signature.as_deref().and_then(|sig| callable(sig, &m.name, Language::Rust));
                    DoesRow {
                        name: m.name.clone(),
                        gives: pipe.as_ref().and_then(|pipe| pipe.output.as_ref()).map(|out| spelled(&out.pieces, &out.source)),
                        fails: pipe.as_ref().is_some_and(|pipe| pipe.fails.is_some()),
                        link: None,
                        ins: pipe.as_ref().map_or(0, |pipe| u8::try_from(pipe.inputs.iter().filter(|input| input.receiver.is_none()).count()).unwrap_or(u8::MAX)),
                    }
                })
                .collect(),
        })
        .collect()
}

impl Render for PageScene {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        draw(&self.source, &self.plan, self.doors.as_ref().map(|doors| doors as &dyn super::Doors), None, window, cx)
    }
}

/// The page as the desktop's reader region draws it: `source`'s plan, with
/// `doors` when it has any, and its title and mark shared under `address`
/// when the scene films a hop.
pub(crate) fn draw<V: 'static>(source: &Source, plan: &PagePlan, doors: Option<&dyn super::Doors>, address: Option<&str>, window: &mut Window, cx: &mut Context<V>) -> gpui::AnyElement {
    let facet = cx.facet();
    let palette = facet.palette();
    let width = f32::from(window.viewport_size().width);
    // The reader's own padding and folio (`reader.rs`): fluid 22–40 px
    // aside, 22–56 px above, the page at most 784 px wide.
    let wide = width >= 900.0;
    let (pad, top) = if wide { (40.0, 56.0) } else { (22.0, 22.0) };
    let folio = (width - pad * 2.0).min(784.0);
    let measure = facet.measure(px(folio));
    let geo = Geometry::new(px(folio), px((width - folio) / 2.0), measure.scale());
    let anchors = Anchors::new();
    let mut gem = crate::anatomy::sigil::sigil(crate::anatomy::sigil::Sigil::of(plan).yours(u8::try_from(plan.reach.yours.len()).unwrap_or(u8::MAX)), geo.gem, palette);
    // The title's box hugs the name (the desktop's does): a shared name grows from and to the text, never from the middle of the column.
    let mut title = div().flex().flex_wrap().items_baseline().self_start();
    if let Some(owner) = &plan.hero.owner {
        title = title.child(said("page-owner", format!("{owner}."), scale::DISPLAY, palette.ink3, &measure));
    }
    let mut title = title.child(said("name:0:page", plan.hero.name.clone(), scale::DISPLAY, palette.ink0, &measure)).into_any_element();
    if let Some(address) = address {
        title = crate::motion::shared::shared(super::title_key(address), title).timing(std::time::Duration::from_millis(460), crate::tokens::motion::GLIDE).into_any_element();
        gem = crate::motion::shared::shared(gpui::ElementId::Name(gpui::SharedString::from(format!("mark:{address}"))), gem).timing(std::time::Duration::from_millis(460), crate::tokens::motion::GLIDE).into_any_element();
    }
    let mut bodies = Vec::new();
    if let Some(body) = does(&does_groups(source, plan), &[] as &[Capability], &geo, &measure, palette, &Still) {
        bodies.push((SectionId::Does, body));
    }
    let still = Still;
    let doors: &dyn super::Doors = doors.unwrap_or(&still);
    let band = {
        let mut narrow = geo;
        narrow.col_w = px(300.0 * geo.scale);
        narrow.reach = px(0.0);
        does(&does_groups(source, plan), &[] as &[Capability], &narrow, &measure, palette, &Still)
    };
    let page = page(plan, gem, title, bodies, band, geo, &anchors, &measure, palette, doors);
    div()
        .size_full()
        .relative()
        .overflow_hidden()
        .bg(palette.g1.hsla())
        .child(div().flex().justify_center().pt(px(top)).child(div().w(px(folio)).child(page)))
        .child(float::layer(window, cx))
        .into_any_element()
}

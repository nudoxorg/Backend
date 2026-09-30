//! The record specimen, compiled from what the index really records for three
//! languages: toml_datetime's `Datetime` (Rust), zod's `$ZodIssueTooSmall`
//! (TypeScript) and pflag's `Flag` (Go). The member texts below are the
//! signatures the harness fixture index returns for those declarations,
//! copied verbatim (including the TypeScript text the producer cuts short).

use super::{CaseKind, Choice, DeclKind, Deprecated, Effect, Lang, PagePlan, Record, SectionFacts, SectionId, Source, SourceMember, Spec, Tier, compile};

fn member(name: &str, kind: DeclKind, signature: &str, summary: Option<&str>) -> SourceMember {
    SourceMember { name: name.to_owned(), kind, signature: Some(signature.to_owned()), summary: summary.map(ToOwned::to_owned), deprecated: None, effect: Effect::None, link: None, owes: Default::default() }
}

fn method(name: &str, effect: Effect, signature: &str) -> SourceMember {
    SourceMember { effect, ..member(name, DeclKind::Method, signature, None) }
}

fn source(name: &str, kind: DeclKind, lang: Lang, package: &str, signature: &str, made_of: Vec<SourceMember>) -> Source {
    Source {
        name: name.to_owned(),
        owner: None,
        kind,
        lang,
        package: package.to_owned(),
        version: None,
        module: String::new(),
        signature: Some(signature.to_owned()),
        lede: None,
        deprecated: None,
        made_of,
        extends: Vec::new(),
        sections: Vec::new(),
        does: Vec::new(),
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

fn datetime() -> Source {
    let mut s = source("Datetime", DeclKind::Struct, Lang::Rust, "toml_datetime", "pub struct Datetime", vec![
        member("date", DeclKind::Field, "pub date: Option<Date>", Some("Optional date.")),
        member("time", DeclKind::Field, "pub time: Option<Time>", Some("Optional time.")),
        member("offset", DeclKind::Field, "pub offset: Option<Offset>", Some("Optional offset.")),
    ]);
    s.lede = Some("A parsed TOML datetime value".to_owned());
    s
}

fn too_small() -> Source {
    let mut s = source("$ZodIssueTooSmall", DeclKind::Interface, Lang::TypeScript, "zod",
        "export interface $ZodIssueTooSmall<Input = unknown> extends $ZodIssueBase", vec![
        member("code", DeclKind::Field, "readonly code: \"too_small\"", None),
        member("origin", DeclKind::Field, "readonly origin: \"number\" | \"int\" | \"bigint\" | \"date\" | \"string\" | \"array\" | \"set\" | \"file\" | (string &", None),
        member("minimum", DeclKind::Field, "readonly minimum: number | bigint", None),
        member("inclusive", DeclKind::Field, "readonly inclusive?: boolean", Some("True if the allowable range includes the minimum")),
        member("exact", DeclKind::Field, "readonly exact?: boolean", Some("True if the allowed value is fixed (e.g.` z.length(5)`), not a range (`z.minLength(5)`)")),
        member("input", DeclKind::Field, "readonly input?: Input", None),
    ]);
    s.extends = vec!["$ZodIssueBase".to_owned()];
    s
}

fn flag() -> Source {
    source("Flag", DeclKind::Struct, Lang::Go, "pflag", "Flag struct", vec![
        member("Name", DeclKind::Field, "Name                string", None),
        member("Shorthand", DeclKind::Field, "Shorthand           string", None),
        member("Usage", DeclKind::Field, "Usage               string", None),
        member("Value", DeclKind::Field, "Value               Value", None),
        member("DefValue", DeclKind::Field, "DefValue            string", None),
        member("Changed", DeclKind::Field, "Changed             bool", None),
        member("NoOptDefVal", DeclKind::Field, "NoOptDefVal         string", None),
        member("Deprecated", DeclKind::Field, "Deprecated          string", None),
        member("Hidden", DeclKind::Field, "Hidden              bool", None),
        member("ShorthandDeprecated", DeclKind::Field, "ShorthandDeprecated string", None),
        member("Annotations", DeclKind::Field, "Annotations         map[string][]string", None),
    ])
}

fn record(plan: &PagePlan) -> &Record {
    match &plan.spec { Spec::Record(record) => record, other => panic!("expected a record specimen, got {other:?}") }
}

#[test]
fn c_and_cpp_are_distinct_page_languages() {
    assert_eq!(Lang::from_name("c"), Lang::C);
    assert_eq!(Lang::from_name("c++"), Lang::Cpp);
    assert_eq!(Lang::from_name("h"), Lang::Other);
}

/// Each rung as the page reads it: `name?  type in words`.
fn rungs(record: &Record) -> Vec<String> {
    record.rungs.iter().map(|rung| format!("{}{}  {}", rung.name, if rung.optional { "?" } else { "" }, rung.ty.plain())).collect()
}

#[test]
fn a_rust_struct_is_a_bracket_of_its_public_fields_in_plain_words() {
    let plan = compile(&datetime());
    assert_eq!(rungs(record(&plan)), ["date?  maybe Date", "time?  maybe Time", "offset?  maybe Offset"]);
    assert_eq!(plan.hero.lede.as_deref(), Some("A parsed TOML datetime value"));
    assert_eq!(record(&plan).private, 0);
}

#[test]
fn a_typescript_interface_of_properties_is_a_record_with_its_literals_generics_and_base() {
    let plan = compile(&too_small());
    let record = record(&plan);
    assert_eq!(rungs(record), [
        "code  \"too_small\"",
        "origin  one of \"number\" · \"int\" · \"bigint\" · \"date\" · \"string\" · \"array\" · \"set\" · \"file\" or any text",
        "minimum  number or big integer",
        "inclusive?  bool",
        "exact?  bool",
        "input?  Input",
    ]);
    assert!(record.all_readonly, "every property is readonly, said once");
    assert_eq!(record.extends, ["$ZodIssueBase"]);
    assert_eq!(record.generics.iter().map(|g| format!("{} = {}", g.name, g.default.as_ref().map(super::Ty::plain).unwrap_or_default())).collect::<Vec<_>>(), ["Input = anything"]);
    // An interface that only holds data takes the type's hue, not the contract's.
    assert_eq!(plan.hero.fam, super::Fam::Type);
    // No doc comment: no lede. The page never speaks in the author's voice.
    assert_eq!(plan.hero.lede, None);
    // Docs never sit in a row at rest; they travel with the rung for hover.
    assert_eq!(record.rungs[3].doc.as_deref(), Some("True if the allowable range includes the minimum"));
}

#[test]
fn a_go_struct_is_the_same_bracket_with_go_spellings_in_words() {
    let plan = compile(&flag());
    let record = record(&plan);
    assert_eq!(rungs(record), [
        "Name  text", "Shorthand  text", "Usage  text", "Value  Value", "DefValue  text", "Changed  bool",
        "NoOptDefVal  text", "Deprecated  text", "Hidden  bool", "ShorthandDeprecated  text",
        "Annotations  map text → list of text",
    ]);
    assert_eq!(record.private, 0, "every pflag.Flag field is exported");
}

#[test]
fn unexported_go_fields_and_private_rust_fields_are_a_count_never_names() {
    let mut go = flag();
    go.made_of.push(member("parsed", DeclKind::Field, "parsed bool", None));
    assert_eq!(record(&compile(&go)).private, 1);
    let mut rust = datetime();
    rust.made_of.push(member("secret", DeclKind::Field, "secret: u8", None));
    rust.made_of.push(member("crate_only", DeclKind::Field, "pub(crate) crate_only: u8", None));
    let plan = compile(&rust);
    assert_eq!(record(&plan).private, 2);
    assert!(!rungs(record(&plan)).iter().any(|rung| rung.contains("secret") || rung.contains("crate_only")));
}

#[test]
fn an_interface_with_methods_is_a_socket_not_a_record() {
    let mut contract = too_small();
    contract.made_of.push(member("add", DeclKind::Method, "add(value: unknown): void", None));
    // It is a contract: a socket, its method a notch, its properties held.
    let plan = compile(&contract);
    let Spec::Contract(socket) = &plan.spec else { panic!("expected a socket, got {:?}", plan.spec) };
    assert_eq!(socket.write.iter().map(|slot| slot.name.as_str()).collect::<Vec<_>>(), ["add"]);
    assert_eq!(socket.held.iter().map(|rung| rung.name.as_str()).collect::<Vec<_>>(), ["code", "origin", "minimum", "inclusive", "exact", "input"]);
}

#[test]
fn sections_come_in_reading_order_with_their_stubs_counted_and_tiered() {
    let mut s = datetime();
    s.sections = vec![
        SectionFacts { id: SectionId::Uses, count: Some(59), yours: Some(7), tier: Tier::Compiler },
        SectionFacts { id: SectionId::Getting, count: Some(4), yours: None, tier: Tier::Compiler },
        SectionFacts { id: SectionId::Words, count: None, yours: None, tier: Tier::Compiler },
    ];
    let plan = compile(&s);
    let heads = plan.sections.iter().map(|section| (section.title.as_str(), section.count.as_deref(), section.yours.as_deref())).collect::<Vec<_>>();
    assert_eq!(
        heads,
        [("Your code and it", None, None), ("Getting one", Some("4 ways"), None), ("Who uses it", Some("59 places"), Some("7 yours")), ("Its own words", None, None)],
        "a type always asks what your code does with it; the index not having read that is said, not hidden"
    );
}

#[test]
fn a_deprecated_declaration_carries_its_mark() {
    let mut s = flag();
    s.deprecated = Some(Deprecated { since: Some("1.1".to_owned()), note: None });
    assert!(compile(&s).hero.marks.iter().any(|mark| matches!(mark, super::Mark::Deprecated(d) if d.since.as_deref() == Some("1.1"))));
}

#[test]
fn compile_is_pure() {
    assert_eq!(compile(&too_small()), compile(&too_small()));
}

// ------------------------------------------------------------------ choices
//
// The member texts are what the harness fixture index returned for these
// declarations (`NUDOX_PAGE_DUMP`, 2026-09-28), copied verbatim.

fn relation_label() -> Source {
    source("RelationLabel", DeclKind::Enum, Lang::Rust, "present", "pub enum RelationLabel", vec![
        member("Typed", DeclKind::Variant, "Typed(SemanticLinkKind, RelationDirection)", Some("A relation whose compiler kind and direction are both known.")),
        member("Neighbourhood", DeclKind::Variant, "Neighbourhood", Some("A bounded neighbourhood whose per-edge kind the reply did not carry.")),
        member("Related", DeclKind::Variant, "Related", Some("Incoming and outgoing neighbours whose per-edge kind is not carried.")),
    ])
}

fn value() -> Source {
    let mut s = source("Value", DeclKind::Enum, Lang::Rust, "toml", "pub enum Value", vec![
        member("String", DeclKind::Variant, "String(String)", Some("Represents a TOML string")),
        member("Integer", DeclKind::Variant, "Integer(i64)", Some("Represents a TOML integer")),
        member("Float", DeclKind::Variant, "Float(f64)", Some("Represents a TOML float")),
        member("Boolean", DeclKind::Variant, "Boolean(bool)", Some("Represents a TOML boolean")),
        member("Datetime", DeclKind::Variant, "Datetime(Datetime)", Some("Represents a TOML datetime")),
        member("Array", DeclKind::Variant, "Array(Array)", Some("Represents a TOML array")),
        member("Table", DeclKind::Variant, "Table(Table)", Some("Represents a TOML table")),
    ]);
    s.does = vec![
        method("as_array_mut", Effect::Changes, "pub fn as_array_mut(&mut self) -> Option<&mut Vec<Value>>"),
        method("as_table_mut", Effect::Changes, "pub fn as_table_mut(&mut self) -> Option<&mut Table>"),
        method("get_mut", Effect::Changes, "pub fn get_mut<I: Index>(&mut self, index: I) -> Option<&mut Value>"),
        method("as_array", Effect::Reads, "pub fn as_array(&self) -> Option<&Vec<Value>>"),
        method("as_bool", Effect::Reads, "pub fn as_bool(&self) -> Option<bool>"),
        method("as_datetime", Effect::Reads, "pub fn as_datetime(&self) -> Option<&Datetime>"),
        method("as_float", Effect::Reads, "pub fn as_float(&self) -> Option<f64>"),
        method("as_integer", Effect::Reads, "pub fn as_integer(&self) -> Option<i64>"),
        method("as_str", Effect::Reads, "pub fn as_str(&self) -> Option<&str>"),
        method("as_table", Effect::Reads, "pub fn as_table(&self) -> Option<&Table>"),
        method("get", Effect::Reads, "pub fn get<I: Index>(&self, index: I) -> Option<&Value>"),
        method("is_array", Effect::Reads, "pub fn is_array(&self) -> bool"),
        method("is_bool", Effect::Reads, "pub fn is_bool(&self) -> bool"),
        method("is_datetime", Effect::Reads, "pub fn is_datetime(&self) -> bool"),
        method("is_float", Effect::Reads, "pub fn is_float(&self) -> bool"),
        method("is_integer", Effect::Reads, "pub fn is_integer(&self) -> bool"),
        method("is_str", Effect::Reads, "pub fn is_str(&self) -> bool"),
        method("is_table", Effect::Reads, "pub fn is_table(&self) -> bool"),
        method("same_type", Effect::Reads, "pub fn same_type(&self, other: &Value) -> bool"),
        method("type_str", Effect::Reads, "pub fn type_str(&self) -> &'static str"),
        method("try_into", Effect::UsesUp, "pub fn try_into<'de, T>(self) -> Result<T, crate::de::Error>\n    where\n        T: de::Deserialize<'de>,"),
        method("from", Effect::Makes, "fn from(val: &'a str) -> Value"),
        method("from", Effect::Makes, "fn from(val: Vec<V>) -> Value"),
        method("from", Effect::Makes, "fn from(val: BTreeMap<S, V>) -> Value"),
        method("from_str", Effect::Makes, "fn from_str(s: &str) -> Result<Value, Self::Err>"),
        method("try_from", Effect::Makes, "pub fn try_from<T>(value: T) -> Result<Value, crate::ser::Error>\n    where\n        T: ser::Serialize,"),
    ];
    s
}

fn string_formats() -> Source {
    source("$ZodStringFormats", DeclKind::Alias, Lang::TypeScript, "zod",
        "export type $ZodStringFormats = \"email\" | \"url\" | \"uuid\" | \"regex\" | \"jwt\" | \"starts_with\" | \"ends_with\" | \"includes\";", vec![])
}

fn choice(plan: &PagePlan) -> &Choice {
    match &plan.spec { Spec::Choice(choice) => choice, other => panic!("expected a fork, got {other:?}") }
}

/// Each tine as the page reads it: `name  carries  [value]  accessors`.
fn tines(choice: &Choice) -> Vec<String> {
    choice.cases.iter().map(|case| {
        let mut row = case.name.clone();
        for ty in &case.carries { row.push_str("  "); row.push_str(&ty.plain()); }
        if let Some(value) = &case.value { row.push_str("  = "); row.push_str(value); }
        for accessor in &case.accessors { row.push_str(if accessor.changes { "  ✎" } else { "  " }); row.push_str(&accessor.name); }
        row
    }).collect()
}

#[test]
fn a_rust_enum_is_a_fork_one_tine_per_variant_with_what_it_carries() {
    let plan = compile(&relation_label());
    assert_eq!(tines(choice(&plan)), ["Typed  SemanticLinkKind  RelationDirection", "Neighbourhood", "Related"]);
    // Docs ride with the case for the hover peek; they are never a row.
    assert_eq!(choice(&plan).cases[1].doc.as_deref(), Some("A bounded neighbourhood whose per-edge kind the reply did not carry."));
    assert_eq!(choice(&plan).open, None);
    assert_eq!(plan.hero.fam, super::Fam::Type);
}

#[test]
fn accessors_that_read_one_case_sit_on_its_tine() {
    let plan = compile(&value());
    assert_eq!(tines(choice(&plan)), [
        "String  text  as_str  is_str",
        "Integer  integer  as_integer  is_integer",
        "Float  float  as_float  is_float",
        "Boolean  bool  as_bool  is_bool",
        "Datetime  Datetime  as_datetime  is_datetime",
        "Array  Array  as_array  ✎as_array_mut  is_array",
        "Table  Table  as_table  ✎as_table_mut  is_table",
    ]);
}

#[test]
fn a_typescript_literal_union_is_a_fork_of_literals() {
    let plan = compile(&string_formats());
    let choice = choice(&plan);
    assert_eq!(tines(choice), ["\"email\"", "\"url\"", "\"uuid\"", "\"regex\"", "\"jwt\"", "\"starts_with\"", "\"ends_with\"", "\"includes\""]);
    assert!(choice.cases.iter().all(|case| case.kind == CaseKind::Literal));
    assert_eq!(choice.open, None, "no `(string & {{}})`: closed");
    let mut open = string_formats();
    open.signature = Some("export type Origin = \"number\" | \"int\" | (string & {});".to_owned());
    assert_eq!(super::choice::choice(&open).and_then(|choice| choice.open).as_deref(), Some("any other text"));
}

#[test]
fn a_typescript_union_of_types_is_a_fork_of_types_in_words() {
    let primitive = source("Primitive", DeclKind::Alias, Lang::TypeScript, "zod",
        "export type Primitive = string | number | symbol | bigint | boolean | null | undefined;", vec![]);
    let plan = compile(&primitive);
    assert_eq!(tines(choice(&plan)), [
        "string  text", "number  number", "symbol  symbol", "bigint  big integer", "boolean  bool", "null  null", "undefined  nothing",
    ]);
}

#[test]
fn getting_one_reads_the_makers_best_first() {
    let plan = compile(&value());
    let rails = plan.getting.iter().map(|rail| format!("{} → {}{}", rail.from.plain(), rail.verb, if rail.fails { " ?" } else { "" })).collect::<Vec<_>>();
    assert_eq!(rails, [
        "text → parse ?",
        "text → from",
        "any Serialize → try_from ?",
        "list of V → from",
        "map S → V → from",
    ]);
}

// ------------------------------------------------------------------ pipes
//
// The signatures are the declarations as their sources write them
// (serde_json 1.0.151 `de.rs:2709`, pflag `flag.go:1164`, `:1264`,
// `int.go:30`); the TypeScript one is written for the test.

fn pipe(plan: &PagePlan) -> &super::Callable {
    match &plan.spec { Spec::Callable(callable) => callable, other => panic!("expected a pipe, got {other:?}") }
}

/// A pipe as the page reads it: `on T effect | port: type… → gives | drop`.
fn reads(callable: &super::Callable) -> String {
    let mut out = String::new();
    if let Some(receiver) = &callable.receiver {
        out.push_str(&format!("on {} {:?} | ", receiver.ty.plain(), receiver.effect));
    }
    out.push_str(&callable.ports.iter().map(|port| format!("{}: {}", port.name, port.ty.plain())).collect::<Vec<_>>().join(", "));
    out.push_str(" → ");
    out.push_str(&callable.gives.as_ref().map_or_else(|| "nothing".to_owned(), super::Ty::plain));
    for drop in &callable.drops {
        out.push_str(&format!(" | {}{}", drop.verb.words(), drop.ty.as_ref().map_or_else(String::new, |ty| format!(" {}", ty.plain()))));
    }
    out
}

#[test]
fn a_rust_function_is_a_pipe_with_its_bound_in_words_and_its_error_as_a_drop() {
    let s = source("from_str", DeclKind::Function, Lang::Rust, "serde_json",
        "pub fn from_str<'a, T>(s: &'a str) -> Result<T>\nwhere\n    T: de::Deserialize<'a>,", vec![]);
    let plan = compile(&s);
    assert_eq!(reads(pipe(&plan)), "s: text → T | or fails with");
    assert_eq!(plan.hero.fam, super::Fam::Callable);
}

#[test]
fn a_go_method_is_a_pipe_that_changes_its_receiver_and_returns_error() {
    let mut s = source("Parse", DeclKind::Method, Lang::Go, "pflag", "func (f *FlagSet) Parse(arguments []string) error", vec![]);
    s.owner = Some("FlagSet".to_owned());
    assert_eq!(reads(pipe(&compile(&s))), "on FlagSet Changes | arguments: list of text → nothing | or returns error");
    let make = source("NewFlagSet", DeclKind::Function, Lang::Go, "pflag", "func NewFlagSet(name string, errorHandling ErrorHandling) *FlagSet", vec![]);
    assert_eq!(reads(pipe(&compile(&make))), "name: text, errorHandling: ErrorHandling → FlagSet");
    let mut two = source("GetInt", DeclKind::Method, Lang::Go, "pflag", "func (f *FlagSet) GetInt(name string) (int, error)", vec![]);
    two.owner = Some("FlagSet".to_owned());
    assert_eq!(reads(pipe(&compile(&two))), "on FlagSet Changes | name: text → integer | or returns error");
}

#[test]
fn a_typescript_function_is_the_same_pipe() {
    let s = source("safeParse", DeclKind::Function, Lang::TypeScript, "zod",
        "export function safeParse(schema: $ZodType, data: unknown): SafeParseResult", vec![]);
    assert_eq!(reads(pipe(&compile(&s))), "schema: $ZodType, data: anything → SafeParseResult");
}

#[test]
fn what_can_go_wrong_carries_the_docs_words_on_the_pipe_s_drop() {
    let mut s = source("from_str", DeclKind::Function, Lang::Rust, "serde_json",
        "pub fn from_str<'a, T>(s: &'a str) -> Result<T>\nwhere\n    T: de::Deserialize<'a>,", vec![]);
    s.failures = vec![super::SourceFailure {
        verb: super::DropVerb::FailsWith,
        member: None,
        words: "This conversion can fail if the structure of the input does not match the structure expected by T.".to_owned(),
    }];
    let plan = compile(&s);
    assert_eq!(plan.fails.len(), 1);
    assert_eq!(plan.fails[0].verb, super::DropVerb::FailsWith);
    assert_eq!(plan.fails[0].words.as_deref(), Some("This conversion can fail if the structure of the input does not match the structure expected by T."));
}

// ------------------------------------------------------------------ sockets
//
// Declarations as their sources write them: serde_core 1.0.229
// `ser/mod.rs`, pflag `flag.go:208`, yaml 2.9.0 `nodes/Collection.d.ts`.

fn owed(name: &str, signature: &str, owes: super::Owes) -> SourceMember {
    SourceMember { owes, ..member(name, DeclKind::Method, signature, None) }
}

fn socket(plan: &PagePlan) -> &super::Contract {
    match &plan.spec { Spec::Contract(contract) => contract, other => panic!("expected a socket, got {other:?}") }
}

/// A socket as the page reads it: `write: a b? | get: c | other: d`.
fn slots(contract: &super::Contract) -> String {
    let list = |slots: &[super::Slot]| slots.iter().map(|slot| format!("{}{}{}", slot.name, if slot.optional { "?" } else { "" }, if slot.fails { "!" } else { "" })).collect::<Vec<_>>().join(" ");
    format!("write: {} | get: {} | other: {}", list(&contract.write), list(&contract.get), list(&contract.other))
}

#[test]
fn a_rust_trait_is_a_socket_its_bodiless_method_a_notch() {
    let mut s = source("Serialize", DeclKind::Trait, Lang::Rust, "serde", "pub trait Serialize", vec![]);
    s.does = vec![owed("serialize", "fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>\n    where\n        S: Serializer;", super::Owes::Required)];
    let plan = compile(&s);
    assert_eq!(slots(socket(&plan)), "write: serialize! | get:  | other: ");
    assert_eq!(plan.hero.fam, super::Fam::Contract);
}

#[test]
fn a_go_interface_is_all_notches_by_the_language_s_rule() {
    let mut s = source("Value", DeclKind::Interface, Lang::Go, "pflag", "Value interface", vec![]);
    s.does = vec![
        owed("String", "String() string", super::Owes::Unknown),
        owed("Set", "Set(string) error", super::Owes::Unknown),
        owed("Type", "Type() string", super::Owes::Unknown),
    ];
    let plan = compile(&s);
    let contract = socket(&plan);
    assert_eq!(slots(contract), "write: String Set! Type | get:  | other: ");
    assert_eq!(contract.write[0].gives.as_ref().map(super::Ty::plain).as_deref(), Some("text"));
}

#[test]
fn a_typescript_abstract_class_writes_its_abstract_members_and_gets_the_rest() {
    let mut s = source("Collection", DeclKind::Class, Lang::TypeScript, "yaml", "export declare abstract class Collection extends NodeBase", vec![]);
    s.does = vec![
        owed("add", "abstract add(value: unknown): void;", super::Owes::Required),
        owed("get", "abstract get(key: unknown, keepScalar?: boolean): unknown;", super::Owes::Required),
        owed("clone", "clone(schema?: Schema): Collection;", super::Owes::Provided),
    ];
    let plan = compile(&s);
    let contract = socket(&plan);
    assert_eq!(slots(contract), "write: add get | get: clone | other: ");
    assert_eq!(contract.get[0].gives.as_ref().map(super::Ty::plain).as_deref(), Some("Collection"));
    // A class with nothing abstract is not a contract.
    let mut plain = s.clone();
    for member in &mut plain.does { member.owes = super::Owes::Provided; }
    assert!(!matches!(compile(&plain).spec, Spec::Contract(_)));
}

// ------------------------------------------------------------------ relations back to us

mod yours {
    use super::super::{DeclKind, Lang, SectionId, Spec, compile};
    use super::{datetime, flag};
    use crate::anatomy::reach::{Basis, CrateUse, Line, Reach};
    use crate::anatomy::sigil::{Form, Sigil};

    fn reach() -> Reach {
        Reach {
            yours: vec![
                CrateUse { name: "desktop".to_owned(), count: 43, members: vec![("as_str".to_owned(), 8)], lines: vec![Line { file: "src/harness.rs".to_owned(), line: 607, text: "let v: toml::Datetime = x;".to_owned(), ..Line::default() }] },
                CrateUse { name: "engine".to_owned(), count: 15, ..CrateUse::default() },
            ],
            basis: Basis::Scanned,
            ..Reach::default()
        }
    }

    #[test]
    fn your_code_and_it_leads_the_sections_and_counts_places_and_crates() {
        let mut s = datetime();
        s.reach = reach();
        let plan = compile(&s);
        let head = &plan.sections[0];
        assert_eq!((head.id, head.title.as_str(), head.count.as_deref(), head.yours.as_deref()), (SectionId::Yours, "Your code and it", Some("58 places"), Some("2 crates")));
    }

    #[test]
    fn a_page_that_has_not_read_your_code_says_unknown_never_none() {
        let s = datetime();
        assert!(!s.reach.read(), "nothing was read");
        let plan = compile(&s);
        let head = plan.sections.iter().find(|section| section.id == SectionId::Yours).expect("the section is there, to say it");
        assert_eq!(head.count, None, "no count is claimed for what nobody read");
        // A module or a field has no such question.
        let mut field = datetime();
        field.kind = DeclKind::Field;
        assert!(compile(&field).sections.iter().all(|section| section.id != SectionId::Yours));
    }

    #[test]
    fn a_crates_real_lines_replace_who_uses_its_call_sites_so_a_line_is_said_once() {
        let mut s = datetime();
        s.uses.sites.push(super::super::Site { caller: "c".to_owned(), place: "p".to_owned(), ..Default::default() });
        s.reach = reach();
        assert!(compile(&s).uses.sites.is_empty(), "the reach draws the lines");
        s.reach.yours[0].lines.clear();
        assert_eq!(compile(&s).uses.sites.len(), 1, "without lines the call sites stay");
    }

    #[test]
    fn a_pipes_sigil_has_a_prong_per_input_an_arrow_and_a_drop_when_it_can_fail() {
        use crate::anatomy::plan::{DeclKind, Lang, Source};
        let mut s = super::datetime();
        s = Source {
            name: "from_str".to_owned(),
            kind: DeclKind::Function,
            lang: Lang::Rust,
            signature: Some("pub fn from_str<'a, T>(s: &'a str, strict: bool) -> Result<T>\nwhere\n    T: de::Deserialize<'a>,".to_owned()),
            made_of: Vec::new(),
            ..s
        };
        let sigil = Sigil::of(&compile(&s));
        assert_eq!((sigil.form, sigil.ins, sigil.gives, sigil.fails, sigil.recv), (Form::Fn, 2, true, true, false));
        // A method is called on something: the stem from above.
        s.kind = DeclKind::Method;
        s.owner = Some("Value".to_owned());
        s.signature = Some("pub fn as_str(&self) -> Option<&str>".to_owned());
        s.name = "as_str".to_owned();
        let sigil = Sigil::of(&compile(&s));
        assert_eq!((sigil.form, sigil.ins, sigil.gives, sigil.fails, sigil.recv), (Form::Method, 0, true, false, true));
    }

    #[test]
    fn the_sigil_carries_the_pages_own_facts() {
        // A record of three fields, named by two crates of yours.
        let mut s = datetime();
        s.reach = reach();
        let sigil = Sigil::of(&compile(&s)).yours(2);
        assert_eq!((sigil.form, sigil.fields, sigil.yours), (Form::Struct, 3, 2));
        // A Go struct is the same form: eleven fields, capped at five ticks when drawn.
        let go = Sigil::of(&compile(&flag()));
        assert_eq!((go.form, go.fields), (Form::Struct, 11));
        assert!(matches!(compile(&flag()).spec, Spec::Record(_)) && Lang::Go != Lang::Rust);
    }
}

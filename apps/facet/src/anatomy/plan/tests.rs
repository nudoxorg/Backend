//! The record specimen, compiled from what the index really records for three
//! languages: toml_datetime's `Datetime` (Rust), zod's `$ZodIssueTooSmall`
//! (TypeScript) and pflag's `Flag` (Go). The member texts below are the
//! signatures the harness fixture index returns for those declarations,
//! copied verbatim (including the TypeScript text the producer cuts short).

use super::{CaseKind, Choice, DeclKind, Deprecated, Effect, Lang, PagePlan, Record, SectionFacts, SectionId, Source, SourceMember, Spec, Tier, compile};

fn member(name: &str, kind: DeclKind, signature: &str, summary: Option<&str>) -> SourceMember {
    SourceMember { name: name.to_owned(), kind, signature: Some(signature.to_owned()), summary: summary.map(ToOwned::to_owned), deprecated: None, effect: Effect::None, link: None }
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
fn an_interface_with_methods_is_not_drawn_as_a_record() {
    let mut contract = too_small();
    contract.made_of.push(member("add", DeclKind::Method, "add(value: unknown): void", None));
    assert_eq!(compile(&contract).spec, Spec::None);
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
    assert_eq!(heads, [("Getting one", Some("4 ways"), None), ("Who uses it", Some("59 places"), Some("7 yours")), ("Its own words", None, None)]);
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
    assert_eq!(super::choice::choice(&open).and_then(|choice| choice.open).as_deref(), Some("any text"));
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

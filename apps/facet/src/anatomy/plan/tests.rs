//! The record specimen, compiled from what the index really records for three
//! languages: toml_datetime's `Datetime` (Rust), zod's `$ZodIssueTooSmall`
//! (TypeScript) and pflag's `Flag` (Go). The member texts below are the
//! signatures the harness fixture index returns for those declarations,
//! copied verbatim (including the TypeScript text the producer cuts short).

use super::{DeclKind, Deprecated, Lang, PagePlan, Record, SectionFacts, SectionId, Source, SourceMember, Spec, Tier, compile};

fn member(name: &str, kind: DeclKind, signature: &str, summary: Option<&str>) -> SourceMember {
    SourceMember { name: name.to_owned(), kind, signature: Some(signature.to_owned()), summary: summary.map(ToOwned::to_owned), deprecated: None }
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
    match &plan.spec { Spec::Record(record) => record, Spec::None => panic!("expected a record specimen, got none") }
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

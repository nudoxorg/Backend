//! End to end on real declarations: what the page says for serde_json's
//! `from_str`, `Value`, `as_str`, a Python and a TypeScript declaration.

use crate::anatomy::symbol::facts::{Facts, Member, Receives, Section, SectionKind, Site};
use crate::anatomy::symbol::view::{Block, FailWord, Joint, Kind, Lang, Origin, Shape};
use crate::anatomy::symbol::compile;

const FROM_STR: &str = "pub fn from_str<'a, T>(s: &'a str) -> Result<T>\nwhere\n    T: de::Deserialize<'a>,";

fn from_str() -> Facts {
    let mut f = Facts::new("from_str", Kind::Function, Lang::Rust, "serde_json");
    f.version = Some("1.0.151".into());
    f.signature = Some(FROM_STR.into());
    f.links = vec![("Error".into(), "serde_json::Error".into())];
    f.docs = vec![
        Block::Para("Deserialize an instance of type `T` from a string of JSON text.".into()),
        Block::Head("Example".into()),
        Block::Code("let u: User = serde_json::from_str(j).unwrap();".into()),
        Block::Head("Errors".into()),
        Block::Para("This conversion can fail if the structure of the input does not match the structure expected by `T`, for example if `T` is a struct type but the input contains something other than a JSON map. It can also fail if the structure is correct but `T`'s implementation of `Deserialize` decides that something is wrong with the data, for example required struct fields are missing from the JSON map or some number is too big to fit in the expected primitive type.".into()),
    ];
    f.site = Some(Site { file: "src/de.rs".into(), line: 2709, open: Some("/reg/serde_json/src/de.rs".into()) });
    f.error_kinds = vec![
        ("Io".into(), "The error was caused by a failure to read or write bytes on an I/O stream.".into()),
        ("Syntax".into(), "The error was caused by input that was not syntactically valid JSON.".into()),
        ("Data".into(), "The error was caused by input data that was semantically incorrect.".into()),
        ("Eof".into(), "The error was caused by prematurely reaching the end of the input data.".into()),
    ];
    f.error_tells = Some("Error::classify() → Category".into());
    f
}

#[test]
fn from_str_is_a_call_with_a_generic_a_failure_and_its_kinds() {
    let view = compile(&from_str());
    let call = view.call.as_ref().expect("a call");
    assert_eq!(call.ports[0].ty.word, "text");
    assert_eq!(call.ports[0].joint, Joint::Required);
    assert_eq!(call.gives.role.as_deref(), Some("you choose it"));
    let fails = call.fails.as_ref().expect("fails");
    assert_eq!(fails.word, FailWord::Fails);
    assert_eq!(fails.when, "if the structure of the input does not match the structure expected by T");
    let section = view.fails.as_ref().expect("if it fails");
    assert_eq!(section.kinds.iter().map(|k| k.name.as_str()).collect::<Vec<_>>(), ["Io", "Syntax", "Data", "Eof"]);
    assert!(section.kinds[0].impossible.is_some(), "Io can't happen for text in memory");
    assert!(section.kinds[1].impossible.is_none());
    assert_eq!(section.tells.as_deref(), Some("Error::classify() → Category"));
    // The errors section is not repeated in the docs; the example folds apart.
    assert!(view.docs.blocks.iter().all(|b| !matches!(b, Block::Para(p) if p.starts_with("This conversion can fail"))));
    assert!(view.docs.blocks.iter().any(|b| matches!(b, Block::Code(_))));
    assert_eq!(view.head.lede.as_deref(), Some("Deserialize an instance of type `T` from a string of JSON text."));
    assert_eq!(view.generics[0].bounds[0].name, "Deserialize");
    assert_eq!(view.rail.source.as_ref().map(|s| (s.file.as_str(), s.line)), Some(("src/de.rs", 2709)));
}

#[test]
fn a_failure_with_nothing_more_to_say_has_no_section() {
    let mut f = from_str();
    f.error_kinds.clear();
    f.docs = vec![Block::Para("Parse it.".into()), Block::Head("Errors".into()), Block::Para("If it is invalid.".into())];
    let view = compile(&f);
    assert!(view.call.as_ref().is_some_and(|c| c.fails.is_some()));
    assert!(view.fails.is_none(), "the call's row already says it");
}

fn value() -> Facts {
    let mut f = Facts::new("Value", Kind::Enum, Lang::Rust, "serde_json");
    let case = |name: &str, sig: &str, doc: &str| Member { name: name.into(), signature: Some(sig.into()), summary: Some(doc.into()), ..Member::default() };
    f.made_of = vec![
        case("Null", "Null", "Represents a JSON null value."),
        case("Bool", "Bool(bool)", "Represents a JSON boolean."),
        case("Number", "Number(Number)", "Represents a JSON number, whether integer or floating point."),
        case("String", "String(String)", "Represents a JSON string."),
        case("Array", "Array(Vec<Value>)", "Represents a JSON array."),
        case("Object", "Object(Map<String, Value>)", "Represents a JSON object."),
    ];
    let m = |name: &str, sig: &str, receives: Receives| Member { name: name.into(), signature: Some(sig.into()), receives, ..Member::default() };
    f.does = vec![
        m("as_str", "pub fn as_str(&self) -> Option<&str>", Receives::Reads),
        m("as_array", "pub fn as_array(&self) -> Option<&Vec<Value>>", Receives::Reads),
        m("as_object_mut", "pub fn as_object_mut(&mut self) -> Option<&mut Map<String, Value>>", Receives::Changes),
        m("from", "fn from(f: f32) -> Self", Receives::Makes),
        m("from", "fn from(f: bool) -> Self", Receives::Makes),
        m("from", "fn from(f: &str) -> Self", Receives::Makes),
        m("from_str", "fn from_str(s: &str) -> Result<Value, Self::Err>", Receives::Makes),
    ];
    f.implements = vec![("Clone".into(), true), ("PartialEq".into(), true), ("Debug".into(), true)];
    f
}

#[test]
fn value_is_a_fork_that_nests_with_methods_by_verb() {
    let view = compile(&value());
    let Some(Shape::OneOf(cases)) = &view.shape else { panic!("a fork") };
    assert_eq!(cases.len(), 6);
    assert!(cases[4].holds[0].loops && cases[5].holds[0].loops && !cases[3].holds[0].loops);
    assert_eq!(cases[5].holds[0].word, "a map of text to Value");
    let heads: Vec<&str> = view.verbs.iter().map(|g| g.verb.head()).collect();
    assert_eq!(heads, ["Makes one", "Reads it", "Changes it"]);
    assert_eq!(view.verbs[0].rows[0].takes, ["a number", "yes or no", "text"]);
    assert!(view.call.is_none());
    assert_eq!(view.rail.can.len(), 3);
}

#[test]
fn as_str_may_give_nothing_and_says_when() {
    let mut f = Facts::new("as_str", Kind::Method, Lang::Rust, "serde_json");
    f.owner = Some("Value".into());
    f.signature = Some("pub fn as_str(&self) -> Option<&str>".into());
    f.docs = vec![Block::Para("If the `Value` is a String, returns the associated `str`. Returns None otherwise.".into())];
    let view = compile(&f);
    let call = view.call.expect("a call");
    assert_eq!(call.none.as_deref(), Some("if it isn't a String"));
    assert_eq!(call.receiver.and_then(|r| r.effect), Some(crate::anatomy::symbol::view::Effect::Reads));
    assert!(view.fails.is_none());
}

#[test]
fn a_python_page_is_read_from_its_docs_and_says_so() {
    let mut f = Facts::new("match", Kind::Function, Lang::Python, "re");
    f.signature = Some("def match(pattern, string, flags=0):".into());
    f.docs = vec![Block::Para("Try to apply the pattern at the start of the string, returning a Match object, or None if no match was found.".into())];
    f.sections = vec![Section { kind: SectionKind::Errors, body: String::new(), entries: vec![("re.error".into(), "If the pattern itself is invalid.".into())] }];
    let view = compile(&f);
    let call = view.call.as_ref().expect("a call");
    assert!(call.ports.iter().all(|p| p.ty.origin == Origin::Code || p.ty.origin == Origin::Docs));
    assert_eq!(call.fails.as_ref().map(|f| f.word), Some(FailWord::Raises));
    assert!(view.rail.how.as_deref().is_some_and(|how| how.contains("read from its docs")));
}

#[test]
fn a_typescript_declaration_keeps_its_declared_types() {
    let mut f = Facts::new("parse", Kind::Method, Lang::TypeScript, "zod");
    f.owner = Some("ZodType".into());
    f.signature = Some("parse(data: unknown, params?: core.ParseContext<core.$ZodIssue>): core.output<this>;".into());
    let view = compile(&f);
    let call = view.call.expect("a call");
    assert_eq!(call.ports[0].ty.word, "anything");
    assert!(!call.ports[0].ty.origin.dotted());
    assert_eq!(call.ports[1].joint, Joint::Optional);
    assert!(view.rail.how.is_none(), "declared types need no note");
}

#[test]
fn tom_and_smallvec_read_like_serde_json() {
    // toml's `try_into` and smallvec's `push`, as their sources write them.
    let mut f = Facts::new("try_into", Kind::Method, Lang::Rust, "toml");
    f.owner = Some("Value".into());
    f.signature = Some("pub fn try_into<'de, T>(self) -> Result<T, crate::de::Error>\n    where\n        T: de::Deserialize<'de>,".into());
    let call = compile(&f).call.expect("a call");
    assert_eq!(call.fails.as_ref().map(|f| f.ty.word.as_str()), Some("Error"));
    let mut f = Facts::new("push", Kind::Method, Lang::Rust, "smallvec");
    f.owner = Some("SmallVec".into());
    f.signature = Some("pub fn push(&mut self, value: A::Item)".into());
    let call = compile(&f).call.expect("a call");
    assert_eq!(call.ports[0].name, "value");
    assert!(call.fails.is_none() && call.none.is_none());
}

#[test]
fn an_io_kind_is_possible_when_the_call_takes_a_reader() {
    let mut f = from_str();
    f.name = "from_reader".into();
    f.signature = Some("pub fn from_reader<R, T>(rdr: R) -> Result<T>\nwhere\n    R: io::Read,\n    T: DeserializeOwned,".into());
    let view = compile(&f);
    let kinds = &view.fails.as_ref().expect("if it fails").kinds;
    assert!(kinds[0].impossible.is_none(), "a reader can fail to read");
    let text = compile(&from_str());
    assert!(text.fails.as_ref().expect("if it fails").kinds[0].impossible.as_deref().is_some_and(|why| why.starts_with("Io can't happen")));
}

#[test]
fn a_kinds_lead_in_is_dropped_whatever_the_crate() {
    let view = compile(&from_str());
    let kinds = &view.fails.as_ref().expect("if it fails").kinds;
    assert_eq!(kinds[1].doc, "input that was not syntactically valid JSON");
    let mut other = from_str();
    other.error_kinds = vec![("Timeout".into(), "This error is due to a peer that stopped answering.".into())];
    let view = compile(&other);
    assert_eq!(view.fails.as_ref().expect("if it fails").kinds[0].doc, "a peer that stopped answering");
}

#[test]
fn a_case_row_says_in_words_how_your_places_use_it() {
    use crate::anatomy::symbol::derive::uses::{Reader, Rel, Site, mark_of, read_all};
    use crate::anatomy::symbol::with_uses;
    let view = compile(&value());
    let site = |text: &str| Site { package: "engine".into(), file: "src/a.rs".into(), path: "/w/a.rs".into(), line: 1, text: text.into(), mark: mark_of(text, "Value"), rel: Rel::TypeReference, exact: true };
    let sites = [
        site("Some(serde_json::Value::Null) => Absent,"),
        site("assert!(matches!(v, Value::Null));"),
        site("other => serde_json::Value::Null,"),
        site("let v = Value::Array(items);"),
    ];
    let uses = read_all(&sites, &Reader::of(&view));
    let view = with_uses(view, &uses);
    let Some(Shape::OneOf(cases)) = &view.shape else { panic!("a fork") };
    let says = |name: &str| cases.iter().find(|case| case.name == name).and_then(|case| case.yours.says());
    assert_eq!(says("Null").as_deref(), Some("you match it · 2 · build it · 1"));
    assert_eq!(says("Array").as_deref(), Some("you build it · 1"));
    assert_eq!(says("Bool"), None, "a case nobody touches says nothing, not a zero");
}

#[test]
fn a_trait_says_how_far_it_is_implemented_on_this_machine() {
    use crate::anatomy::symbol::facts::Implementors;
    assert_eq!(Implementors { total: 8322, crates: 343, derived: Some(83) }.says(), "8,322 types implement it across 343 crates on this machine, 83% of them by derive.");
    assert_eq!(Implementors { total: 1, crates: 1, derived: None }.says(), "1 type implements it across 1 crate on this machine.");
    assert_eq!(Implementors { total: 1_204_005, crates: 12, derived: None }.says(), "1,204,005 types implement it across 12 crates on this machine.");
}

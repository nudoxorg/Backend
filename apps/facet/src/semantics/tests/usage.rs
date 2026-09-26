//! Mining the statement a caller writes, over the fixture's registry sources
//! (vendored, so their lines never move) and quoted workspace excerpts.

use crate::semantics::usage::{Needle, find, mine};
use std::path::PathBuf;

fn registry(file: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../Nudox-Design-System/v4/graph/registry").join(file);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn free(name: &str) -> Needle {
    Needle { name: name.into(), member: false }
}

fn member(name: &str) -> Needle {
    Needle { name: name.into(), member: true }
}

#[test]
fn raw_value_from_string_uses_from_str_in_its_first_statement() {
    // serde_json::raw::RawValue::from_string, lines 186–192 in the fixture.
    let source = registry("serde_json-1.0.151/src/raw.rs");
    let got = mine(&source, 186, 192, &free("from_str")).expect("a use");
    assert_eq!(got.line, 187);
    assert_eq!(got.text(), "let borrowed = tri!(crate::from_str::<&Self>(&json));");
    assert_eq!(&got.lines[got.hit][got.mark.clone()], "from_str");
}

#[test]
fn a_tail_expression_stops_before_the_callers_closing_brace() {
    // serde_json::de::from_str (2709–2714): signature over four lines, then
    // `from_trait(read::StrRead::new(s))` as the tail expression.
    let source = registry("serde_json-1.0.151/src/de.rs");
    let got = mine(&source, 2709, 2714, &free("from_trait")).expect("a use");
    assert_eq!(got.line, 2713);
    assert_eq!(got.lines, ["from_trait(read::StrRead::new(s))"]);
}

#[test]
fn the_signature_is_never_searched() {
    // `from_str` names itself in its own signature (line 2709); the body does
    // not call `from_str`, so there is no use.
    let source = registry("serde_json-1.0.151/src/de.rs");
    assert_eq!(mine(&source, 2709, 2714, &free("from_str")), None);
}

/// `extensions/qdrant/src/server/wire/response.rs`, lines 26–31.
const QDRANT: &str = "pub(crate) fn decode<'body, T: Deserialize<'body>>(
    phase: RequestPhase,
    body: &'body str,
) -> Result<T, QdrantError> {
    serde_json::from_str(body).map_err(|source| QdrantError::Decode { phase, source })
}";

#[test]
fn qdrant_decode_uses_serde_json_from_str() {
    let got = mine(QDRANT, 1, 6, &free("from_str")).expect("a use");
    assert_eq!(got.line, 5);
    assert_eq!(got.text(), "serde_json::from_str(body).map_err(|source| QdrantError::Decode { phase, source })");
    assert_eq!(got.mark, 12..20);
}

/// `crates/present/assemble.rs`, the tail of `relation_groups` (the call is
/// on its line 250).
const ASSEMBLE: &str = "fn relation_groups() -> Box<[RelationGroup]> {
    if groups.is_empty() {
        return Box::new([]);
    }
    groups
        .into_iter()
        .map(|(label, relations)| RelationGroup::new(label, relations.into_boxed_slice()))
        .collect::<Vec<_>>()
        .into_boxed_slice()
}";

#[test]
fn a_chained_statement_takes_at_most_three_lines_dedented() {
    let got = mine(ASSEMBLE, 1, 10, &free("RelationGroup")).expect("a use");
    assert_eq!(got.line, 7);
    assert_eq!(
        got.lines,
        [
            ".map(|(label, relations)| RelationGroup::new(label, relations.into_boxed_slice()))",
            ".collect::<Vec<_>>()",
            ".into_boxed_slice()",
        ]
    );
    // The method needle wants `::new` or `.new`, never a bare word.
    let got = mine(ASSEMBLE, 1, 10, &member("new")).expect("a use");
    assert_eq!(got.line, 3);
    assert_eq!(got.lines[0], "return Box::new([]);");
}

#[test]
fn needles_respect_word_boundaries() {
    assert_eq!(find("let x = from_str_lossy(s);", &free("from_str")), None);
    assert_eq!(find("let x = my_from_str(s);", &free("from_str")), None);
    assert_eq!(find("x.from_str(s)", &free("from_str")), Some(2..10));
    assert_eq!(find("renew(x); Group::new(y)", &member("new")), Some(17..20));
    assert_eq!(find("let new = 1;", &member("new")), None);
    assert_eq!(find("é new", &free("new")), Some(3..6));
}

#[test]
fn comments_attributes_and_nested_items_are_skipped() {
    let source = "fn outer() {
    // RelationGroup is built below
    #[allow(unused)]
    fn inner() -> RelationGroup { todo!() }
    let g = RelationGroup::default();
}";
    let got = mine(source, 1, 6, &free("RelationGroup")).expect("a use");
    assert_eq!(got.line, 5);
}

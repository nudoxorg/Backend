//! Generic bounds as sentences, over fixture generics and where-clauses.

use crate::semantics::bounds::{Generic, generics};
use crate::semantics::types::{Nowhere, Piece, Scope};

fn plain(pieces: &[Piece]) -> String {
    pieces.iter().map(Piece::text).collect()
}

fn sentences(lists: &[&str], wh: &str) -> Vec<String> {
    let scope = Scope::new(&Nowhere);
    generics(lists, wh)
        .iter()
        .map(|g| format!("{} {}", g.name, plain(&scope.sentence(g))))
        .collect()
}

#[test]
fn from_str_says_t_is_any_deserialize() {
    // serde_json::de::from_str: gen "'a, T", wh "T:de::Deserialize<'a>,"
    assert_eq!(sentences(&["'a, T"], "T:de::Deserialize<'a>,"), ["T is any Deserialize"]);
}

#[test]
fn visitor_methods_take_their_bound_from_the_where_clause() {
    // serde_core::de::Visitor::visit_seq: gen "A", wh "A:SeqAccess<'de>,"
    assert_eq!(sentences(&["A"], "A:SeqAccess<'de>,"), ["A is any SeqAccess"]);
    // visit_some: D:Deserializer<'de>
    assert_eq!(sentences(&["D"], "D:Deserializer<'de>,"), ["D is any Deserializer"]);
}

#[test]
fn parents_generics_join_and_several_bounds_read_with_and() {
    assert_eq!(
        sentences(&["F", "K: Ord + Clone, V"], "F: Fn(&K) -> bool, V: ?Sized + Send"),
        ["F is a function of K → bool", "K is any Ord and Clone", "V is any Send"]
    );
}

#[test]
fn an_unbounded_parameter_is_any_type_and_lifetimes_never_appear() {
    assert_eq!(sentences(&["'de, T: 'de"], ""), ["T is any type"]);
    assert_eq!(sentences(&["'a, 'b"], "'a: 'b"), Vec::<String>::new());
}

#[test]
fn a_const_parameter_is_a_fixed_value() {
    let gs = generics(&["const N: usize, T = u8"], "");
    assert_eq!(gs[0], Generic { name: "N".into(), bounds: vec![], constant: Some("usize".into()) });
    assert_eq!(gs[1].name, "T");
    assert_eq!(sentences(&["const N: usize"], ""), ["N is a fixed usize"]);
}

#[test]
fn a_later_list_replaces_an_earlier_name_as_the_prototype_does() {
    let gs = generics(&["T: Clone", "T: Copy"], "");
    assert_eq!(gs.len(), 1);
    assert_eq!(gs[0].bounds, ["Copy"]);
}

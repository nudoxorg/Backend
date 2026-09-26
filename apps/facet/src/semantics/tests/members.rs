//! Parameters, receivers and look-alike folding over fixture members.

use crate::semantics::members::{Look, Receiver, fold, is_receiver, params, prefix};

#[test]
fn receivers_are_never_parameters() {
    for p in ["self", "&self", "&mut self", "mut self", "&'a self", "&'a mut self", "self: Box<Self>", "self:Pin<&mut Self>"] {
        assert!(is_receiver(p), "{p}");
    }
    for p in ["selfish: u8", "s:&'a str", "formatter:&mut fmt::Formatter<'_>", "_:D"] {
        assert!(!is_receiver(p), "{p}");
    }
}

#[test]
fn parameters_split_at_the_first_single_colon() {
    // serde_core::de::Visitor::expecting and present::page::RelationGroup::new
    let ps = params(&["&self", "formatter:&mut fmt::Formatter<'_>"]);
    assert_eq!(ps.len(), 1);
    assert_eq!((ps[0].name.as_str(), ps[0].ty.as_str()), ("formatter", "&mut fmt::Formatter<'_>"));
    let ps = params(&["label:RelationLabel", "relations:impl Into<Box<[Relation]>>"]);
    assert_eq!(ps[1].ty, "impl Into<Box<[Relation]>>");
    let ps = params(&["mut out: std::fmt::Formatter", "x"]);
    assert_eq!((ps[0].name.as_str(), ps[0].ty.as_str()), ("out", "std::fmt::Formatter"));
    assert_eq!((ps[1].name.as_str(), ps[1].ty.as_str()), ("", "x"));
}

#[test]
fn a_by_value_receiver_on_a_copy_type_only_reads_it() {
    // RelationLabel::as_str takes `self` and RelationLabel is Copy.
    assert_eq!(Receiver::of(Some("consumes"), true), Receiver::Reads);
    assert_eq!(Receiver::of(Some("consumes"), false), Receiver::UsesUp);
    assert_eq!(Receiver::of(Some("changes"), true), Receiver::Changes);
    assert_eq!(Receiver::of(None, false), Receiver::Makes);
    assert_eq!(Receiver::UsesUp.heading(), "uses it up");
    assert_eq!(Receiver::Makes.heading(), "makes one, or stands alone");
    assert_eq!(Receiver::Makes.input(), None);
}

#[test]
fn prefixes_are_a_lowercase_word_and_an_underscore() {
    assert_eq!(prefix("visit_bool"), Some("visit_"));
    assert_eq!(prefix("visit_borrowed_str"), Some("visit_"));
    assert_eq!(prefix("__private_visit_untagged_option"), None);
    assert_eq!(prefix("expecting"), None);
    assert_eq!(prefix("as_str"), Some("as_"));
    assert_eq!(prefix("Visit_x"), None);
}

/// serde_core::de::Visitor's provided methods, as the fixture lists them.
fn visitor() -> Vec<(&'static str, &'static str)> {
    let e = "Result<Self::Value, E>";
    let d = "Result<Self::Value, D::Error>";
    let a = "Result<Self::Value, A::Error>";
    vec![
        ("visit_bool", e), ("visit_i8", e), ("visit_i16", e), ("visit_i32", e), ("visit_i64", e),
        ("visit_i128", e), ("visit_u8", e), ("visit_u16", e), ("visit_u32", e), ("visit_u64", e),
        ("visit_u128", e), ("visit_f32", e), ("visit_f64", e), ("visit_char", e), ("visit_str", e),
        ("visit_borrowed_str", e), ("visit_string", e), ("visit_bytes", e), ("visit_borrowed_bytes", e),
        ("visit_byte_buf", e), ("visit_none", e), ("visit_some", d), ("visit_unit", e),
        ("visit_newtype_struct", d), ("visit_seq", a), ("visit_map", a), ("visit_enum", a),
    ]
}

#[test]
fn visitors_twenty_two_look_alikes_fold_and_the_rest_stay_rows() {
    let v = visitor();
    let looks: Vec<Look<'_>> = v.iter().map(|(n, r)| Look { name: n, ret: Some(r), recv: Some("consumes") }).collect();
    let groups = fold(&looks);
    let sizes: Vec<usize> = groups.iter().map(Vec::len).collect();
    // 22 share `visit_` and `Result<Self::Value, E>`; the 2 D- and 3 A-results are too few to fold.
    assert_eq!(sizes, [22, 1, 1, 1, 1, 1]);
    let names: Vec<&str> = groups[1..].iter().map(|g| v[g[0]].0).collect();
    assert_eq!(names, ["visit_some", "visit_newtype_struct", "visit_seq", "visit_map", "visit_enum"]);
    // The folded group keeps first-appearance order.
    assert_eq!(v[groups[0][0]].0, "visit_bool");
    assert_eq!(v[groups[0][21]].0, "visit_unit");
}

#[test]
fn three_look_alikes_or_a_different_receiver_never_fold() {
    let looks = [
        Look { name: "get_a", ret: Some("u8"), recv: Some("reads") },
        Look { name: "get_b", ret: Some("u8"), recv: Some("reads") },
        Look { name: "get_c", ret: Some("u8"), recv: Some("reads") },
        Look { name: "get_d", ret: Some("u8"), recv: Some("changes") },
    ];
    assert_eq!(fold(&looks), [vec![0], vec![1], vec![2], vec![3]]);
}

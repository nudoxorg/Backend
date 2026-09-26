//! Capabilities in words, over the fixture's derives and impls.

use crate::semantics::caps::{Arrives, caps, minimal_derives, trait_name, word};
use gpui::SharedString;

fn words(c: &[crate::semantics::Cap]) -> Vec<String> {
    c.iter().map(|c| c.word.to_string()).collect()
}

#[test]
fn relation_label_can_copy_debug_print_hash_sort_print_and_to_text() {
    // present::glyph::RelationLabel: derives Clone Copy Debug Eq Hash Ord
    // PartialEq PartialOrd, and a written Display.
    let derives = ["Clone", "Copy", "Debug", "Eq", "Hash", "Ord", "PartialEq", "PartialOrd"];
    let got = caps(&derives, &[(SharedString::from("Display"), Some(53_839))], &[] as &[&str]);
    assert_eq!(words(&got), ["copies freely", "debug-prints", "hashes", "sorts", "prints", "to text"]);
    assert_eq!(got[0].arrives, Arrives::Derived);
    assert_eq!(got[4].arrives, Arrives::Written);
    assert_eq!(got[4].node, Some(53_839));
    assert_eq!(got[5].arrives, Arrives::Via("Display".into()));
    assert_eq!(got[5].arrives.text(), "via Display");
}

#[test]
fn relation_group_clones_debug_prints_and_compares() {
    // present::page::RelationGroup: derives Clone Debug Eq PartialEq.
    let got = caps(&["Clone", "Debug", "Eq", "PartialEq"], &[], &[] as &[&str]);
    assert_eq!(words(&got), ["clones", "debug-prints", "compares"]);
}

#[test]
fn implied_derives_drop_out() {
    assert_eq!(minimal_derives(&["Clone", "Copy"]), ["Copy"]);
    assert_eq!(minimal_derives(&["PartialEq", "Eq"]), ["Eq"]);
    assert_eq!(minimal_derives(&["PartialOrd", "Ord", "PartialEq", "Eq"]), ["Ord"]);
    // Without its implier, each stays.
    assert_eq!(minimal_derives(&["Clone", "PartialEq", "PartialOrd"]), ["Clone", "PartialEq", "PartialOrd"]);
    assert_eq!(minimal_derives(&["Debug", "Default", "Hash"]), ["Debug", "Default", "Hash"]);
}

#[test]
fn one_word_once_and_foreign_traits_by_their_last_segment() {
    // PartialEq and Eq both read "compares": written after derived, once.
    let got = caps(&["PartialEq"], &[(SharedString::from("Eq"), None)], &["std::fmt::Display", "From<u8>"]);
    assert_eq!(words(&got), ["compares", "prints", "converts", "to text"]);
    assert_eq!(trait_name("serde::de::Deserialize<'de>"), "Deserialize");
    assert_eq!(word("Iterator"), "iterates");
    assert_eq!(word("Visitor"), "Visitor");
}

//! Content truth for the upgrade lens, against the fixture (a slice of the
//! real toml and smallvec releases and this workspace's uses of them).

use super::{Mark, What, fixture, lens, marked, summary};
use crate::semantics::types::Nowhere;

const TOML_PIN: &str = "0.8.23";
const TOML_NEXT: &str = "1.1.6+spec-1.1.0";

fn toml() -> &'static super::Crate {
    fixture::get("toml").expect("toml is in the fixture")
}

#[test]
fn every_use_the_toml_upgrade_touches_is_only_respelled() {
    let krate = toml();
    let impact = krate.impact(TOML_PIN, TOML_NEXT);
    let paths: Vec<&str> = impact.iter().map(|u| u.change.path.as_ref()).collect();
    assert_eq!(paths, ["toml::from_str"; 4], "the four uses the upgrade touches");
    for u in impact {
        assert!(u.change.respelled(), "{} at {} should read the same in plain words", u.change.path, u.site.place());
    }

    let lens = lens(krate, "toml::value::Value", TOML_NEXT, &Nowhere);
    assert_eq!(lens.heading(), "Upgrading to 1.1.6");
    assert_eq!(lens.your_code().as_deref(), Some("none of the 80 places your code uses toml change"));
    let (lead, places, more) = lens.respelled_line().expect("the respelled uses get their quiet line");
    assert_eq!(lead, "4 touch toml::from_str, respelled but the same in plain words: ");
    assert_eq!(
        places,
        [
            "forge/manifest.rs:91",
            "builtin/local_manifest.rs:18",
            "builtin/local_manifest.rs:35",
            "local_package/manifest.rs:165",
        ]
    );
    assert_eq!(more, None);
    assert!(lens.affected.is_empty());

    let line = summary(krate, TOML_PIN, TOML_NEXT);
    println!("shelf: {}", line.words(TOML_NEXT));
    assert!(line.words(TOML_NEXT).ends_with(" · none of your 80 uses change"), "{}", line.words(TOML_NEXT));
    assert_eq!(line.changing, 0);
}

#[test]
fn map_insert_is_a_real_change_marked_where_it_differs() {
    let krate = toml();
    let change = krate
        .changes(TOML_PIN, TOML_NEXT)
        .into_iter()
        .find(|c| c.path == "toml::map::Map::insert")
        .expect("Map::insert changed");
    assert_eq!(change.what, What::Changed);
    assert!(!change.respelled(), "Map::insert really changed");
    let (before, after) = marked(change.before.as_deref().unwrap_or(""), change.after.as_deref().unwrap_or(""), &Nowhere);
    assert_eq!(before.plain(), "(k text, v Value) → maybe Value");
    assert_eq!(after.plain(), "(k K, v V) → maybe V");
    // Exactly the differing subtrees are marked; `maybe`, the names and the
    // punctuation are shared.
    assert_eq!(before.marked(Mark::Old), ["text", "Value", "Value"]);
    assert_eq!(after.marked(Mark::New), ["K", "V", "V"]);
    assert!(before.marked(Mark::New).is_empty() && after.marked(Mark::Old).is_empty());
}

#[test]
fn deserializer_new_changes_is_deprecated_and_can_now_fail_once_each() {
    let krate = toml();
    let changes = krate.changes(TOML_PIN, TOML_NEXT);
    let new: Vec<_> = changes.iter().filter(|c| c.name() == "new" && c.path.contains("::Deserializer::")).collect();
    let whats: Vec<What> = new.iter().map(|c| c.what).collect();
    assert_eq!(whats, [What::Changed, What::Deprecated], "one change and one deprecation, re-exports folded: {new:#?}");
    let changed = new[0];
    assert!(!changed.respelled());
    assert!(changed.newly_fails(), "`-> Deserializer` became `-> Result<Deserializer, Error>`");

    let lens = lens(krate, "toml::Deserializer", TOML_NEXT, &Nowhere);
    let rows: Vec<(What, &str, bool)> =
        lens.rows.iter().filter(|r| r.name == "new").map(|r| (r.what, r.name.as_ref(), r.newly_fails)).collect();
    assert_eq!(rows, [(What::Changed, "new", true), (What::Deprecated, "new", false)]);
    let row = lens.rows.iter().find(|r| r.name == "new" && r.what == What::Changed).expect("the changed row");
    assert_eq!(row.after.as_ref().map(|m| m.plain()).as_deref(), Some("(raw text) → Deserializer or fails with Error"));
    assert_eq!(row.after.as_ref().map(|m| m.marked(Mark::New)), Some(vec!["raw".to_owned(), "or fails with Error".to_owned()]));
}

#[test]
fn the_value_page_shows_one_row_per_item_not_per_re_export() {
    let lens = lens(toml(), "toml::value::Value", TOML_NEXT, &Nowhere);
    let rows: Vec<String> = lens
        .rows
        .iter()
        .map(|r| format!("{} {} {}", r.what.word(), r.name, r.after.as_ref().map(|m| m.plain()).unwrap_or_default()))
        .collect();
    assert_eq!(rows, ["added deserialize_struct (name text, _fields list of text, visitor V) → V’s Value or fails with Error"]);
    // The same page reached through the root re-export says the same.
    assert_eq!(lens, super::lens(toml(), "toml::Value", TOML_NEXT, &Nowhere));
}

#[test]
fn smallvec_1_16_0_to_1_16_1_changes_nothing() {
    let krate = fixture::get("smallvec").expect("smallvec is in the fixture");
    assert_eq!(krate.pinned, "1.16.0");
    assert!(krate.changes("1.16.0", "1.16.1").is_empty());
    assert!(krate.impact("1.16.0", "1.16.1").is_empty());
    let line = summary(krate, "1.16.0", "1.16.1");
    assert_eq!(line.words("1.16.1"), "nothing breaking · 0 added · none of your 8 uses change");
    let lens = lens(krate, "smallvec::SmallVec", "1.16.1", &Nowhere);
    assert!(lens.rows.is_empty() && lens.affected.is_empty() && lens.respelled.is_empty());
    assert_eq!(lens.your_code().as_deref(), Some("none of the 8 places your code uses smallvec change"));
}

#[test]
fn a_release_not_on_this_machine_is_said_so() {
    let krate = toml();
    let old = krate.versions.iter().find(|v| !v.local).expect("some toml release is date-only");
    let line = summary(krate, TOML_PIN, &old.v);
    assert!(!line.local);
    assert_eq!(line.words(&old.v), format!("{} is not on this machine; only its date is known", super::short(&old.v)));
    let lens = lens(krate, "toml::value::Value", &old.v, &Nowhere);
    assert!(!lens.compared() && lens.rows.is_empty());
}

#[test]
fn going_back_reads_as_going_back() {
    let lens = lens(toml(), "toml::value::Value", "0.5.11", &Nowhere);
    assert_eq!(lens.heading(), "Going back to 0.5.11");
    assert!(lens.compared());
}

/// The crate-wide counts, and the three places they differ from the
/// prototype's (73 breaking · 78 added · 5 respelled): each is a prototype
/// defect, pinned here so it cannot come back.
#[test]
fn the_toml_upgrade_counts_each_item_once() {
    let krate = toml();
    let line = summary(krate, TOML_PIN, TOML_NEXT);
    assert_eq!(line.words(TOML_NEXT), "71 breaking · 77 added · 7 respelled · none of your 80 uses change");
    let changes = krate.changes(TOML_PIN, TOML_NEXT);
    let find = |what: What, path: &str| changes.iter().filter(|c| c.what == what && c.path == path).count();
    // 1. `toml::from_slice` is `toml::de::from_slice` re-exported: one item
    //    (the prototype keyed on the last two segments, so counted it twice).
    assert_eq!(find(What::Added, "toml::from_slice") + find(What::Added, "toml::de::from_slice"), 1);
    // 2. `de::Error::fmt` and `ser::Error::fmt` are two items that happen to
    //    share a tail (the prototype merged them).
    assert_eq!(find(What::Changed, "toml::de::Error::fmt"), 1);
    assert_eq!(find(What::Changed, "toml::ser::Error::fmt"), 1);
    // 3. `v: f32` → `mut v: f32` is a binding the caller never sees.
    let f32 = changes.iter().find(|c| c.path == "toml::ser::ValueSerializer::serialize_f32").expect("serialize_f32 changed");
    assert!(f32.respelled(), "{:?} → {:?}", f32.before, f32.after);
}

#[test]
fn the_comb_touches_only_releases_that_really_change_your_code() {
    // Every local toml release only respells `toml::from_str` against the
    // pin (a lifetime moved), so none of them touches your code.
    let krate = toml();
    assert!(krate.touches().is_empty(), "{:?}", krate.touches());
    assert!(fixture::get("smallvec").expect("smallvec is in the fixture").touches().is_empty());
    // Had one of your uses been `Map::insert`, 1.1.6 would touch it.
    let insert = krate
        .changes(TOML_PIN, TOML_NEXT)
        .into_iter()
        .find(|c| c.path == "toml::map::Map::insert")
        .expect("Map::insert changed");
    let mut yours = krate.clone();
    for (from, to, list) in &mut yours.impact {
        if from == TOML_PIN && to == TOML_NEXT {
            list.push(super::Impacted { site: list[0].site.clone(), change: insert.clone() });
        }
    }
    let touches: Vec<String> = yours.touches().iter().map(ToString::to_string).collect();
    assert_eq!(touches, [TOML_NEXT]);
}

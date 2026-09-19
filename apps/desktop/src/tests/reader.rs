//! Pure tests for the reader's new rules: what a typed name resolves to,
//! what the marks file says, how a click chooses a tab target, and how a
//! file module is named. Nothing here opens a window or reaches a service.
//!
//! Every assertion compares the exact string, the exact sentence, or the
//! exact variant a reader would see. A resolution that picked a plausible
//! version instead of the right one, or a recents file that dropped one
//! entry on the way back, would pass any count and fail here.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "a test states its property directly and aborts on a broken fixture"
)]

use crate::store::catalog::{Ask, Resolution, Suggestion, resolve};
use crate::store::document::{Target, modifier_target};
use crate::store::marks::{Marks, Recent};
use backend_present::PackagePath;
use gpui::Modifiers;

// ------------------------------------------------------------ the ask --

#[test]
fn a_bare_name_asks_for_the_newest_version() {
    assert_eq!(
        Ask::parse("serde"),
        Ask::Named {
            name: "serde".to_owned(),
            version: None
        }
    );
}

#[test]
fn a_name_with_an_at_or_a_space_carries_the_version() {
    for text in ["serde@1.0.200", "serde 1.0.200", "  serde   1.0.200 "] {
        assert_eq!(
            Ask::parse(text),
            Ask::Named {
                name: "serde".to_owned(),
                version: Some("1.0.200".to_owned())
            },
            "{text:?}"
        );
    }
}

#[test]
fn a_scoped_npm_name_keeps_its_scope_and_still_finds_the_version() {
    assert_eq!(
        Ask::parse("@types/node@22.10.1"),
        Ask::Named {
            name: "@types/node".to_owned(),
            version: Some("22.10.1".to_owned())
        }
    );
}

#[test]
fn a_package_url_and_a_folder_are_taken_as_written() {
    assert_eq!(
        Ask::parse("pkg:cargo/serde@1.0.0"),
        Ask::Pinned("pkg:cargo/serde@1.0.0".to_owned())
    );
    assert_eq!(Ask::parse("/tmp/project"), Ask::Folder("/tmp/project".to_owned()));
    assert_eq!(Ask::parse("~/code/x"), Ask::Folder("~/code/x".to_owned()));
}

// ----------------------------------------------------------- resolving --

fn suggestion(ecosystem: &str, name: &str, version: &str) -> Suggestion {
    Suggestion::for_test(
        format!("pkg:{ecosystem}/{name}@{version}"),
        name,
        version,
        ecosystem,
    )
}

fn serde_rows() -> Vec<Suggestion> {
    vec![
        suggestion("cargo", "serde", "1.0.200"),
        suggestion("cargo", "serde", "1.0.210"),
        suggestion("cargo", "serde", "0.9.15"),
        suggestion("cargo", "serde_json", "1.0.128"),
        suggestion("npm", "serde", "3.0.0"),
    ]
}

#[test]
fn no_version_resolves_to_the_newest_recorded_in_the_chosen_ecosystem() {
    assert_eq!(
        resolve(&serde_rows(), "cargo", "serde", None),
        Resolution::Exact {
            coordinate: "pkg:cargo/serde@1.0.210".to_owned()
        }
    );
    assert_eq!(
        resolve(&serde_rows(), "npm", "serde", None),
        Resolution::Exact {
            coordinate: "pkg:npm/serde@3.0.0".to_owned()
        }
    );
}

#[test]
fn an_exact_version_resolves_to_itself_and_says_nothing() {
    let resolved = resolve(&serde_rows(), "cargo", "serde", Some("1.0.200"));
    assert_eq!(
        resolved,
        Resolution::Exact {
            coordinate: "pkg:cargo/serde@1.0.200".to_owned()
        }
    );
    assert_eq!(resolved.notice(), None);
}

#[test]
fn a_missing_version_resolves_to_the_closest_recorded_and_says_so() {
    let resolved = resolve(&serde_rows(), "cargo", "serde", Some("1.0.99"));
    assert_eq!(
        resolved.coordinate(),
        Some("pkg:cargo/serde@1.0.210"),
        "the newest version sharing the 1.0 prefix"
    );
    assert_eq!(
        resolved.notice().as_deref(),
        Some("1.0.99 is not in the local index; indexing 1.0.210, the closest recorded version.")
    );
    let old = resolve(&serde_rows(), "cargo", "serde", Some("0.9.0"));
    assert_eq!(old.coordinate(), Some("pkg:cargo/serde@0.9.15"));
}

#[test]
fn a_version_sharing_nothing_falls_back_to_the_newest() {
    let resolved = resolve(&serde_rows(), "cargo", "serde", Some("7.0.0"));
    assert_eq!(resolved.coordinate(), Some("pkg:cargo/serde@1.0.210"));
}

#[test]
fn a_name_the_catalog_lacks_resolves_to_nothing_with_a_reason() {
    let resolved = resolve(&serde_rows(), "pypi", "serde", None);
    assert_eq!(resolved.coordinate(), None);
    assert_eq!(
        resolved.notice().as_deref(),
        Some("The local index records no pypi package named serde.")
    );
}

#[test]
fn a_prefix_match_on_the_name_is_not_the_package() {
    let resolved = resolve(&serde_rows(), "cargo", "serde_", None);
    assert!(matches!(resolved, Resolution::Unknown { .. }), "{resolved:?}");
}

// --------------------------------------------------------------- marks --

#[test]
fn marks_round_trip_through_the_file_text_in_order() {
    let mut marks = Marks::default();
    assert!(marks.toggle_pin("/home/me/present"));
    assert!(marks.toggle_pin("pkg:cargo/serde@1.0.210"));
    marks.remember(Recent::Project {
        coordinate: "/home/me/present".to_owned(),
    });
    marks.remember(Recent::Declaration {
        coordinate: "/home/me/present::glyph.rs:136::RelationLabel".to_owned(),
    });
    marks.remember(Recent::Package {
        coordinate: "pkg:cargo/serde@1.0.210".to_owned(),
    });
    let text = marks.encode();
    assert_eq!(
        text,
        "pin = /home/me/present\n\
         pin = pkg:cargo/serde@1.0.210\n\
         recent = package pkg:cargo/serde@1.0.210\n\
         recent = symbol /home/me/present::glyph.rs:136::RelationLabel\n\
         recent = project /home/me/present\n"
    );
    assert_eq!(Marks::decode(&text), marks);
}

#[test]
fn toggling_a_pin_twice_removes_it_and_remembering_twice_moves_it_up() {
    let mut marks = Marks::default();
    assert!(marks.toggle_pin("a"));
    assert!(!marks.toggle_pin("a"));
    assert!(marks.pinned().is_empty());
    let first = Recent::Project {
        coordinate: "a".to_owned(),
    };
    let second = Recent::Project {
        coordinate: "b".to_owned(),
    };
    marks.remember(first.clone());
    marks.remember(second);
    marks.remember(first.clone());
    assert_eq!(marks.recent().first(), Some(&first));
    assert_eq!(marks.recent().len(), 2);
}

#[test]
fn forgetting_a_project_drops_its_pin_and_its_pages() {
    let mut marks = Marks::default();
    marks.toggle_pin("/p");
    marks.remember(Recent::Declaration {
        coordinate: "/p::a.rs:1::A".to_owned(),
    });
    marks.remember(Recent::Package {
        coordinate: "pkg:cargo/x@1".to_owned(),
    });
    marks.unpin("/p");
    marks.forget_project("/p");
    assert!(marks.pinned().is_empty());
    assert_eq!(marks.recent().len(), 1);
    assert_eq!(marks.recent()[0].coordinate(), "pkg:cargo/x@1");
}

#[test]
fn a_malformed_marks_line_is_ignored_rather_than_fatal() {
    let marks = Marks::decode("pin = \nrecent = nonsense\nrecent = project /ok\ngarbage\n");
    assert!(marks.pinned().is_empty());
    assert_eq!(marks.recent().len(), 1);
    assert_eq!(marks.recent()[0].noun(), "project");
}

// -------------------------------------------------------------- targets --

#[test]
fn a_plain_click_branches_and_the_primary_modifier_reads_in_place() {
    assert_eq!(modifier_target(Modifiers::default()), Target::Child);
    assert_eq!(modifier_target(Modifiers::secondary_key()), Target::Here);
    let alt = Modifiers {
        alt: true,
        ..Modifiers::default()
    };
    assert_eq!(modifier_target(alt), Target::Background);
}

// -------------------------------------------------------------- modules --

#[test]
fn a_file_is_named_as_the_module_it_is() {
    assert_eq!(PackagePath::new("src/components.rs").stem(), "components");
    assert_eq!(PackagePath::new("pkg/main.py").stem(), "main");
    assert_eq!(PackagePath::new("Makefile").stem(), "Makefile");
    assert_eq!(PackagePath::new(".env").stem(), ".env");
}

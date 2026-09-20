//! Pure tests for theme ramps, motion springs, presentation, and preferences.
//! Every assertion here is about rendered content, never about a count.
//! Nothing in this file opens a window.
//!
//! The reason these live apart from the transport suite is that every defect
//! they are written to catch is *silent*: a ramp that collapses two hues onto
//! one colour still renders, a spring that discards its velocity on retarget
//! still animates, a preferences codec that falls back to the whole-struct
//! default still loads, and a fault whose operand is dropped still draws a
//! box. None of those fail a build, and none of them are visible in a count —
//! the row is still there, it just no longer says anything true. So each test
//! here compares the exact string, the exact slug, or the exact colour plane a
//! reader would see, and a test that could pass by observing only how many
//! things exist is not worth writing.
//!
//! Tab history is deliberately absent rather than faked:
//! [`crate::store::document::Tab::new`] is private and every mutation on
//! `DocumentStore` takes a GPUI `Context`, so nothing about back/forward is
//! reachable without a `TestAppContext`. Covering it needs an entity suite,
//! not a pure one.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "a test states its property directly and aborts on a broken fixture"
)]

use crate::presentation::crumb::{elide_middle, trail};
use crate::presentation::fault::{from_subscription, headline, operand_spelling};
use crate::presentation::project::{badge, is_local};
use crate::store::prefs::{EditorScheme, Preferences};
use crate::store::search::{Mode, Parsed};
use crate::theme::palette::{Appearance, Paint, Palette};
use crate::theme::tokens::{InterfaceSize, PanelWidth};
use backend_library::{
    COMMANDS, CommandFailure, DeclarationKind, ViewRevision, view_state_root,
};
use backend_present::{
    Coordinate, Fault, GRAMMARS, Identity, IdentityShape, KeyTag, Language, Operand, Readiness,
    RowCount, Shelf, domain_name, domains, registry_size,
};
use backend_replication::ReplicationError;

// ---------------------------------------------------------------- identity --

fn crumb_labels(identity: &Identity) -> Vec<String> {
    trail(identity)
        .iter()
        .map(|step| step.label().to_owned())
        .collect()
}

#[test]
fn a_declaration_label_parses_into_its_project_path_line_and_leaf() {
    let identity = Identity::parse("/abs/project::src/lib.rs:2::ferris");
    assert_eq!(identity.shape(), IdentityShape::Declaration);
    assert_eq!(
        identity.project().expect("a project root").name(),
        "project"
    );
    assert_eq!(identity.project().expect("a project root").root(), "/abs/project");
    assert_eq!(identity.path().expect("a source path").as_str(), "src/lib.rs");
    assert_eq!(identity.line().expect("a source line").get(), 2);
    assert_eq!(identity.name(), "ferris");
    assert_eq!(identity.language(), Language::Rust);
    assert_eq!(
        crumb_labels(&identity),
        vec!["project", "src/lib.rs:2", "ferris"]
    );
}

#[test]
fn a_pinned_package_row_parses_as_a_package_shaped_identity() {
    let identity = Identity::parse("pkg:cargo/memchr@2.7.4");
    assert_eq!(identity.shape(), IdentityShape::Package);
    assert_eq!(identity.name(), "memchr@2.7.4");
    assert!(identity.path().is_none(), "a package row names no source file");
    assert!(identity.line().is_none(), "a package row names no line");
    assert_eq!(crumb_labels(&identity), vec!["memchr@2.7.4"]);
}

#[test]
fn a_windows_label_keeps_its_drive_letter_and_still_finds_the_line() {
    let identity = Identity::parse(r"C:\proj::src\lib.rs:2::Thing");
    assert_eq!(identity.shape(), IdentityShape::Declaration);
    assert_eq!(identity.project().expect("a project root").root(), r"C:\proj");
    assert_eq!(identity.project().expect("a project root").name(), "proj");
    assert_eq!(identity.path().expect("a source path").as_str(), r"src\lib.rs");
    assert_eq!(identity.line().expect("a source line").get(), 2);
    assert_eq!(
        crumb_labels(&identity),
        vec!["proj", r"src\lib.rs:2", "Thing"]
    );
}

#[test]
fn a_cyrillic_label_renders_its_own_script_in_every_crumb() {
    let identity = Identity::parse("/tmp/проект::src/файл.rs:9::Структура");
    assert_eq!(identity.project().expect("a project root").name(), "проект");
    assert_eq!(identity.path().expect("a source path").as_str(), "src/файл.rs");
    assert_eq!(identity.line().expect("a source line").get(), 9);
    assert_eq!(identity.name(), "Структура");
    assert_eq!(
        crumb_labels(&identity),
        vec!["проект", "src/файл.rs:9", "Структура"]
    );
}

#[test]
fn an_arabic_identifier_survives_parsing_without_reordering() {
    let identity = Identity::parse("/tmp/مشروع::src/lib.rs:3::متغير");
    assert_eq!(identity.project().expect("a project root").name(), "مشروع");
    assert_eq!(identity.name(), "متغير");
    assert_eq!(
        crumb_labels(&identity),
        vec!["مشروع", "src/lib.rs:3", "متغير"]
    );
}

#[test]
fn every_parsed_identity_round_trips_to_the_exact_coordinate_bytes() {
    let labels = [
        "/abs/project::src/lib.rs:2::ferris",
        "pkg:cargo/memchr@2.7.4",
        r"C:\proj::src\lib.rs:2::Thing",
        "/tmp/проект::src/файл.rs:9::Структура",
        "/tmp/مشروع::src/lib.rs:3::متغير",
        "/abs/project::src/lib.rs",
        "/abs/project",
    ];
    for label in labels {
        let identity = Identity::parse(label);
        assert_eq!(
            identity.coordinate().as_str(),
            label,
            "the engine only accepts the spelling it gave us"
        );
    }
}

// ------------------------------------------------------------------- elide --

#[test]
fn eliding_a_long_label_keeps_both_of_its_ends() {
    let text = "/Users/reader/code/polyglot/crates/present/identity.rs";
    let elided = elide_middle(text, 24);
    assert_eq!(elided, "/Users/read…/identity.rs");
    assert!(elided.starts_with("/Users/read"), "the head is never elided");
    assert!(elided.ends_with("identity.rs"), "the tail is never elided");
}

#[test]
fn a_label_shorter_than_its_budget_is_returned_unchanged() {
    assert_eq!(elide_middle("src/lib.rs", 24), "src/lib.rs");
    assert_eq!(elide_middle("src/lib.rs", 10), "src/lib.rs");
}

#[test]
fn a_budget_under_six_returns_the_label_unchanged() {
    let text = "/Users/reader/code/polyglot";
    for budget in 0..6_usize {
        assert_eq!(
            elide_middle(text, budget),
            text,
            "a budget of {budget} cannot say anything, so it must not try"
        );
    }
}

#[test]
fn eliding_multi_byte_text_cuts_on_character_boundaries() {
    let text = "αβγδεζηθικλμνξοπρστυ";
    let elided = elide_middle(text, 9);
    assert_eq!(elided, "αβγδ…ρστυ");
    assert_eq!(elided.chars().count(), 9);
    assert_eq!(elide_middle("日本語のファイル名です", 7), "日本語…名です");
}

// ------------------------------------------------------------------ omnibar --

#[test]
fn empty_text_and_plain_text_both_mean_search() {
    let empty = Parsed::of("");
    assert_eq!(empty.mode(), &Mode::Search);
    assert_eq!(empty.term(), "");
    let plain = Parsed::of("ferris");
    assert_eq!(plain.mode(), &Mode::Search);
    assert_eq!(plain.term(), "ferris");
}

#[test]
fn a_leading_chevron_means_palette_and_carries_the_rest_as_the_term() {
    let bare = Parsed::of(">");
    assert_eq!(bare.mode(), &Mode::Palette);
    assert_eq!(bare.term(), "");
    let named = Parsed::of("> health");
    assert_eq!(named.mode(), &Mode::Palette);
    assert_eq!(named.term(), "health");
}

#[test]
fn a_leading_at_scopes_the_search_to_the_project_it_names() {
    let scoped = Parsed::of("@polyglot foo");
    assert_eq!(
        scoped.mode(),
        &Mode::Scoped {
            project: "polyglot".to_owned()
        }
    );
    assert_eq!(scoped.term(), "foo");
}

#[test]
fn a_bare_at_names_no_project_and_stays_a_search() {
    let bare = Parsed::of("@");
    assert_eq!(bare.mode(), &Mode::Search);
    assert_eq!(bare.term(), "@");
}

// ------------------------------------------------------------------ palette --

/// Mirrors the palette's own filter over the shared grammar table.
///
/// `store::search::palette_rows` is private and its store needs a GPUI
/// context, so the property is asserted against the same source the palette
/// reads — [`GRAMMARS`] joined to the registry — rather than against a fake.
fn matching_commands(term: &str) -> Vec<(&'static str, &'static str, &'static str)> {
    let needle = term.trim().to_ascii_lowercase();
    GRAMMARS
        .into_iter()
        .filter_map(|grammar| grammar.spec().map(|spec| (grammar, spec)))
        .filter(|(grammar, spec)| {
            needle.is_empty()
                || spec.name.to_ascii_lowercase().contains(&needle)
                || spec.title.to_ascii_lowercase().contains(&needle)
                || spec.description.to_ascii_lowercase().contains(&needle)
                || grammar
                    .aliases()
                    .iter()
                    .any(|alias| alias.to_ascii_lowercase().contains(&needle))
        })
        .map(|(_, spec)| (spec.name, spec.title, spec.description))
        .collect()
}

#[test]
fn filtering_the_palette_yields_the_registrys_verbatim_words() {
    assert_eq!(
        matching_commands("health"),
        vec![(
            "health",
            "Health",
            "Report every capability as ready, unreachable, unconfigured, or detached."
        )],
        "the palette must not paraphrase the registry"
    );
}

#[test]
fn every_grammar_names_a_registry_row_and_every_row_has_a_grammar() {
    let mut from_grammars: Vec<&'static str> = GRAMMARS
        .into_iter()
        .map(|grammar| {
            let spec = grammar
                .spec()
                .unwrap_or_else(|| panic!("grammar {} has no registry row", grammar.name()));
            assert_eq!(spec.name, grammar.name());
            spec.name
        })
        .collect();
    let mut from_registry: Vec<&'static str> = COMMANDS.iter().map(|spec| spec.name).collect();
    from_grammars.sort_unstable();
    from_registry.sort_unstable();
    assert_eq!(from_grammars, from_registry);
    assert_eq!(GRAMMARS.len(), registry_size());
}

#[test]
fn an_unfiltered_palette_lists_the_whole_registry_in_five_domains() {
    assert_eq!(matching_commands("").len(), COMMANDS.len());
    let names: Vec<&'static str> = domains().into_iter().map(domain_name).collect();
    assert_eq!(
        names,
        vec!["library", "registry", "home", "session", "system"]
    );
}

// -------------------------------------------------------------------- shelf --

// These three reach `crate::ui::glyph`, which exists on every platform that
// has a window — `#[cfg(unix)]` was one platform too narrow and quietly took
// the shelf marks, including the ✗ a failed index now draws, out of the
// Windows suite along with the imports they use.
#[cfg(any(unix, windows))]
fn shelf_fault() -> Fault {
    Fault::from_command_failure(
        &CommandFailure::NotFound,
        Operand::Coordinate(Coordinate::new("/abs/project")),
    )
}

#[cfg(any(unix, windows))]
#[test]
fn each_standing_draws_the_mark_this_window_reserves_for_it() {
    use crate::presentation::project::standing;
    use crate::ui::glyph::standing_glyph;
    use backend_present::{Language, LanguageCount, ShelfEntry};

    let identity = Identity::parse("/abs/project");
    let published = vec![LanguageCount::new(Language::Rust, 12)];
    let states = [
        (Readiness::Ready, published.clone(), "✓"),
        (
            Readiness::Indexing {
                rows: RowCount::new(12),
            },
            published.clone(),
            "◐",
        ),
        (
            Readiness::Failed {
                fault: shelf_fault(),
            },
            published,
            "✗",
        ),
        (Readiness::Requested, Vec::new(), "○"),
    ];
    for (readiness, languages, mark) in states {
        let name = readiness.name();
        let entry = ShelfEntry::new(identity.clone(), readiness).with_languages(languages);
        assert_eq!(
            standing_glyph(standing(&entry)),
            mark,
            "readiness {name} drew the wrong mark"
        );
    }
}

#[cfg(any(unix, windows))]
#[test]
fn a_ready_project_that_published_nothing_does_not_draw_as_finished() {
    use crate::presentation::project::{Standing, standing, summary};
    use crate::ui::glyph::standing_glyph;
    use backend_present::ShelfEntry;

    let entry = ShelfEntry::new(Identity::parse("pkg:cargo/memchr@2.7.4"), Readiness::Ready);
    assert_eq!(standing(&entry), Standing::Empty);
    assert_eq!(
        standing_glyph(standing(&entry)),
        "○",
        "a project with nothing under it must not wear the ready mark"
    );
    assert_eq!(summary(&entry), "ready · nothing published under it");
}

#[test]
fn a_folder_badges_as_local_and_a_pinned_package_badges_as_its_version() {
    let folder = Identity::parse("/abs/project::src/lib.rs:2::ferris");
    assert!(is_local(&folder));
    assert_eq!(badge(&folder), "local");

    let package = Identity::parse("pkg:cargo/memchr@2.7.4");
    assert!(!is_local(&package));
    assert_eq!(badge(&package), "2.7.4");
}

// ------------------------------------------------------------------- motion --

#[test]
fn a_spring_retarget_preserves_its_current_velocity() {
    use crate::motion::spring::{Spring, Stiffness};
    use std::time::Duration;

    let step = Duration::from_millis(16);
    let mut spring = Spring::at(0.0, Stiffness::PANEL);
    spring.retarget(100.0);
    assert!(spring.advance(step));
    assert!(spring.advance(step));
    let before = spring.value();
    assert!(before > 0.0, "the spring must be moving before it is retargeted");

    // Retarget to a value the spring has already passed. A spring that kept
    // its speed carries on past it for at least one more step; a spring that
    // dropped its speed would turn round immediately. Reading the velocity
    // back would only prove the field was stored, not that motion is carried.
    spring.retarget(before / 2.0);
    assert!(spring.advance(step));
    assert!(
        spring.value() > before,
        "retargeting must carry the spring's speed, not restart it from rest"
    );

    let mut restarted = Spring::at(before, Stiffness::PANEL);
    restarted.retarget(before / 2.0);
    assert!(restarted.advance(step));
    assert!(
        restarted.value() < before,
        "a spring starting from rest must move straight toward its target"
    );
}

#[test]
fn snapping_a_spring_zeroes_its_velocity_and_lands_exactly() {
    use crate::motion::spring::{Spring, Stiffness};
    use std::time::Duration;

    let mut spring = Spring::at(0.0, Stiffness::PANEL);
    spring.retarget(100.0);
    assert!(spring.advance(Duration::from_millis(16)));
    spring.snap(42.0);
    assert!((spring.value() - 42.0).abs() <= f32::EPSILON);
    assert!((spring.target() - 42.0).abs() <= f32::EPSILON);
    assert!(spring.settled());
}

#[test]
fn a_settled_spring_asks_for_no_further_frame_and_sits_on_its_target() {
    use crate::motion::spring::{Spring, Stiffness};
    use std::time::Duration;

    let mut spring = Spring::at(0.0, Stiffness::PANEL);
    spring.retarget(1.0);
    let mut frames = 0_u32;
    while spring.advance(Duration::from_millis(16)) {
        frames += 1;
        assert!(frames < 600, "a panel spring must settle inside ten seconds");
    }
    assert!(
        (spring.value() - spring.target()).abs() <= f32::EPSILON,
        "a settled spring must sit exactly on its target, not near it"
    );
    assert!(
        !spring.advance(Duration::from_millis(16)),
        "an idle window must schedule no frames"
    );
}

#[test]
fn reduced_motion_collapses_every_beat_to_nothing() {
    use crate::motion::Beat;
    use std::time::Duration;

    for beat in [Beat::Touch, Beat::Reveal, Beat::Unfold] {
        assert_eq!(
            beat.duration(true),
            Duration::ZERO,
            "reduced motion must remove the beat, not shorten it"
        );
        assert!(beat.duration(false) > Duration::ZERO);
    }
    assert_eq!(Beat::Touch.duration(false), Duration::from_millis(90));
    assert_eq!(Beat::Reveal.duration(false), Duration::from_millis(140));
    assert_eq!(Beat::Unfold.duration(false), Duration::from_millis(220));
}

// -------------------------------------------------------------------- prefs --

fn altered_preferences() -> Preferences {
    Preferences::default()
        .with_appearance(Appearance::Vellum)
        .with_interface(InterfaceSize::percent(130))
        .with_widths(300.0, 200.0)
        .with_open(false, false)
        .with_reduced_motion(true)
        .with_editor(EditorScheme::Zed)
}

#[test]
fn encoding_and_decoding_preferences_returns_every_changed_field() {
    let prefs = altered_preferences();
    let text = prefs.encode();
    assert_eq!(
        text,
        "appearance = vellum\ninterface = 130\nlibrary-width = 300\ncontext-width = 200\n\
         library-open = false\ncontext-open = false\nreduced-motion = true\neditor = zed\n"
    );
    let decoded = Preferences::decode(&text);
    assert_eq!(decoded.appearance(), Appearance::Vellum);
    assert_eq!(decoded.interface().get(), 130);
    assert_eq!(decoded.editor().name(), "zed");
    assert!(!decoded.library_open());
    assert!(!decoded.context_open());
    assert!(decoded.reduced_motion());
    assert_eq!(decoded, prefs);
}

#[test]
fn an_unknown_preference_key_is_ignored_rather_than_fatal() {
    let decoded = Preferences::decode("appearance = vellum\nchromatic-aberration = 3\n");
    assert_eq!(decoded.appearance(), Appearance::Vellum);
    assert_eq!(decoded.editor().name(), EditorScheme::default().name());
    assert_eq!(decoded.interface().get(), InterfaceSize::DEFAULT.get());
}

#[test]
fn a_malformed_value_falls_back_to_that_field_and_keeps_the_rest() {
    let decoded = Preferences::decode(
        "appearance = vellum\ninterface = banana\nreduced-motion = perhaps\neditor = zed\n",
    );
    assert_eq!(
        decoded.appearance(),
        Appearance::Vellum,
        "one bad line must not cost the reader every other setting"
    );
    assert_eq!(decoded.editor().name(), "zed");
    assert_eq!(decoded.interface().get(), InterfaceSize::DEFAULT.get());
    assert!(!decoded.reduced_motion());
}

#[test]
fn panel_widths_decode_clamped_into_their_supported_range() {
    let decoded = Preferences::decode("library-width = 40\ncontext-width = 9000\n");
    assert!((decoded.library_width() - PanelWidth::MIN_LIBRARY).abs() <= f32::EPSILON);
    assert!((decoded.context_width() - PanelWidth::MAX_CONTEXT).abs() <= f32::EPSILON);
}

// --------------------------------------------------------------------- ramp --

#[test]
fn every_declaration_kind_sits_on_one_luminance_plane_at_its_own_hue() {
    use crate::theme::kind::{ALL_KINDS, kind_glyph};

    for appearance in [Appearance::Ink, Appearance::Vellum] {
        let palette = Palette::new(appearance);
        let plane = palette.on_plane(kind_glyph(DeclarationKind::Module).hue()).l;
        for kind in ALL_KINDS {
            let colour = palette.on_plane(kind_glyph(kind).hue());
            assert!(
                (colour.l - plane).abs() <= f32::EPSILON,
                "{} left the plane: {} instead of {plane}",
                kind_glyph(kind).label(),
                colour.l
            );
        }
        let structure = palette.on_plane(kind_glyph(DeclarationKind::Struct).hue());
        let contract = palette.on_plane(kind_glyph(DeclarationKind::Trait).hue());
        assert!(
            (structure.h - contract.h).abs() > 0.01,
            "a struct and a trait must not collapse onto one hue"
        );
    }
}

#[test]
fn every_language_sits_on_one_luminance_plane_at_its_own_hue() {
    use crate::theme::language::{hue, label};

    for appearance in [Appearance::Ink, Appearance::Vellum] {
        let palette = Palette::new(appearance);
        let plane = palette.on_plane(hue(Language::Rust)).l;
        for language in Language::ALL {
            let colour = palette.on_plane(hue(language));
            assert!(
                (colour.l - plane).abs() <= f32::EPSILON,
                "{} left the plane: {} instead of {plane}",
                label(language),
                colour.l
            );
        }
        let rust = palette.on_plane(hue(Language::Rust));
        let typescript = palette.on_plane(hue(Language::TypeScript));
        assert!(
            (rust.h - typescript.h).abs() > 0.01,
            "Rust and TypeScript must not collapse onto one hue"
        );
    }
}

#[test]
fn the_two_appearances_light_different_grounds_and_both_keep_gilt_apart() {
    let ink = Palette::new(Appearance::Ink);
    let vellum = Palette::new(Appearance::Vellum);
    assert_ne!(
        ink.paint(Paint::Ground),
        vellum.paint(Paint::Ground),
        "Vellum is the same design re-lit, not the same colours"
    );
    assert!(ink.paint(Paint::Ground).l < vellum.paint(Paint::Ground).l);
    for palette in [ink, vellum] {
        assert_ne!(
            palette.paint(Paint::Gilt),
            palette.paint(Paint::Text),
            "gilt is reserved for copyable identity and must read as its own tone"
        );
    }
}

// -------------------------------------------------------------------- fault --

fn every_command_failure() -> Vec<(&'static str, CommandFailure)> {
    let expected = ViewRevision::from(view_state_root(&[("a".to_owned(), "one".to_owned())]));
    let observed = ViewRevision::from(view_state_root(&[("b".to_owned(), "two".to_owned())]));
    vec![
        ("not-found", CommandFailure::NotFound),
        ("wrong-basis", CommandFailure::WrongBasis { expected, observed }),
        (
            "invalid-query",
            CommandFailure::InvalidQuery("the query names no admitted field".to_owned()),
        ),
        ("cursor-mismatch", CommandFailure::CursorMismatch),
        (
            "incoherent-view",
            CommandFailure::IncoherentView("row 4 has no basis".to_owned()),
        ),
        ("sequence-overflow", CommandFailure::SequenceOverflow),
        ("mutation-requires-owner", CommandFailure::MutationRequiresOwner),
    ]
}

fn every_client_error() -> Vec<(&'static str, backend_client::ClientError)> {
    use backend_client::ClientError as Wire;
    let expected = view_state_root(&[("a".to_owned(), "one".to_owned())]);
    let observed = view_state_root(&[("b".to_owned(), "two".to_owned())]);
    vec![
        ("endpoint", Wire::Io("connection refused".to_owned())),
        ("protocol", Wire::Protocol("frame 3 failed admission".to_owned())),
        ("transport", Wire::Transport(ReplicationError::MessageTooLarge)),
        ("not-found", Wire::CommandFailed(CommandFailure::NotFound)),
        ("incoherent-view", Wire::IncoherentView),
        ("wrong-basis", Wire::BasisMismatch { expected, observed }),
        ("freshness", Wire::FreshnessMismatch),
        (
            "request-mismatch",
            Wire::RequestMismatch {
                expected: 7,
                observed: 9,
            },
        ),
        ("cursor-mismatch", Wire::CursorMismatch),
    ]
}

fn operand_for_tests() -> Operand {
    Operand::Coordinate(Coordinate::new("/abs/project::src/lib.rs:2::ferris"))
}

#[test]
fn every_command_failure_lowers_to_a_fault_with_an_operand_and_a_headline() {
    for (slug, failure) in every_command_failure() {
        let fault = Fault::from_command_failure(&failure, operand_for_tests());
        assert_eq!(fault.slug().as_str(), slug);
        assert!(
            !fault.cause().sentence().is_empty(),
            "{slug} lowered without a sentence a reader can act on"
        );
        assert!(!headline(&fault).is_empty(), "{slug} lowered without a headline");
        assert!(
            !operand_spelling(fault.operand()).is_empty(),
            "{slug} lost the thing it was about"
        );
    }
}

#[test]
fn every_client_error_lowers_to_a_fault_with_an_operand_and_a_headline() {
    for (slug, error) in every_client_error() {
        let fault = Fault::from_client_error(&error, operand_for_tests());
        assert_eq!(fault.slug().as_str(), slug);
        assert!(
            !fault.cause().sentence().is_empty(),
            "{slug} lowered without a sentence a reader can act on"
        );
        assert!(!headline(&fault).is_empty(), "{slug} lowered without a headline");
        assert!(
            !operand_spelling(fault.operand()).is_empty(),
            "{slug} lost the thing it was about"
        );
    }
}

#[test]
fn three_named_failures_lower_to_exactly_the_words_every_surface_prints() {
    let missing = Fault::from_command_failure(&CommandFailure::NotFound, operand_for_tests());
    assert_eq!(missing.slug().as_str(), "not-found");
    assert_eq!(missing.cause().slug().as_str(), "absent");
    assert_eq!(
        missing.cause().sentence(),
        "no record is published at that identity in this revision"
    );
    assert_eq!(headline(&missing), "Nothing on the shelf has this identity");
    assert_eq!(
        operand_spelling(missing.operand()),
        "/abs/project::src/lib.rs:2::ferris"
    );

    let detail = "the query names no admitted field".to_owned();
    let invalid = Fault::from_command_failure(
        &CommandFailure::InvalidQuery(detail.clone()),
        operand_for_tests(),
    );
    assert_eq!(invalid.slug().as_str(), "invalid-query");
    assert_eq!(invalid.cause().sentence(), detail);

    let owner =
        Fault::from_command_failure(&CommandFailure::MutationRequiresOwner, operand_for_tests());
    assert_eq!(owner.slug().as_str(), "mutation-requires-owner");
    assert_eq!(owner.cause().slug().as_str(), "refused");
    assert_eq!(headline(&owner), "This change needs the durable owner");
}

// ------------------------------------------------------- subscription fault --

fn every_subscription_error() -> Vec<(&'static str, crate::transport::error::ClientError)> {
    use crate::transport::error::ClientError as Feed;
    vec![
        ("endpoint", Feed::Io("the socket closed".to_owned())),
        ("protocol", Feed::Protocol("delta 2 failed admission".to_owned())),
        ("transport", Feed::Transport(ReplicationError::MessageTooLarge)),
        ("incoherent-view", Feed::IncoherentRoot),
        (
            "wrong-basis",
            Feed::BasisMismatch {
                expected: view_state_root(&[("a".to_owned(), "one".to_owned())]),
                observed: view_state_root(&[("b".to_owned(), "two".to_owned())]),
            },
        ),
        ("cursor-mismatch", Feed::CursorMismatch),
    ]
}

#[test]
fn every_subscription_failure_names_the_endpoint_it_happened_on() {
    for (slug, error) in every_subscription_error() {
        let fault = from_subscription(&error, "/tmp/x.sock");
        assert_eq!(fault.slug().as_str(), slug);
        assert_eq!(
            operand_spelling(fault.operand()),
            "/tmp/x.sock",
            "{slug} must say which endpoint dropped"
        );
        assert!(!headline(&fault).is_empty(), "{slug} lowered without a headline");
        assert!(!fault.cause().sentence().is_empty());
    }
}

#[test]
fn a_dropped_feed_and_an_incoherent_root_keep_their_own_slugs() {
    use crate::transport::error::ClientError as Feed;
    let dropped = from_subscription(&Feed::Io("broken pipe".to_owned()), "/tmp/x.sock");
    assert_eq!(dropped.slug().as_str(), "endpoint");
    assert_eq!(dropped.cause().slug().as_str(), "unreachable");
    assert_eq!(
        dropped.cause().sentence(),
        "the live feed disconnected: broken pipe"
    );
    assert_eq!(headline(&dropped), "The local service did not answer");

    let incoherent = from_subscription(&Feed::IncoherentRoot, "/tmp/x.sock");
    assert_eq!(incoherent.slug().as_str(), "incoherent-view");
    assert_eq!(incoherent.cause().slug().as_str(), "unproven");
    assert_eq!(headline(&incoherent), "The view is being rebuilt");
}

// ------------------------------------------------------------ add-a-project --

// `crate::views` only exists on a platform with a window, so the add-flow
// proofs below are gated the same way the module is. `validate` is
// `pub(crate)` for exactly this reason: every sentence a reader sees under the
// add field is decided by a pure function, and a pure function is asserted
// here rather than clicked through by hand on two operating systems.

/// An absolute directory that certainly exists, spelled the way this platform
/// spells one: a drive letter and backslashes on Windows, a leading slash on
/// Unix. Hard-coding either spelling would make this suite pass on one host
/// and lie on the other.
#[cfg(any(unix, windows))]
fn a_real_directory() -> &'static str {
    env!("CARGO_MANIFEST_DIR")
}

#[cfg(any(unix, windows))]
#[test]
fn an_absolute_folder_validates_whether_or_not_it_arrives_quoted() {
    use crate::views::library::validate;

    let bare = a_real_directory();
    assert_eq!(validate(bare).as_deref(), Ok(bare));
    assert_eq!(
        validate(&format!("\"{bare}\"")).as_deref(),
        Ok(bare),
        "Windows Explorer's \"Copy as path\" quotes what it copies"
    );
    assert_eq!(
        validate(&format!("'{bare}'")).as_deref(),
        Ok(bare),
        "a shell copy quotes it the other way"
    );
    assert_eq!(
        validate(&format!("  \"{bare}\"  ")).as_deref(),
        Ok(bare),
        "surrounding whitespace is the reader's, not the path's"
    );
}

#[cfg(any(unix, windows))]
#[test]
fn a_quoted_path_is_no_longer_refused_for_not_being_absolute() {
    use crate::views::library::validate;

    let quoted = format!("\"{}\"", a_real_directory());
    assert_ne!(
        validate(&quoted).err().as_deref(),
        Some("A local project needs an absolute path."),
        "the path inside the quotes is absolute; saying otherwise is a false statement"
    );
}

#[cfg(any(unix, windows))]
#[test]
fn every_refusal_under_the_add_field_says_which_thing_is_wrong() {
    use crate::views::library::validate;

    assert_eq!(
        validate("   ").err().as_deref(),
        Some("Type a package coordinate, or choose a folder.")
    );
    assert_eq!(
        validate("\"\"").err().as_deref(),
        Some("Type a package coordinate, or choose a folder."),
        "an empty pair of quotes carries no path"
    );
    assert_eq!(
        validate("some/relative/folder").err().as_deref(),
        Some("A local project needs an absolute path.")
    );
    let missing = std::path::Path::new(a_real_directory())
        .join("no-such-folder-8f21c3")
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        validate(&missing).err().as_deref(),
        Some("No folder exists at that path."),
        "an absolute path that does not exist is a different failure from a relative one"
    );
    assert_eq!(
        validate(&format!("\"{missing}\"")).err().as_deref(),
        Some("No folder exists at that path."),
        "unquoting must not disguise a missing folder as a relative path"
    );
}

#[cfg(any(unix, windows))]
#[test]
fn a_package_coordinate_still_parses_beside_the_folder_path() {
    use crate::views::library::validate;

    assert_eq!(
        validate("pkg:cargo/memchr@2.7.4").as_deref(),
        Ok("pkg:cargo/memchr@2.7.4")
    );
    assert_eq!(
        validate("\"pkg:cargo/memchr@2.7.4\"").as_deref(),
        Ok("pkg:cargo/memchr@2.7.4"),
        "a quoted coordinate is the same coordinate"
    );
    assert_eq!(
        validate("pkg:cargo/memchr").err().as_deref(),
        Some("A package coordinate needs a pinned @version.")
    );
    assert_eq!(
        validate("pkg:memchr@2.7.4").err().as_deref(),
        Some("A package coordinate looks like pkg:cargo/name@version.")
    );
}

#[cfg(windows)]
#[test]
fn every_windows_spelling_of_one_folder_reaches_the_same_answer() {
    use crate::views::library::validate;

    let native = a_real_directory();
    let forward = native.replace('\\', "/");
    let trailing = format!("{native}\\");
    for spelling in [native, forward.as_str(), trailing.as_str()] {
        assert!(
            validate(spelling).is_ok(),
            "{spelling} names the same existing folder"
        );
        assert!(
            validate(&format!("\"{spelling}\"")).is_ok(),
            "{spelling} is still that folder when Explorer quotes it"
        );
    }
}

#[cfg(any(unix, windows))]
#[test]
fn a_cancelled_folder_picker_is_silent_and_a_broken_one_is_not() {
    use crate::views::library::{FolderChoice, PICKER_DROPPED, folder_choice};
    use std::path::PathBuf;

    assert_eq!(
        folder_choice(Ok(Some(vec![PathBuf::from(a_real_directory())]))),
        FolderChoice::Chosen(PathBuf::from(a_real_directory()))
    );
    assert_eq!(folder_choice(Ok(None)), FolderChoice::Cancelled);
    assert_eq!(
        folder_choice(Ok(Some(Vec::new()))),
        FolderChoice::Cancelled,
        "a dialog that returned no path chose nothing"
    );
    assert_eq!(
        folder_choice(Err(PICKER_DROPPED.to_owned())),
        FolderChoice::Failed(PICKER_DROPPED.to_owned()),
        "a dialog that could not answer must not look like a cancel"
    );
}

#[test]
fn an_index_that_failed_on_a_project_never_seen_before_still_gets_a_row() {
    use crate::presentation::fault::project_missing;
    use crate::store::jobs::{Job, JobKind, JobState, JobsStore};

    let coordinate = "/abs/GymBroApp";
    let published = Shelf::new(KeyTag::from_key(&[0_u8; 32]), Vec::new());
    let store = JobsStore::fixture(vec![Job::fixture(
        JobKind::Index,
        coordinate,
        JobState::Failed(Box::new(project_missing(coordinate))),
    )]);

    let merged = store.merge(&published);
    let entry = merged
        .entries()
        .first()
        .expect("a failed index must leave something on the shelf to explain it");
    assert_eq!(entry.identity().coordinate().as_str(), coordinate);
    assert!(
        matches!(entry.readiness(), Readiness::Failed { .. }),
        "the row exists to carry the failure, so it must say failed"
    );
    assert_eq!(entry.identity().name(), "GymBroApp");
}

#[test]
fn an_index_still_in_flight_shows_as_requested_rather_than_failed() {
    use crate::store::jobs::{Job, JobKind, JobState, JobsStore};

    let coordinate = "/abs/GymBroApp";
    let published = Shelf::new(KeyTag::from_key(&[0_u8; 32]), Vec::new());
    let store = JobsStore::fixture(vec![Job::fixture(
        JobKind::Index,
        coordinate,
        JobState::Submitted,
    )]);

    let merged = store.merge(&published);
    let entry = merged.entries().first().expect("a requested row");
    assert_eq!(entry.readiness(), &Readiness::Requested);
}

//! Unit coverage for the shared presentation model.
//!
//! These cases assert rendered content, never row counts: a coordinate that
//! round-trips, a trail that reads, a lane mark that distinguishes an empty
//! complete answer from a lane that refused, a fault that names its operand.

use super::*;
use backend_library::{
    Coverage, DeclarationKind, Fragment, Lane, OutlineExtent, OutlineNode, Reason, RowId,
    SourceExcerpt, SourceExcerptExtent, SourceLocation, package_key, symbol_key, view_state_root,
};

const PROJECT: &str = "/abs/polyglot";
const DECLARATION: &str = "/abs/polyglot::src/lib.rs:2::ferris";

fn basis() -> backend_library::Basis {
    backend_library::Basis::new(
        view_state_root(&[]),
        backend_library::object_version(&[]),
    )
}

#[test]
fn declaration_coordinates_round_trip_exactly() {
    let identity = Identity::parse(DECLARATION);
    assert_eq!(identity.coordinate().as_str(), DECLARATION);
    assert_eq!(identity.shape(), IdentityShape::Declaration);
    assert_eq!(identity.project().map(ProjectRef::name), Some("polyglot"));
    assert_eq!(
        identity.path().map(|path| path.as_str().to_owned()),
        Some("src/lib.rs".to_owned())
    );
    assert_eq!(identity.line().map(LineNumber::get), Some(2));
    assert_eq!(identity.name(), "ferris");
    assert_eq!(identity.language(), Language::Rust);
}

#[test]
fn the_readable_trail_is_the_one_the_owner_asked_for() {
    let identity = Identity::parse(DECLARATION);
    assert_eq!(
        identity.trail_within(None),
        "polyglot › src/lib.rs:2 › ferris"
    );
    let project = ProjectRef::new(PROJECT);
    assert_eq!(
        identity.trail_within(Some(&project)),
        "src/lib.rs:2 › ferris"
    );
}

#[test]
fn package_module_semantic_and_windows_spellings_all_parse() {
    let package = Identity::parse(PROJECT);
    assert_eq!(package.shape(), IdentityShape::Package);
    assert_eq!(package.name(), "polyglot");

    let module = Identity::parse("/abs/polyglot::src/lib.rs");
    assert_eq!(module.shape(), IdentityShape::Module);
    assert_eq!(module.line(), None);
    assert_eq!(module.name(), "lib.rs");

    let semantic = Identity::parse("/abs/polyglot::semantic::00ff::ferris");
    assert_eq!(semantic.shape(), IdentityShape::Semantic);
    assert_eq!(semantic.name(), "ferris");
    assert_eq!(semantic.path(), None);

    let windows = Identity::parse("C:\\code\\polyglot::src\\lib.rs:2::ferris");
    assert_eq!(windows.project().map(ProjectRef::name), Some("polyglot"));
    assert_eq!(
        windows.path().map(|path| path.as_str().to_owned()),
        Some("src\\lib.rs".to_owned())
    );
    assert_eq!(windows.line().map(LineNumber::get), Some(2));

    let unicode = Identity::parse("/abs/日本::src/lib.rs:7::χ");
    assert_eq!(unicode.name(), "χ");
    assert_eq!(unicode.coordinate().as_str(), "/abs/日本::src/lib.rs:7::χ");
}

#[test]
fn a_nested_symbol_path_keeps_every_segment() {
    let identity = Identity::parse("/abs/p::src/lib.rs:9::Outer::inner::deep");
    assert_eq!(identity.trail().segments().len(), 3);
    assert_eq!(identity.name(), "deep");
    assert_eq!(identity.trail().joined(), "Outer::inner::deep");
}

#[test]
fn key_tags_are_eight_hex_and_never_parsed_back() {
    let key = IdentityKey::Symbol(symbol_key(DECLARATION));
    let tag = key.tag().expect("symbol keys abbreviate");
    assert_eq!(tag.to_string().len(), 8);
    assert!(tag.to_string().chars().all(|c| c.is_ascii_hexdigit()));
    let full = key.encoded().expect("symbol keys encode");
    assert_eq!(full.len(), 64);
    assert!(full.starts_with(&tag.to_string()));
}

#[test]
fn an_unavailable_lane_never_reads_like_an_empty_success() {
    let complete = CoverageLine::new(&[Coverage::Complete], Some(12));
    assert_eq!(complete.render(), "~lanes exact✓12 names✓12 graph✓12 semantic✓12");
    assert_eq!(complete.readiness(), "ready");

    let thin = CoverageLine::new(
        &[
            Coverage::Complete,
            Coverage::Partial {
                lane: Lane::Graph,
                completed: 3,
                total: 7,
            },
            Coverage::Unavailable {
                lane: Lane::Semantic,
                reason: Reason::Unconfigured,
            },
        ],
        Some(12),
    );
    assert_eq!(
        thin.render(),
        "~lanes exact✓12 names✓12 graph◐3/7 semantic✗ unconfigured"
    );
    assert_eq!(thin.readiness(), "unavailable");
    assert!(thin.has_unavailable());

    let silent = CoverageLine::new(&[], None);
    assert_eq!(silent.render(), "~lanes exact· names· graph· semantic·");
    assert_eq!(silent.readiness(), "unknown");
}

#[test]
fn a_fault_names_its_operand_its_cause_and_its_next_step() {
    let fault = Fault::from_command_failure(
        &backend_library::CommandFailure::NotFound,
        Operand::Coordinate(Coordinate::new(DECLARATION)),
    )
    .with_affordance(Affordance::search("ferris"));
    assert_eq!(fault.slug(), FaultSlug::NotFound);
    assert_eq!(fault.cause().slug(), CauseSlug::Absent);
    let rendered = fault.render(fault.affordance().shell().as_deref());
    assert!(rendered.starts_with("✗ not-found /abs/polyglot::src/lib.rs:2::ferris"));
    assert!(rendered.contains("no record is published at that identity"));
    assert!(rendered.ends_with("→ backend search ferris"));

    let call = fault
        .affordance()
        .tool_call()
        .expect("search affordance is a tool call");
    assert_eq!(call["name"], "backend.search");
    assert_eq!(call["arguments"]["query"], "ferris");
}

#[test]
fn every_lane_reason_produces_a_sentence_and_an_affordance() {
    for reason in [
        Reason::NoIndex,
        Reason::Unconfigured,
        Reason::Offline,
        Reason::Cancelled,
        Reason::Incomplete,
    ] {
        let fault = Fault::lane(Lane::Semantic, reason);
        assert_eq!(fault.slug(), FaultSlug::LaneUnavailable);
        assert!(fault.cause().sentence().contains("semantic"));
        assert_eq!(fault.operand().render(), "semantic");
    }
}

#[test]
fn rust_signatures_classify_bindings_types_and_lifetimes() {
    let signature = Signature::tokenize("pub fn ferris<'a>(lane: &'a str) -> Beacon", Language::Rust);
    assert_eq!(signature.text(), "pub fn ferris<'a>(lane: &'a str) -> Beacon");
    let kinds: Vec<(&str, TokenKind)> = signature
        .tokens()
        .iter()
        .filter(|token| !token.text().trim().is_empty())
        .map(|token| (token.text(), token.kind()))
        .collect();
    assert!(kinds.contains(&("fn", TokenKind::Keyword)));
    assert!(kinds.contains(&("ferris", TokenKind::Name)));
    assert!(kinds.contains(&("lane", TokenKind::Binding)));
    assert!(kinds.contains(&("str", TokenKind::Type)));
    assert!(kinds.contains(&("Beacon", TokenKind::Type)));
    assert!(kinds.contains(&("'a", TokenKind::Lifetime)));
}

#[test]
fn each_frontends_parameter_spelling_is_classified() {
    let cases = [
        (
            "def ferris(lane: str) -> Beacon",
            Language::Python,
            "ferris",
            "lane",
            "str",
        ),
        (
            "function ferris(lane: string): Beacon",
            Language::TypeScript,
            "ferris",
            "lane",
            "string",
        ),
        (
            "public static Beacon ferris(String lane)",
            Language::Java,
            "ferris",
            "lane",
            "String",
        ),
        (
            "public Beacon Ferris(string lane)",
            Language::CSharp,
            "Ferris",
            "lane",
            "string",
        ),
        (
            "func ferris(lane string) Beacon",
            Language::Go,
            "ferris",
            "lane",
            "string",
        ),
        (
            "Beacon ferris(char *lane)",
            Language::C,
            "ferris",
            "lane",
            "char",
        ),
    ];
    for (text, language, name, binding, kind) in cases {
        let signature = Signature::tokenize(text, language);
        assert_eq!(signature.text(), text, "{language} signature round-trips");
        let found = |needle: &str, want: TokenKind| {
            signature
                .tokens()
                .iter()
                .any(|token| token.text() == needle && token.kind() == want)
        };
        assert!(found(name, TokenKind::Name), "{language}: {name} is a name");
        assert!(
            found(binding, TokenKind::Binding),
            "{language}: {binding} is a binding"
        );
        assert!(found(kind, TokenKind::Type), "{language}: {kind} is a type");
    }
}

#[test]
fn a_resolved_type_is_marked_by_name_and_never_claims_more() {
    let signature = Signature::tokenize("fn ferris() -> Beacon", Language::Rust).resolve_types(
        |text| {
            (text == "Beacon").then(|| {
                (
                    Coordinate::new("/abs/polyglot::src/lib.rs:9::Beacon"),
                    IdentityKey::Symbol(symbol_key("/abs/polyglot::src/lib.rs:9::Beacon")),
                )
            })
        },
    );
    let beacon = signature
        .tokens()
        .iter()
        .find(|token| token.text() == "Beacon")
        .expect("the return type is tokenized");
    let target = beacon.target().expect("Beacon resolves by name");
    assert_eq!(target.resolved(), Resolved::ByName);
    assert_eq!(
        target.coordinate().as_str(),
        "/abs/polyglot::src/lib.rs:9::Beacon"
    );
}

#[test]
fn prose_folds_fragment_runs_into_paragraphs() {
    let prose = Prose::from_fragments(&[
        Fragment::Text("Lights the ".to_owned()),
        Fragment::Text("beacon.".to_owned()),
        Fragment::Break,
        Fragment::Code("ferris()".to_owned()),
        Fragment::Break,
        Fragment::Break,
    ]);
    assert_eq!(prose.len(), 2);
    assert_eq!(prose.first(), Some(&Prose::Text("Lights the beacon.".to_owned())));
    assert_eq!(prose.get(1), Some(&Prose::Code("ferris()".to_owned())));
}

fn declaration_row() -> backend_library::Row {
    backend_library::Row::in_package(
        RowId::Symbol(symbol_key(DECLARATION)),
        basis(),
        package_key(PROJECT),
        DECLARATION,
    )
    .with_signature("pub fn ferris() -> Beacon")
    .with_kind(DeclarationKind::Function)
    .with_source(SourceLocation::new("src/lib.rs", 2).expect("one-based location"))
    .with_excerpt(
        SourceExcerpt::captured("pub fn ferris() -> Beacon {\n    Beacon\n}", SourceExcerptExtent::Complete)
            .expect("bounded excerpt"),
    )
}

#[test]
fn a_record_renders_identity_first_and_never_clips_the_coordinate() {
    let row = declaration_row();
    let record = Record::from_row(&row).with_summary("Lights the beacon.");
    let list = RecordList::new(
        "ferris",
        CoverageLine::new(&[Coverage::Complete], Some(1)),
        vec![record],
    );
    let rendered = text::records(&list, None, Theme::plain().with_width(Width::new(40)));
    let lines: Vec<&str> = rendered.lines().collect();
    assert_eq!(lines.first().copied(), Some("~lanes exact✓1 names✓1 graph✓1 semantic✓1"));
    assert_eq!(
        lines.get(1).copied(),
        Some("● polyglot › src/lib.rs:2 › ferris  ƒ function · rs")
    );
    assert_eq!(lines.get(2).copied(), Some("  /abs/polyglot::src/lib.rs:2::ferris"));
    assert!(rendered.contains("pub fn ferris() -> Beacon"));
}

#[test]
fn the_markdown_page_leads_with_the_identity_then_the_evidence() {
    let row = declaration_row();
    let identity = Identity::parse_with_key(&row.label, row.id.into());
    let site = SourceSite::new(
        PackagePath::new("src/lib.rs"),
        LineNumber::new(2).expect("one-based"),
    );
    let source = Source::Captured {
        lines: Source::number_lines(
            "pub fn ferris() -> Beacon {\n    Beacon\n}",
            site.line(),
        ),
        site,
        truncation: Truncation::Complete,
    };
    let page = Page::new(identity, Some(DeclarationKind::Function), source)
        .with_signature(Signature::tokenize("pub fn ferris() -> Beacon", Language::Rust))
        .with_prose(Prose::from_fragments(&[Fragment::Text(
            "Lights the beacon.".to_owned(),
        )]));
    let rendered = markdown::page(&page);
    let lines: Vec<&str> = rendered.lines().collect();
    assert_eq!(
        lines.first().copied(),
        Some("# polyglot › src/lib.rs:2 › ferris")
    );
    assert!(
        lines
            .get(1)
            .is_some_and(|line| line.starts_with("function · rust · key ")
                && line.ends_with("src/lib.rs:2"))
    );
    assert_eq!(
        lines.get(2).copied(),
        Some("`/abs/polyglot::src/lib.rs:2::ferris`")
    );
    assert!(rendered.contains("```rust\npub fn ferris() -> Beacon\n```"));
    assert!(rendered.contains("## source"));
    assert!(rendered.contains("   2 pub fn ferris() -> Beacon {"));
    assert!(rendered.contains("   3     Beacon"));
    assert!(!rendered.contains('|'), "no Markdown table may appear");
}

#[test]
fn an_outline_renders_names_not_digests() {
    let child = symbol_key(DECLARATION);
    let root = symbol_key("/abs/polyglot::src/lib.rs");
    let node = OutlineNode {
        symbol: root,
        children: vec![OutlineNode {
            symbol: child,
            children: Box::new([]),
        }]
        .into_boxed_slice(),
    };
    let module = backend_library::Row::in_package(
        RowId::Symbol(root),
        basis(),
        package_key(PROJECT),
        "/abs/polyglot::src/lib.rs",
    )
    .with_kind(DeclarationKind::Module);
    let rows = vec![module, declaration_row()];
    let mut resolver = row_resolver(&rows);
    let entry = OutlineEntry::resolve(&node, &mut resolver);
    let tree = OutlineTree::new(
        Identity::parse_with_key(PROJECT, IdentityKey::Package(package_key(PROJECT))),
        vec![entry],
        OutlineExtent::Complete,
    );
    let rendered = text::outline(&tree, Theme::plain());
    assert!(rendered.contains("▦ lib.rs"), "{rendered}");
    assert!(rendered.contains("  ƒ ferris"), "{rendered}");
    assert!(rendered.contains("2 declaration(s) · complete"));
    assert!(
        !rendered.contains(&KeyTag::from_key(child.as_bytes()).to_string()),
        "a resolved node must never fall back to its digest"
    );
}

#[test]
fn an_unresolved_outline_node_says_so_instead_of_guessing() {
    let node = OutlineNode {
        symbol: symbol_key("missing"),
        children: Box::new([]),
    };
    let rows: Vec<backend_library::Row> = Vec::new();
    let mut resolver = row_resolver(&rows);
    let entry = OutlineEntry::resolve(&node, &mut resolver);
    assert!(entry.identity().is_none());
    assert!(entry.name().starts_with('‹'));
    assert!(!entry.is_resolved());
}

#[test]
fn the_shelf_states_readiness_for_every_project() {
    let identity = Identity::parse_with_key(PROJECT, IdentityKey::Package(package_key(PROJECT)));
    let shelf = Shelf::new(
        KeyTag::from_key(&[0xab; 32]),
        vec![
            ShelfEntry::new(identity.clone(), Readiness::Ready)
                .with_languages(vec![LanguageCount::new(Language::Rust, 12)]),
            ShelfEntry::new(
                identity.clone(),
                Readiness::Indexing {
                    rows: RowCount::new(4),
                },
            ),
            ShelfEntry::new(
                identity,
                Readiness::Failed {
                    fault: Fault::lane(Lane::Names, Reason::NoIndex),
                },
            ),
        ],
    );
    let rendered = text::shelf(&shelf, Theme::plain());
    assert!(rendered.contains("● polyglot  ready · 12 declaration(s) · rs 12"));
    assert!(rendered.contains("◐ polyglot  indexing · 4 row(s)"));
    assert!(rendered.contains("✗ lane-unavailable names"));
    assert!(rendered.contains("3 project(s) · revision abababab"));
}

#[test]
fn the_json_projection_keeps_the_exact_coordinate() {
    let row = declaration_row();
    let record = Record::from_row(&row);
    let dto = RecordDto::new(&record);
    let encoded = serde_json::to_value(&dto).expect("record DTO encodes");
    assert_eq!(encoded["identity"]["coordinate"], DECLARATION);
    assert_eq!(encoded["identity"]["trail"], "polyglot › src/lib.rs:2 › ferris");
    assert_eq!(encoded["identity"]["name"], "ferris");
    assert_eq!(encoded["kind"], "function");
    assert_eq!(encoded["language"], "rust");
}

#[test]
fn every_registry_row_has_exactly_one_grammar() {
    assert_eq!(GRAMMARS.len(), registry_size());
    for spec in backend_library::COMMANDS {
        let grammar = grammar_for(spec.name)
            .unwrap_or_else(|| panic!("registry row `{}` has no grammar", spec.name));
        assert_eq!(grammar.name(), spec.name);
        assert!(
            grammar.description().starts_with(spec.description),
            "`{}` must lead with its registry sentence",
            spec.name
        );
        assert!(
            grammar.when().len() > 20,
            "`{}` must say when to use it",
            spec.name
        );
        assert!(grammar.tool().starts_with("backend."));
    }
    let mut tools: Vec<&str> = GRAMMARS.iter().map(|grammar| grammar.tool()).collect();
    tools.sort_unstable();
    let before = tools.len();
    tools.dedup();
    assert_eq!(before, tools.len(), "two registry rows share one tool name");
}

#[test]
fn no_two_commands_share_a_cli_spelling() {
    let mut spellings: Vec<&str> = Vec::new();
    for grammar in GRAMMARS {
        spellings.push(grammar.name());
        spellings.extend(grammar.aliases());
    }
    spellings.sort_unstable();
    let before = spellings.len();
    spellings.dedup();
    assert_eq!(before, spellings.len(), "a CLI spelling is ambiguous");
}

#[test]
fn colour_is_the_only_difference_between_a_tty_and_a_pipe() {
    let row = declaration_row();
    let list = RecordList::new(
        "ferris",
        CoverageLine::new(&[Coverage::Complete], Some(1)),
        vec![Record::from_row(&row)],
    );
    let plain = text::records(&list, None, Theme::plain());
    let painted = text::records(&list, None, Theme::coloured(Width::new(100)));
    assert_ne!(plain, painted);
    let stripped: String = strip_escapes(&painted);
    assert_eq!(stripped, plain);
}

fn strip_escapes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut characters = text.chars();
    while let Some(character) = characters.next() {
        if character != '\u{1b}' {
            out.push(character);
            continue;
        }
        for escape in characters.by_ref() {
            if escape == 'm' {
                break;
            }
        }
    }
    out
}

#[test]
fn the_status_summary_replaces_the_capability_dump() {
    let library = backend_library::Library::new();
    let report = backend_library::HealthReport::from_root(library.view(), library.cursor());
    let status = Status::from_report(&report, Some(ProjectRef::new(PROJECT)));
    let rendered = markdown::status(&status);
    assert!(
        rendered.lines().count() <= 25,
        "status must stay readable: {rendered}"
    );
    let summary = status.capabilities().render();
    assert!(summary.contains("oracles 0/"), "{summary}");
    assert!(summary.contains("no-manifest"), "{summary}");
    assert!(summary.contains("embedding unconfigured"), "{summary}");
}

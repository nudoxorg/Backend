//! Unit coverage for the shared presentation model.
//!
//! These cases assert rendered content, never row counts: a coordinate that
//! round-trips, a trail that reads, a lane mark that distinguishes an empty
//! complete answer from a lane that refused, a fault that names its operand.

use super::*;
use backend_library::{
    Coverage, DeclarationKind, Document, Fragment, GraphRelation, Lane, OutlineExtent,
    OutlineNode, Reason, RowId, SemanticLinkKind, SourceAvailability, SourceExcerpt,
    SourceExcerptExtent, SourceLocation, package_key, symbol_key, view_state_root,
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

/// Every closed coordinate spelling, with the parts a reader is owed.
///
/// The awkward ones are the point: a path with a space, a project whose
/// name is not ASCII, a Windows drive letter that is not a line number, a
/// declaration the producer never gave a line.
const SPELLINGS: [(&str, IdentityShape, &str, &str); 8] = [
    (
        "/abs/polyglot",
        IdentityShape::Package,
        "polyglot",
        "polyglot",
    ),
    (
        "/abs/polyglot::src/lib.rs",
        IdentityShape::Module,
        "lib.rs",
        "polyglot › src/lib.rs",
    ),
    (
        "/abs/polyglot::src/lib.rs::ferris",
        IdentityShape::Declaration,
        "ferris",
        "polyglot › src/lib.rs › ferris",
    ),
    (
        "/abs/my project::src/my file.rs:3::my fn",
        IdentityShape::Declaration,
        "my fn",
        "my project › src/my file.rs:3 › my fn",
    ),
    (
        "C:\\code\\polyglot::src\\lib.rs:2::ferris",
        IdentityShape::Declaration,
        "ferris",
        "polyglot › src\\lib.rs:2 › ferris",
    ),
    (
        "/abs/日本::src/lib.rs:7::χ",
        IdentityShape::Declaration,
        "χ",
        "日本 › src/lib.rs:7 › χ",
    ),
    (
        "/abs/polyglot::src/lib.rs:9::Outer::inner::deep",
        IdentityShape::Declaration,
        "deep",
        "polyglot › src/lib.rs:9 › Outer::inner::deep",
    ),
    (
        "/abs/polyglot::external::libc::malloc",
        IdentityShape::External,
        "malloc",
        "polyglot › libc::malloc",
    ),
];

#[test]
fn every_closed_spelling_round_trips_byte_exact() {
    for (coordinate, shape, name, trail) in SPELLINGS {
        let identity = Identity::parse(coordinate);
        assert_eq!(
            identity.coordinate().as_str(),
            coordinate,
            "`{coordinate}` must round-trip to the exact bytes the engine accepts"
        );
        assert_eq!(identity.shape(), shape, "`{coordinate}` shape");
        assert_eq!(identity.name(), name, "`{coordinate}` name");
        assert_eq!(identity.trail_within(None), trail, "`{coordinate}` trail");
        let dto = IdentityDto::new(&identity);
        assert_eq!(dto.coordinate, coordinate);
        assert_eq!(dto.trail, trail);
    }
}

#[test]
fn a_symbol_the_producer_gave_no_line_says_so_instead_of_inventing_one() {
    let identity = Identity::parse("/abs/polyglot::src/lib.rs::ferris");
    assert_eq!(identity.line(), None);
    assert_eq!(
        identity.path().map(|path| path.as_str().to_owned()),
        Some("src/lib.rs".to_owned())
    );
    assert_eq!(IdentityDto::new(&identity).line, None);
}

#[test]
fn a_project_root_that_contains_the_separator_is_recovered_from_the_shelf() {
    // `/abs/a::b` is a legal directory name and an ambiguous coordinate. The
    // general parser cannot know; a caller that already holds the shelf row can
    // say, and then nothing is guessed.
    let project = ProjectRef::new("/abs/a::b");
    let coordinate = "/abs/a::b::src/lib.rs:4::ferris";
    let blind = Identity::parse(coordinate);
    assert_ne!(blind.project().map(ProjectRef::root), Some("/abs/a::b"));
    let known = Identity::parse_within(coordinate, &project, IdentityKey::Absent);
    assert_eq!(known.coordinate().as_str(), coordinate);
    assert_eq!(known.project().map(ProjectRef::root), Some("/abs/a::b"));
    assert_eq!(known.name(), "ferris");
    assert_eq!(known.line().map(LineNumber::get), Some(4));
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
    // The graph lane is still working, so that is what the reader is waiting
    // on. The semantic lane is off in this deployment and is still shown as
    // unavailable — it just is not the answer to "what is this owner doing".
    assert_eq!(thin.readiness(), "indexing");
    assert!(thin.has_unavailable());
    assert!(!thin.has_failed_lane());

    // Every lane the deployment declared finished; one it never configured did
    // not. That owner is ready, and saying otherwise leaves a healthy project
    // reporting a fault for the life of the deployment.
    let unconfigured = CoverageLine::new(
        &[
            Coverage::Complete,
            Coverage::Unavailable {
                lane: Lane::Semantic,
                reason: Reason::Unconfigured,
            },
        ],
        Some(12),
    );
    assert_eq!(
        unconfigured.render(),
        "~lanes exact✓12 names✓12 graph✓12 semantic✗ unconfigured"
    );
    assert_eq!(unconfigured.readiness(), "ready");
    assert!(unconfigured.has_unavailable());
    assert!(!unconfigured.has_failed_lane());

    // A lane that was asked to answer and could not is a different statement
    // and must never be folded into the one above.
    let failed = CoverageLine::new(
        &[
            Coverage::Complete,
            Coverage::Unavailable {
                lane: Lane::Semantic,
                reason: Reason::Offline,
            },
        ],
        Some(12),
    );
    assert_eq!(failed.readiness(), "unavailable");
    assert!(failed.has_failed_lane());

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
fn a_refusal_a_peer_flattened_into_a_message_is_still_a_typed_refusal() {
    // A producer on the pre-typed reply schema sends only the failure's own
    // text, and the shared client can only call that a protocol failure. Left
    // alone, a mistyped coordinate reaches a reader as "a frame, DTO, or
    // identity proof failed admission" — a sentence about wire proofs, in
    // answer to a question about a name.
    let operand = Operand::Coordinate(Coordinate::new(DECLARATION));
    let flattened = backend_client::ClientError::Protocol(
        backend_library::CommandFailure::NotFound.to_string(),
    );
    let fault = Fault::from_client_error(&flattened, operand.clone());
    assert_eq!(fault.slug(), FaultSlug::NotFound);
    assert_eq!(fault.cause().slug(), CauseSlug::Absent);
    assert_eq!(fault.operand().render(), DECLARATION);

    let invalid = backend_client::ClientError::Protocol(
        backend_library::CommandFailure::InvalidQuery("limit is out of range".to_owned())
            .to_string(),
    );
    let fault = Fault::from_client_error(&invalid, operand.clone());
    assert_eq!(fault.slug(), FaultSlug::InvalidQuery);
    assert_eq!(fault.cause().sentence(), "limit is out of range");

    // A message this model does not recognise stays a protocol failure: a
    // surface must not guess a class it was not told.
    let unknown =
        backend_client::ClientError::Protocol("the frame length prefix was truncated".to_owned());
    let fault = Fault::from_client_error(&unknown, operand);
    assert_eq!(fault.slug(), FaultSlug::Protocol);
    assert!(fault.cause().sentence().contains("truncated"));
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
fn compiler_graph_relation_kinds_survive_owner_page_assembly() {
    let centre = symbol_key(DECLARATION);
    let calls = symbol_key("/abs/polyglot::src/lib.rs:5::calls");
    let implements = symbol_key("/abs/polyglot::src/lib.rs:8::implements");
    let depends = symbol_key("/abs/polyglot::src/lib.rs:11::depends");
    let calls_row = backend_library::Row::new(RowId::Symbol(calls), basis(), "calls")
        .with_kind(DeclarationKind::Function);
    let implements_row = backend_library::Row::new(
        RowId::Symbol(implements),
        basis(),
        "implements",
    )
    .with_kind(DeclarationKind::Trait);
    let depends_row = backend_library::Row::new(RowId::Symbol(depends), basis(), "depends")
        .with_kind(DeclarationKind::Module);
    let document = Document::new(
        centre,
        basis().root,
        vec![Fragment::Text("ferris".to_owned())],
    )
    .with_location(SourceAvailability::NotCaptured);
    let page = page_from_document_with_graph_relations(
        DECLARATION,
        &document,
        &[declaration_row()],
        &[calls_row, implements_row, depends_row],
        &[
            GraphRelation::new(
                RowId::Symbol(centre),
                RowId::Symbol(calls),
                SemanticLinkKind::Calls,
            ),
            GraphRelation::new(
                RowId::Symbol(centre),
                RowId::Symbol(implements),
                SemanticLinkKind::Implements,
            ),
            GraphRelation::new(
                RowId::Symbol(centre),
                RowId::Symbol(depends),
                SemanticLinkKind::Imports,
            ),
        ],
        Vec::new(),
    );
    let labels = page
        .relations()
        .iter()
        .map(|group| group.label())
        .collect::<Vec<_>>();
    assert!(labels.contains(&RelationLabel::Typed(
        SemanticLinkKind::Calls,
        RelationDirection::Outgoing,
    )));
    assert!(labels.contains(&RelationLabel::Typed(
        SemanticLinkKind::Implements,
        RelationDirection::Outgoing,
    )));
    assert!(labels.contains(&RelationLabel::Typed(
        SemanticLinkKind::Imports,
        RelationDirection::Outgoing,
    )));
    assert!(!labels.contains(&RelationLabel::Related));
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
fn the_answer_tag_never_overwrites_a_fact_the_payload_already_carries() {
    // The first spelling of this tag was `kind`, and a page already has one:
    // its declaration kind. The tag won, `"kind": "function"` silently became
    // `"kind": "page"`, and nothing said so — a consumer simply read a fact that
    // was no longer there. The discriminator now has a name no DTO uses, and
    // this holds that true for every answer rather than for the one that was
    // noticed.
    let row = declaration_row();
    let identity = Identity::parse_with_key(&row.label, row.id.into());
    let site = SourceSite::new(
        PackagePath::new("src/lib.rs"),
        LineNumber::new(2).expect("one-based"),
    );
    let page = Page::new(
        identity,
        Some(DeclarationKind::Function),
        Source::Captured {
            lines: Source::number_lines("pub fn ferris() -> Beacon {}", site.line()),
            site,
            truncation: Truncation::Complete,
        },
    );
    let answers = [
        Answer::Page(Box::new(page)),
        Answer::Records(Box::new(RecordList::new(
            "ferris",
            CoverageLine::new(&[Coverage::Complete], Some(1)),
            vec![Record::from_row(&row)],
        ))),
        Answer::Shelf(Box::new(Shelf::new(KeyTag::from_key(&[0; 32]), Vec::new()))),
        Answer::Product(Box::new(ProductView::stated("tree", "no node is open"))),
    ];
    for answer in &answers {
        let value = answer_value(answer);
        assert_eq!(
            value["answer"], answer.kind(),
            "every answer states which one it is"
        );
        let fields = value.as_object().expect("an answer projects to an object");
        assert_eq!(
            fields.keys().filter(|key| key.as_str() == "answer").count(),
            1
        );
    }
    let page_value = answer_value(&answers[0]);
    assert_eq!(page_value["answer"], "page");
    assert_eq!(
        page_value["kind"], "function",
        "the declaration kind survives the tag"
    );
    assert_eq!(page_value["identity"]["coordinate"], DECLARATION);

    let fault = fault_value(&Fault::usage("limit", "`900` is not a page bound"));
    assert_eq!(fault["answer"], "fault");
    assert_eq!(fault["slug"], "usage");
    assert_eq!(fault["operand"], "limit");
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
fn every_registry_row_is_reachable_by_both_a_cli_spelling_and_a_tool_name() {
    // The two surfaces address the same rows through different vocabularies.
    // Neither vocabulary is maintained by hand, and this is what says so: a row
    // that lost one of them fails here rather than becoming unreachable on one
    // surface and nobody noticing.
    for spec in backend_library::COMMANDS {
        let grammar = grammar_for(spec.name)
            .unwrap_or_else(|| panic!("`{}` has no CLI spelling", spec.name));
        let by_tool = grammar_for_tool(grammar.tool())
            .unwrap_or_else(|| panic!("`{}` has no MCP tool name", spec.name));
        assert_eq!(
            by_tool, grammar,
            "`{}` resolves to two different rows",
            spec.name
        );
        assert_eq!(grammar.domain(), Some(spec.domain));
        assert_eq!(
            grammar.is_write(),
            spec.mutation == backend_library::CommandMutation::Write
        );
        assert!(
            !grammar.is_destructive() || grammar.is_write(),
            "`{}` cannot take something away without writing",
            spec.name
        );
    }
    let grouped: usize = domains().iter().map(|domain| grammars_in(*domain).len()).sum();
    assert_eq!(
        grouped,
        GRAMMARS.len(),
        "a row whose domain is not in the display order would vanish from help"
    );
}

#[test]
fn the_five_removal_rows_are_the_only_destructive_ones() {
    let destructive: Vec<&str> = GRAMMARS
        .iter()
        .filter(|grammar| grammar.is_destructive())
        .map(|grammar| grammar.name())
        .collect();
    assert_eq!(
        destructive,
        vec![
            "remove",
            "unsubscribe",
            "project-delete",
            "project-remove",
            "tree-close"
        ],
        "an agent gates on destructiveHint; changing this set is a product decision"
    );
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

#[test]
fn a_references_reply_renders_sites_with_their_provenance() {
    let reply = backend_library::SurfaceReply::References {
        target: backend_library::ProductText::new("pkg::semantic::callee")
            .expect("target text"),
        references: Box::new([backend_library::ReferenceRecord {
            site: backend_library::ProductText::new("pkg::semantic::caller")
                .expect("site text"),
            target: backend_library::SemanticLinkTarget::Local {
                declaration: backend_library::SemanticDeclarationIdentity {
                    family: [1; 16],
                    variant: [2; 16],
                },
            },
            relation: backend_library::SemanticLinkKind::Calls,
            evidence: backend_library::SemanticLinkEvidence {
                confidence: backend_library::SemanticConfidence::Compiler,
                source: Some(backend_library::SemanticSourceSpan {
                    file: backend_library::ProductText::new("src/main.rs").expect("path"),
                    start: 40,
                    end: 46,
                }),
            },
        }]),
    };
    let view = product_view(&reply);
    assert_eq!(view.heading(), "references to pkg::semantic::callee");
    let record = view.records().first().expect("record");
    assert_eq!(record.title(), "pkg::semantic::caller");
    assert_eq!(record.operand(), Some("pkg::semantic::caller"));
    let tags = record.tags();
    assert!(tags.contains(&"calls".to_owned()), "tags are {tags:?}");
    assert!(tags.contains(&"compiler".to_owned()), "tags are {tags:?}");
    assert!(
        tags.iter().any(|tag| tag == "src/main.rs:40-46"),
        "the source span must survive rendering, tags are {tags:?}"
    );
}

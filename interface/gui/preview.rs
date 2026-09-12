//! Typed fixtures for offline screenshots and rendering tests.
//! This module exists only under the `preview` feature and is filled by the fixture wave.
//! Its narrow surface is sample values rich enough to drive a window with no engine at all.

use compiler_ir::{Confidence, EntityId, LinkKind, Visibility};
use compiler_ir_vocabulary::{
    DeclarationFamilyId, DeclarationIdentity, EntityKind, VariantFingerprint,
};
use compiler_vocabulary::{Language, LanguageProfile, RustEdition};
use heart_identity::{ArtifactId, GenerationId, IrSemanticImageEncoding, IrSemanticImageDomain};
use interface_core::{CorrelationId, PackageCompilePhase, SemanticImageAuthority};
use interface_documents::{
    Block, ByteSpan, Census, Count, Direction, ExternalRef, ForeignOrigin, Inline, MemberGroup,
    MemberRow, Name, Page, PageTruncation, Prose, RelationGroup, RelationRow, RelationRole,
    Signature, SourceLocation, Symbol, Target, Text, Token, TokenKind,
};
use interface_identity::{ContentKey, ExactAddress, PackageCoordinate, SymbolPath};
use interface_library::{
    Cycle, ExploreCoverage, FeedChecksum, IndexHitRow, IndexSearchPage, LibraryEpoch, PackageCard,
    PackageProfile, PackageVersionRow, PackageVersionRows, PackageVersionText, Shelf, ShelfEntry,
    ShelfFailure, ShelfStatus, Timestamp, VersionActive,
};
use interface_search::{
    Coverage, Degradation, Hit, Lane, LaneReport, LaneSet, Micros, QueryText, ResultLimit, Score,
    SearchRequest, SearchTerminal, Unavailability, merge_lanes,
};

/// The package every fixture declaration lives in.
const PACKAGE: &str = "cargo:serde@1.0.196";

/// Path of the primary fixture page.
const PAGE_PATH: &str = "serde::de::Deserializer[struct]";

/// Name of the primary fixture page.
const PAGE_NAME: &str = "Deserializer";

/// Path of the page the primary page links to.
const DEPENDENT_PATH: &str = "serde::de::DeserializeSeed[trait]";

/// Name of the page the primary page links to.
const DEPENDENT_NAME: &str = "DeserializeSeed";

/// Totalizes a fixture constructor whose literals the preview tests prove admitted.
#[allow(
    clippy::expect_used,
    reason = "fixture literals are constant, and a refusal here is a fixture bug, not a runtime state"
)]
const fn insisted<T>(built: Option<T>, what: &'static str) -> T {
    built.expect(what)
}

/// Totalizes a fixture constructor whose literals the preview tests prove admitted.
#[allow(
    clippy::expect_used,
    reason = "fixture literals are constant, and a refusal here is a fixture bug, not a runtime state"
)]
fn word<T, E: core::fmt::Debug>(built: Result<T, E>, what: &'static str) -> T {
    built.expect(what)
}

/// The fixture package coordinate.
fn coordinate() -> PackageCoordinate {
    word(
        PackageCoordinate::parse(PACKAGE),
        "the fixture package coordinate must parse",
    )
}

/// The fixture content key for one seed byte.
const fn key(seed: u8) -> ContentKey {
    ContentKey::new(DeclarationIdentity {
        family: DeclarationFamilyId::from_raw([seed; 16]),
        variant: VariantFingerprint::from_raw([seed ^ 0xA5; 16]),
    })
}

/// One fixture symbol minted from a constant spelling and a seed.
fn symbol(seed: u8, entity: u32, path: &str, name: &str, kind: EntityKind) -> Symbol {
    Symbol {
        address: ExactAddress::mint(
            coordinate(),
            word(SymbolPath::parse(path), "the fixture path must parse"),
            key(seed),
        ),
        entity: EntityId::new(entity),
        name: word(
            Name::exact(name.as_bytes()),
            "the fixture name must be valid utf-8",
        ),
        kind,
        visibility: Visibility::Public,
    }
}

/// The primary page's own symbol.
fn page_symbol() -> Symbol {
    symbol(0x51, 100, PAGE_PATH, PAGE_NAME, EntityKind::Record)
}

/// The dependent page's own symbol.
fn dependent_symbol() -> Symbol {
    symbol(
        0x52,
        101,
        DEPENDENT_PATH,
        DEPENDENT_NAME,
        EntityKind::Trait,
    )
}

/// The `serde::de::Error` trait symbol the signature's second local target names.
fn error_symbol() -> Symbol {
    symbol(0x53, 102, "serde::de::Error[trait]", "Error", EntityKind::Trait)
}

/// The two ancestor crumbs every fixture page shows.
fn crumbs() -> Vec<Symbol> {
    vec![
        symbol(0x54, 103, "serde[mod]", "serde", EntityKind::Module),
        symbol(0x55, 104, "serde::de[mod]", "de", EntityKind::Module),
    ]
}

/// One signature token with no target.
fn token(kind: TokenKind, text: &str) -> Token {
    Token {
        kind,
        text: Text::new(text),
        target: None,
    }
}

/// One signature token linking to a local declaration.
fn local(kind: TokenKind, text: &str, target: Symbol) -> Token {
    Token {
        kind,
        text: Text::new(text),
        target: Some(Target::Local(target)),
    }
}

/// The primary page's signature: every token kind, two distinct local targets.
fn page_signature(dependent: &Symbol, error: &Symbol) -> Signature {
    Signature::new(vec![
        token(TokenKind::Keyword, "pub"),
        token(TokenKind::Text, " "),
        token(TokenKind::Keyword, "struct"),
        token(TokenKind::Text, " "),
        token(TokenKind::Name, PAGE_NAME),
        token(TokenKind::Punctuation, "<"),
        token(TokenKind::Lifetime, "'de"),
        token(TokenKind::Punctuation, ","),
        token(TokenKind::Text, " "),
        local(TokenKind::Type, "Seed", dependent.clone()),
        token(TokenKind::Text, " "),
        token(TokenKind::Punctuation, "="),
        token(TokenKind::Text, " "),
        local(
            TokenKind::Type,
            DEPENDENT_NAME,
            dependent.clone(),
        ),
        token(TokenKind::Punctuation, ","),
        token(TokenKind::Text, " "),
        token(TokenKind::Keyword, "const"),
        token(TokenKind::Text, " "),
        token(TokenKind::Binding, "DEPTH"),
        token(TokenKind::Punctuation, ":"),
        token(TokenKind::Text, " "),
        token(TokenKind::Type, "usize"),
        token(TokenKind::Text, " "),
        token(TokenKind::Punctuation, "="),
        token(TokenKind::Text, " "),
        token(TokenKind::Literal, "32"),
        token(TokenKind::Punctuation, ">"),
        token(TokenKind::Text, " "),
        token(TokenKind::Keyword, "where"),
        token(TokenKind::Text, " "),
        token(TokenKind::Binding, "S"),
        token(TokenKind::Punctuation, ":"),
        token(TokenKind::Text, " "),
        local(TokenKind::Type, "Error", error.clone()),
    ])
}

/// The primary page's prose: one paragraph with inline code and a link, then one code block.
fn page_prose(dependent: &Symbol) -> Prose {
    Prose::new(vec![
        Block::Paragraph(Box::new([
            Inline::Text(Text::new("Reads a value from a borrowed byte slice. ")),
            Inline::Code(Text::new("from_slice")),
            Inline::Text(Text::new(" builds one without copying, and a ")),
            Inline::Link {
                label: Text::new(DEPENDENT_NAME),
                target: Target::Local(dependent.clone()),
            },
            Inline::Text(
                Text::new(" drives the parse when the caller owns the allocation strategy."),
            ),
        ])),
        Block::Code(Text::new(
            "let de = Deserializer::from_slice(bytes)?;\nlet value = seed.deserialize(de)?;",
        )),
    ])
}

/// One member row with a compact keyword signature.
fn member_row(
    seed: u8,
    entity: u32,
    path: &str,
    name: &str,
    kind: EntityKind,
    intro: &str,
    summary: &str,
) -> MemberRow {
    MemberRow {
        symbol: symbol(seed, entity, path, name, kind),
        signature: Signature::new(vec![
            token(TokenKind::Keyword, intro),
            token(TokenKind::Text, " "),
            token(TokenKind::Name, name),
        ]),
        summary: Some(Text::new(summary)),
    }
}

/// The primary page's function members.
fn function_group() -> MemberGroup {
    MemberGroup {
        kind: EntityKind::Function,
        rows: Box::new([
            member_row(
                0x61,
                110,
                "serde::de::Deserializer::from_slice[fn]",
                "from_slice",
                EntityKind::Function,
                "fn",
                "Builds a deserializer over borrowed bytes.",
            ),
            member_row(
                0x62,
                111,
                "serde::de::Deserializer::deserialize_bool[fn]",
                "deserialize_bool",
                EntityKind::Function,
                "fn",
                "Reads one boolean from the cursor.",
            ),
            member_row(
                0x63,
                112,
                "serde::de::Deserializer::deserialize_map[fn]",
                "deserialize_map",
                EntityKind::Function,
                "fn",
                "Reads a key-value map through a visitor.",
            ),
        ]),
    }
}

/// The primary page's constant members.
fn constant_group() -> MemberGroup {
    MemberGroup {
        kind: EntityKind::Constant,
        rows: Box::new([
            member_row(
                0x71,
                120,
                "serde::de::Deserializer::MAX_DEPTH[const]",
                "MAX_DEPTH",
                EntityKind::Constant,
                "const",
                "The deepest nested value the reader admits.",
            ),
            member_row(
                0x72,
                121,
                "serde::de::Deserializer::FORMAT[const]",
                "FORMAT",
                EntityKind::Constant,
                "const",
                "The wire format tag this reader spells.",
            ),
            member_row(
                0x73,
                122,
                "serde::de::Deserializer::END[const]",
                "END",
                EntityKind::Constant,
                "const",
                "The byte that terminates a top-level value.",
            ),
        ]),
    }
}

/// The primary page's outgoing local relations.
fn calls_group(dependent: Symbol, error: Symbol) -> RelationGroup {
    RelationGroup {
        role: RelationRole {
            kind: LinkKind::Calls,
            direction: Direction::Outgoing,
        },
        rows: Box::new([
            RelationRow {
                target: Target::Local(dependent),
                confidence: Confidence::Compiler,
            },
            RelationRow {
                target: Target::Local(error),
                confidence: Confidence::Indexed,
            },
        ]),
    }
}

/// The primary page's incoming external relation.
fn implements_group() -> RelationGroup {
    RelationGroup {
        role: RelationRole {
            kind: LinkKind::Implements,
            direction: Direction::Incoming,
        },
        rows: Box::new([RelationRow {
            target: Target::External(ExternalRef {
                display: Text::new("core::fmt::Debug"),
                path: Text::new("core::fmt::Debug"),
                origin: Some(ForeignOrigin {
                    ecosystem: Text::new("std"),
                    package: Some(Text::new("core")),
                }),
                kind: Some(EntityKind::Trait),
            }),
            confidence: Confidence::Imported,
        }]),
    }
}

/// The primary fixture page: crumbs, an eight-kind signature with local targets, prose with a
/// link and a code block, two member groups, local and external relations, source, attributes.
#[must_use]
pub fn page() -> Page {
    let page_symbol = page_symbol();
    let dependent = dependent_symbol();
    let error = error_symbol();
    Page {
        language: Language::Rust,
        symbol: page_symbol.clone(),
        crumbs: crumbs().into_boxed_slice(),
        signature: page_signature(&dependent, &error),
        prose: page_prose(&dependent),
        members: Box::new([function_group(), constant_group()]),
        relations: Box::new([calls_group(dependent, error), implements_group()]),
        source: Some(SourceLocation {
            file: Text::new("src/de/mod.rs"),
            span: insisted(
                ByteSpan::new(1024, 4096),
                "the fixture source span must not invert",
            ),
        }),
        attributes: Box::new([Text::new("derive(Clone)"), Text::new("non_exhaustive")]),
        truncation: PageTruncation::default(),
    }
}

/// The second fixture page: the declaration the primary page's signature and prose link to.
#[must_use]
pub fn dependent_page() -> Page {
    let dependent = dependent_symbol();
    let page_symbol = page_symbol();
    Page {
        language: Language::Rust,
        symbol: dependent.clone(),
        crumbs: crumbs().into_boxed_slice(),
        signature: Signature::new(vec![
            token(TokenKind::Keyword, "pub"),
            token(TokenKind::Text, " "),
            token(TokenKind::Keyword, "trait"),
            token(TokenKind::Text, " "),
            token(TokenKind::Name, DEPENDENT_NAME),
            token(TokenKind::Punctuation, "<"),
            token(TokenKind::Lifetime, "'de"),
            token(TokenKind::Punctuation, ">"),
            token(TokenKind::Text, " "),
            token(TokenKind::Punctuation, ":"),
            token(TokenKind::Text, " "),
            token(TokenKind::Type, "Sized"),
        ]),
        prose: Prose::new(vec![Block::Paragraph(Box::new([
            Inline::Text(Text::new("Runs one deserialization job under the caller's allocation ")),
            Inline::Text(Text::new("strategy. The ")),
            Inline::Link {
                label: Text::new(PAGE_NAME),
                target: Target::Local(page_symbol),
            },
            Inline::Text(Text::new(" hands the seed its borrowed cursor.")),
        ]))]),
        members: Box::new([]),
        relations: Box::new([]),
        source: Some(SourceLocation {
            file: Text::new("src/de/seed.rs"),
            span: insisted(
                ByteSpan::new(4096, 8192),
                "the fixture source span must not invert",
            ),
        }),
        attributes: Box::new([]),
        truncation: PageTruncation::default(),
    }
}

/// One hit row as a lane would rank it.
const fn hit(
    symbol: Symbol,
    lane: Lane,
    score: u32,
    signature: Option<Signature>,
    summary: Option<Text>,
) -> Hit {
    Hit {
        symbol,
        signature,
        summary,
        lane,
        score: Score(score),
    }
}

/// The fixture terminal for `query`: four honest lanes whose rows merge into one ranked page.
///
/// Exact ran complete over one row, lexical covered one of two packages, graph ran degraded on a
/// stale projection, and semantic could not run for want of an embedder, so it contributes no rows.
#[must_use]
pub fn terminal(query: &str) -> SearchTerminal {
    let page_symbol = page_symbol();
    let dependent = dependent_symbol();
    let page_summary = Some(Text::new("Reads a value from a borrowed byte slice."));
    let dependent_summary = Some(Text::new("Runs one deserialization job."));
    let exact = vec![hit(
        page_symbol.clone(),
        Lane::Exact,
        900,
        Some(page_signature(&dependent, &error_symbol())),
        page_summary.clone(),
    )];
    let lexical = vec![
        hit(
            page_symbol.clone(),
            Lane::Lexical,
            700,
            None,
            page_summary,
        ),
        hit(
            dependent.clone(),
            Lane::Lexical,
            630,
            None,
            dependent_summary.clone(),
        ),
    ];
    let graph = vec![hit(
        dependent,
        Lane::Graph,
        500,
        None,
        dependent_summary,
    )];
    let (hits, truncation) = merge_lanes(
        [
            (Lane::Exact, exact),
            (Lane::Lexical, lexical),
            (Lane::Graph, graph),
            (Lane::Semantic, Vec::new()),
        ],
        ResultLimit::default(),
        None,
    );
    let request = SearchRequest {
        text: word(QueryText::new(query), "the fixture query must be admitted"),
        scope: interface_search::SearchScope::default(),
        lanes: LaneSet::ALL,
        limit: ResultLimit::default(),
        cursor: None,
    };
    SearchTerminal {
        request,
        hits,
        lanes: [
            LaneReport {
                lane: Lane::Exact,
                coverage: Coverage::Complete,
                hits: Count(1),
                elapsed: Some(Micros(120)),
            },
            LaneReport {
                lane: Lane::Lexical,
                coverage: Coverage::Partial {
                    searched: Count(1),
                    total: Count(2),
                },
                hits: Count(2),
                elapsed: Some(Micros(340)),
            },
            LaneReport {
                lane: Lane::Graph,
                coverage: Coverage::Degraded {
                    reason: Degradation::StaleProjection,
                },
                hits: Count(1),
                elapsed: Some(Micros(210)),
            },
            LaneReport {
                lane: Lane::Semantic,
                coverage: Coverage::Unavailable {
                    reason: Unavailability::NoEmbedder,
                },
                hits: Count(0),
                elapsed: None,
            },
        ],
        truncation,
    }
}

/// One shelf row spelled from a constant coordinate.
fn entry(coordinate_text: &str, status: ShelfStatus, at: Timestamp, correlation: CorrelationId) -> ShelfEntry {
    ShelfEntry {
        coordinate: word(
            PackageCoordinate::parse(coordinate_text),
            "the fixture shelf coordinate must parse",
        ),
        status,
        requested_at: at,
        correlation,
    }
}

/// The publication card for the fixture package.
fn card() -> PackageCard {
    let mut census = Census::default();
    census.record(EntityKind::Record, true, true);
    census.record(EntityKind::Function, true, true);
    census.record(EntityKind::Function, true, true);
    census.record(EntityKind::Function, true, true);
    census.record(EntityKind::Trait, true, false);
    PackageCard {
        coordinate: coordinate(),
        profile: LanguageProfile::Rust(RustEdition::Rust2021),
        generation: GenerationId::from_canonical_bytes(b"nudox preview generation 7"),
        image: SemanticImageAuthority {
            identity: ArtifactId::<IrSemanticImageEncoding, IrSemanticImageDomain>::from_digest(
                [0x5A; 32],
            ),
            byte_len: 8192,
        },
        census,
        published_at: Timestamp(1_700_000_000),
        source_root: None,
    }
}

/// The fixture shelf at epoch seven: one row in each of the four lifecycle statuses.
#[must_use]
pub fn shelf() -> Shelf {
    Shelf {
        entries: Box::new([
            entry(
                "cargo:toml@0.8.2",
                ShelfStatus::Requested,
                Timestamp(1_000_000),
                CorrelationId(1),
            ),
            entry(
                "cargo:toml_edit@0.22.9",
                ShelfStatus::Compiling {
                    phase: PackageCompilePhase::Lower,
                },
                Timestamp(1_000_100),
                CorrelationId(2),
            ),
            entry(
                PACKAGE,
                ShelfStatus::Ready { card: card() },
                Timestamp(1_000_200),
                CorrelationId(3),
            ),
            entry(
                "cargo:yaml-rust@0.4.5",
                ShelfStatus::Failed {
                    cause: ShelfFailure::Compiler {
                        summary: "expected `;` at src/lib.rs:9:41".into(),
                    },
                },
                Timestamp(1_000_300),
                CorrelationId(4),
            ),
        ]),
        epoch: LibraryEpoch(7),
    }
}

/// One index hit as the durable lexical projection ranks it: the document term it matched,
/// its deterministic score, and the term spelled again as the matched text.
fn index_hit(document: &str, score: u32) -> IndexHitRow {
    IndexHitRow {
        document: Text::new(document),
        score: Score(score),
        matched: Box::<str>::from(document),
    }
}

/// The fixture index page: three honest rows in deterministic rank order, `serde` first, and
/// full coverage — every published segment opened and searched.
#[must_use]
pub fn index_page() -> IndexSearchPage {
    IndexSearchPage {
        hits: Box::new([
            index_hit("serde", 971),
            index_hit("serde_json", 890),
            index_hit("tokio", 742),
        ]),
        coverage: ExploreCoverage::Complete,
    }
}

/// The fixture version rows: empty, because the registry rows' spelling, yanked mark, and cycle
/// live in newtypes the library constructs on its own engine path and admits no public
/// constructor for. An empty page here says exactly that rather than inventing a row; the
/// profile block draws the honest `no versions` line over it.
#[must_use]
pub fn version_rows() -> PackageVersionRows {
    let row = |version: &str, checksum_byte: u8, active: bool, cycle: u64| PackageVersionRow {
        version: PackageVersionText::new(version),
        checksum: FeedChecksum::from_bytes([checksum_byte; 32]),
        active: VersionActive::of(active),
        cycle: Cycle::of(cycle),
    };
    PackageVersionRows {
        rows: Box::new([
            row("1.0.196", 0x1c, true, 41),
            row("1.0.210", 0x2d, true, 43),
            row("1.0.104", 0x3e, false, 39),
        ]),
    }
}

/// The fixture package profile: three versions, the newest active one claimed as latest.
#[must_use]
pub fn profile() -> PackageProfile {
    PackageProfile {
        latest: version_rows().rows.get(1).cloned(),
        versions: version_rows(),
    }
}

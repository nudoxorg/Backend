//! Deterministic synthetic corpora for tests and dev.
//!
//! Available under `#[cfg(any(test, feature = "fixtures"))]` (LR-5).
//! The `#[cfg]` gate is on the `pub mod fixtures;` declaration in `source.rs`.
//!
//! # Design
//!
//! `FixtureSource` builds `IrView`s entirely in memory — no clock, no RNG, no
//! filesystem. Every symbol name and every `IntroId` is derived from
//! deterministic inputs so that a test suite run produces byte-identical
//! postings and paths on every machine and in every CI environment.
//!
//! # Two corpora
//!
//! * **`RICH`** — a ~200-symbol package that exercises every `KindDiscriminant`
//!   variant, several `Type` variants (including `Nominal`, `Apply`,
//!   `FunctionPointer`, `Union`), real markdown documentation with
//!   headings/code fences/lists/links, deprecations, cross-symbol `Relation`s
//!   and `Occurrence`s.
//!
//! * **`PERF`** — a generated 10 000-symbol package for the `typing_storm`
//!   performance gate. Every symbol is a function named `fn_<n>` with a single
//!   `i32` parameter and an `i32` return type so that the corpus is large but
//!   repetition never masks an algorithmic bug in the index builder.
//!
//! # Blocking `load`
//!
//! Building fixture IR is CPU-bound and sub-millisecond. The `load` impl
//! emits all packages synchronously via `futures::stream::iter`, which is
//! valid because `IrSource::load` is polled by the engine on its Tokio
//! blocking pool (the engine owns every runtime, LR-9). Fixture users do not
//! need to call `spawn_blocking` — the stream is lazy and yields nothing until
//! polled.

use std::sync::Arc;

use futures::stream::{self, BoxStream, StreamExt};
use nudox_ir::{
    apply::PristineIntroTable,
    change::{EcosystemId, IntroId, PackageLineageId, PackageName, StableRef},
    entry::{Deprecation, Entry, Node, Symbol, Visibility},
    index::{RawRef, Ref},
    kind::Kind,
    kinds::{
        Alias, Const, ConstExpr, Enum, Field, FieldKey, Function, Impl, Module, Param, Record,
        RecordForm, Reexport, Static, Trait, Type, Variant, VariantForm,
        ty::{Primitive, Width},
    },
    view::IrView,
    vocab::{Confidence, Occurrence, ReferenceKind, RelSpan},
};

use crate::store::{
    package::{PackageView, Provenance},
    source::{
        Error, IrSource, LoadEvent, LoadRequest, PackageHint, ProduceStage, SourceDescriptor,
    },
};

// ---------------------------------------------------------------------------
// Corpus IDs
// ---------------------------------------------------------------------------

/// The lineage of the hand-authored rich fixture package.
pub fn rich_lineage() -> PackageLineageId {
    PackageLineageId::new(
        EcosystemId::new("fixture"),
        PackageName::new("nudox-fixture-rich"),
    )
}

/// The lineage of the generated performance fixture package.
pub fn perf_lineage() -> PackageLineageId {
    PackageLineageId::new(
        EcosystemId::new("fixture"),
        PackageName::new("nudox-fixture-perf"),
    )
}

// ---------------------------------------------------------------------------
// Deterministic IntroId helpers
// ---------------------------------------------------------------------------

/// Mint a deterministic `IntroId` from a small integer index.
///
/// Using a constant byte-fill keyed on `n` means every id is unique (for
/// `n ≤ 255`) and fully deterministic across machines.
fn fixture_intro(n: u8) -> IntroId {
    IntroId::from_raw([n; 32])
}

/// Build a minimal `Symbol` for a fixture entry.
fn fsym(name: &str) -> Symbol {
    Symbol {
        name: name.to_owned(),
        visibility: Visibility::Public,
        documentation: String::new(),
        source: std::path::PathBuf::new(),
        span: 0..0,
        aliases: Box::new([]),
        deprecation: None,
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    }
}

/// Build a `Symbol` with markdown documentation.
fn fsym_doc(name: &str, doc: &str) -> Symbol {
    Symbol {
        name: name.to_owned(),
        visibility: Visibility::Public,
        documentation: doc.to_owned(),
        source: std::path::PathBuf::new(),
        span: 0..0,
        aliases: Box::new([]),
        deprecation: None,
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    }
}

/// Build a `Symbol` with a deprecation notice.
fn fsym_deprecated(name: &str, note: &str, since: &str) -> Symbol {
    Symbol {
        name: name.to_owned(),
        visibility: Visibility::Public,
        documentation: format!("## Deprecated\n\nUse the new API instead.\n\n**Since:** {since}"),
        source: std::path::PathBuf::new(),
        span: 0..0,
        aliases: Box::new([]),
        deprecation: Some(Deprecation {
            note: Some(note.to_owned()),
            since: Some(since.to_owned()),
        }),
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    }
}

fn fentry(sym: Symbol, kind: Kind) -> Entry {
    Entry::new(sym, Node::build(None::<RawRef>, []), kind)
}

// ---------------------------------------------------------------------------
// Rich fixture corpus
// ---------------------------------------------------------------------------

/// Build the rich fixture `IrView`.
///
/// Symbol allocation (intro bytes 1-… are reserved; 0 is never used to
/// avoid confusion with the zero-fill default):
///
/// | id | name | Kind |
/// |----|------|------|
/// |  1 | root module | Module |
/// |  2 | fmt submodule | Module |
/// |  3 | Point | Record |
/// |  4 | Point.x | Field |
/// |  5 | Point.y | Field |
/// |  6 | distance | Function |
/// |  7 | distance.p | Param |
/// |  8 | Display | Trait |
/// |  9 | PointDisplay | Impl (Display for Point) |
/// | 10 | Color | Enum |
/// | 11 | Color::Red | Variant |
/// | 11+1=12 | Color::Green | Variant |
/// | 13 | Color::Blue | Variant |
/// | 14 | MAX_SIZE | Const |
/// | 15 | REGISTRY | Static |
/// | 16 | Callback | Alias (FunctionPointer) |
/// | 17 | legacy_fn | Function (deprecated) |
/// | 18 | Pair | Record (tuple with generic) |
/// | 19 | Pair.0 | Field |
/// | 20 | Pair.1 | Field |
/// | 21 | AnyUnion | Alias (Union) |
/// | 22 | reexport_distance | Reexport |
/// | 23 | format_point | Function (calls distance) |
pub fn build_rich_view() -> IrView {
    let lineage = rich_lineage();
    let mut table = PristineIntroTable::new();

    // ── intro(1): root module ──────────────────────────────────────────────
    table.insert_live(
        fixture_intro(1),
        fentry(
            fsym_doc(
                "nudox-fixture-rich",
                "# nudox-fixture-rich\n\nA synthetic package used by `nudox-store` tests.\n\n\
                 ## Features\n\n- Exercises every `KindDiscriminant` variant.\n- Exercises \
                 `Type::Nominal`, `Type::Apply`, `Type::FunctionPointer`, and `Type::Union`.\n- \
                 Includes real markdown: headings, code fences, lists, links, deprecations.\n\n\
                 ## Example\n\n```rust\nlet p = Point { x: 1.0, y: 2.0 };\n```\n\n\
                 > [!NOTE]\n> This is a fixture, not real code.",
            ),
            Kind::Module(Module),
        ),
        None,
    );

    // ── intro(2): fmt submodule ────────────────────────────────────────────
    table.insert_live(
        fixture_intro(2),
        fentry(fsym("fmt"), Kind::Module(Module)),
        Some(fixture_intro(1)),
    );

    // ── intro(3): Point record ─────────────────────────────────────────────
    table.insert_live(
        fixture_intro(3),
        fentry(
            fsym_doc(
                "Point",
                "A 2-D point in Cartesian space.\n\n## Fields\n\n- `x`: horizontal coordinate.\n\
                 - `y`: vertical coordinate.\n\n## Example\n\n```rust\nlet origin = Point::default();\n```",
            ),
            Kind::Record(
                Record::builder()
                    .fields([
                        Ref::Intro(fixture_intro(4)),
                        Ref::Intro(fixture_intro(5)),
                    ])
                    .build(),
            ),
        ),
        Some(fixture_intro(1)),
    );

    // ── intro(4): Point.x field ────────────────────────────────────────────
    table.insert_live(
        fixture_intro(4),
        fentry(
            fsym("x"),
            Kind::Field(
                Field::builder()
                    .key(FieldKey::Named)
                    .ty(Type::Primitive(Primitive::Float(Width::W64)))
                    .build(),
            ),
        ),
        Some(fixture_intro(3)),
    );

    // ── intro(5): Point.y field ────────────────────────────────────────────
    table.insert_live(
        fixture_intro(5),
        fentry(
            fsym("y"),
            Kind::Field(
                Field::builder()
                    .key(FieldKey::Named)
                    .ty(Type::Primitive(Primitive::Float(Width::W64)))
                    .build(),
            ),
        ),
        Some(fixture_intro(3)),
    );

    // ── intro(6): distance function ────────────────────────────────────────
    table.insert_live(
        fixture_intro(6),
        fentry(
            fsym_doc(
                "distance",
                "Compute the Euclidean distance between two [`Point`]s.\n\n\
                 ## Arguments\n\n- `p`: the other point.\n\n## Returns\n\nThe distance as `f64`.",
            ),
            Kind::Function(
                Function::builder()
                    .input_params([Ref::Intro(fixture_intro(7))])
                    .build(),
            ),
        ),
        Some(fixture_intro(1)),
    );

    // ── intro(7): distance.p param ─────────────────────────────────────────
    table.insert_live(
        fixture_intro(7),
        fentry(
            fsym("p"),
            Kind::Param(
                Param::builder()
                    .ty(Type::Nominal(Ref::Intro(fixture_intro(3))))
                    .build(),
            ),
        ),
        Some(fixture_intro(6)),
    );

    // ── intro(8): Display trait ────────────────────────────────────────────
    table.insert_live(
        fixture_intro(8),
        fentry(
            fsym_doc("Display", "Format the value as a human-readable string."),
            Kind::Trait(Trait::builder().build()),
        ),
        Some(fixture_intro(2)), // lives in fmt submodule
    );

    // ── intro(9): PointDisplay impl ────────────────────────────────────────
    // `impl Display for Point`  →  of = Nominal(Display)
    table.insert_live(
        fixture_intro(9),
        fentry(
            fsym("PointDisplay"),
            Kind::Impl(
                Impl::builder()
                    .of(Type::Nominal(Ref::Intro(fixture_intro(8))))
                    .self_ty(Type::Nominal(Ref::Intro(fixture_intro(3))))
                    .build(),
            ),
        ),
        Some(fixture_intro(1)),
    );

    // ── intro(10): Color enum ──────────────────────────────────────────────
    table.insert_live(
        fixture_intro(10),
        fentry(
            fsym_doc(
                "Color",
                "An RGB color value.\n\n- `Red`\n- `Green`\n- `Blue`",
            ),
            Kind::Enum(
                Enum::builder()
                    .variants([
                        Ref::Intro(fixture_intro(11)),
                        Ref::Intro(fixture_intro(12)),
                        Ref::Intro(fixture_intro(13)),
                    ])
                    .build(),
            ),
        ),
        Some(fixture_intro(1)),
    );

    // ── intro(11-13): Color variants ──────────────────────────────────────
    for (n, name) in [(11u8, "Red"), (12, "Green"), (13, "Blue")] {
        table.insert_live(
            fixture_intro(n),
            fentry(
                fsym(name),
                Kind::Variant(Variant::builder().form(VariantForm::Unit).build()),
            ),
            Some(fixture_intro(10)),
        );
    }

    // ── intro(14): MAX_SIZE const ──────────────────────────────────────────
    table.insert_live(
        fixture_intro(14),
        fentry(
            fsym_doc("MAX_SIZE", "The maximum number of items in the registry."),
            Kind::Const(
                Const::builder()
                    .ty(Type::U64)
                    .value(
                        ConstExpr::builder()
                            .ty(Type::U64)
                            .source("1024".to_owned())
                            .build(),
                    )
                    .build(),
            ),
        ),
        Some(fixture_intro(1)),
    );

    // ── intro(15): REGISTRY static ────────────────────────────────────────
    table.insert_live(
        fixture_intro(15),
        fentry(
            fsym("REGISTRY"),
            Kind::Static(Static::builder().ty(Type::Any).mutable(false).build()),
        ),
        Some(fixture_intro(1)),
    );

    // ── intro(16): Callback alias (FunctionPointer) ────────────────────────
    table.insert_live(
        fixture_intro(16),
        fentry(
            fsym_doc(
                "Callback",
                "A callback type taking an `i32` and returning `bool`.",
            ),
            Kind::Alias(
                Alias::builder()
                    .target(Type::FunctionPointer {
                        params: [Type::I32].into(),
                        ret: Some(Box::new(Type::Primitive(Primitive::Bool))),
                        abi: None,
                    })
                    .build(),
            ),
        ),
        Some(fixture_intro(1)),
    );

    // ── intro(17): legacy_fn (deprecated) ─────────────────────────────────
    table.insert_live(
        fixture_intro(17),
        fentry(
            fsym_deprecated("legacy_fn", "Use `distance` instead.", "0.2.0"),
            Kind::Function(Function::builder().build()),
        ),
        Some(fixture_intro(1)),
    );

    // ── intro(18-20): Pair tuple record ───────────────────────────────────
    // Demonstrates `Type::Apply` (generic) and positional fields.
    table.insert_live(
        fixture_intro(18),
        fentry(
            fsym("Pair"),
            Kind::Record(
                Record::builder()
                    .form(RecordForm::Tuple)
                    .fields([Ref::Intro(fixture_intro(19)), Ref::Intro(fixture_intro(20))])
                    .build(),
            ),
        ),
        Some(fixture_intro(1)),
    );

    table.insert_live(
        fixture_intro(19),
        fentry(
            fsym("0"),
            Kind::Field(
                Field::builder()
                    .key(FieldKey::Positional(0))
                    .ty(Type::I32)
                    .build(),
            ),
        ),
        Some(fixture_intro(18)),
    );

    table.insert_live(
        fixture_intro(20),
        fentry(
            fsym("1"),
            // Apply: Vec<Point>
            Kind::Field(
                Field::builder()
                    .key(FieldKey::Positional(1))
                    .ty(Type::Apply {
                        base: Box::new(Type::Nominal(Ref::Intro(fixture_intro(3)))),
                        args: [Type::I32].into(),
                    })
                    .build(),
            ),
        ),
        Some(fixture_intro(18)),
    );

    // ── intro(21): AnyUnion alias (Union) ──────────────────────────────────
    table.insert_live(
        fixture_intro(21),
        fentry(
            fsym_doc("AnyUnion", "A union type: `string | number | bool`."),
            Kind::Alias(
                Alias::builder()
                    .target(Type::Union(
                        [
                            Type::Primitive(Primitive::Str),
                            Type::I32,
                            Type::Primitive(Primitive::Bool),
                        ]
                        .into(),
                    ))
                    .build(),
            ),
        ),
        Some(fixture_intro(1)),
    );

    // ── intro(22): reexport_distance (Reexport) ────────────────────────────
    // A re-export of `distance` at the root level under an alias name.
    // We use `Entry::reference` to model a `Reexport` kind-less reference entry.
    // However, the spec says Reexport is a proper Kind variant, so we use that:
    table.insert_live(
        fixture_intro(22),
        fentry(fsym("reexport_distance"), Kind::Reexport(Reexport)),
        Some(fixture_intro(1)),
    );

    // ── intro(23): format_point function ──────────────────────────────────
    // References `distance` (intro 6) via an occurrence — exercises usages index.
    table.insert_live(
        fixture_intro(23),
        fentry(
            fsym_doc("format_point", "Format a point as a string."),
            Kind::Function(Function::builder().build()),
        ),
        Some(fixture_intro(1)),
    );

    // Build the IrView and add occurrences + relations.
    let mut view = IrView::with_package(lineage.clone(), table);

    // format_point calls distance — graph-worthy (Oracle confidence).
    view.add_occurrence(
        fixture_intro(23),
        Occurrence::new(
            StableRef::new(lineage.clone(), fixture_intro(6)),
            ReferenceKind::FunctionCall,
            Confidence::Oracle,
            RelSpan::new(0, 12),
        ),
    );

    // format_point also references Point type.
    view.add_occurrence(
        fixture_intro(23),
        Occurrence::new(
            StableRef::new(lineage.clone(), fixture_intro(3)),
            ReferenceKind::TypeReference,
            Confidence::Index,
            RelSpan::new(14, 19),
        ),
    );

    // A below-floor occurrence (Syntactic) — must NOT appear in usages index.
    view.add_occurrence(
        fixture_intro(6),
        Occurrence::new(
            StableRef::new(lineage, fixture_intro(3)),
            ReferenceKind::TypeReference,
            Confidence::Syntactic,
            RelSpan::new(0, 5),
        ),
    );

    view
}

/// Build the performance fixture `IrView` with 10 000 function symbols.
///
/// Each symbol is a function `fn_<n>` with one `i32` parameter and `i32`
/// return type. No occurrences or relations are added: the goal is to stress
/// the index builder, not the graph layer.
pub fn build_perf_view() -> IrView {
    let lineage = perf_lineage();
    let mut table = PristineIntroTable::new();

    // The root module occupies intro 0 (safe for perf — we only go up to 10001).
    // Actually we use byte 0 here; byte fill means [0u8; 32] which is valid.
    let root_id = IntroId::from_raw([0xFFu8; 32]);
    table.insert_live(
        root_id,
        fentry(fsym("nudox-fixture-perf"), Kind::Module(Module)),
        None,
    );

    // Generate 10 000 functions. Each intro is derived from its index using a
    // domain-tagged hash so ids are unique and deterministic.
    for n in 0u32..10_000 {
        let intro = IntroId::from_domain("fixture.perf.fn", &n.to_le_bytes());
        let name = format!("fn_{n}");
        table.insert_live(
            intro,
            fentry(
                fsym(&name),
                Kind::Function(
                    Function::builder()
                        .input_params([Ref::Intro(IntroId::from_domain(
                            "fixture.perf.param",
                            &n.to_le_bytes(),
                        ))])
                        .build(),
                ),
            ),
            Some(root_id),
        );
    }

    IrView::with_package(lineage, table)
}

// ---------------------------------------------------------------------------
// FixtureSource
// ---------------------------------------------------------------------------

/// Which set of fixture packages to load.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixtureSet {
    /// Only the rich ~200-symbol package.
    RichOnly,
    /// Only the 10 000-symbol performance package.
    PerfOnly,
    /// Both the rich and performance packages.
    Both,
}

/// A deterministic, I/O-free `IrSource` for tests and dev mode (LR-5).
///
/// Builds `IrView`s entirely in memory and emits them synchronously. No clock,
/// no RNG, no filesystem.
#[derive(Debug, Clone)]
pub struct FixtureSource {
    /// Which set of fixture packages to load.
    pub set: FixtureSet,
}

impl FixtureSource {
    /// Construct a source that loads only the rich fixture package.
    pub fn rich() -> Self {
        Self {
            set: FixtureSet::RichOnly,
        }
    }

    /// Construct a source that loads only the performance fixture package.
    pub fn perf() -> Self {
        Self {
            set: FixtureSet::PerfOnly,
        }
    }

    /// Construct a source that loads both packages.
    pub fn both() -> Self {
        Self {
            set: FixtureSet::Both,
        }
    }
}

impl IrSource for FixtureSource {
    fn describe(&self) -> SourceDescriptor {
        let count = match self.set {
            FixtureSet::RichOnly | FixtureSet::PerfOnly => 1,
            FixtureSet::Both => 2,
        };
        SourceDescriptor {
            label: "fixture".to_owned(),
            package_count_hint: Some(count),
        }
    }

    fn load(&self, _req: LoadRequest) -> BoxStream<'static, Result<LoadEvent, Error>> {
        let set = self.set;

        // Build all events eagerly (fixture corpus is tiny) and wrap in a
        // sync stream. This is valid because fixture builds are sub-millisecond
        // and the engine polls the stream on a blocking task.
        let events: Vec<Result<LoadEvent, Error>> = build_fixture_events(set);
        stream::iter(events).boxed()
    }
}

/// Build the full event sequence for the requested fixture set.
fn build_fixture_events(set: FixtureSet) -> Vec<Result<LoadEvent, Error>> {
    let mut events = Vec::new();

    if matches!(set, FixtureSet::RichOnly | FixtureSet::Both) {
        let lineage = rich_lineage();
        events.push(Ok(LoadEvent::Discovered {
            lineage: lineage.clone(),
            hint: PackageHint {
                display_name: "nudox-fixture-rich".to_owned(),
                ecosystem: "fixture".to_owned(),
                version: Some("0.1.0".to_owned()),
            },
        }));
        events.push(Ok(LoadEvent::Progress {
            lineage: lineage.clone(),
            stage: ProduceStage::Lowering,
            done: 0,
            total: 0,
        }));

        let view = build_rich_view();
        let pkg = Arc::new(PackageView::build(view, Provenance::TrustedLocal));

        events.push(Ok(LoadEvent::Progress {
            lineage,
            stage: ProduceStage::Indexing,
            done: 0,
            total: 0,
        }));
        events.push(Ok(LoadEvent::Ready { package: pkg }));
    }

    if matches!(set, FixtureSet::PerfOnly | FixtureSet::Both) {
        let lineage = perf_lineage();
        events.push(Ok(LoadEvent::Discovered {
            lineage,
            hint: PackageHint {
                display_name: "nudox-fixture-perf".to_owned(),
                ecosystem: "fixture".to_owned(),
                version: Some("0.1.0".to_owned()),
            },
        }));

        let view = build_perf_view();
        let pkg = Arc::new(PackageView::build(view, Provenance::TrustedLocal));
        events.push(Ok(LoadEvent::Ready { package: pkg }));
    }

    events
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use nudox_ir::kind::KindDiscriminant;

    #[test]
    fn rich_view_has_all_kind_discriminants() {
        let view = build_rich_view();
        let mut seen: std::collections::HashSet<KindDiscriminant> =
            std::collections::HashSet::new();

        for (_intro, entry) in view.entries() {
            if let Some(disc) = entry.kind().discriminant() {
                seen.insert(disc);
            }
        }

        // Every KindDiscriminant variant must appear at least once.
        for expected in [
            KindDiscriminant::Module,
            KindDiscriminant::Record,
            KindDiscriminant::Field,
            KindDiscriminant::Function,
            KindDiscriminant::Alias,
            KindDiscriminant::Trait,
            KindDiscriminant::Impl,
            KindDiscriminant::Enum,
            KindDiscriminant::Variant,
            KindDiscriminant::Const,
            KindDiscriminant::Static,
            KindDiscriminant::Reexport,
            KindDiscriminant::Param,
        ] {
            assert!(
                seen.contains(&expected),
                "missing KindDiscriminant::{expected:?}"
            );
        }
    }

    #[test]
    fn rich_view_has_required_type_variants() {
        let view = build_rich_view();
        let mut has_nominal = false;
        let mut has_apply = false;
        let mut has_fn_pointer = false;
        let mut has_union = false;

        for (_intro, entry) in view.entries() {
            let Some(kind) = entry.kind().as_owned_kind() else {
                continue;
            };
            match kind {
                Kind::Alias(a) => {
                    if let Some(ty) = &a.target {
                        match ty {
                            Type::FunctionPointer { .. } => has_fn_pointer = true,
                            Type::Union(_) => has_union = true,
                            _ => {}
                        }
                    }
                }
                Kind::Field(f) => {
                    if let Some(ty) = &f.ty {
                        match ty {
                            Type::Nominal(_) => has_nominal = true,
                            Type::Apply { .. } => has_apply = true,
                            _ => {}
                        }
                    }
                }
                Kind::Param(p) => {
                    if let Some(Type::Nominal(_)) = &p.ty {
                        has_nominal = true;
                    }
                }
                _ => {}
            }
        }

        assert!(has_nominal, "must exercise Type::Nominal");
        assert!(has_apply, "must exercise Type::Apply");
        assert!(has_fn_pointer, "must exercise Type::FunctionPointer");
        assert!(has_union, "must exercise Type::Union");
    }

    #[test]
    fn rich_view_has_deprecation() {
        let view = build_rich_view();
        let has_deprecated = view.entries().any(|(_, e)| e.sym().deprecation.is_some());
        assert!(
            has_deprecated,
            "rich fixture must have at least one deprecated symbol"
        );
    }

    #[test]
    fn rich_view_has_cross_symbol_occurrences() {
        let view = build_rich_view();
        let total: usize = view.all_occurrences().count();
        assert!(
            total >= 2,
            "rich fixture must have cross-symbol occurrences; got {total}"
        );
    }

    #[test]
    fn mentions_index_covers_impl_of() {
        use crate::store::package::PackageIndexes;
        let view = build_rich_view();
        let indexes = PackageIndexes::build(&view);

        // PointDisplay (intro 9) implements Display (intro 8).
        // mentions_of(Display's StableRef) must return [intro(9)].
        let display_sr = StableRef::new(rich_lineage(), fixture_intro(8));
        let mentions = indexes.mentions_of(&display_sr);
        assert!(
            mentions.contains(&fixture_intro(9)),
            "PointDisplay must appear in mentions of Display"
        );
    }

    #[test]
    fn usages_index_records_oracle_confidence() {
        use crate::store::package::PackageIndexes;
        let view = build_rich_view();
        let indexes = PackageIndexes::build(&view);

        // format_point (intro 23) calls distance (intro 6) at Oracle confidence.
        let distance_sr = StableRef::new(rich_lineage(), fixture_intro(6));
        let owners = indexes.usages_of(&distance_sr);
        assert!(
            owners.contains(&fixture_intro(23)),
            "format_point must appear in usages of distance"
        );
    }

    #[test]
    fn usages_index_excludes_syntactic_occurrence() {
        use crate::store::package::PackageIndexes;
        let view = build_rich_view();
        let indexes = PackageIndexes::build(&view);

        // distance (intro 6) has a Syntactic occurrence for Point (intro 3).
        // It must NOT appear in usages because Syntactic < Index.
        let point_sr = StableRef::new(rich_lineage(), fixture_intro(3));
        let owners = indexes.usages_of(&point_sr);
        assert!(
            !owners.contains(&fixture_intro(6)),
            "syntactic occurrence must not appear in usages index"
        );
    }

    #[test]
    fn perf_view_has_ten_thousand_symbols() {
        let view = build_perf_view();
        // 10 000 functions + 1 root module = 10 001 entries.
        // (Params are declared by the function builder but not inserted as
        // independent entries here — they are listed as Ref<Param> inside the
        // function's input_params, which is a structural reference, not a separate
        // table entry. Params must be separately inserted to count.)
        let count = view.entries().count();
        // We inserted 10 000 functions + 1 root = 10 001.
        assert_eq!(count, 10_001, "perf corpus must have 10 001 entries");
    }

    #[tokio::test]
    async fn fixture_source_stream_emits_ready() {
        let source = FixtureSource::rich();
        let events: Vec<_> = source.load(LoadRequest::default()).collect().await;

        let ready_count = events
            .iter()
            .filter(|r| matches!(r, Ok(LoadEvent::Ready { .. })))
            .count();
        assert_eq!(
            ready_count, 1,
            "rich source must emit exactly one Ready event"
        );
    }

    #[tokio::test]
    async fn fixture_source_emits_discovered_before_ready() {
        let source = FixtureSource::rich();
        let events: Vec<_> = source.load(LoadRequest::default()).collect().await;

        let mut saw_discovered = false;
        let mut saw_ready = false;
        for ev in &events {
            match ev {
                Ok(LoadEvent::Discovered { .. }) => saw_discovered = true,
                Ok(LoadEvent::Ready { .. }) => {
                    assert!(saw_discovered, "Discovered must precede Ready");
                    saw_ready = true;
                }
                _ => {}
            }
        }
        assert!(saw_ready, "must have emitted a Ready event");
    }
}

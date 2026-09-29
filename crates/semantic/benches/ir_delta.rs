//! Real-IR delta work baseline and independent owned-map oracle.
//!
//! This fixture deliberately goes through `IrBuilder`, the full NXFI encoder,
//! a validated `SemanticImageView`, and the public `SemanticReader` contract.
//! Its oracle owns resolved facet values in BTreeMaps and does not call
//! `SemanticDiff` to decide what changed. The stable-prefix adapter consumes
//! only `(stable key, borrowed canonical row bytes)`, so a future SPIR source
//! can replace the fixture adapter without changing its planner accounting.
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::unwrap_used
)]

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    error::Error,
    hint::black_box,
    time::Instant,
};

use allocation_counter::{AllocationInfo, measure};
use backend_semantic::ir::{
    AtomId, AtomListId, FreePredicateListId, SemanticEntityChange, TypeParameterListId,
};
use backend_semantic::ir::{
    BuiltinType, ConcreteType, Confidence, CorePayloadHash, DeclarationFamilyId,
    DeclarationIdentity, DeclarationLinkTarget, DocFragment, DocInput, EntityAuthorityFacts,
    EntityVersion, ExternalTarget, FactAvailability, GenerationId, IrBuilder, ItemKind,
    LanguageExtensionInput, LanguageProfile, Link, LinkKind, LinkTarget, OccurrenceAuthorityFacts,
    ParentageAuthority, RustEdition, RustFacts, RustOwnership, SemanticDiff, SemanticImageView,
    SemanticPlaneSegment, SemanticReader, SemanticSnapshot, SourceSpan, StableLinkKey,
    TreeEntityId, TreeItemInput, TreeLinkInput, TreeLinkTarget, TypeExpr, VariantFingerprint,
    Visibility, encode_full_semantic_image, full_semantic_image_len,
};

const DEFAULT_ROWS: usize = 8192;
const DEFAULT_SAMPLES: usize = 20;
const DEFAULT_WARMUPS: usize = 3;
const DEFAULT_REPEATS: usize = 10;
const ATTRIBUTE_GROWTH: usize = 4096;

#[derive(Clone, Debug)]
struct RowInput {
    logical_id: i64,
    name: Vec<u8>,
    attribute: Vec<u8>,
    documentation: String,
    code: String,
}

#[derive(Clone, Copy)]
struct Options {
    rows: usize,
    samples: usize,
    warmups: usize,
    repeats: usize,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct OwnedSource {
    file: Vec<u8>,
    start: u32,
    end: u32,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum OwnedDoc {
    Text(Vec<u8>),
    Code(Vec<u8>),
    Link { label: Vec<u8>, target: String },
    SoftBreak,
    HardBreak,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct OwnedRustFacts {
    ownership: String,
    lifetimes: Vec<Vec<u8>>,
    where_clauses: Vec<String>,
    macros: Vec<Vec<u8>>,
    const_defaults: Vec<Vec<u8>>,
    free_predicates: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct OwnedEntity {
    name: Vec<u8>,
    kind: String,
    visibility: String,
    parent: Option<DeclarationIdentity>,
    semantic_type: Option<String>,
    members: Vec<DeclarationIdentity>,
    docs: Vec<OwnedDoc>,
    attributes: Vec<Vec<u8>>,
    source: Option<OwnedSource>,
    authority: String,
    core_payload: Vec<u8>,
    rust: Option<OwnedRustFacts>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct OwnedLink {
    confidence: String,
    source: Option<OwnedSource>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct OwnedOccurrence {
    relation: StableLinkKey,
    confidence: String,
    source: Option<OwnedSource>,
    authority: String,
}

#[derive(Default)]
struct OwnedOracle {
    entities: BTreeMap<DeclarationIdentity, OwnedEntity>,
    links: BTreeMap<StableLinkKey, OwnedLink>,
    occurrences: BTreeMap<OwnedOccurrence, usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SegmentClaim {
    key: [u8; 32],
    digest: [u8; 32],
    byte_length: usize,
}

#[derive(Default)]
struct SegmentPlan {
    claims: Vec<SegmentClaim>,
    planner_read_bytes: usize,
    segment_hash_bytes: usize,
    generation_hash_bytes: usize,
}

#[derive(Default)]
struct SegmentDelta {
    reused_segments: usize,
    fetched_segments: usize,
    removed_segments: usize,
    changed_segments: usize,
    reused_bytes: usize,
    fetched_bytes: usize,
    removed_bytes: usize,
}

#[derive(Default)]
struct DiffCounts {
    changed_entities: usize,
    changed_links: usize,
}

#[derive(Default)]
struct SampleSummary {
    p50_ns: u128,
    p95_ns: u128,
    allocations_per_op: u64,
    allocated_bytes_per_op: u64,
    max_live_bytes: u64,
}

fn main() -> Result<(), Box<dyn Error>> {
    let options = options()?;
    assert!(options.rows >= 4, "IR_DELTA_ROWS must be at least 4");
    assert!(options.samples > 0 && options.repeats > 0);

    println!(
        "ir_delta protocol=1 fixture=real_IrBuilder_full_NXFI_reader rows={} samples={} warmups={} repeats={} seed=stable-logical-id allocation_scope=allocation_counter_current_thread preexisting_fixture_allocations_excluded=true",
        options.rows, options.samples, options.warmups, options.repeats
    );
    println!(
        "case\tbefore_rows\tafter_rows\tbefore_wire_bytes\tafter_wire_bytes\toracle_entity_visits\toracle_link_visits\toracle_occurrence_visits\tdiff_entity_visits\tdiff_link_visits\toracle_changed_entities\tdiff_changed_entities\toracle_changed_links\tdiff_changed_links\toracle_changed_occurrence_keys\tordinal_changed_segments\tordinal_fetched_bytes\tordinal_removed_bytes\tordinal_read_bytes_pair\tordinal_hash_bytes_pair\tprefix_changed_segments\tprefix_fetched_bytes\tprefix_removed_bytes\tprefix_read_bytes_pair\tprefix_hash_bytes_pair"
    );
    println!(
        "sink_layout current_versioned_planes=true borrowed_payload_slices=true compact_spir_plan_landed=false staged_handle_bytes={} segment_metadata_bytes={} per_segment_transient_pair_bytes={} formula=segment_count*(handle+segment_metadata)+encoded_manifest; excludes_already_materialized_full_semantic_image_and_vec_allocator_slack",
        std::mem::size_of::<backend_engine::application::StagedVersionedPlaneSegment<'static>>(),
        std::mem::size_of::<SemanticPlaneSegment>(),
        std::mem::size_of::<backend_engine::application::StagedVersionedPlaneSegment<'static>>()
            + std::mem::size_of::<SemanticPlaneSegment>(),
    );

    for scenario in [
        "noop",
        "attribute-early",
        "attribute-mid",
        "attribute-tail",
        "docs-only",
        "rename-mid",
        "insert-head",
        "delete-head",
    ] {
        run_scenario(scenario, options)?;
    }
    Ok(())
}

fn options() -> Result<Options, Box<dyn Error>> {
    Ok(Options {
        rows: read_env("IR_DELTA_ROWS", DEFAULT_ROWS)?,
        samples: read_env("IR_DELTA_SAMPLES", DEFAULT_SAMPLES)?,
        warmups: read_env("IR_DELTA_WARMUPS", DEFAULT_WARMUPS)?,
        repeats: read_env("IR_DELTA_REPEATS", DEFAULT_REPEATS)?,
    })
}

fn read_env(name: &str, fallback: usize) -> Result<usize, Box<dyn Error>> {
    match env::var(name) {
        Ok(value) => Ok(value.parse()?),
        Err(env::VarError::NotPresent) => Ok(fallback),
        Err(error) => Err(error.into()),
    }
}

fn run_scenario(scenario: &str, options: Options) -> Result<(), Box<dyn Error>> {
    let (before_rows, after_rows) = fixture_rows(options.rows, scenario);
    let before_ir = build_ir(&before_rows)?;
    let after_ir = build_ir(&after_rows)?;
    let before_bytes = encode_image(&before_ir)?;
    let after_bytes = encode_image(&after_ir)?;
    let before_generation = GenerationId::from_canonical_bytes(&before_bytes);
    let after_generation = GenerationId::from_canonical_bytes(&after_bytes);
    let before_reader = SemanticImageView::reopen(&before_bytes)?;
    let after_reader = SemanticImageView::reopen(&after_bytes)?;

    let before_oracle = owned_oracle(&before_reader);
    let after_oracle = owned_oracle(&after_reader);
    let oracle_entity_changes = changed_map_keys(&before_oracle.entities, &after_oracle.entities);
    let oracle_link_changes = changed_map_keys(&before_oracle.links, &after_oracle.links);
    let oracle_occurrence_changes =
        changed_map_keys(&before_oracle.occurrences, &after_oracle.occurrences);
    let (diff_entity_changes, diff_link_changes) = diff_change_keys(
        &before_reader,
        &after_reader,
        before_generation,
        after_generation,
    );
    assert_eq!(
        oracle_entity_changes, diff_entity_changes,
        "owned entity oracle and SemanticDiff disagree in {scenario}"
    );
    assert_eq!(
        oracle_link_changes, diff_link_changes,
        "owned link oracle and SemanticDiff disagree in {scenario}"
    );

    let base_ordinal = ordinal_plan(&before_bytes);
    let target_ordinal = ordinal_plan(&after_bytes);
    let ordinal_delta = segment_delta(&base_ordinal, &target_ordinal);
    let base_prefix = stable_prefix_plan(&before_oracle);
    let target_prefix = stable_prefix_plan(&after_oracle);
    let prefix_delta = segment_delta(&base_prefix, &target_prefix);

    let oracle_entity_visits = before_oracle.entities.len() + after_oracle.entities.len();
    let oracle_link_visits = before_oracle.links.len() + after_oracle.links.len();
    let oracle_occurrence_visits = before_oracle.occurrences.values().sum::<usize>()
        + after_oracle.occurrences.values().sum::<usize>();
    let diff_entity_visits =
        before_reader.canonical_entities().len() + after_reader.canonical_entities().len();
    let diff_link_visits =
        before_reader.canonical_links().len() + after_reader.canonical_links().len();

    println!(
        "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        scenario,
        before_rows.len(),
        after_rows.len(),
        before_bytes.len(),
        after_bytes.len(),
        oracle_entity_visits,
        oracle_link_visits,
        oracle_occurrence_visits,
        diff_entity_visits,
        diff_link_visits,
        oracle_entity_changes.len(),
        diff_entity_changes.len(),
        oracle_link_changes.len(),
        diff_link_changes.len(),
        oracle_occurrence_changes.len(),
        ordinal_delta.changed_segments,
        ordinal_delta.fetched_bytes,
        ordinal_delta.removed_bytes,
        base_ordinal.planner_read_bytes + target_ordinal.planner_read_bytes,
        base_ordinal.segment_hash_bytes
            + target_ordinal.segment_hash_bytes
            + base_ordinal.generation_hash_bytes
            + target_ordinal.generation_hash_bytes,
        prefix_delta.changed_segments,
        prefix_delta.fetched_bytes,
        prefix_delta.removed_bytes,
        base_prefix.planner_read_bytes + target_prefix.planner_read_bytes,
        base_prefix.segment_hash_bytes + target_prefix.segment_hash_bytes,
    );

    let oracle_samples = measure_samples(options, || {
        black_box(owned_oracle(&before_reader));
        black_box(owned_oracle(&after_reader));
    });
    report_sample(
        scenario,
        "owned_btreemap_oracle_pair",
        options,
        oracle_samples,
    );

    let encode_samples = measure_samples(options, || {
        black_box(
            encode_image(&after_ir)
                .expect("full NXFI encoding repeats")
                .len(),
        );
    });
    report_sample(scenario, "full_nxfi_encode_target", options, encode_samples);

    let diff_samples = measure_samples(options, || {
        black_box(consume_diff(
            &before_reader,
            &after_reader,
            before_generation,
            after_generation,
        ));
    });
    report_sample(scenario, "semantic_diff", options, diff_samples);

    let ordinal_samples = measure_samples(options, || {
        black_box(ordinal_plan(&before_bytes));
        black_box(ordinal_plan(&after_bytes));
    });
    report_sample(scenario, "ordinal_1mib_plan_pair", options, ordinal_samples);

    let prefix_samples = measure_samples(options, || {
        black_box(stable_prefix_plan(&before_oracle));
        black_box(stable_prefix_plan(&after_oracle));
    });
    report_sample(
        scenario,
        "stable_prefix_8bit_plan_pair",
        options,
        prefix_samples,
    );

    if scenario == "noop" {
        assert_eq!(ordinal_delta.fetched_bytes, 0);
        assert_eq!(prefix_delta.fetched_bytes, 0);
        assert!(oracle_entity_changes.is_empty());
        assert!(oracle_link_changes.is_empty());
        assert!(oracle_occurrence_changes.is_empty());
    }
    if scenario.starts_with("attribute-") || scenario == "docs-only" || scenario == "rename-mid" {
        assert_eq!(oracle_entity_changes.len(), 1);
        assert_eq!(oracle_occurrence_changes.len(), 0);
    }
    if scenario != "noop" && after_bytes.len() > backend_semantic::ir::MAX_SEMANTIC_SEGMENT_BYTES {
        assert!(
            ordinal_delta.fetched_bytes > prefix_delta.fetched_bytes,
            "the ordinal byte-window fixture should expose more fetched bytes than stable prefixes in {scenario}"
        );
    }
    if scenario == "docs-only" {
        assert_eq!(before_bytes.len(), after_bytes.len());
    }
    if scenario == "insert-head" || scenario == "delete-head" {
        assert!(oracle_occurrence_changes.len() >= 2);
    }
    Ok(())
}

fn fixture_rows(count: usize, scenario: &str) -> (Vec<RowInput>, Vec<RowInput>) {
    let before = (0..count)
        .map(|logical_id| make_row(i64::try_from(logical_id).expect("fixture id fits i64")))
        .collect::<Vec<_>>();
    let mut after = before.clone();
    match scenario {
        "noop" => {}
        "attribute-early" => append_attribute(&mut after[0]),
        "attribute-mid" => append_attribute(&mut after[count / 2]),
        "attribute-tail" => append_attribute(&mut after[count - 1]),
        "docs-only" => {
            after[count / 2].documentation =
                after[count / 2].documentation.replace("before", "after!");
        }
        "rename-mid" => {
            after[count / 2].name = format!("renamed-symbol-{:08}", count / 2).into_bytes();
        }
        "insert-head" => after.insert(0, make_row(-1)),
        "delete-head" => {
            after.remove(0);
        }
        _ => panic!("unknown IR delta fixture scenario {scenario}"),
    }
    (before, after)
}

fn make_row(logical_id: i64) -> RowInput {
    let suffix = u64::try_from(logical_id).unwrap_or(u64::MAX);
    RowInput {
        logical_id,
        name: format!("symbol-{suffix:08}").into_bytes(),
        attribute: format!("attribute-{suffix:08}").into_bytes(),
        documentation: format!("documentation-{suffix:08}-before"),
        code: format!("code-sample-{suffix:08}-{}", "c".repeat(64)),
    }
}

fn append_attribute(row: &mut RowInput) {
    row.attribute
        .extend(std::iter::repeat_n(b'x', ATTRIBUTE_GROWTH));
}

fn source_position(logical_id: i64, lane: u32) -> u32 {
    if logical_id < 0 {
        3_000_000_000 + lane * 4
    } else {
        u32::try_from(logical_id).expect("fixture id fits u32") * 32 + lane * 4
    }
}

fn source_span(file: AtomId, start: u32) -> SourceSpan {
    SourceSpan::new(file, start, start + 2).expect("fixture source span is ordered")
}

fn complete_entity_authority() -> EntityAuthorityFacts {
    EntityAuthorityFacts {
        parentage: ParentageAuthority::Root,
        source: FactAvailability::Captured,
        source_file: FactAvailability::Captured,
        members: FactAvailability::Captured,
        semantic_type: FactAvailability::Captured,
        documentation: FactAvailability::Captured,
        visibility: FactAvailability::Captured,
        attributes: FactAvailability::Captured,
        language_extension: FactAvailability::Captured,
    }
}

fn build_ir(rows: &[RowInput]) -> Result<backend_semantic::ir::Ir, Box<dyn Error>> {
    let mut builder = IrBuilder::new();
    builder.set_language_profile(LanguageProfile::Rust(RustEdition::Rust2024))?;
    let integer_type =
        builder.intern_type(TypeExpr::Concrete(ConcreteType::Builtin(BuiltinType::I32)))?;
    let file = builder.intern_atom(b"src/lib.rs")?;
    let macro_atom = builder.intern_atom(b"fixture_macro!()")?;
    let empty_atoms = builder.intern_attributes(&[])?;
    let macros = builder.intern_attributes(&[macro_atom])?;
    let empty_parameters: TypeParameterListId = builder.intern_type_parameters(&[])?;
    let empty_predicates: FreePredicateListId = builder.intern_free_predicates(&[])?;
    let rust_facts = RustFacts {
        ownership: RustOwnership::SharedBorrow,
        lifetimes: empty_atoms,
        where_clauses: empty_parameters,
        macros,
        const_defaults: empty_atoms,
        free_predicates: empty_predicates,
    };

    let docs = rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            vec![
                DocInput::Text(&row.documentation),
                DocInput::SoftBreak,
                DocInput::Code(&row.code),
                DocInput::Link {
                    label: "self",
                    target: TreeLinkTarget::Local(TreeEntityId::new(
                        u32::try_from(index).expect("fixture coordinate fits u32"),
                    )),
                },
                DocInput::HardBreak,
            ]
        })
        .collect::<Vec<_>>();
    let attributes = rows
        .iter()
        .map(|row| vec![row.attribute.as_slice()])
        .collect::<Vec<_>>();
    let versions = rows.iter().map(entity_version).collect::<Vec<_>>();
    let items = rows
        .iter()
        .enumerate()
        .map(|(index, row)| TreeItemInput {
            name: &row.name,
            kind: ItemKind::Function,
            visibility: Visibility::Public,
            authority: complete_entity_authority(),
            parent: None,
            semantic_type: Some(integer_type),
            members: &[],
            docs: &docs[index],
            attributes: &attributes[index],
            source: Some(source_span(file, source_position(row.logical_id, 0))),
            extension: Some(LanguageExtensionInput::Rust(&rust_facts)),
        })
        .collect::<Vec<_>>();

    let mut links = Vec::with_capacity(rows.len() * 2);
    if !rows.is_empty() {
        for (index, row) in rows.iter().enumerate() {
            let target = (index + 1) % rows.len();
            for (lane, confidence) in [(1, Confidence::Syntactic), (2, Confidence::Compiler)] {
                links.push(TreeLinkInput {
                    from: TreeEntityId::new(
                        u32::try_from(index).expect("fixture coordinate fits u32"),
                    ),
                    target: TreeLinkTarget::Local(TreeEntityId::new(
                        u32::try_from(target).expect("fixture coordinate fits u32"),
                    )),
                    kind: LinkKind::TypeReference,
                    confidence,
                    authority: OccurrenceAuthorityFacts {
                        source: FactAvailability::Captured,
                    },
                    source: Some(source_span(file, source_position(row.logical_id, lane))),
                });
            }
        }
    }

    builder.add_borrowed_tree(backend_semantic::ir::BorrowedTree {
        versions: &versions,
        items: &items,
        links: &links,
    })?;
    Ok(builder.finish()?)
}

fn entity_version(row: &RowInput) -> EntityVersion {
    let family = DeclarationFamilyId::from_canonical_bytes(&row.logical_id.to_be_bytes());
    let variant = VariantFingerprint::from_canonical_bytes(b"ir-delta-fixture-singleton-v1");
    let mut core = Vec::with_capacity(row.name.len() + 16);
    core.extend_from_slice(&row.logical_id.to_be_bytes());
    core.extend_from_slice(&row.name);
    EntityVersion {
        family,
        variant,
        core_payload: CorePayloadHash::from_canonical_bytes(&core),
    }
}

fn encode_image(ir: &backend_semantic::ir::Ir) -> Result<Vec<u8>, Box<dyn Error>> {
    let length = full_semantic_image_len(ir)?;
    let mut bytes = vec![0; length];
    encode_full_semantic_image(ir, &mut bytes)?;
    Ok(bytes)
}

fn owned_oracle<Reader: SemanticReader + ?Sized>(reader: &Reader) -> OwnedOracle {
    let mut oracle = OwnedOracle::default();
    for entity in reader.canonical_entities() {
        let identity = entity.version.identity();
        let docs = reader
            .docs(entity.docs)
            .expect("captured documentation list resolves")
            .map(|fragment| match fragment {
                DocFragment::Text(text) => OwnedDoc::Text(
                    reader
                        .text(text)
                        .expect("text ID resolves")
                        .as_bytes()
                        .to_vec(),
                ),
                DocFragment::Code(code) => OwnedDoc::Code(
                    reader
                        .text(code)
                        .expect("code ID resolves")
                        .as_bytes()
                        .to_vec(),
                ),
                DocFragment::Link { label, target } => OwnedDoc::Link {
                    label: reader
                        .text(label)
                        .expect("link label resolves")
                        .as_bytes()
                        .to_vec(),
                    target: owned_doc_target(reader, target),
                },
                DocFragment::SoftBreak => OwnedDoc::SoftBreak,
                DocFragment::HardBreak => OwnedDoc::HardBreak,
            })
            .collect();
        let attributes = reader
            .atom_list(entity.attributes)
            .expect("attribute list resolves")
            .map(|atom| reader.atom(atom).expect("attribute atom resolves").to_vec())
            .collect();
        let members = reader
            .entity_list(entity.members)
            .expect("member list resolves")
            .map(|member| {
                reader
                    .entity(member)
                    .expect("member entity resolves")
                    .version
                    .identity()
            })
            .collect();
        let source = entity.source.map(|span| owned_source(reader, span));
        let rust = reader
            .rust_extension(entity.id)
            .map(|facts| OwnedRustFacts {
                ownership: format!("{:?}", facts.ownership),
                lifetimes: owned_atom_list(reader, facts.lifetimes),
                where_clauses: reader
                    .type_parameters(facts.where_clauses)
                    .expect("Rust type-parameter list resolves")
                    .map(|parameter| format!("{parameter:?}"))
                    .collect(),
                macros: owned_atom_list(reader, facts.macros),
                const_defaults: owned_atom_list(reader, facts.const_defaults),
                free_predicates: reader
                    .free_predicates(facts.free_predicates)
                    .expect("Rust free-predicate list resolves")
                    .map(|predicate| format!("{predicate:?}"))
                    .collect(),
            });
        let parent = entity.parent.map(|parent| {
            reader
                .entity(parent)
                .expect("parent entity resolves")
                .version
                .identity()
        });
        let semantic_type = entity
            .semantic_type
            .map(|ty| format!("{:?}", reader.ty(ty).expect("semantic type resolves")));
        let row = OwnedEntity {
            name: reader
                .atom(entity.name)
                .expect("entity name resolves")
                .to_vec(),
            kind: format!("{:?}", entity.kind),
            visibility: format!("{:?}", entity.visibility),
            parent,
            semantic_type,
            members,
            docs,
            attributes,
            source,
            authority: format!("{:?}", entity.authority),
            core_payload: entity.version.core_payload.as_bytes().to_vec(),
            rust,
        };
        assert!(oracle.entities.insert(identity, row).is_none());
    }

    for (_, link) in reader.canonical_links() {
        let key = stable_link_key(reader, link);
        let value = OwnedLink {
            confidence: format!("{:?}", link.confidence),
            source: link.source.map(|span| owned_source(reader, span)),
        };
        assert!(oracle.links.insert(key, value).is_none());
    }
    for (occurrence_id, occurrence) in reader.link_occurrences() {
        let relation = stable_link_key(
            reader,
            reader
                .link(occurrence.link)
                .expect("occurrence relation resolves"),
        );
        let key = OwnedOccurrence {
            relation,
            confidence: format!("{:?}", occurrence.confidence),
            source: occurrence.source.map(|span| owned_source(reader, span)),
            authority: format!(
                "{:?}",
                reader
                    .occurrence_authority(occurrence_id)
                    .expect("occurrence authority resolves")
            ),
        };
        *oracle.occurrences.entry(key).or_default() += 1;
    }
    oracle
}

fn owned_atom_list<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    list: AtomListId,
) -> Vec<Vec<u8>> {
    reader
        .atom_list(list)
        .expect("extension atom list resolves")
        .map(|atom| reader.atom(atom).expect("extension atom resolves").to_vec())
        .collect()
}

fn owned_source<Reader: SemanticReader + ?Sized>(reader: &Reader, span: SourceSpan) -> OwnedSource {
    OwnedSource {
        file: reader
            .atom(span.file())
            .expect("source file atom resolves")
            .to_vec(),
        start: span.start(),
        end: span.end(),
    }
}

fn owned_doc_target<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    target: LinkTarget,
) -> String {
    match target {
        LinkTarget::Local(entity) => format!(
            "local:{:?}",
            reader
                .entity(entity)
                .expect("documentation link target resolves")
                .version
                .identity()
        ),
        LinkTarget::External(external) => format!(
            "external:{:?}",
            reader.external(external).expect("external target resolves")
        ),
    }
}

fn stable_link_key<Reader: SemanticReader + ?Sized>(reader: &Reader, link: Link) -> StableLinkKey {
    let from = reader
        .entity(link.from)
        .expect("link source resolves")
        .version
        .identity();
    let target = match link.target {
        LinkTarget::Local(entity) => DeclarationLinkTarget::Local(
            reader
                .entity(entity)
                .expect("link target resolves")
                .version
                .identity(),
        ),
        LinkTarget::External(external) => match reader
            .external(external)
            .expect("external link target resolves")
        {
            ExternalTarget::Stable { target } => DeclarationLinkTarget::Stable(target),
            ExternalTarget::Foreign(target) => DeclarationLinkTarget::Foreign(target.identity),
            ExternalTarget::FragmentEntity { target, .. } => {
                DeclarationLinkTarget::FragmentEntity(target)
            }
        },
    };
    StableLinkKey {
        from,
        target,
        kind: link.kind,
    }
}

fn changed_map_keys<K: Copy + Ord, V: Eq>(
    before: &BTreeMap<K, V>,
    after: &BTreeMap<K, V>,
) -> BTreeSet<K> {
    before
        .keys()
        .chain(after.keys())
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|key| before.get(key) != after.get(key))
        .collect()
}

fn diff_change_keys<Before: SemanticReader + ?Sized, After: SemanticReader + ?Sized>(
    before: &Before,
    after: &After,
    before_generation: GenerationId,
    after_generation: GenerationId,
) -> (BTreeSet<DeclarationIdentity>, BTreeSet<StableLinkKey>) {
    let diff = SemanticDiff::between(
        SemanticSnapshot {
            generation: before_generation,
            reader: before,
        },
        SemanticSnapshot {
            generation: after_generation,
            reader: after,
        },
    );
    let mut entities = BTreeSet::new();
    for change in diff.entities {
        let identity = match change {
            SemanticEntityChange::Introduced { identity, .. }
            | SemanticEntityChange::Deleted { identity, .. }
            | SemanticEntityChange::Retained { identity, .. } => identity,
        };
        entities.insert(identity);
    }
    let mut links = BTreeSet::new();
    for change in diff.links {
        links.insert(change.key);
    }
    (entities, links)
}

fn consume_diff<Before: SemanticReader + ?Sized, After: SemanticReader + ?Sized>(
    before: &Before,
    after: &After,
    before_generation: GenerationId,
    after_generation: GenerationId,
) -> DiffCounts {
    let diff = SemanticDiff::between(
        SemanticSnapshot {
            generation: before_generation,
            reader: before,
        },
        SemanticSnapshot {
            generation: after_generation,
            reader: after,
        },
    );
    let changed_entities = diff.entities.count();
    let changed_links = diff.links.count();
    DiffCounts {
        changed_entities,
        changed_links,
    }
}

fn ordinal_plan(bytes: &[u8]) -> SegmentPlan {
    let mut plan = SegmentPlan::default();
    plan.generation_hash_bytes = bytes.len();
    let _generation = GenerationId::from_canonical_bytes(bytes);
    for (ordinal, payload) in bytes
        .chunks(backend_semantic::ir::MAX_SEMANTIC_SEGMENT_BYTES)
        .enumerate()
    {
        let mut key = [0_u8; 32];
        key[24..].copy_from_slice(
            &u64::try_from(ordinal)
                .expect("fixture ordinal fits u64")
                .to_be_bytes(),
        );
        plan.claims.push(SegmentClaim {
            key,
            digest: *blake3::hash(payload).as_bytes(),
            byte_length: payload.len(),
        });
        plan.planner_read_bytes += payload.len();
        plan.segment_hash_bytes += payload.len();
    }
    plan.planner_read_bytes += bytes.len();
    plan
}

fn stable_prefix_plan(oracle: &OwnedOracle) -> SegmentPlan {
    let mut records = Vec::<([u8; 32], Vec<u8>)>::new();
    for (identity, entity) in &oracle.entities {
        records.push((
            identity_bytes(*identity),
            format!("{entity:?}").into_bytes(),
        ));
    }
    for (key, link) in &oracle.links {
        let encoded_key = format!("{key:?}");
        records.push((
            record_key(b"ir-delta-link-v1\0", encoded_key.as_bytes()),
            format!("{link:?}").into_bytes(),
        ));
    }
    for (occurrence, count) in &oracle.occurrences {
        let encoded_key = format!("{occurrence:?}");
        records.push((
            record_key(b"ir-delta-occurrence-v1\0", encoded_key.as_bytes()),
            count.to_be_bytes().to_vec(),
        ));
    }
    stable_prefix_plan_records(
        records
            .iter()
            .map(|(key, payload)| (*key, payload.as_slice())),
    )
}

fn identity_bytes(identity: DeclarationIdentity) -> [u8; 32] {
    let mut key = [0_u8; 32];
    key[..16].copy_from_slice(identity.family.as_bytes());
    key[16..].copy_from_slice(identity.variant.as_bytes());
    key
}

fn record_key(domain: &[u8], bytes: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(domain);
    hasher.update(bytes);
    *hasher.finalize().as_bytes()
}

/// Generic row adapter boundary: a future stable-SPIR implementation can feed
/// stable keys and borrowed canonical row bytes here without touching the
/// fixture's owned oracle or depending on a producer API that is not landed.
fn stable_prefix_plan_records<'row>(
    records: impl IntoIterator<Item = ([u8; 32], &'row [u8])>,
) -> SegmentPlan {
    let mut groups: BTreeMap<u8, Vec<([u8; 32], &'row [u8])>> = BTreeMap::new();
    for (key, payload) in records {
        groups.entry(key[0]).or_default().push((key, payload));
    }
    let mut plan = SegmentPlan::default();
    for (prefix, mut rows) in groups {
        rows.sort_by_key(|(key, _)| *key);
        let mut payload = Vec::new();
        for (key, row) in rows {
            payload.extend_from_slice(&key);
            payload.extend_from_slice(
                &u64::try_from(row.len())
                    .expect("fixture row length fits u64")
                    .to_be_bytes(),
            );
            payload.extend_from_slice(row);
        }
        let mut segment_key = [0_u8; 32];
        segment_key[0] = 0xf0;
        segment_key[1] = prefix;
        plan.claims.push(SegmentClaim {
            key: segment_key,
            digest: *blake3::hash(&payload).as_bytes(),
            byte_length: payload.len(),
        });
        plan.planner_read_bytes += payload.len();
        plan.segment_hash_bytes += payload.len();
    }
    plan
}

fn segment_delta(before: &SegmentPlan, after: &SegmentPlan) -> SegmentDelta {
    let before_by_key = before
        .claims
        .iter()
        .map(|claim| (claim.key, *claim))
        .collect::<BTreeMap<_, _>>();
    let after_by_key = after
        .claims
        .iter()
        .map(|claim| (claim.key, *claim))
        .collect::<BTreeMap<_, _>>();
    let mut result = SegmentDelta::default();
    for (key, target) in &after_by_key {
        match before_by_key.get(key) {
            Some(previous) if previous == target => {
                result.reused_segments += 1;
                result.reused_bytes += target.byte_length;
            }
            previous => {
                result.fetched_segments += 1;
                result.fetched_bytes += target.byte_length;
                if let Some(previous) = previous {
                    result.removed_segments += 1;
                    result.removed_bytes += previous.byte_length;
                }
            }
        }
    }
    for (key, previous) in &before_by_key {
        if !after_by_key.contains_key(key) {
            result.removed_segments += 1;
            result.removed_bytes += previous.byte_length;
        }
    }
    result.changed_segments = result.fetched_segments + result.removed_segments;
    result
}

fn measure_samples(options: Options, mut operation: impl FnMut()) -> SampleSummary {
    for _ in 0..options.warmups {
        operation();
    }
    let mut timings = Vec::with_capacity(options.samples);
    let mut allocation_counts = Vec::with_capacity(options.samples);
    let mut allocated_bytes = Vec::with_capacity(options.samples);
    let mut max_live_bytes = Vec::with_capacity(options.samples);
    for _ in 0..options.samples {
        let mut elapsed_ns = 0_u128;
        let allocation: AllocationInfo = measure(|| {
            let start = Instant::now();
            for _ in 0..options.repeats {
                operation();
            }
            elapsed_ns = start.elapsed().as_nanos()
                / u128::try_from(options.repeats).expect("repeat count fits u128");
        });
        timings.push(elapsed_ns);
        allocation_counts.push(allocation.count_total);
        allocated_bytes.push(allocation.bytes_total);
        max_live_bytes.push(allocation.bytes_max);
    }
    timings.sort_unstable();
    allocation_counts.sort_unstable();
    allocated_bytes.sort_unstable();
    max_live_bytes.sort_unstable();
    let samples = options.samples;
    let p50_index = samples / 2;
    let p95_index = ((samples * 95).div_ceil(100)).saturating_sub(1);
    SampleSummary {
        p50_ns: timings[p50_index],
        p95_ns: timings[p95_index],
        allocations_per_op: allocation_counts[samples / 2]
            / u64::try_from(options.repeats).expect("repeat count fits u64"),
        allocated_bytes_per_op: allocated_bytes[samples / 2]
            / u64::try_from(options.repeats).expect("repeat count fits u64"),
        max_live_bytes: max_live_bytes[p95_index],
    }
}

fn report_sample(scenario: &str, operation: &str, options: Options, sample: SampleSummary) {
    println!(
        "timing\tcase={}\top={}\tsamples={}\twarmups={}\trepeats={}\tp50_ns={}\tp95_ns={}\tallocations_per_op_median={}\tallocated_bytes_per_op_median={}\tmax_live_bytes_p95_sample={}",
        scenario,
        operation,
        options.samples,
        options.warmups,
        options.repeats,
        sample.p50_ns,
        sample.p95_ns,
        sample.allocations_per_op,
        sample.allocated_bytes_per_op,
        sample.max_live_bytes,
    );
}

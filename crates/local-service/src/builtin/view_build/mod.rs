//! Typed source-to-document row projection.
//!
//! This module is the enforcement point of the two-lane answer contract
//! (`super::lanes`): published semantic images are projected first, files
//! their profile covers keep no structural rows, files without complete
//! coverage fall back to the tree-sitter baseline, and a stale publication
//! answers semantic and typed stale rather than being silently replaced.

mod call_join;
#[cfg(test)]
mod go_field_join;
#[cfg(test)]
mod go_type_mention_join;
mod identity;
mod query;
mod semantic;
mod structural;

pub(crate) use call_join::{
    ProjectCallableIndex, foreign_display_name, foreign_namespace_call_retarget,
    foreign_namespace_field_retarget, foreign_package_call_retarget,
    foreign_package_field_retarget, foreign_package_mention_retarget, join_project_call,
    join_project_field, join_project_mention, join_project_value, project_paths_for_package,
};

pub(crate) use identity::query_semantic_id;
pub(super) use identity::{external_semantic_symbol, package_token, semantic_symbol};
pub(crate) use identity::semantic_coordinate;
pub(super) use query::semantic_query_corpus;
pub(super) use semantic::{
    ForeignPublication, ProjectedRows, StructuralSites, rows_for_indexed_sources,
};
pub(crate) use semantic::compiled_source_path;
pub(crate) use structural::{
    resolve_specifier_paths, structural_call_coordinate_pairs, structural_call_graph_relations,
    structural_call_graph_relations_mapped, structural_call_span, structural_reference_facts,
    structural_symbol_identity, view_row_for_structural_coordinate,
};

use query::append_structural_query_facts;
use semantic::{ProfileStalePaths, SourceRowProjection};
use structural::{StructuralParent, StructuralProjectionPlan};

/// Builds the structural declaration plan and drops it. Benchmarks time this.
pub(super) fn project_structural_plan(
    sources: &super::IndexedSources,
) -> Result<(), super::BuiltinModelError> {
    StructuralProjectionPlan::of(sources, &std::collections::BTreeSet::new()).map(|_| ())
}

/// Plans structural rows for `only` these files and drops the plan.
///
/// The type index still walks every file in `sources`.
pub(super) fn project_structural_files(
    sources: &super::IndexedSources,
    only: &std::collections::BTreeSet<[u8; 32]>,
) -> Result<(), super::BuiltinModelError> {
    StructuralProjectionPlan::of_files(sources, &std::collections::BTreeSet::new(), only).map(|_| ())
}

/// Projects structural rows for `only` these files.
///
/// A parent coordinate planned in this call wins. Otherwise `resident_labels`
/// supplies the symbol already published for that coordinate.
pub(super) fn rows_for_structural_files(
    initial: &backend_engine::ViewRoot,
    sources: &super::IndexedSources,
    only: &std::collections::BTreeSet<[u8; 32]>,
    resident_labels: &std::collections::BTreeMap<String, backend_engine::SymbolKey>,
) -> Result<Vec<backend_engine::Row>, super::BuiltinModelError> {
    let plan =
        StructuralProjectionPlan::of_files(sources, &std::collections::BTreeSet::new(), only)?;
    semantic::rows_for_changed_structural_files(initial, sources, &plan, resident_labels)
}

const MAX_SEMANTIC_TYPE_DEPTH: usize = 256;
const MAX_SEMANTIC_SIGNATURE_BYTES: usize = 16 * 1024;
const MAX_SEMANTIC_DOCUMENT_BYTES: usize = 256 * 1024;
const MAX_SEMANTIC_QUERY_ROWS: usize = 65_536;

/// Document note appended to every row projected from a stale semantic image.
const STALE_NOTE: &str =
    "stale semantic image: compiled from an earlier source snapshot; re-index to refresh";

type DeclarationOccurrenceKey = (String, String, String);

use super::ProjectionLedger;
use identity::declaration_symbol;
use semantic::{
    SemanticRowSink, SemanticTargets, append_image_rows, compiled_source, freshness_decision,
};
#[cfg(test)]
use structural::structural_excerpt_calls;
use structural::structural_parent_rank;

fn semantic_profile_is_complete(
    complete: &std::collections::BTreeSet<(
        [u8; 32],
        backend_semantic::vocabulary::LanguageProfile,
    )>,
    package: Option<backend_engine::PackageKey>,
    path: &str,
) -> Result<bool, super::BuiltinModelError> {
    let Some(package) = package else {
        return Ok(false);
    };
    // A file whose extension has no semantic profile is never covered by a
    // completed compile, so it keeps its structural facts.
    let Some(profile) = super::ingest::source_profile(std::path::Path::new(path))
        .map_err(super::BuiltinModelError)?
    else {
        return Ok(false);
    };
    Ok(complete.contains(&(package.to_bytes(), profile)))
}

#[cfg(test)]
mod csharp_field_namespace_join;

#[cfg(test)]
mod tests {
    use super::super::initial_view;
    use super::super::{FileLane, IndexedProject, SemanticFreshness, StructuralCause};
    use super::ProfileStalePaths;
    use super::{
        STALE_NOTE, SourceRowProjection, StructuralParent, StructuralProjectionPlan,
        append_structural_query_facts, semantic_profile_is_complete, structural_call_graph_relations,
        structural_excerpt_calls,
    };
    use backend_engine::{
        Row, RowId, ViewRoot, package_key, product_source_file_key, symbol_key,
    };
    use backend_semantic::ir::{
        BorrowedTree, CorePayloadHash, DeclarationFamilyId, DeclarationIdentity,
        EntityAuthorityFacts, EntityVersion, FactAvailability, IrBuilder, ItemKind,
        ParentageAuthority, SemanticImageView, SourceIdentity, TreeEntityId, TreeItemInput,
        VariantFingerprint, Visibility, encode_full_semantic_image, full_semantic_image_len,
    };
    use backend_semantic::vocabulary::{
        CStandard, CompileRecipeFact, LanguageProfile, NativeTool, PackageUrl, RustEdition, Stage,
    };
    use backend_version::{ContentId, SourceFactDomain, ToolchainDomain};
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;

    const FIXTURE_PATH: &str = "src/worker.rs";

    const FIXTURE_SOURCE: &str = r"
pub struct Worker {
    pub name: String,
}

pub enum Event {
    Started,
}

impl Worker {
    pub fn run(&self) {}
}

pub fn execute() {}
";

    fn fixture_version(identity: u8) -> EntityVersion {
        EntityVersion {
            family: DeclarationFamilyId::from_raw([identity; 16]),
            variant: VariantFingerprint::from_raw([identity; 16]),
            core_payload: CorePayloadHash::from_raw([identity; 16]),
        }
    }

    fn fixture_authority(parentage: ParentageAuthority) -> EntityAuthorityFacts {
        EntityAuthorityFacts {
            parentage,
            visibility: FactAvailability::Captured,
            ..EntityAuthorityFacts::default()
        }
    }

    /// The exact declared identity `fixture_version(identity)` carries.
    fn fixture_identity(identity: u8) -> ParentageAuthority {
        ParentageAuthority::Bound(DeclarationIdentity {
            family: DeclarationFamilyId::from_raw([identity; 16]),
            variant: VariantFingerprint::from_raw([identity; 16]),
        })
    }

    /// Encodes one Rust semantic image whose IR contains a struct field and
    /// an enum variant — the members the TAGS baseline cannot see.
    fn fixture_semantic_image(path: &str) -> Result<Vec<u8>, String> {
        let source = SourceIdentity {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b"fixture-source"),
            byte_len: 14,
        };
        let recipe = CompileRecipeFact::derive(
            LanguageProfile::Rust(RustEdition::Rust2024),
            Stage::LowerIr,
            NativeTool::Rustc,
            source.identity,
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"fixture-toolchain"),
        );
        let coordinate = PackageUrl::parse("pkg:cargo/fixture@1.0.0".to_owned())
            .map_err(|error| format!("fixture coordinate: {error:?}"))?;
        let mut builder = IrBuilder::new();
        builder
            .set_image_provenance_for_package(source, recipe, &coordinate, path)
            .map_err(|error| error.to_string())?;
        let struct_id = TreeEntityId::new(0);
        let enum_id = TreeEntityId::new(2);
        let items = [
            TreeItemInput {
                name: b"Worker",
                kind: ItemKind::Record,
                visibility: Visibility::Public,
                authority: fixture_authority(ParentageAuthority::Root),
                parent: None,
                semantic_type: None,
                members: &[],
                docs: &[],
                attributes: &[],
                source: None,
                extension: None,
            },
            TreeItemInput {
                name: b"name",
                kind: ItemKind::Field,
                visibility: Visibility::Public,
                authority: fixture_authority(fixture_identity(1)),
                parent: Some(struct_id),
                semantic_type: None,
                members: &[],
                docs: &[],
                attributes: &[],
                source: None,
                extension: None,
            },
            TreeItemInput {
                name: b"Event",
                kind: ItemKind::Enum,
                visibility: Visibility::Public,
                authority: fixture_authority(ParentageAuthority::Root),
                parent: None,
                semantic_type: None,
                members: &[],
                docs: &[],
                attributes: &[],
                source: None,
                extension: None,
            },
            TreeItemInput {
                name: b"Started",
                kind: ItemKind::Variant,
                visibility: Visibility::Public,
                authority: fixture_authority(fixture_identity(3)),
                parent: Some(enum_id),
                semantic_type: None,
                members: &[],
                docs: &[],
                attributes: &[],
                source: None,
                extension: None,
            },
        ];
        builder
            .add_borrowed_tree(BorrowedTree {
                versions: &[
                    fixture_version(1),
                    fixture_version(2),
                    fixture_version(3),
                    fixture_version(4),
                ],
                items: &items,
                links: &[],
            })
            .map_err(|error| error.to_string())?;
        let ir = builder.finish().map_err(|error| error.to_string())?;
        let mut bytes = vec![0; full_semantic_image_len(&ir).map_err(|error| error.to_string())?];
        encode_full_semantic_image(&ir, &mut bytes).map_err(|error| error.to_string())?;
        Ok(bytes)
    }

    /// Projects the fixture through the real row projection.
    ///
    /// The declarations come from the real Rust frontend rather than from
    /// hand-written containment, so the test fails if extraction and
    /// projection ever disagree about what a parent is addressed by.
    fn projected_rows() -> Result<(Vec<Row>, super::ProjectionLedger), String> {
        let frontend = backend_frontend_rust::syntax_frontend().map_err(|e| e.to_string())?;
        let analysis = frontend
            .analyze(
                std::path::Path::new(FIXTURE_PATH),
                FIXTURE_SOURCE.as_bytes(),
            )
            .map_err(|e| e.to_string())?;
        let project_key = [7u8; 32];
        let file_key = product_source_file_key(project_key, FIXTURE_PATH);
        let record = super::super::ProductSourceRecord::file(
            project_key,
            FIXTURE_PATH,
            backend_engine::SourceLanguage::Rust,
            [1; 32],
            [2; 32],
            analysis.declarations().clone(),
        )?;
        let sources = super::super::IndexedSources {
            projects: BTreeMap::from([(
                project_key,
                IndexedProject {
                    package: package_key("fixture"),
                    label: "fixture".to_owned(),
                    files: Arc::from([file_key]),
                },
            )]),
            files: vec![(file_key, record)],
        };
        let (initial, _) = initial_view().map_err(|e| e.to_string())?;
        let structural_plan =
            StructuralProjectionPlan::of(&sources, &BTreeSet::new()).map_err(|e| e.to_string())?;
        let targets = super::SemanticTargets::default();
        let mut projection =
            SourceRowProjection::new(
                &initial,
                &sources.projects,
                64,
                &targets,
                &structural_plan,
                std::path::Path::new("/tmp"),
            )
                .map_err(|e| e.to_string())?;
        for (key, file) in &sources.files {
            projection
                .append_file(*key, file, &BTreeSet::new(), &ProfileStalePaths::new())
                .map_err(|e| e.to_string())?;
        }
        let ledger = projection.take_ledger();
        projection
            .finish(Vec::new())
            .map(|rows| (rows, ledger))
            .map_err(|e| e.to_string())
    }

    /// Projects the fixture image through the real semantic row sink, with
    /// the requested staleness, and returns the projected rows.
    fn projected_semantic_rows(stale: bool) -> Result<Vec<Row>, String> {
        let bytes = fixture_semantic_image(FIXTURE_PATH)?;
        let view = SemanticImageView::reopen(&bytes).map_err(|error| error.to_string())?;
        let project = IndexedProject {
            package: package_key("fixture"),
            label: "fixture".to_owned(),
            files: Arc::<[[u8; 32]]>::from([]),
        };
        let (initial, _) = initial_view().map_err(|e| e.to_string())?;
        let mut symbols = BTreeSet::new();
        let mut rows = Vec::new();
        let mut remaining = usize::MAX;
        let mut sink = super::SemanticRowSink {
            initial: &initial,
            symbols: &mut symbols,
            rows: &mut rows,
            capacity: 64,
            remaining_bytes: &mut remaining,
            stale,
            path: FIXTURE_PATH,
            site_declarations: &[],
        };
        super::append_image_rows(
            &view,
            &project,
            LanguageProfile::Rust(RustEdition::Rust2024),
            &mut sink,
        )
        .map_err(|error| error.to_string())?;
        Ok(rows)
    }

    /// Finds one row whose coordinate ends with the given name.
    fn row_named<'a>(rows: &'a [Row], name: &str) -> Result<&'a Row, String> {
        rows.iter()
            .find(|row| row.label.ends_with(&format!("::{name}")))
            .ok_or_else(|| {
                let rendered = rows
                    .iter()
                    .map(|row| row.label.clone())
                    .collect::<Vec<_>>()
                    .join("\n  ");
                format!("no row named {name}\nprojected:\n  {rendered}")
            })
    }

    fn structural_sources(
        project_key: [u8; 32],
        path: &str,
        language: backend_engine::SourceLanguage,
        declarations: Arc<[backend_compile::SourceDeclaration]>,
    ) -> Result<super::super::IndexedSources, String> {
        let file_key = product_source_file_key(project_key, path);
        let record = super::super::ProductSourceRecord::file(
            project_key,
            path,
            language,
            [1; 32],
            [2; 32],
            declarations,
        )?;
        Ok(super::super::IndexedSources {
            projects: BTreeMap::from([(
                project_key,
                IndexedProject {
                    package: package_key("fixture"),
                    label: "fixture".to_owned(),
                    files: Arc::from([file_key]),
                },
            )]),
            files: vec![(file_key, record)],
        })
    }

    #[test]
    fn duplicate_structural_coordinates_keep_each_identity_and_close_parent_edges()
    -> Result<(), String> {
        let project_key = [8; 32];
        let declarations = vec![
            backend_compile::SourceDeclaration::at_path(
                "src/main.c",
                "main",
                "module",
                1,
                "C module main",
                "",
            )?,
            backend_compile::SourceDeclaration::at_path(
                "src/main.c",
                "Beacon",
                "struct",
                2,
                "struct Beacon",
                "",
            )?,
            backend_compile::SourceDeclaration::at_path(
                "src/main.c",
                "Beacon",
                "type",
                2,
                "typedef Beacon",
                "",
            )?,
            backend_compile::SourceDeclaration::at_path(
                "src/main.c",
                "intensity",
                "field",
                2,
                "unsigned intensity",
                "",
            )?
            .with_container(backend_compile::Container::enclosing(
                "Beacon",
                std::num::NonZeroU32::new(2).expect("nonzero fixture line"),
            )),
        ];
        let reversed_declarations = declarations.iter().cloned().rev().collect::<Vec<_>>();
        let sources = structural_sources(
            project_key,
            "src/main.c",
            backend_engine::SourceLanguage::Clang,
            Arc::from(declarations),
        )?;
        let reversed_sources = structural_sources(
            project_key,
            "src/main.c",
            backend_engine::SourceLanguage::Clang,
            Arc::from(reversed_declarations),
        )?;
        let plan = StructuralProjectionPlan::of(&sources, &BTreeSet::new())
            .map_err(|error| error.to_string())?;
        let reversed_plan = StructuralProjectionPlan::of(&reversed_sources, &BTreeSet::new())
            .map_err(|error| error.to_string())?;
        let coordinate = "fixture::src/main.c:2::Beacon";
        let symbols = plan
            .by_coordinate
            .get(&(project_key, coordinate.to_owned()))
            .ok_or("duplicate coordinate was not indexed")?;
        let reversed_symbols = reversed_plan
            .by_coordinate
            .get(&(project_key, coordinate.to_owned()))
            .ok_or("reversed duplicate coordinate was not indexed")?;
        if symbols != reversed_symbols {
            return Err(format!(
                "declaration order changed duplicate identities: {symbols:?} vs {reversed_symbols:?}"
            ));
        }
        if symbols.len() != 2 || symbols[0].id == symbols[1].id {
            return Err(format!("duplicate structural identities are {symbols:?}"));
        }
        let file_plan = plan
            .file(product_source_file_key(project_key, "src/main.c"))
            .ok_or("C structural file plan was not retained")?;
        for declaration in file_plan
            .declarations
            .iter()
            .filter(|declaration| declaration.coordinate == coordinate)
        {
            let preimage = declaration
                .identity_preimage
                .as_ref()
                .ok_or("duplicate C row lost its identity preimage")?;
            if declaration.id != RowId::Symbol(symbol_key(preimage.as_str())) {
                return Err(format!("C row identity did not admit {preimage:?}"));
            }
        }
        let parent = plan
            .parent_id(
                product_source_file_key(project_key, "src/main.c"),
                coordinate,
            )
            .map_err(|error| error.to_string())?;
        let StructuralParent::Symbol(parent) = parent else {
            return Err(format!("parent fell back to package: {parent:?}"));
        };
        if !symbols.iter().any(|symbol| symbol.id == parent)
            || super::structural_parent_rank(symbols[0].kind) != 0
        {
            return Err(format!(
                "parent {parent:?} did not resolve into {symbols:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn frontend_duplicate_coordinates_are_retained_without_a_dangling_target() -> Result<(), String>
    {
        let frontend = backend_frontend_csharp::syntax_frontend().map_err(|e| e.to_string())?;
        let analysis = frontend
            .analyze(std::path::Path::new("src/A.cs"), b"namespace A { }\n")
            .map_err(|e| e.to_string())?;
        let project_key = [9; 32];
        let sources = structural_sources(
            project_key,
            "src/A.cs",
            backend_engine::SourceLanguage::CSharp,
            analysis.declarations().clone(),
        )?;
        let plan = StructuralProjectionPlan::of(&sources, &BTreeSet::new())
            .map_err(|error| error.to_string())?;
        let duplicate = plan
            .by_coordinate
            .values()
            .find(|symbols| symbols.len() > 1)
            .ok_or("C# namespace fixture did not retain duplicate captures")?;
        if duplicate.windows(2).any(|pair| pair[0].id == pair[1].id) {
            return Err(format!("C# duplicate identities collided: {duplicate:?}"));
        }
        let file_plan = plan
            .file(product_source_file_key(project_key, "src/A.cs"))
            .ok_or("C# structural file plan was not retained")?;
        for declaration in file_plan
            .declarations
            .iter()
            .filter(|declaration| declaration.coordinate == "fixture::src/A.cs")
        {
            let preimage = declaration
                .identity_preimage
                .as_ref()
                .ok_or("C# duplicate namespace row lost its identity preimage")?;
            if declaration.id != RowId::Symbol(symbol_key(preimage.as_str())) {
                return Err(format!("C# row identity did not admit {preimage:?}"));
            }
        }
        Ok(())
    }

    #[test]
    fn rust_impl_trait_duplicate_method_kinds_keep_preimages_and_parentage() -> Result<(), String> {
        let project_key = [10; 32];
        let declarations = vec![
            backend_compile::SourceDeclaration::at_path(
                "src/lib.rs",
                "lib",
                "module",
                1,
                "Rust module lib",
                "",
            )?,
            backend_compile::SourceDeclaration::at_path(
                "src/lib.rs",
                "Worker",
                "struct",
                3,
                "struct Worker",
                "",
            )?,
            backend_compile::SourceDeclaration::at_path(
                "src/lib.rs",
                "run",
                "method",
                4,
                "fn run(&self)",
                "",
            )?
            .with_container(backend_compile::Container::enclosing(
                "Worker",
                std::num::NonZeroU32::new(3).expect("nonzero fixture line"),
            )),
            backend_compile::SourceDeclaration::at_path(
                "src/lib.rs",
                "run",
                "function",
                4,
                "fn run(&self)",
                "",
            )?
            .with_container(backend_compile::Container::enclosing(
                "Worker",
                std::num::NonZeroU32::new(3).expect("nonzero fixture line"),
            )),
        ];
        let reversed_declarations = declarations.iter().cloned().rev().collect::<Vec<_>>();
        let sources = structural_sources(
            project_key,
            "src/lib.rs",
            backend_engine::SourceLanguage::Rust,
            Arc::from(declarations),
        )?;
        let reversed_sources = structural_sources(
            project_key,
            "src/lib.rs",
            backend_engine::SourceLanguage::Rust,
            Arc::from(reversed_declarations),
        )?;
        let plan = StructuralProjectionPlan::of(&sources, &BTreeSet::new())
            .map_err(|error| error.to_string())?;
        let reversed_plan = StructuralProjectionPlan::of(&reversed_sources, &BTreeSet::new())
            .map_err(|error| error.to_string())?;
        let coordinate = "fixture::src/lib.rs:4::run";
        let symbols = plan
            .by_coordinate
            .get(&(project_key, coordinate.to_owned()))
            .ok_or("Rust duplicate method coordinate was not indexed")?;
        let reversed_symbols = reversed_plan
            .by_coordinate
            .get(&(project_key, coordinate.to_owned()))
            .ok_or("reversed Rust duplicate coordinate was not indexed")?;
        if symbols != reversed_symbols {
            return Err(format!(
                "Rust declaration order changed duplicate identities: {symbols:?} vs {reversed_symbols:?}"
            ));
        }
        if symbols.len() != 2 || symbols[0].id == symbols[1].id {
            return Err(format!("Rust duplicate identities are {symbols:?}"));
        }
        let file_plan = plan
            .file(product_source_file_key(project_key, "src/lib.rs"))
            .ok_or("Rust structural file plan was not retained")?;
        for declaration in file_plan
            .declarations
            .iter()
            .filter(|declaration| declaration.coordinate == coordinate)
        {
            let preimage = declaration
                .identity_preimage
                .as_ref()
                .ok_or("duplicate Rust row lost its identity preimage")?;
            if declaration.id != RowId::Symbol(symbol_key(preimage.as_str())) {
                return Err(format!("Rust row identity did not admit {preimage:?}"));
            }
        }
        let parent = plan
            .parent_id(
                product_source_file_key(project_key, "src/lib.rs"),
                "fixture::src/lib.rs:3::Worker",
            )
            .map_err(|error| error.to_string())?;
        if !matches!(parent, StructuralParent::Symbol(RowId::Symbol(_))) {
            return Err(format!("Rust parent is not a symbol: {parent:?}"));
        }
        Ok(())
    }

    #[test]
    fn cross_profile_attached_parent_falls_back_to_an_emitted_module() -> Result<(), String> {
        let project_key = [11; 32];
        let package = package_key("mixed");
        let child_path = "src/main.c";
        let semantic_path = "src/lib.rs";
        let child_key = product_source_file_key(project_key, child_path);
        let semantic_key = product_source_file_key(project_key, semantic_path);
        let child_declarations: Arc<[backend_compile::SourceDeclaration]> = Arc::from(vec![
            backend_compile::SourceDeclaration::at_path(
                child_path,
                "main",
                "module",
                1,
                "C module main",
                "",
            )?,
            backend_compile::SourceDeclaration::at_path(
                child_path,
                "run",
                "method",
                3,
                "unsigned run(Worker)",
                "",
            )?
            .with_container(backend_compile::Container::attached("Worker")),
        ]);
        let semantic_declarations: Arc<[backend_compile::SourceDeclaration]> = Arc::from(vec![
            backend_compile::SourceDeclaration::at_path(
                semantic_path,
                "lib",
                "module",
                1,
                "Rust module lib",
                "",
            )?,
            backend_compile::SourceDeclaration::at_path(
                semantic_path,
                "Worker",
                "struct",
                2,
                "struct Worker",
                "",
            )?,
        ]);
        let child_record = super::super::ProductSourceRecord::file(
            project_key,
            child_path,
            backend_engine::SourceLanguage::Clang,
            [1; 32],
            [2; 32],
            child_declarations,
        )?;
        let semantic_record = super::super::ProductSourceRecord::file(
            project_key,
            semantic_path,
            backend_engine::SourceLanguage::Rust,
            [3; 32],
            [4; 32],
            semantic_declarations,
        )?;
        let mut project_files = vec![child_key, semantic_key];
        project_files.sort_unstable();
        let sources = super::super::IndexedSources {
            projects: BTreeMap::from([(
                project_key,
                IndexedProject {
                    package,
                    label: "mixed".to_owned(),
                    files: Arc::from(project_files.into_boxed_slice()),
                },
            )]),
            files: vec![(child_key, child_record), (semantic_key, semantic_record)],
        };
        let complete = BTreeSet::from([(
            package.to_bytes(),
            LanguageProfile::Rust(RustEdition::Rust2024),
        )]);
        let plan =
            StructuralProjectionPlan::of(&sources, &complete).map_err(|error| error.to_string())?;
        if plan.file(child_key).is_none() || plan.file(semantic_key).is_some() {
            return Err("lane decisions retained the wrong structural files".to_owned());
        }
        let child_plan = plan
            .file(child_key)
            .ok_or("missing emitting child structural plan")?;
        let module = child_plan
            .declarations
            .iter()
            .find(|declaration| declaration.is_file_module)
            .ok_or("emitting child has no file module")?;
        let target_coordinate = "mixed::src/lib.rs:2::Worker";
        let fallback = plan
            .parent_id(child_key, target_coordinate)
            .map_err(|error| error.to_string())?;
        if fallback != StructuralParent::Symbol(module.id) {
            return Err(
                "suppressed cross-file target did not fall back to child module".to_owned(),
            );
        }

        let (initial, _) = initial_view().map_err(|e| e.to_string())?;
        let targets = super::SemanticTargets::default();
        let mut projection =
            SourceRowProjection::new(
                &initial,
                &sources.projects,
                64,
                &targets,
                &plan,
                std::path::Path::new("/tmp"),
            )
                .map_err(|e| e.to_string())?;
        for (key, file) in &sources.files {
            projection
                .append_file(*key, file, &complete, &ProfileStalePaths::new())
                .map_err(|e| e.to_string())?;
        }
        let rows = projection.finish(Vec::new()).map_err(|e| e.to_string())?;
        let row_ids = rows.iter().map(|row| row.id).collect::<BTreeSet<_>>();
        for row in &rows {
            if let Some(parent) = row.parent
                && !row_ids.contains(&RowId::Symbol(parent))
            {
                return Err(format!("row {} has a missing parent", row.label));
            }
        }
        let run = rows
            .iter()
            .find(|row| row.label == "mixed::src/main.c:3::run")
            .ok_or("cross-profile attached declaration was not emitted")?;
        let RowId::Symbol(module_id) = module.id else {
            return Err("emitted module is not a symbol".to_owned());
        };
        if run.parent != Some(module_id) {
            return Err(format!(
                "cross-profile parent was {:?}, expected {module:?}",
                run.parent
            ));
        }

        let mut facts = Vec::new();
        append_structural_query_facts(&sources, &plan, &mut facts)
            .map_err(|error| error.to_string())?;
        let fact_ids = facts
            .iter()
            .map(|fact| fact.presentation().id.as_str())
            .collect::<BTreeSet<_>>();
        for fact in &facts {
            if let Some(parent) = fact.presentation().parent.as_deref()
                && !fact_ids.contains(parent)
            {
                return Err(format!(
                    "fact {} has a missing parent",
                    fact.presentation().id
                ));
            }
        }
        Ok(())
    }

    #[test]
    fn repeated_tag_coordinates_keep_distinct_checked_identity_preimages() {
        let coordinate = "fixture::src/lib.rs:1::run";
        let mut occurrences = BTreeMap::new();
        let (first, first_preimage) = super::declaration_symbol(
            coordinate,
            backend_engine::DeclarationKind::Method,
            "fn run()",
            true,
            &mut occurrences,
        );
        let (second, second_preimage) = super::declaration_symbol(
            coordinate,
            backend_engine::DeclarationKind::Function,
            "fn run()",
            true,
            &mut occurrences,
        );
        let first_preimage = first_preimage.expect("overlapping tag gets a preimage");
        assert_eq!(first, RowId::Symbol(symbol_key(&first_preimage)));
        let second_preimage = second_preimage.expect("overlapping tag gets a preimage");
        assert_eq!(second, RowId::Symbol(symbol_key(&second_preimage)));
        assert_ne!(first, second);

        let mut reverse = BTreeMap::new();
        let (_, reverse_function) = super::declaration_symbol(
            coordinate,
            backend_engine::DeclarationKind::Function,
            "fn run()",
            true,
            &mut reverse,
        );
        let (_, reverse_method) = super::declaration_symbol(
            coordinate,
            backend_engine::DeclarationKind::Method,
            "fn run()",
            true,
            &mut reverse,
        );
        assert_eq!(
            second_preimage,
            reverse_function.expect("function identity is order independent")
        );
        assert_eq!(
            first_preimage,
            reverse_method.expect("method identity is order independent")
        );

        let mut unique = BTreeMap::new();
        let (unique_id, unique_preimage) = super::declaration_symbol(
            "fixture::src/index.ts:4::run",
            backend_engine::DeclarationKind::Function,
            "function run(): void",
            false,
            &mut unique,
        );
        assert_eq!(
            unique_id,
            RowId::Symbol(symbol_key("fixture::src/index.ts:4::run"))
        );
        assert!(
            unique_preimage.is_none(),
            "unique TypeScript rows stay compact"
        );
    }

    /// Renders one row's parent as the coordinate it points at, or `-`.
    fn parent_of(rows: &[Row], coordinate: &str) -> Result<String, String> {
        let row = rows
            .iter()
            .find(|row| row.label == coordinate)
            .ok_or_else(|| {
                let rendered = rows
                    .iter()
                    .map(|row| row.label.clone())
                    .collect::<Vec<_>>()
                    .join("\n  ");
                format!("no row for {coordinate}\nprojected:\n  {rendered}")
            })?;
        let Some(parent) = row.parent else {
            return Ok("-".to_owned());
        };
        Ok(rows
            .iter()
            .find(|candidate| candidate.id == RowId::Symbol(parent))
            .map_or_else(|| "<dangling>".to_owned(), |found| found.label.clone()))
    }

    #[test]
    fn declarations_are_parented_to_their_type_and_not_to_the_file() -> Result<(), String> {
        let (rows, _) = projected_rows()?;
        let module = "fixture::src/worker.rs";
        let worker = "fixture::src/worker.rs:2::Worker";
        let event = "fixture::src/worker.rs:6::Event";
        for (row, expected) in [
            (module, "-"),
            (worker, module),
            ("fixture::src/worker.rs:3::name", worker),
            (event, module),
            ("fixture::src/worker.rs:7::Started", event),
            ("fixture::src/worker.rs:11::run", worker),
            ("fixture::src/worker.rs:14::execute", module),
        ] {
            let parent = parent_of(&rows, row)?;
            if parent != expected {
                return Err(format!(
                    "{row} is parented to {parent}, expected {expected}"
                ));
            }
        }
        Ok(())
    }

    #[test]
    fn an_attached_method_row_keeps_the_type_symbol_as_its_parent() -> Result<(), String> {
        let (rows, _) = projected_rows()?;
        let method = rows
            .iter()
            .find(|row| row.label == "fixture::src/worker.rs:11::run")
            .ok_or("no method row")?;
        if method.parent != Some(symbol_key("fixture::src/worker.rs:2::Worker")) {
            return Err(format!("method parent is {:?}", method.parent));
        }
        if method.kind != Some(backend_engine::DeclarationKind::Method) {
            return Err(format!("method row kind is {:?}", method.kind));
        }
        Ok(())
    }

    #[test]
    fn the_structural_fallback_answers_with_typed_provenance() -> Result<(), String> {
        let (rows, ledger) = projected_rows()?;
        if rows.iter().any(|row| row.label.contains("::semantic::")) {
            return Err("a structural fallback projection emitted semantic rows".to_owned());
        }
        let lane = ledger
            .file_lane(FIXTURE_PATH)
            .ok_or("the fixture file has no lane decision")?;
        if lane
            != (FileLane::Structural {
                cause: StructuralCause::NoCompletePublication,
            })
        {
            return Err(format!("structural fallback lane is {lane:?}"));
        }
        if ledger.all_semantic_fresh() {
            return Err("a structural answer was recorded as semantic".to_owned());
        }
        if ledger.files().collect::<Vec<_>>().len() != 1 {
            return Err(format!(
                "the single-file projection recorded {:?}",
                ledger.files().collect::<Vec<_>>()
            ));
        }
        Ok(())
    }

    #[test]
    fn the_semantic_lane_answers_fields_and_variants_the_baseline_cannot_see() -> Result<(), String>
    {
        let rows = projected_semantic_rows(false)?;
        for name in ["Worker", "name", "Event", "Started"] {
            let row = row_named(&rows, name)?;
            if !row.label.contains("::semantic::") {
                return Err(format!("{name} row is not addressed as semantic: {row:?}"));
            }
        }
        let field = row_named(&rows, "name")?;
        if field.kind != Some(backend_engine::DeclarationKind::Field) {
            return Err(format!("field row kind is {:?}", field.kind));
        }
        let worker = row_named(&rows, "Worker")?;
        let expected_parent = match worker.id {
            RowId::Symbol(key) => key,
            other => return Err(format!("worker row id is {other:?}")),
        };
        if field.parent != Some(expected_parent) {
            return Err(format!("field parent is {:?}", field.parent));
        }
        let variant = row_named(&rows, "Started")?;
        let event = row_named(&rows, "Event")?;
        let event_parent = match event.id {
            RowId::Symbol(key) => key,
            other => return Err(format!("event row id is {other:?}")),
        };
        if variant.parent != Some(event_parent) {
            return Err(format!("variant parent is {:?}", variant.parent));
        }
        Ok(())
    }

    #[test]
    fn a_stale_image_answers_semantic_with_a_typed_stale_note() -> Result<(), String> {
        let fresh = projected_semantic_rows(false)?;
        let stale = projected_semantic_rows(true)?;
        if stale.len() != fresh.len() {
            return Err("staleness changed the projected row set".to_owned());
        }
        for row in &stale {
            let documented = row
                .document
                .iter()
                .any(|fragment| matches!(fragment, backend_engine::Fragment::Text(text) if text.contains("stale semantic image")));
            if !documented {
                return Err(format!("stale row {} lost its typed note", row.label));
            }
            if row.label.contains("::semantic::") {
                continue;
            }
            return Err(format!(
                "stale row {} fell back to a structural address",
                row.label
            ));
        }
        if fresh.iter().any(|row| {
            row.document
                .iter()
                .any(|fragment| matches!(fragment, backend_engine::Fragment::Text(text) if text.contains("stale semantic image")))
        }) {
            return Err("a fresh row carried the stale note".to_owned());
        }
        Ok(())
    }

    #[test]
    fn a_stale_publication_suppresses_structural_rows_instead_of_silently_falling_back()
    -> Result<(), String> {
        let frontend = backend_frontend_rust::syntax_frontend().map_err(|e| e.to_string())?;
        let analysis = frontend
            .analyze(
                std::path::Path::new(FIXTURE_PATH),
                FIXTURE_SOURCE.as_bytes(),
            )
            .map_err(|e| e.to_string())?;
        let project_key = [7u8; 32];
        let file_key = product_source_file_key(project_key, FIXTURE_PATH);
        let record = super::super::ProductSourceRecord::file(
            project_key,
            FIXTURE_PATH,
            backend_engine::SourceLanguage::Rust,
            [1; 32],
            [2; 32],
            analysis.declarations().clone(),
        )?;
        let package = package_key("fixture");
        let sources = super::super::IndexedSources {
            projects: BTreeMap::from([(
                project_key,
                IndexedProject {
                    package,
                    label: "fixture".to_owned(),
                    files: Arc::from([file_key]),
                },
            )]),
            files: vec![(file_key, record)],
        };
        let (initial, _) = initial_view().map_err(|e| e.to_string())?;
        let complete = BTreeSet::from([(
            package.to_bytes(),
            LanguageProfile::Rust(RustEdition::Rust2024),
        )]);
        let stale = ProfileStalePaths::from([(
            (
                package.to_bytes(),
                LanguageProfile::Rust(RustEdition::Rust2024),
            ),
            BTreeSet::from([FIXTURE_PATH.to_owned()]),
        )]);
        let structural_plan =
            StructuralProjectionPlan::of(&sources, &complete).map_err(|e| e.to_string())?;
        let targets = super::SemanticTargets::default();
        let mut projection =
            SourceRowProjection::new(
                &initial,
                &sources.projects,
                64,
                &targets,
                &structural_plan,
                std::path::Path::new("/tmp"),
            )
                .map_err(|e| e.to_string())?;
        for (key, file) in &sources.files {
            projection
                .append_file(*key, file, &complete, &stale)
                .map_err(|e| e.to_string())?;
        }
        let ledger = projection.take_ledger();
        let rows = projection.finish(Vec::new()).map_err(|e| e.to_string())?;
        if rows
            .iter()
            .any(|row| row.label.contains(FIXTURE_PATH) && !row.label.contains("::semantic::"))
        {
            return Err("a stale semantic file was silently answered structurally".to_owned());
        }
        if ledger.file_lane(FIXTURE_PATH)
            != Some(FileLane::Semantic {
                freshness: SemanticFreshness::Stale,
            })
        {
            return Err(format!(
                "stale file lane is {:?}",
                ledger.file_lane(FIXTURE_PATH)
            ));
        }
        Ok(())
    }

    /// Per-file precision at the projection level: an in-place edit under an
    /// unchanged path set marks exactly the edited file's lane stale, keeps
    /// both files on the semantic lane, and never lets either fall back to
    /// structural rows.
    #[test]
    fn an_in_place_edit_marks_only_the_edited_file_stale_and_suppresses_structure_for_both()
    -> Result<(), String> {
        const EDITED: &str = "src/edited.rs";
        const UNTOUCHED: &str = "src/untouched.rs";
        let frontend = backend_frontend_rust::syntax_frontend().map_err(|e| e.to_string())?;
        let analysis = frontend
            .analyze(
                std::path::Path::new(FIXTURE_PATH),
                FIXTURE_SOURCE.as_bytes(),
            )
            .map_err(|e| e.to_string())?;
        let project_key = [7u8; 32];
        let package = package_key("fixture");
        let mut files = Vec::new();
        let mut keys = Vec::new();
        for path in [EDITED, UNTOUCHED] {
            let key = product_source_file_key(project_key, path);
            keys.push(key);
            let record = super::super::ProductSourceRecord::file(
                project_key,
                path,
                backend_engine::SourceLanguage::Rust,
                [1; 32],
                [2; 32],
                analysis.declarations().clone(),
            )
            .map_err(|e| e.to_string())?;
            files.push((key, record));
        }
        files.sort_by_key(|(key, _)| *key);
        keys.sort();
        let sources = super::super::IndexedSources {
            projects: BTreeMap::from([(
                project_key,
                IndexedProject {
                    package,
                    label: "fixture".to_owned(),
                    files: Arc::from(keys.into_boxed_slice()),
                },
            )]),
            files,
        };
        let (initial, _) = initial_view().map_err(|e| e.to_string())?;
        let rust = LanguageProfile::Rust(RustEdition::Rust2024);
        let complete = BTreeSet::from([(package.to_bytes(), rust)]);
        let stale = ProfileStalePaths::from([(
            (package.to_bytes(), rust),
            BTreeSet::from([EDITED.to_owned()]),
        )]);
        let structural_plan =
            StructuralProjectionPlan::of(&sources, &complete).map_err(|e| e.to_string())?;
        let targets = super::SemanticTargets::default();
        let mut projection =
            SourceRowProjection::new(
                &initial,
                &sources.projects,
                64,
                &targets,
                &structural_plan,
                std::path::Path::new("/tmp"),
            )
                .map_err(|e| e.to_string())?;
        for (key, file) in &sources.files {
            projection
                .append_file(*key, file, &complete, &stale)
                .map_err(|e| e.to_string())?;
        }
        let ledger = projection.take_ledger();
        let rows = projection.finish(Vec::new()).map_err(|e| e.to_string())?;
        // Neither file falls back to structural rows: the stale image still
        // answers, typed stale, and never silently structural.
        for path in [EDITED, UNTOUCHED] {
            if rows
                .iter()
                .any(|row| row.label.contains(path) && !row.label.contains("::semantic::"))
            {
                return Err(format!("{path} was silently answered structurally"));
            }
        }
        if ledger.file_lane(EDITED)
            != Some(FileLane::Semantic {
                freshness: SemanticFreshness::Stale,
            })
        {
            return Err(format!(
                "edited file lane is {:?}",
                ledger.file_lane(EDITED)
            ));
        }
        if ledger.file_lane(UNTOUCHED)
            != Some(FileLane::Semantic {
                freshness: SemanticFreshness::Fresh,
            })
        {
            return Err(format!(
                "untouched file lane is {:?}",
                ledger.file_lane(UNTOUCHED)
            ));
        }
        Ok(())
    }

    #[test]
    fn an_unavailable_publication_types_the_structural_cause_with_its_reason() -> Result<(), String>
    {
        let frontend = backend_frontend_rust::syntax_frontend().map_err(|e| e.to_string())?;
        let analysis = frontend
            .analyze(
                std::path::Path::new(FIXTURE_PATH),
                FIXTURE_SOURCE.as_bytes(),
            )
            .map_err(|e| e.to_string())?;
        let project_key = [7u8; 32];
        let file_key = product_source_file_key(project_key, FIXTURE_PATH);
        let record = super::super::ProductSourceRecord::file(
            project_key,
            FIXTURE_PATH,
            backend_engine::SourceLanguage::Rust,
            [1; 32],
            [2; 32],
            analysis.declarations().clone(),
        )?;
        let package = package_key("fixture");
        let sources = super::super::IndexedSources {
            projects: BTreeMap::from([(
                project_key,
                IndexedProject {
                    package,
                    label: "fixture".to_owned(),
                    files: Arc::from([file_key]),
                },
            )]),
            files: vec![(file_key, record)],
        };
        let (initial, _) = initial_view().map_err(|e| e.to_string())?;
        let mut targets = super::SemanticTargets::default();
        targets.unavailable.insert(
            (
                package.to_bytes(),
                LanguageProfile::Rust(RustEdition::Rust2024),
            ),
            backend_engine::builtin::SemanticUnavailableReason::Toolchain,
        );
        let structural_plan =
            StructuralProjectionPlan::of(&sources, &BTreeSet::new()).map_err(|e| e.to_string())?;
        let mut projection =
            SourceRowProjection::new(
                &initial,
                &sources.projects,
                64,
                &targets,
                &structural_plan,
                std::path::Path::new("/tmp"),
            )
                .map_err(|e| e.to_string())?;
        for (key, file) in &sources.files {
            projection
                .append_file(*key, file, &BTreeSet::new(), &ProfileStalePaths::new())
                .map_err(|e| e.to_string())?;
        }
        let ledger = projection.take_ledger();
        let rows = projection.finish(Vec::new()).map_err(|e| e.to_string())?;
        if rows
            .iter()
            .find(|row| row.label == "fixture::src/worker.rs:2::Worker")
            .is_none()
        {
            return Err("an unavailable publication lost its structural fallback".to_owned());
        }
        if ledger.file_lane(FIXTURE_PATH)
            != Some(FileLane::Structural {
                cause: StructuralCause::PublicationUnavailable(
                    backend_engine::builtin::SemanticUnavailableReason::Toolchain,
                ),
            })
        {
            return Err(format!(
                "unavailable publication lane is {:?}",
                ledger.file_lane(FIXTURE_PATH)
            ));
        }
        Ok(())
    }

    type SourceId = ContentId<SourceFactDomain>;

    fn fixture_identity_map(path: &str, identity: SourceId) -> BTreeMap<String, SourceId> {
        BTreeMap::from([(path.to_owned(), identity)])
    }

    #[test]
    fn freshness_compares_compiled_paths_against_the_current_scan() -> Result<(), String> {
        let bytes = fixture_semantic_image("src/worker.rs")?;
        let view = SemanticImageView::reopen(&bytes).map_err(|error| error.to_string())?;
        let (path, identity) = super::compiled_source(&view).map_err(|e| e.to_string())?;
        let compiled = BTreeMap::from([(path.clone(), identity)]);
        let current = BTreeSet::from([path.clone()]);
        // The exact bytes the image was compiled from are fresh.
        let fresh = super::freshness_decision(
            &compiled,
            &current,
            Some(&fixture_identity_map(&path, identity)),
        );
        if fresh.stale_paths.contains(&path) || fresh.path_sets_differ {
            return Err("an identical source snapshot was judged stale".to_owned());
        }
        // A file added after the compile is stale; the unchanged file is not.
        let with_extra = BTreeSet::from([path.clone(), "src/extra.rs".to_owned()]);
        let identities = fixture_identity_map(&path, identity);
        let mut with_new_file = identities.clone();
        with_new_file.insert(
            "src/extra.rs".to_owned(),
            ContentId::<SourceFactDomain>::from_canonical_bytes(b"extra source"),
        );
        let expanded = super::freshness_decision(&compiled, &with_extra, Some(&with_new_file));
        if expanded.stale_paths != BTreeSet::from(["src/extra.rs".to_owned()]) {
            return Err(format!(
                "added-file staleness is {:?}",
                expanded.stale_paths
            ));
        }
        // Without persisted identities the fallback marks the whole scan
        // stale instead of claiming per-file precision it cannot prove.
        let expanded_legacy = super::freshness_decision(&compiled, &with_extra, Some(&identities));
        if expanded_legacy.identity_decisive || expanded_legacy.stale_paths != with_extra {
            return Err(format!(
                "legacy added-file staleness is {:?}",
                expanded_legacy.stale_paths
            ));
        }
        // A file deleted after the compile leaves a stale orphan image.
        let empty = BTreeSet::new();
        let deleted = super::freshness_decision(&compiled, &empty, None);
        if !deleted.path_sets_differ {
            return Err("a deleted source set was judged matching".to_owned());
        }
        Ok(())
    }

    /// The residual the path-set comparison could never catch: the same path
    /// set with one file edited in place must flip that file's freshness to
    /// stale while every untouched file stays fresh.
    #[test]
    fn an_in_place_edit_is_stale_without_a_path_set_change() -> Result<(), String> {
        let bytes = fixture_semantic_image("src/worker.rs")?;
        let view = SemanticImageView::reopen(&bytes).map_err(|error| error.to_string())?;
        let (path, compiled_identity) = super::compiled_source(&view).map_err(|e| e.to_string())?;
        let compiled = BTreeMap::from([(path.clone(), compiled_identity)]);
        let current = BTreeSet::from([path.clone()]);
        let edited = ContentId::<SourceFactDomain>::from_canonical_bytes(b"edited in place");
        let decision = super::freshness_decision(
            &compiled,
            &current,
            Some(&fixture_identity_map(&path, edited)),
        );
        if decision.stale_paths != BTreeSet::from([path.clone()]) {
            return Err(format!(
                "in-place edit staleness is {:?}",
                decision.stale_paths
            ));
        }
        if !decision.image_stale(&path, compiled_identity) {
            return Err(
                "the compiled image was not judged stale after an in-place edit".to_owned(),
            );
        }
        // The same comparison keeps an untouched file fresh.
        let untouched = super::freshness_decision(
            &compiled,
            &current,
            Some(&fixture_identity_map(&path, compiled_identity)),
        );
        if !untouched.stale_paths.is_empty() || untouched.image_stale(&path, compiled_identity) {
            return Err("an untouched file was judged stale".to_owned());
        }
        Ok(())
    }

    /// Records persisted before identities existed cannot prove freshness by
    /// content; they keep the coarse path-set comparison, which marks every
    /// current path stale when the path sets disagree and fresh otherwise.
    #[test]
    fn records_without_identities_keep_the_path_set_fallback() -> Result<(), String> {
        let bytes = fixture_semantic_image("src/worker.rs")?;
        let view = SemanticImageView::reopen(&bytes).map_err(|error| error.to_string())?;
        let (path, identity) = super::compiled_source(&view).map_err(|e| e.to_string())?;
        let compiled = BTreeMap::from([(path.clone(), identity)]);
        let current = BTreeSet::from([path.clone()]);
        let fallback = super::freshness_decision(&compiled, &current, None);
        if !fallback.stale_paths.is_empty() || fallback.identity_decisive {
            return Err("an unchanged legacy snapshot was judged stale".to_owned());
        }
        if fallback.image_stale(&path, identity) {
            return Err("a legacy image under an unchanged path set was judged stale".to_owned());
        }
        let shifted = BTreeSet::from([path.clone(), "src/other.rs".to_owned()]);
        let mismatched = super::freshness_decision(&compiled, &shifted, None);
        if mismatched.stale_paths != shifted {
            return Err(
                "a legacy path-set mismatch did not mark the current paths stale".to_owned(),
            );
        }
        Ok(())
    }

    #[test]
    fn complete_c_publication_does_not_suppress_cxx_fallback() -> Result<(), String> {
        let package = package_key("mixed-c-cxx");
        let complete = BTreeSet::from([(package.to_bytes(), LanguageProfile::C(CStandard::C23))]);
        if !semantic_profile_is_complete(&complete, Some(package), "source.c")
            .map_err(|error| error.to_string())?
            || semantic_profile_is_complete(&complete, Some(package), "source.cpp")
                .map_err(|error| error.to_string())?
        {
            return Err("C and C++ semantic fallback profiles collapsed".to_owned());
        }
        Ok(())
    }

    #[test]
    fn the_stale_note_is_stable() {
        assert!(STALE_NOTE.contains("stale semantic image"));
    }

    fn analyze_source(
        language: backend_engine::SourceLanguage,
        path: &str,
        source: &str,
    ) -> Result<Arc<[backend_compile::SourceDeclaration]>, String> {
        match language {
            backend_engine::SourceLanguage::TypeScript => Ok(
                backend_frontend_typescript::syntax_frontend()
                    .map_err(|error| error.to_string())?
                    .analyze(std::path::Path::new(path), source.as_bytes())
                    .map_err(|error| error.to_string())?
                    .declarations()
                    .clone(),
            ),
            backend_engine::SourceLanguage::Rust => Ok(
                backend_frontend_rust::syntax_frontend()
                    .map_err(|error| error.to_string())?
                    .analyze(std::path::Path::new(path), source.as_bytes())
                    .map_err(|error| error.to_string())?
                    .declarations()
                    .clone(),
            ),
            backend_engine::SourceLanguage::Python => Ok(
                backend_frontend_python::syntax_frontend()
                    .map_err(|error| error.to_string())?
                    .analyze(std::path::Path::new(path), source.as_bytes())
                    .map_err(|error| error.to_string())?
                    .declarations()
                    .clone(),
            ),
            _ => Err(format!("unsupported cross-file test language: {language:?}")),
        }
    }

    fn cross_file_sources(
        files: &[(&str, backend_engine::SourceLanguage, Arc<[backend_compile::SourceDeclaration]>)],
    ) -> Result<(super::super::IndexedSources, backend_engine::PackageKey), String> {
        let package = package_key("fixture");
        let project_key = package.to_bytes();
        let mut file_records = Vec::new();
        let mut file_keys = Vec::new();
        for (path, language, declarations) in files {
            let file_key = product_source_file_key(project_key, *path);
            let record = super::super::ProductSourceRecord::file(
                project_key,
                *path,
                *language,
                [1; 32],
                [2; 32],
                declarations.clone(),
            )?;
            file_keys.push(file_key);
            file_records.push((file_key, record));
        }
        file_keys.sort();
        file_records.sort_by_key(|(file_key, _)| *file_key);
        Ok((
            super::super::IndexedSources {
                projects: BTreeMap::from([(
                    project_key,
                    IndexedProject {
                        package,
                        label: "fixture".to_owned(),
                        files: Arc::from(file_keys),
                    },
                )]),
                files: file_records,
            },
            package,
        ))
    }

    fn cross_file_view(
        sources: &super::super::IndexedSources,
    ) -> Result<(ViewRoot, Vec<Row>), String> {
        let (initial, _) = initial_view().map_err(|error| error.to_string())?;
        let structural_plan =
            StructuralProjectionPlan::of(sources, &BTreeSet::new()).map_err(|error| error.to_string())?;
        let targets = super::SemanticTargets::default();
        let mut projection = SourceRowProjection::new(
            &initial,
            &sources.projects,
            256,
            &targets,
            &structural_plan,
            std::path::Path::new("/tmp"),
        )
        .map_err(|error| error.to_string())?;
        for (key, file) in &sources.files {
            projection
                .append_file(*key, file, &BTreeSet::new(), &ProfileStalePaths::new())
                .map_err(|error| error.to_string())?;
        }
        let rows = projection.finish(Vec::new()).map_err(|error| error.to_string())?;
        let capability = super::super::test_builtin_view_capability().map_err(|error| error.to_string())?;
        let view = ViewRoot::new_checked(
            initial.recipe(),
            initial.basis(),
            initial.frontier(),
            rows.clone(),
            vec![backend_engine::ViewCoverage::Complete],
            capability,
        )
        .map_err(|error| format!("{error:?}"))?;
        Ok((view, rows))
    }

    fn row_for_coordinate(rows: &[Row], coordinate: &str) -> Result<RowId, String> {
        rows.iter()
            .find(|row| row.label == coordinate)
            .map(|row| row.id)
            .ok_or_else(|| format!("missing row for coordinate {coordinate}"))
    }

    fn calls_relations(
        view: &ViewRoot,
        sources: &super::super::IndexedSources,
        package: backend_engine::PackageKey,
        source_id: RowId,
        include_incoming: bool,
    ) -> Result<Vec<backend_engine::GraphRelation>, String> {
        Ok(
            structural_call_graph_relations(view, sources, package, source_id, include_incoming)
                .map_err(|error| error.to_string())?
                .unwrap_or_default(),
        )
    }

    fn assert_single_calls_target(
        view: &ViewRoot,
        sources: &super::super::IndexedSources,
        package: backend_engine::PackageKey,
        from: RowId,
        to_coordinate: &str,
        rows: &[Row],
        include_incoming: bool,
    ) -> Result<(), String> {
        let to = row_for_coordinate(rows, to_coordinate)?;
        let relations = calls_relations(view, sources, package, from, include_incoming)?;
        if relations.len() != 1 {
            return Err(format!(
                "expected exactly one Calls relation, got {relations:?}"
            ));
        }
        let relation = &relations[0];
        if relation.relation != backend_library::SemanticLinkKind::Calls {
            return Err(format!("expected Calls relation, got {relation:?}"));
        }
        if relation.to != to {
            return Err(format!(
                "expected target {to_coordinate}, got relation {relation:?}"
            ));
        }
        if !include_incoming && relation.from != from {
            return Err(format!("expected outgoing from {from:?}, got {relation:?}"));
        }
        Ok(())
    }

    fn assert_no_calls_target(
        view: &ViewRoot,
        sources: &super::super::IndexedSources,
        package: backend_engine::PackageKey,
        from: RowId,
        to_coordinate: &str,
        rows: &[Row],
    ) -> Result<(), String> {
        let to = row_for_coordinate(rows, to_coordinate)?;
        let relations = calls_relations(view, sources, package, from, false)?;
        if relations.iter().any(|relation| relation.to == to) {
            return Err(format!(
                "unexpected Calls edge to {to_coordinate}: {relations:?}"
            ));
        }
        Ok(())
    }

    #[test]
    fn cross_file_call_typescript_named_import() -> Result<(), String> {
        let apply = analyze_source(
            backend_engine::SourceLanguage::TypeScript,
            "apply-set.ts",
            "export function entriesFromItems() {}\nexport function entriesFromWeekSet() {}\n",
        )?;
        let weeks = analyze_source(
            backend_engine::SourceLanguage::TypeScript,
            "weeks.ts",
            "import { entriesFromItems } from \"./apply-set\";\nexport function syncWorkout() { entriesFromItems(); }\n",
        )?;
        let decoy = analyze_source(
            backend_engine::SourceLanguage::TypeScript,
            "decoy.ts",
            "export function rogue() { entriesFromItems(); }\n",
        )?;
        let (sources, package) = cross_file_sources(&[
            ("apply-set.ts", backend_engine::SourceLanguage::TypeScript, apply),
            ("weeks.ts", backend_engine::SourceLanguage::TypeScript, weeks),
            ("decoy.ts", backend_engine::SourceLanguage::TypeScript, decoy),
        ])?;
        let (view, rows) = cross_file_view(&sources)?;
        let sync = row_for_coordinate(&rows, "fixture::weeks.ts:2::syncWorkout")?;
        let target = "fixture::apply-set.ts:1::entriesFromItems";
        assert_single_calls_target(&view, &sources, package, sync, target, &rows, false)?;
        let callee = row_for_coordinate(&rows, target)?;
        let incoming = calls_relations(&view, &sources, package, callee, true)?;
        if incoming.len() != 1 || incoming[0].from != sync {
            return Err(format!("expected incoming call from syncWorkout, got {incoming:?}"));
        }
        let rogue = row_for_coordinate(&rows, "fixture::decoy.ts:1::rogue")?;
        assert_no_calls_target(&view, &sources, package, rogue, target, &rows)?;
        Ok(())
    }

    #[test]
    fn cross_file_call_typescript_alias_import() -> Result<(), String> {
        let apply = analyze_source(
            backend_engine::SourceLanguage::TypeScript,
            "apply-set.ts",
            "export function entriesFromItems() {}\nexport function otherFn() {}\n",
        )?;
        let weeks = analyze_source(
            backend_engine::SourceLanguage::TypeScript,
            "weeks.ts",
            "import { entriesFromItems as items } from \"./apply-set\";\nexport function syncWorkout() { items(); }\n",
        )?;
        let (sources, package) = cross_file_sources(&[
            ("apply-set.ts", backend_engine::SourceLanguage::TypeScript, apply),
            ("weeks.ts", backend_engine::SourceLanguage::TypeScript, weeks),
        ])?;
        let (view, rows) = cross_file_view(&sources)?;
        let sync = row_for_coordinate(&rows, "fixture::weeks.ts:2::syncWorkout")?;
        assert_single_calls_target(
            &view,
            &sources,
            package,
            sync,
            "fixture::apply-set.ts:1::entriesFromItems",
            &rows,
            false,
        )?;
        assert_no_calls_target(
            &view,
            &sources,
            package,
            sync,
            "fixture::apply-set.ts:2::otherFn",
            &rows,
        )?;
        Ok(())
    }

    #[test]
    fn cross_file_call_typescript_imported_class_method() -> Result<(), String> {
        let workout = analyze_source(
            backend_engine::SourceLanguage::TypeScript,
            "workout.service.ts",
            "export class WorkoutService { setNote() {} }\n",
        )?;
        let other = analyze_source(
            backend_engine::SourceLanguage::TypeScript,
            "other.service.ts",
            "export class OtherService { setNote() {} }\n",
        )?;
        let weeks = analyze_source(
            backend_engine::SourceLanguage::TypeScript,
            "weeks.ts",
            "import { WorkoutService } from \"./workout.service\";\nexport class Weeks { service!: WorkoutService; sync() { this.service.setNote(); } }\n",
        )?;
        let (sources, package) = cross_file_sources(&[
            ("workout.service.ts", backend_engine::SourceLanguage::TypeScript, workout),
            ("other.service.ts", backend_engine::SourceLanguage::TypeScript, other),
            ("weeks.ts", backend_engine::SourceLanguage::TypeScript, weeks),
        ])?;
        let (view, rows) = cross_file_view(&sources)?;
        let sync = row_for_coordinate(&rows, "fixture::weeks.ts:2::sync")?;
        assert_single_calls_target(
            &view,
            &sources,
            package,
            sync,
            "fixture::workout.service.ts:1::setNote",
            &rows,
            false,
        )?;
        assert_no_calls_target(
            &view,
            &sources,
            package,
            sync,
            "fixture::other.service.ts:1::setNote",
            &rows,
        )?;
        Ok(())
    }

    #[test]
    fn cross_file_call_typescript_ambiguous_imported_class_method() -> Result<(), String> {
        let workout = analyze_source(
            backend_engine::SourceLanguage::TypeScript,
            "workout.service.ts",
            "export class WorkoutService { setNote() {} }\n",
        )?;
        let other = analyze_source(
            backend_engine::SourceLanguage::TypeScript,
            "other.service.ts",
            "export class OtherService { setNote() {} }\n",
        )?;
        let weeks = analyze_source(
            backend_engine::SourceLanguage::TypeScript,
            "weeks.ts",
            "import { WorkoutService } from \"./workout.service\";\nimport { OtherService } from \"./other.service\";\nexport class Weeks { service!: WorkoutService; sync() { this.service.setNote(); } }\n",
        )?;
        let (sources, package) = cross_file_sources(&[
            ("workout.service.ts", backend_engine::SourceLanguage::TypeScript, workout),
            ("other.service.ts", backend_engine::SourceLanguage::TypeScript, other),
            ("weeks.ts", backend_engine::SourceLanguage::TypeScript, weeks),
        ])?;
        let (view, rows) = cross_file_view(&sources)?;
        let sync = row_for_coordinate(&rows, "fixture::weeks.ts:3::sync")?;
        assert_no_calls_target(
            &view,
            &sources,
            package,
            sync,
            "fixture::workout.service.ts:1::setNote",
            &rows,
        )?;
        assert_no_calls_target(
            &view,
            &sources,
            package,
            sync,
            "fixture::other.service.ts:1::setNote",
            &rows,
        )?;
        let relations = calls_relations(&view, &sources, package, sync, false)?;
        if !relations.is_empty() {
            return Err(format!("ambiguous import must emit no Calls edges: {relations:?}"));
        }
        Ok(())
    }

    #[test]
    fn cross_file_call_typescript_property_name_does_not_matter() -> Result<(), String> {
        let workout = analyze_source(
            backend_engine::SourceLanguage::TypeScript,
            "workout.service.ts",
            "export class WorkoutService { setNote() {} }\n",
        )?;
        let weeks = analyze_source(
            backend_engine::SourceLanguage::TypeScript,
            "weeks.ts",
            "import { WorkoutService } from \"./workout.service\";\nexport class Weeks { svc!: WorkoutService; sync() { svc.setNote(); } }\n",
        )?;
        let (sources, package) = cross_file_sources(&[
            ("workout.service.ts", backend_engine::SourceLanguage::TypeScript, workout),
            ("weeks.ts", backend_engine::SourceLanguage::TypeScript, weeks),
        ])?;
        let (view, rows) = cross_file_view(&sources)?;
        let sync = row_for_coordinate(&rows, "fixture::weeks.ts:2::sync")?;
        assert_single_calls_target(
            &view,
            &sources,
            package,
            sync,
            "fixture::workout.service.ts:1::setNote",
            &rows,
            false,
        )?;
        Ok(())
    }

    #[test]
    fn cross_file_call_typescript_type_only_import_skipped() -> Result<(), String> {
        let workout = analyze_source(
            backend_engine::SourceLanguage::TypeScript,
            "workout.service.ts",
            "export class WorkoutService { setNote() {} }\n",
        )?;
        let weeks = analyze_source(
            backend_engine::SourceLanguage::TypeScript,
            "weeks.ts",
            "import type { WorkoutService } from \"./workout.service\";\nexport class Weeks { workoutService!: WorkoutService; sync() { this.workoutService.setNote(); } }\n",
        )?;
        let (sources, package) = cross_file_sources(&[
            ("workout.service.ts", backend_engine::SourceLanguage::TypeScript, workout),
            ("weeks.ts", backend_engine::SourceLanguage::TypeScript, weeks),
        ])?;
        let (view, rows) = cross_file_view(&sources)?;
        let sync = row_for_coordinate(&rows, "fixture::weeks.ts:2::sync")?;
        let relations = calls_relations(&view, &sources, package, sync, false)?;
        if !relations.is_empty() {
            return Err(format!("type-only import must not create Calls edges: {relations:?}"));
        }
        Ok(())
    }

    #[test]
    fn cross_file_call_same_file_unchanged() -> Result<(), String> {
        let source = r#"
pub fn parse_config() -> Result<String, String> { Ok(String::new()) }
pub fn run_app() -> Result<String, String> { parse_config() }
pub fn decoy_mention() { let _ = "parse_config("; }
"#;
        let declarations = analyze_source(
            backend_engine::SourceLanguage::Rust,
            "src/main.rs",
            source,
        )?;
        let (sources, package) = cross_file_sources(&[(
            "src/main.rs",
            backend_engine::SourceLanguage::Rust,
            declarations,
        )])?;
        let (view, rows) = cross_file_view(&sources)?;
        let run_app = row_for_coordinate(&rows, "fixture::src/main.rs:3::run_app")?;
        assert_single_calls_target(
            &view,
            &sources,
            package,
            run_app,
            "fixture::src/main.rs:2::parse_config",
            &rows,
            false,
        )?;
        assert_no_calls_target(
            &view,
            &sources,
            package,
            run_app,
            "fixture::src/main.rs:4::decoy_mention",
            &rows,
        )?;
        Ok(())
    }

    #[test]
    fn cross_file_call_rust_use_path() -> Result<(), String> {
        let apply = analyze_source(
            backend_engine::SourceLanguage::Rust,
            "src/apply.rs",
            "pub fn entries_from_items() {}\n",
        )?;
        let weeks = analyze_source(
            backend_engine::SourceLanguage::Rust,
            "src/weeks.rs",
            "use crate::apply::entries_from_items;\npub fn sync_week() { entries_from_items(); }\n",
        )?;
        let (sources, package) = cross_file_sources(&[
            ("src/apply.rs", backend_engine::SourceLanguage::Rust, apply),
            ("src/weeks.rs", backend_engine::SourceLanguage::Rust, weeks),
        ])?;
        let (view, rows) = cross_file_view(&sources)?;
        let sync = row_for_coordinate(&rows, "fixture::src/weeks.rs:2::sync_week")?;
        assert_single_calls_target(
            &view,
            &sources,
            package,
            sync,
            "fixture::src/apply.rs:1::entries_from_items",
            &rows,
            false,
        )?;
        Ok(())
    }

    #[test]
    fn cross_file_call_python_from_import() -> Result<(), String> {
        let apply = analyze_source(
            backend_engine::SourceLanguage::Python,
            "apply_set.py",
            "def entries_from_items():\n    pass\n",
        )?;
        let weeks = analyze_source(
            backend_engine::SourceLanguage::Python,
            "weeks.py",
            "from apply_set import entries_from_items\n\ndef sync_week():\n    entries_from_items()\n",
        )?;
        let (sources, package) = cross_file_sources(&[
            ("apply_set.py", backend_engine::SourceLanguage::Python, apply),
            ("weeks.py", backend_engine::SourceLanguage::Python, weeks),
        ])?;
        let (view, rows) = cross_file_view(&sources)?;
        let sync = row_for_coordinate(&rows, "fixture::weeks.py:3::sync_week")?;
        assert_single_calls_target(
            &view,
            &sources,
            package,
            sync,
            "fixture::apply_set.py:1::entries_from_items",
            &rows,
            false,
        )?;
        Ok(())
    }

    #[test]
    fn cross_file_call_typescript_namespace_qualifier() -> Result<(), String> {
        let apply = analyze_source(
            backend_engine::SourceLanguage::TypeScript,
            "apply-set.ts",
            "export function entriesFromItems() {}\n",
        )?;
        let weeks = analyze_source(
            backend_engine::SourceLanguage::TypeScript,
            "weeks.ts",
            "import * as apply from \"./apply-set\";\nexport function syncWorkout() { apply.entriesFromItems(); }\nexport function bare() { entriesFromItems(); }\n",
        )?;
        let (sources, package) = cross_file_sources(&[
            ("apply-set.ts", backend_engine::SourceLanguage::TypeScript, apply),
            ("weeks.ts", backend_engine::SourceLanguage::TypeScript, weeks),
        ])?;
        let (view, rows) = cross_file_view(&sources)?;
        let sync = row_for_coordinate(&rows, "fixture::weeks.ts:2::syncWorkout")?;
        assert_single_calls_target(
            &view,
            &sources,
            package,
            sync,
            "fixture::apply-set.ts:1::entriesFromItems",
            &rows,
            false,
        )?;
        let bare = row_for_coordinate(&rows, "fixture::weeks.ts:3::bare")?;
        let relations = calls_relations(&view, &sources, package, bare, false)?;
        if !relations.is_empty() {
            return Err(format!("bare call without import must not link cross-file: {relations:?}"));
        }
        Ok(())
    }

    #[test]
    fn cross_file_call_earlier_punctuation_does_not_qualify() -> Result<(), String> {
        let apply = analyze_source(
            backend_engine::SourceLanguage::TypeScript,
            "apply-set.ts",
            "export function entriesFromItems() {}\n",
        )?;
        let weeks = analyze_source(
            backend_engine::SourceLanguage::TypeScript,
            "weeks.ts",
            "import { entriesFromItems } from \"./apply-set\";\nexport function sync(file: { name: number }): Record<string, number> {\n  const nested = obj.inner.prop;\n  const label = \"a::b\";\n  const file = { name: 1 };\n  entriesFromItems();\n}\n",
        )?;
        let (sources, package) = cross_file_sources(&[
            ("apply-set.ts", backend_engine::SourceLanguage::TypeScript, apply),
            ("weeks.ts", backend_engine::SourceLanguage::TypeScript, weeks),
        ])?;
        let (view, rows) = cross_file_view(&sources)?;
        let sync = row_for_coordinate(&rows, "fixture::weeks.ts:2::sync")?;
        assert_single_calls_target(
            &view,
            &sources,
            package,
            sync,
            "fixture::apply-set.ts:1::entriesFromItems",
            &rows,
            false,
        )?;
        Ok(())
    }

    #[test]
    fn cross_file_call_namespace_not_poisoned_by_earlier_property() -> Result<(), String> {
        let apply = analyze_source(
            backend_engine::SourceLanguage::TypeScript,
            "apply-set.ts",
            "export function entriesFromItems() {}\n",
        )?;
        let weeks = analyze_source(
            backend_engine::SourceLanguage::TypeScript,
            "weeks.ts",
            "import * as apply from \"./apply-set\";\nexport function bare() { const obj = { apply: 1 }; entriesFromItems(); }\nexport function qualified() { apply.entriesFromItems(); }\n",
        )?;
        let (sources, package) = cross_file_sources(&[
            ("apply-set.ts", backend_engine::SourceLanguage::TypeScript, apply),
            ("weeks.ts", backend_engine::SourceLanguage::TypeScript, weeks),
        ])?;
        let (view, rows) = cross_file_view(&sources)?;
        let bare = row_for_coordinate(&rows, "fixture::weeks.ts:2::bare")?;
        let relations = calls_relations(&view, &sources, package, bare, false)?;
        if !relations.is_empty() {
            return Err(format!(
                "bare call must not link through an earlier property mention: {relations:?}"
            ));
        }
        let qualified = row_for_coordinate(&rows, "fixture::weeks.ts:3::qualified")?;
        assert_single_calls_target(
            &view,
            &sources,
            package,
            qualified,
            "fixture::apply-set.ts:1::entriesFromItems",
            &rows,
            false,
        )?;
        Ok(())
    }

    #[test]
    fn structural_excerpt_calls_matches_real_body_calls_only() {
        assert!(structural_excerpt_calls(
            "pub fn run_app() -> Result<String, String> {\n    parse_config()\n}",
            "parse_config"
        ));
        assert!(!structural_excerpt_calls(
            "pub fn parse_config() -> Result<String, String> {\n    Ok(\"configured\".to_string())\n}",
            "parse_config"
        ));
        assert!(!structural_excerpt_calls(
            "pub fn decoy_mention() {\n    let _ = \"parse_config(\";\n    // parse_config( is not a call\n}",
            "parse_config"
        ));
        assert!(!structural_excerpt_calls(
            "pub fn decoy_mention() {\n    /* parse_config( */\n}",
            "parse_config"
        ));
    }
}

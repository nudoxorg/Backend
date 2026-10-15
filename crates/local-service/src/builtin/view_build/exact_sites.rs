//! Exact compiler declaration identity shared by mixed-lane projections.
//!
//! A display coordinate, signature, name or source order never proves a twin.
//! Callers supply syntax captures from the image's exact SourceFactDomain
//! bytes. Equal, nonempty compiler/syntax declaration extents must then be
//! singular in both lanes. Missing or conflicting evidence leaves both rows.

use super::super::BuiltinModelError;
use backend_compile::{SourceBinding, SourceBindingIdentity, SourceDeclaration};
use backend_engine::application::DocumentationSession;
use backend_engine::{PackageKey, RowId};
use backend_semantic::ir::{DeclarationIdentity, FactAvailability, SemanticReader};
use backend_semantic::vocabulary::LanguageProfile;
use std::collections::BTreeMap;

/// One selected projection's exact source-binding to compiler-row assignment.
/// Conflicting compiler identities poison the assignment instead of choosing.
#[derive(Default)]
pub(super) struct ExactDeclarationSites {
    sites: BTreeMap<([u8; 32], String, SourceBinding), Option<RowId>>,
}

impl ExactDeclarationSites {
    pub(super) fn record(&mut self, package: PackageKey, path: &str, binding: SourceBinding, id: RowId) {
        self.sites.entry((package.to_bytes(), path.to_owned(), binding))
            .and_modify(|previous| {
                if *previous != Some(id) { *previous = None; }
            }).or_insert(Some(id));
    }

    pub(super) fn get(&self, project: [u8; 32], path: &str, binding: SourceBinding) -> Option<RowId> {
        self.sites.get(&(project, path.to_owned(), binding)).copied().flatten()
    }
}

/// Pairs equal authority extents after the caller has authenticated the source
/// identity. Go v6 retains the primary extent in canonical IR, so cold image
/// reopening needs no new field or wire interpretation. Older unbound rows and
/// syntax producers without exact captures intentionally contribute no proof.
pub(super) fn go_sites<Reader: SemanticReader + ?Sized>(
    profile: LanguageProfile,
    reader: &Reader,
    session: &DocumentationSession<'_, Reader>,
    path: &str,
    declarations: &[&SourceDeclaration],
) -> Result<BTreeMap<DeclarationIdentity, SourceBinding>, BuiltinModelError> {
    if !matches!(profile, LanguageProfile::Go(_)) { return Ok(BTreeMap::new()); }
    let mut written = BTreeMap::<_, Vec<_>>::new();
    for declaration in declarations {
        if let SourceBindingIdentity::Exact(capture) = declaration.binding_identity() {
            written.entry(capture.owner()).or_default().push((capture.binding(), declaration.name().as_bytes()));
        }
    }
    let mut compiled = BTreeMap::<_, Vec<_>>::new();
    for entity in session.canonical_entities() {
        let entity = entity.map_err(|error| BuiltinModelError(format!("read exact compiler site: {error}")))?;
        let facts = entity.entity.authority;
        if facts.source != FactAvailability::Captured || facts.source_file != FactAvailability::Captured { continue; }
        let Some(span) = entity.entity.source.filter(|span| !span.is_empty()) else { continue; };
        if reader.atom(span.file()) != Some(path.as_bytes()) { continue; }
        let Some(name) = entity.name.named_bytes() else { continue; };
        compiled.entry((span.start(), span.end())).or_default()
            .push((entity.entity.version.identity(), name));
    }
    Ok(pair_extents(&written, &compiled))
}

fn pair_extents(
    written: &BTreeMap<(u32, u32), Vec<(SourceBinding, &[u8])>>,
    compiled: &BTreeMap<(u32, u32), Vec<(DeclarationIdentity, &[u8])>>,
) -> BTreeMap<DeclarationIdentity, SourceBinding> {
    let mut paired = BTreeMap::new();
    for (extent, candidates) in compiled {
        let Some(sites) = written.get(extent) else { continue; };
        if let ([(identity, name)], [(binding, written_name)]) = (candidates.as_slice(), sites.as_slice())
            && name == written_name
        {
            paired.insert(*identity, *binding);
        }
    }
    paired
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_semantic::ir::{DeclarationFamilyId, VariantFingerprint};

    fn identity(byte: u8) -> DeclarationIdentity {
        DeclarationIdentity {
            family: DeclarationFamilyId::from_raw([byte; 16]),
            variant: VariantFingerprint::from_raw([byte; 16]),
        }
    }

    #[test]
    fn equal_extents_unify_twins_but_distinct_same_names_remain_distinct() {
        let a = SourceBinding::new(15, 21).expect("a");
        let b = SourceBinding::new(45, 51).expect("b");
        let written = BTreeMap::from([((10, 30), vec![(a, b"Shared".as_slice())]), ((40, 60), vec![(b, b"Shared".as_slice())])]);
        let compiled = BTreeMap::from([((10, 30), vec![(identity(1), b"Shared".as_slice())]), ((40, 60), vec![(identity(2), b"Shared".as_slice())])]);
        let paired = pair_extents(&written, &compiled);
        assert_eq!(paired.len(), 2);
        assert_eq!(paired[&identity(1)], a);
        assert_eq!(paired[&identity(2)], b);
        let mut conflicting = compiled.clone();
        conflicting.get_mut(&(10, 30)).expect("extent").push((identity(3), b"Shared"));
        assert!(!pair_extents(&written, &conflicting).contains_key(&identity(1)));
        let mut duplicate_syntax = written.clone();
        duplicate_syntax.get_mut(&(40, 60)).expect("extent").push((b, b"Shared"));
        assert!(!pair_extents(&duplicate_syntax, &compiled).contains_key(&identity(2)));
        assert!(pair_extents(&BTreeMap::from([((11, 30), vec![(a, b"Shared".as_slice())])]), &compiled).is_empty(), "overlap does not prove equality");
    }

    #[test]
    fn conflicting_selected_identities_poison_one_binding_and_keep_scope_separate() {
        let package = backend_engine::package_key("fixture");
        let binding = SourceBinding::new(10, 14).expect("binding");
        let first = RowId::Symbol(backend_engine::symbol_key("first"));
        let second = RowId::Symbol(backend_engine::symbol_key("second"));
        let mut exact = ExactDeclarationSites::default();
        exact.record(package, "a.go", binding, first);
        exact.record(package, "a.go", binding, first);
        assert_eq!(exact.get(package.to_bytes(), "a.go", binding), Some(first));
        assert_eq!(exact.get(package.to_bytes(), "b.go", binding), None);
        exact.record(package, "a.go", binding, second);
        exact.record(package, "a.go", binding, first);
        assert_eq!(exact.get(package.to_bytes(), "a.go", binding), None);
    }

    #[test]
    fn cold_canonical_sites_unify_only_current_exact_twins_in_rows_and_query_plan() -> Result<(), String> {
        use super::super::{image_rows, query, semantic, structural};
        use super::super::super::{IndexedProject, IndexedSources, ProductSourceRecord};
        use backend_semantic::ir::{BorrowedTree, CorePayloadHash, EntityAuthorityFacts, EntityVersion, IrBuilder, ItemKind, ParentageAuthority, SemanticImageView, SourceIdentity, SourceSpan, TreeItemInput, Visibility, encode_full_semantic_image, full_semantic_image_len};
        use backend_semantic::vocabulary::{CompileRecipeFact, GoVersion, NativeTool, PackageUrl, Stage};
        use backend_version::{ContentId, SourceFactDomain, ToolchainDomain};
        use std::collections::BTreeSet;
        use std::sync::Arc;
        let path = "main.go";
        let source = "package demo\n// λ byte offsets\ntype One struct{}\ntype Two int\nfunc Café() {}\nfunc (One) Shared() {}\nfunc (Two) Shared() {}\nfunc Unlowered() {}\n";
        let analysis = backend_frontend_go::syntax_frontend().map_err(|error| error.to_string())?
            .analyze(std::path::Path::new(path), source.as_bytes()).map_err(|error| error.to_string())?;
        let source_id = ContentId::<SourceFactDomain>::from_canonical_bytes(source.as_bytes());
        let package = backend_engine::package_key("fixture");
        let file_key = backend_engine::product_source_file_key(package.to_bytes(), path);
        let record = ProductSourceRecord::file(package.to_bytes(), path, backend_engine::SourceLanguage::Go, [1; 32], [2; 32], analysis.declarations().clone())?
            .with_source_identity(source_id)?;
        let mut sources = IndexedSources {
            projects: BTreeMap::from([(package.to_bytes(), IndexedProject { package, label: "fixture".to_owned(), files: Arc::from([file_key]) })]),
            files: vec![(file_key, record)], cargo_aliases: BTreeMap::new(), source_snapshot: None,
        };
        let profile = LanguageProfile::Go(GoVersion::Go125);
        let mut builder = IrBuilder::new();
        let recipe = CompileRecipeFact::derive(profile, Stage::LowerIr, NativeTool::GoCompiler, source_id, ContentId::<ToolchainDomain>::from_canonical_bytes(b"fixture-toolchain"));
        let coordinate = PackageUrl::parse("pkg:golang/example.com/fixture@1.0.0".to_owned()).map_err(|error| error.to_string())?;
        builder.set_image_provenance_for_package(SourceIdentity { identity: source_id, byte_len: source.len() as u32 }, recipe, &coordinate, path).map_err(|error| error.to_string())?;
        let file = builder.intern_atom(path.as_bytes()).map_err(|error| error.to_string())?;
        // These are literal Go authority declaration extents, independently
        // supplied to the canonical image rather than read from syntax captures.
        let definitions = [("Café", "func Café() {}"), ("Shared", "func (One) Shared() {}"), ("Shared", "func (Two) Shared() {}")];
        let items = definitions.iter().map(|(name, extent)| {
            let start = source.find(*extent).expect("literal declaration") as u32;
            TreeItemInput {
                name: name.as_bytes(), anonymous_callable_anchor: None, kind: ItemKind::Function,
                visibility: Visibility::Public, authority: EntityAuthorityFacts {
                    parentage: ParentageAuthority::Root, source: FactAvailability::Captured,
                    source_file: FactAvailability::Captured, ..EntityAuthorityFacts::default()
                }, parent: None, semantic_type: None, members: &[], docs: &[], attributes: &[],
                source: SourceSpan::new(file, start, start + extent.len() as u32), extension: None,
            }
        }).collect::<Vec<_>>();
        let versions = (1..=3).map(|byte| EntityVersion { family: identity(byte).family, variant: identity(byte).variant, core_payload: CorePayloadHash::from_raw([byte; 16]) }).collect::<Vec<_>>();
        builder.add_borrowed_tree(BorrowedTree { versions: &versions, items: &items, links: &[] }).map_err(|error| error.to_string())?;
        let ir = builder.finish().map_err(|error| error.to_string())?;
        let mut bytes = vec![0; full_semantic_image_len(&ir).map_err(|error| error.to_string())?];
        encode_full_semantic_image(&ir, &mut bytes).map_err(|error| error.to_string())?;
        drop(ir);
        let image = SemanticImageView::reopen(&bytes).map_err(|error| error.to_string())?;
        let current = semantic::StructuralSites::of(&sources).map_err(|error| error.to_string())?
            .declarations_of_source(package.as_bytes(), path, source_id).map_err(|error| error.to_string())?;
        let current = current.iter().collect::<Vec<_>>();
        let (initial, _) = super::super::super::initial_view().map_err(|error| error.to_string())?;
        let project = sources.projects.get(package.as_bytes()).ok_or("project")?;
        let projected = semantic::project_image_rows(&image, *blake3::hash(&bytes).as_bytes(), project, profile, initial.basis(), false, path, &current).map_err(|error| error.to_string())?;
        let mut exact = ExactDeclarationSites::default();
        let mut rows = Vec::new();
        let mut symbols = BTreeSet::new();
        let mut budget = usize::MAX;
        image_rows::apply_projected_image(&projected, &mut semantic::SemanticRowSink {
            initial: &initial, symbols: &mut symbols, rows: &mut rows, capacity: 64,
            remaining_bytes: &mut budget, stale: false, path, site_declarations: &current, exact_sites: &mut exact,
        }).map_err(|error| error.to_string())?;
        let mut plan = structural::StructuralProjectionPlan::of(&sources, &BTreeSet::new()).map_err(|error| error.to_string())?;
        plan.unify_exact_sites(&sources, &exact).map_err(|error| error.to_string())?;
        let mut structural_facts = Vec::new();
        query::append_structural_query_facts(&sources, &plan, &mut structural_facts).map_err(|error| error.to_string())?;
        assert!(structural_facts.iter().all(|fact| !["Café", "Shared"].contains(&fact.presentation().name.as_str())));
        assert!(structural_facts.iter().any(|fact| fact.presentation().name == "Unlowered"));
        let targets = semantic::SemanticTargets::default();
        let mut projection = semantic::SourceRowProjection::new(&initial, &sources, 64, &targets, &plan, std::path::Path::new("/tmp")).map_err(|error| error.to_string())?;
        projection.append_file(file_key, &sources.files[0].1, &BTreeSet::new(), &BTreeMap::new()).map_err(|error| error.to_string())?;
        let rows = projection.finish(rows).map_err(|error| error.to_string())?;
        assert_eq!(rows.iter().filter(|row| row.label.ends_with("::Café")).count(), 1);
        assert_eq!(rows.iter().filter(|row| row.label.ends_with("::Shared")).count(), 2, "real receiver collisions remain distinct");
        assert!(rows.iter().any(|row| row.label.ends_with("::Unlowered")), "partial publication preserves unlowered declarations");
        let legacy = current.iter().map(|site| (**site).clone().with_binding_identity(SourceBindingIdentity::Legacy)).collect::<Vec<_>>();
        assert!(go_sites(profile, &image, &DocumentationSession::new(&image), path, &legacy.iter().collect::<Vec<_>>()).map_err(|error| error.to_string())?.is_empty());
        drop(current);
        sources.files[0].1 = sources.files[0].1.clone().with_source_identity(ContentId::<SourceFactDomain>::from_canonical_bytes(b"edited generation"))?;
        let stale = semantic::StructuralSites::of(&sources).map_err(|error| error.to_string())?
            .declarations_of_source(package.as_bytes(), path, source_id).map_err(|error| error.to_string())?;
        assert!(stale.is_empty(), "old image bytes cannot unify new source bindings");
        assert!(go_sites(profile, &image, &DocumentationSession::new(&image), path, &[]).map_err(|error| error.to_string())?.is_empty());
        Ok(())
    }
}

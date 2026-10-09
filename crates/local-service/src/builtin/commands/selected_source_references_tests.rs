//! Artifact-selection controls, not native compiler or installed-app claims.

use super::*;
use crate::builtin::{
    ActivatedProductSemantics, IndexedProject, IndexedSources, ProductSourceRecord,
};
use backend_engine::builtin::{
    PartialSemanticCoverage, ProductSemanticPublicationKey, SemanticPublicationCoverage,
};
use backend_library::interface::{SemanticImageAuthority, SemanticImageSnapshot};
use backend_semantic::ir::{
    BorrowedTree, CorePayloadHash, DeclarationFamilyId, EntityAuthorityFacts, EntityVersion,
    FactAvailability, ImageProvenance, IrBuilder, ItemKind, OccurrenceAuthorityFacts,
    ParentageAuthority, SemanticCoreReader, SourceIdentity, SourceSpan, TreeEntityId,
    TreeItemInput, TreeLinkInput, TreeLinkTarget, VariantFingerprint, Visibility,
    encode_full_semantic_image, full_semantic_image_len,
};
use backend_semantic::vocabulary::{
    CompileRecipeFact, GoVersion, LanguageProfile, NativeTool, PackageUrl, PythonVersion,
    RustEdition, Stage, TypeScriptSource,
};
use backend_version::{ArtifactId, ContentId, SourceFactDomain, ToolchainDomain};
use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::sync::Arc;

const SOURCE: &[u8] = b"fn caller() { callee(); } fn callee() {}";

fn version(id: u8) -> EntityVersion {
    EntityVersion {
        family: DeclarationFamilyId::from_raw([id; 16]),
        variant: VariantFingerprint::from_raw([id; 16]),
        core_payload: CorePayloadHash::from_raw([id; 16]),
    }
}

fn partial(completed: u32, total: u32) -> Result<SemanticPublicationCoverage, String> {
    Ok(SemanticPublicationCoverage::Partial(
        PartialSemanticCoverage::new(
            NonZeroU32::new(completed).ok_or("zero completed")?,
            NonZeroU32::new(total).ok_or("zero total")?,
        )?,
    ))
}

fn publication_key(
    profile: LanguageProfile,
    coordinate: &str,
) -> Result<ProductSemanticPublicationKey, String> {
    ProductSemanticPublicationKey::new(
        backend_engine::PackageReference::parse("fixture".to_owned())
            .map_err(|error| error.to_string())?,
        PackageUrl::parse(coordinate.to_owned()).map_err(|error| format!("{error:?}"))?,
        profile,
    )
    .map_err(str::to_owned)
}

fn snapshot(bytes: &[u8]) -> Result<SemanticImageSnapshot, String> {
    SemanticImageSnapshot::try_from_reopened(
        SemanticImageAuthority {
            identity: ArtifactId::from_encoded_bytes(bytes),
            byte_len: u32::try_from(bytes.len()).map_err(|error| error.to_string())?,
        },
        bytes,
    )
    .map_err(|error| format!("{error:?}"))
}

fn activated(bytes: &[&[u8]]) -> Result<ActivatedProductSemantics, String> {
    let images = bytes
        .iter()
        .map(|bytes| snapshot(bytes))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ActivatedProductSemantics {
        native_program_sources: None,
        images: Arc::from(images),
    })
}

fn sources(
    key: &ProductSemanticPublicationKey,
    language: backend_engine::SourceLanguage,
    files: &[(&str, Option<ContentId<SourceFactDomain>>)],
) -> Result<IndexedSources, String> {
    let package = key.package_key();
    let mut records = Vec::new();
    let mut file_keys = Vec::new();
    for (path, identity) in files {
        let file_key = backend_engine::product_source_file_key(package.to_bytes(), path);
        let mut record = ProductSourceRecord::file(
            package.to_bytes(),
            *path,
            language,
            [1; 32],
            [2; 32],
            Vec::<backend_compile::SourceDeclaration>::new(),
        )?;
        if let Some(identity) = identity {
            record = record.with_source_identity(*identity)?;
        }
        records.push((file_key, record));
        file_keys.push(file_key);
    }
    file_keys.sort_unstable();
    Ok(IndexedSources {
        projects: BTreeMap::from([(
            package.to_bytes(),
            IndexedProject {
                package,
                label: "fixture".to_owned(),
                files: Arc::from(file_keys),
            },
        )]),
        files: records,
        cargo_aliases: BTreeMap::new(),
        source_snapshot: None,
    })
}

fn local_call_image(
    key: &ProductSemanticPublicationKey,
    path: &str,
    tool: NativeTool,
) -> Result<Vec<u8>, String> {
    let source = SourceIdentity::from_bytes(SOURCE).ok_or("source length")?;
    let recipe = CompileRecipeFact::derive(
        key.profile(),
        Stage::LowerIr,
        tool,
        source.identity,
        ContentId::<ToolchainDomain>::from_canonical_bytes(
            b"artifact fixture, not native execution",
        ),
    );
    let mut builder = IrBuilder::new();
    builder
        .set_image_provenance_for_package(source, recipe, key.coordinate(), path)
        .map_err(|error| error.to_string())?;
    let path_atom = builder
        .intern_atom(path.as_bytes())
        .map_err(|error| error.to_string())?;
    let authority = EntityAuthorityFacts {
        parentage: ParentageAuthority::Root,
        source: FactAvailability::Captured,
        source_file: FactAvailability::Captured,
        visibility: FactAvailability::Captured,
        ..EntityAuthorityFacts::default()
    };
    let items = [b"caller".as_slice(), b"callee".as_slice()].map(|name| TreeItemInput {
        name,
        anonymous_callable_anchor: None,
        kind: ItemKind::Function,
        visibility: Visibility::Public,
        authority,
        parent: None,
        semantic_type: None,
        members: &[],
        docs: &[],
        attributes: &[],
        source: SourceSpan::new(path_atom, 0, 10),
        extension: None,
    });
    let links = [TreeLinkInput {
        from: TreeEntityId::new(0),
        target: TreeLinkTarget::Local(TreeEntityId::new(1)),
        kind: LinkKind::Calls,
        confidence: backend_semantic::ir::Confidence::Compiler,
        authority: OccurrenceAuthorityFacts {
            source: FactAvailability::Captured,
        },
        source: SourceSpan::new(path_atom, 14, 20),
    }];
    builder
        .add_borrowed_tree(BorrowedTree {
            versions: &[version(1), version(2)],
            items: &items,
            links: &links,
        })
        .map_err(|error| error.to_string())?;
    let ir = builder.finish().map_err(|error| error.to_string())?;
    let mut bytes = vec![0; full_semantic_image_len(&ir).map_err(|error| error.to_string())?];
    encode_full_semantic_image(&ir, &mut bytes).map_err(|error| error.to_string())?;
    Ok(bytes)
}

#[test]
fn missing_or_unavailable_sibling_profile_restricts_only_its_language() -> Result<(), String> {
    use backend_engine::builtin::{ProductSemanticPublicationRecord, SemanticUnavailableReason};
    use backend_semantic::vocabulary::Language;
    use view_build::RetargetingScope::{CompletePublication, SelectedArtifacts};

    let ts = LanguageProfile::TypeScript(TypeScriptSource::TypeScript);
    let tsx = LanguageProfile::TypeScript(TypeScriptSource::Tsx);
    let go = LanguageProfile::Go(GoVersion::Go125);
    let key = publication_key(ts, "pkg:npm/fixture@1.0.0")?;
    let identity = ContentId::<SourceFactDomain>::from_canonical_bytes(SOURCE);
    let mut current = sources(
        &key,
        backend_engine::SourceLanguage::TypeScript,
        &[
            ("call.ts", Some(identity)),
            ("screen.tsx", Some(identity)),
            ("main.go", Some(identity)),
        ],
    )?;
    current.files[2].1 = ProductSourceRecord::file(
        key.package_key().to_bytes(),
        "main.go",
        backend_engine::SourceLanguage::Go,
        [1; 32],
        [2; 32],
        Vec::<backend_compile::SourceDeclaration>::new(),
    )?
    .with_source_identity(identity)?;
    let available =
        view_build::SourceAvailability::of(&current).map_err(|error| error.to_string())?;
    let mut profiles = BTreeMap::from([(ts, CompletePublication), (go, CompletePublication)]);
    let expected = BTreeMap::from([
        (Language::TypeScript, SelectedArtifacts),
        (Language::Go, CompletePublication),
    ]);
    assert_eq!(
        available.retargeting_scopes(key.package_key(), &profiles),
        expected
    );

    let tsx_key = publication_key(tsx, "pkg:npm/fixture@1.0.0")?;
    view_build::SourceAvailability::record_publication_scope(
        &mut profiles,
        &tsx_key,
        &ProductSemanticPublicationRecord::Unavailable(SemanticUnavailableReason::Toolchain),
    );
    assert_eq!(profiles[&tsx], SelectedArtifacts);
    assert_eq!(
        available.retargeting_scopes(key.package_key(), &profiles),
        expected
    );

    let complete_profiles = BTreeMap::from([
        (ts, CompletePublication),
        (tsx, CompletePublication),
        (go, CompletePublication),
    ]);
    assert_eq!(
        available.retargeting_scopes(key.package_key(), &complete_profiles),
        BTreeMap::from([
            (Language::TypeScript, CompletePublication),
            (Language::Go, CompletePublication),
        ])
    );

    let rust_key = publication_key(
        LanguageProfile::Rust(RustEdition::Rust2021),
        "pkg:cargo/fixture@1.0.0",
    )?;
    view_build::SourceAvailability::record_publication_scope(
        &mut profiles,
        &rust_key,
        &ProductSemanticPublicationRecord::Unavailable(SemanticUnavailableReason::Toolchain),
    );
    assert_eq!(
        profiles[&LanguageProfile::Rust(RustEdition::Rust2024)],
        SelectedArtifacts
    );
    Ok(())
}

fn projected_view(
    package: backend_engine::PackageKey,
    images: &[backend_semantic::ir::SemanticImageView<'_>],
) -> Result<backend_engine::ViewRoot, String> {
    let (initial, _) = crate::builtin::initial_view().map_err(|error| error.to_string())?;
    let mut rows = Vec::new();
    for image in images {
        for entity in DocumentationSession::new(image).canonical_entities() {
            let entity = entity.map_err(|error| error.to_string())?;
            let identity = entity.entity.version.identity();
            rows.push(backend_engine::Row::in_package(
                backend_engine::RowId::Symbol(view_build::semantic_symbol(package, identity)),
                initial.basis(),
                package,
                view_build::semantic_coordinate(
                    "fixture",
                    identity,
                    std::str::from_utf8(entity.name.named_bytes().unwrap_or(b"<anonymous>"))
                        .map_err(|error| error.to_string())?,
                ),
            ));
        }
    }
    backend_engine::ViewRoot::new_checked(
        initial.recipe(),
        initial.basis(),
        initial.frontier(),
        rows,
        vec![backend_engine::ViewCoverage::Partial {
            lane: backend_engine::Lane::Semantic,
            completed: 0,
            total: 1,
        }],
        crate::builtin::test_builtin_view_capability().map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("{error:?}"))
}

#[test]
fn partial_selected_artifacts_answer_local_references_in_four_languages() -> Result<(), String> {
    use backend_engine::SourceLanguage;
    let cases = [
        (
            LanguageProfile::Rust(RustEdition::Rust2021),
            "src/call.rs",
            "src/inactive.rs",
            "pkg:cargo/fixture@1.0.0",
            NativeTool::Rustc,
            SourceLanguage::Rust,
        ),
        (
            LanguageProfile::Python(PythonVersion::Python314),
            "call.py",
            "inactive.py",
            "pkg:pypi/fixture@1.0.0",
            NativeTool::Python,
            SourceLanguage::Python,
        ),
        (
            LanguageProfile::Go(GoVersion::Go125),
            "call.go",
            "inactive.go",
            "pkg:golang/fixture@1.0.0",
            NativeTool::GoCompiler,
            SourceLanguage::Go,
        ),
        (
            LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            "call.ts",
            "inactive.ts",
            "pkg:npm/fixture@1.0.0",
            NativeTool::TypeScriptCompiler,
            SourceLanguage::TypeScript,
        ),
    ];
    for (profile, path, gap, coordinate, tool, language) in cases {
        let key = publication_key(profile, coordinate)?;
        let bytes = local_call_image(&key, path, tool)?;
        let identity = ContentId::<SourceFactDomain>::from_canonical_bytes(SOURCE);
        let current = sources(
            &key,
            language,
            &[(path, Some(identity)), (gap, Some(identity))],
        )?;
        let frontier =
            view_build::SourceAvailability::of(&current).map_err(|error| error.to_string())?;
        let selected = frontier
            .select(
                &key,
                partial(1, 2)?,
                activated(&[&bytes])?,
                &mut view_build::ImageRowResidence::default(),
            )
            .map_err(|error| error.to_string())?;
        if selected.images().len() != 1 {
            return Err(format!("{profile:?}: current artifact lost"));
        }
        let opened = selected
            .images()
            .map(|image| image.reopen().map_err(|error| error.to_string()))
            .collect::<Result<Vec<_>, _>>()?;
        let package = key.package_key();
        let target = view_build::semantic_symbol(package, version(2).identity());
        let caller = view_build::semantic_symbol(package, version(1).identity());
        let view = projected_view(package, &opened)?;
        let paths = BTreeSet::from([path.to_owned(), gap.to_owned()]);
        let facts = project_opened_reference_facts(
            &opened,
            &view,
            package,
            target,
            &paths,
            &[],
            None,
            &frontier.retargeting_scopes(package, &BTreeMap::new()),
            &[],
            true,
        )
        .map_err(|error| error.to_string())?;
        let graph = project_opened_semantic_graph(
            &opened,
            &view,
            package,
            target,
            backend_engine::RowId::Symbol(target),
            true,
            &paths,
            &frontier.retargeting_scopes(package, &BTreeMap::new()),
            &[],
            true,
        )
        .map_err(|error| error.to_string())?;
        let expected = backend_engine::GraphRelation::new(
            backend_engine::RowId::Symbol(caller),
            backend_engine::RowId::Symbol(target),
            backend_engine::SemanticLinkKind::Calls,
        );
        if graph != BTreeSet::from([expected]) {
            return Err(format!("{profile:?}: exact local graph relation was lost"));
        }
        if facts.len() != 1
            || facts[0].evidence.confidence != backend_engine::SemanticConfidence::Compiler
        {
            return Err(format!(
                "{profile:?}: selected artifact did not supply its exact local reference"
            ));
        }
    }
    Ok(())
}

#[test]
fn partial_artifacts_refuse_changed_missing_unidentified_and_foreign_sources() -> Result<(), String>
{
    let key = publication_key(
        LanguageProfile::Rust(RustEdition::Rust2024),
        "pkg:cargo/fixture@1.0.0",
    )?;
    let bytes = local_call_image(&key, "src/call.rs", NativeTool::Rustc)?;
    let identity = ContentId::<SourceFactDomain>::from_canonical_bytes(SOURCE);
    let changed = ContentId::<SourceFactDomain>::from_canonical_bytes(b"changed source");
    for files in [
        vec![("src/call.rs", Some(changed))],
        vec![("src/call.rs", None)],
        vec![("src/other.rs", Some(identity))],
    ] {
        let current = sources(&key, backend_engine::SourceLanguage::Rust, &files)?;
        let frontier =
            view_build::SourceAvailability::of(&current).map_err(|error| error.to_string())?;
        let selected = frontier
            .select(
                &key,
                partial(1, 2)?,
                activated(&[&bytes])?,
                &mut view_build::ImageRowResidence::default(),
            )
            .map_err(|error| error.to_string())?;
        if selected.images().len() != 0 {
            return Err("unproved current source admitted an artifact".to_owned());
        }
    }
    let current = sources(
        &key,
        backend_engine::SourceLanguage::Rust,
        &[("src/call.rs", Some(identity))],
    )?;
    let frontier =
        view_build::SourceAvailability::of(&current).map_err(|error| error.to_string())?;
    let wrong_key = publication_key(key.profile(), "pkg:cargo/fixture@2.0.0")?;
    let mut residence = view_build::ImageRowResidence::default();
    // Warm the valid admission first; a cached path/hash must not bypass the
    // new product coordinate's recipe/lineage admission.
    frontier
        .select(&key, partial(1, 2)?, activated(&[&bytes])?, &mut residence)
        .map_err(|error| error.to_string())?;
    if frontier
        .select(
            &wrong_key,
            partial(1, 2)?,
            activated(&[&bytes])?,
            &mut residence,
        )
        .is_ok()
    {
        return Err("another selected coordinate borrowed a resident image".to_owned());
    }
    let wrong_profile = publication_key(
        LanguageProfile::Rust(RustEdition::Rust2021),
        "pkg:cargo/fixture@1.0.0",
    )?;
    if frontier
        .select(
            &wrong_profile,
            partial(1, 2)?,
            activated(&[&bytes])?,
            &mut residence,
        )
        .is_ok()
    {
        return Err("another profile borrowed a resident image admission".to_owned());
    }
    let history = key.for_generation(
        backend_engine::publication::binding::CompilationBindingIdentity::from_encoded_bytes(
            b"history-only test binding",
        ),
    );
    if frontier
        .select(
            &history,
            partial(1, 2)?,
            activated(&[&bytes])?,
            &mut residence,
        )
        .is_ok()
    {
        return Err("an unselected history row supplied public artifacts".to_owned());
    }
    Ok(())
}

#[test]
fn historical_ripgrep_selected_images_keep_the_unbound_call_unbound() -> Result<(), String> {
    use backend_engine::publication::manifest::CompilationManifestView;
    use backend_semantic::ir::{ExternalTarget, ForeignTargetOrigin, VariantAvailability};

    // These are historical installed-c001 bytes, not a freshly compiled fixture
    // or an accepted current product State. The existing codecs establish the
    // image/manifest/source/recipe correspondence without an observer decoder.
    let main_bytes = include_bytes!("fixtures/ripgrep-installed-c001/main-semantic-image.bin");
    let parse_bytes = include_bytes!("fixtures/ripgrep-installed-c001/parse-semantic-image.bin");
    let manifest_bytes =
        include_bytes!("fixtures/ripgrep-installed-c001/actual-compilation-manifest.bin");
    let mut scratch = vec![None; 107];
    let manifest = CompilationManifestView::validate(manifest_bytes, &mut scratch)
        .map_err(|error| error.to_string())?;
    if manifest.fragment_count != 107 {
        return Err("historical selected manifest changed".to_owned());
    }
    let snapshots = [snapshot(main_bytes)?, snapshot(parse_bytes)?];
    let mut source_facts = Vec::new();
    let mut profile_coordinate = None;
    for image in &snapshots {
        let view = image.reopen().map_err(|error| error.to_string())?;
        let ImageProvenance::Captured {
            source,
            recipe,
            scope,
            ..
        } = view.image_facts().provenance
        else {
            return Err("historical artifact lost captured provenance".to_owned());
        };
        let matches = manifest
            .fragments()
            .filter(|entry| {
                entry.semantic_image.is_some_and(|facts| {
                    facts.identity == image.authority.identity
                        && facts.byte_length == image.authority.byte_len
                })
            })
            .collect::<Vec<_>>();
        if matches.len() != 1 || matches[0].source != source || matches[0].recipe != recipe {
            return Err(
                "historical image is detached from its selected manifest source/recipe".to_owned(),
            );
        }
        let coordinate = scope
            .coordinate
            .and_then(|atom| view.atom(atom))
            .ok_or("historical coordinate absent")?;
        let coordinate = std::str::from_utf8(coordinate)
            .map_err(|error| error.to_string())?
            .to_owned();
        let observed = (recipe.profile, coordinate);
        if profile_coordinate
            .as_ref()
            .is_some_and(|prior| prior != &observed)
        {
            return Err("historical images disagree on publication scope".to_owned());
        }
        profile_coordinate = Some(observed);
        source_facts.push((
            view_build::compiled_source_path(&view).map_err(|error| error.to_string())?,
            source.identity,
        ));
    }
    let (profile, coordinate) = profile_coordinate.ok_or("no historical scope")?;
    let key = publication_key(profile, &coordinate)?;
    let mut files = source_facts
        .iter()
        .map(|(path, identity)| (path.as_str(), Some(*identity)))
        .collect::<Vec<_>>();
    // The failed/inactive sources have no image in the selected manifest. An
    // available image cannot certify an absent source's facts or dependencies.
    let gap_ids = [
        (
            "crates/core/index/disabled.rs",
            "0e205524b04cb8c5cbb71dea962d97aff8262a260856477db297e9fc60566ca4",
        ),
        (
            "crates/searcher/src/testutil.rs",
            "0e7ce415bc0cdd1c1e1690d614b82533bd3e9052471c0e7734436cd2f5dce369",
        ),
        (
            "fuzz/fuzz_targets/fuzz_glob.rs",
            "0e225933ca009d88d0486406038d910c4d2b56a2c652dcdfc5cdf20c9387730f",
        ),
    ];
    for (path, hex) in gap_ids {
        let bytes = (0..hex.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&hex[at..at + 2], 16))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        let identity = ContentId::<SourceFactDomain>::try_from(bytes.as_slice())
            .map_err(|error| error.to_string())?;
        if manifest
            .fragments()
            .any(|entry| entry.source.identity == identity)
        {
            return Err(format!(
                "historical gap {path} unexpectedly has a selected artifact"
            ));
        }
        files.push((path, Some(identity)));
    }
    let current = sources(&key, backend_engine::SourceLanguage::Rust, &files)?;
    let frontier =
        view_build::SourceAvailability::of(&current).map_err(|error| error.to_string())?;
    // Only the two retained images are supplied to this test activation. This
    // 2/5 fixture fraction is not a fabricated whole historical generation.
    let selected = frontier
        .select(
            &key,
            partial(2, 5)?,
            ActivatedProductSemantics {
                native_program_sources: None,
                images: Arc::from(snapshots),
            },
            &mut view_build::ImageRowResidence::default(),
        )
        .map_err(|error| error.to_string())?;
    if selected.images().len() != 2 {
        return Err("an unrelated inactive gap suppressed a retained current image".to_owned());
    }
    let opened = selected
        .images()
        .map(|image| image.reopen().map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    let main = &opened[0];
    let parse = &opened[1];
    let target = DocumentationSession::new(parse)
        .canonical_entities()
        .find_map(|entity| match entity {
            Ok(entity)
                if entity.name.named_bytes() == Some(b"parse".as_slice())
                    && entity.entity.kind == ItemKind::Function =>
            {
                Some(Ok(entity.entity.version.identity()))
            }
            Ok(_) => None,
            Err(error) => Some(Err(error.to_string())),
        })
        .transpose()?
        .ok_or("historical parse declaration missing")?;
    let caller = DocumentationSession::new(main)
        .canonical_entities()
        .find_map(|entity| match entity {
            Ok(entity)
                if entity.name.named_bytes() == Some(b"main".as_slice())
                    && entity.entity.kind == ItemKind::Function =>
            {
                Some(Ok((entity.entity.id, entity.entity.version.identity())))
            }
            Ok(_) => None,
            Err(error) => Some(Err(error.to_string())),
        })
        .transpose()?
        .ok_or("historical main declaration missing")?;
    let call = main
        .links_from(caller.0)
        .find(|(_, link)| {
            link.source
                .is_some_and(|span| (span.start(), span.end()) == (1575, 1580))
        })
        .ok_or("historical main call missing")?
        .1;
    let LinkTarget::External(external) = call.target else {
        return Err("unbound historical call became local".to_owned());
    };
    let Some(ExternalTarget::Foreign(foreign)) = main.external(external) else {
        return Err("historical foreign target missing".to_owned());
    };
    if call.kind != LinkKind::Calls
        || call.confidence != backend_semantic::ir::Confidence::Compiler
        || !matches!(foreign.origin, ForeignTargetOrigin::Universe { .. })
        || !matches!(foreign.identity.variant, VariantAvailability::Unavailable)
        || main.atom(foreign.path) != Some(b"flags::parse".as_slice())
    {
        return Err("historical target evidence changed".to_owned());
    }
    let view = projected_view(key.package_key(), &opened)?;
    let paths = files.iter().map(|(path, _)| (*path).to_owned()).collect();
    let facts = project_opened_reference_facts(
        &opened,
        &view,
        key.package_key(),
        view_build::semantic_symbol(key.package_key(), target),
        &paths,
        &[],
        None,
        &frontier.retargeting_scopes(key.package_key(), &BTreeMap::new()),
        &[],
        true,
    )
    .map_err(|error| error.to_string())?;
    let caller_symbol = view_build::semantic_symbol(key.package_key(), caller.1);
    if facts.iter().any(|fact| {
        fact.site == caller_symbol && fact.relation == backend_engine::SemanticLinkKind::Calls
    }) {
        return Err(
            "available artifacts manufactured main -> parse without an exact binding".to_owned(),
        );
    }
    Ok(())
}

fn program_call_image(
    key: &ProductSemanticPublicationKey,
    path: &str,
    source: &[u8],
    target: Option<backend_semantic::ir::TypeScriptSourceCoordinate<'_>>,
) -> Result<Vec<u8>, String> {
    use backend_semantic::ir::{
        ExternalDeclarationIdentity, ExternalTarget, ForeignDeclarationId, ForeignExternalTarget,
        ForeignTargetOrigin, VariantAvailability,
    };
    let source = SourceIdentity::from_bytes(source).ok_or("source extent")?;
    let recipe = CompileRecipeFact::derive(
        key.profile(),
        Stage::LowerIr,
        NativeTool::TypeScriptCompiler,
        source.identity,
        ContentId::from_canonical_bytes(b"program fixture toolchain"),
    );
    let mut builder = IrBuilder::new();
    builder
        .set_image_provenance_for_package(source, recipe, key.coordinate(), path)
        .map_err(|e| e.to_string())?;
    let file = builder
        .intern_atom(path.as_bytes())
        .map_err(|e| e.to_string())?;
    let mut links = Vec::new();
    let caller = target.is_some();
    if let Some(target) = target {
        let ecosystem = builder
            .intern_atom(backend_semantic::ir::TYPESCRIPT_TSZ_SOURCE_ECOSYSTEM.as_bytes())
            .map_err(|e| e.to_string())?;
        let path = builder
            .intern_atom(target.encode().ok_or("coordinate")?.as_bytes())
            .map_err(|e| e.to_string())?;
        let display = builder.intern_atom(b"callee").map_err(|e| e.to_string())?;
        let external = builder
            .intern_external(ExternalTarget::Foreign(ForeignExternalTarget {
                identity: ExternalDeclarationIdentity {
                    foreign: ForeignDeclarationId::from_raw([9; 16]),
                    variant: VariantAvailability::Unavailable,
                },
                origin: ForeignTargetOrigin::Universe { ecosystem },
                path,
                display,
                kind: Some(ItemKind::Function),
            }))
            .map_err(|e| e.to_string())?;
        links.push(TreeLinkInput {
            from: TreeEntityId::new(0),
            target: TreeLinkTarget::External(external),
            kind: LinkKind::Calls,
            confidence: backend_semantic::ir::Confidence::Compiler,
            authority: OccurrenceAuthorityFacts {
                source: FactAvailability::Captured,
            },
            source: SourceSpan::new(file, 12, 18),
        });
    }
    builder
        .add_borrowed_tree(BorrowedTree {
            versions: &[version(if caller { 1 } else { 2 })],
            items: &[TreeItemInput {
                name: if caller { b"caller" } else { b"callee" },
                anonymous_callable_anchor: None,
                kind: ItemKind::Function,
                visibility: Visibility::Public,
                authority: EntityAuthorityFacts {
                    parentage: ParentageAuthority::Root,
                    source: FactAvailability::Captured,
                    source_file: FactAvailability::Captured,
                    visibility: FactAvailability::Captured,
                    ..EntityAuthorityFacts::default()
                },
                parent: None,
                semantic_type: None,
                members: &[],
                docs: &[],
                attributes: &[],
                source: SourceSpan::new(file, 0, 10),
                extension: None,
            }],
            links: &links,
        })
        .map_err(|e| e.to_string())?;
    let ir = builder.finish().map_err(|e| e.to_string())?;
    let mut bytes = vec![0; full_semantic_image_len(&ir).map_err(|e| e.to_string())?];
    encode_full_semantic_image(&ir, &mut bytes).map_err(|e| e.to_string())?;
    Ok(bytes)
}

#[test]
fn partial_full_program_witness_survives_unrelated_image_omission_but_not_source_change()
-> Result<(), String> {
    use backend_semantic::ir::{
        NativeProgramSource, NativeProgramSourceManifest, TypeScriptSourceCoordinate,
    };
    let key = publication_key(
        LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
        "pkg:npm/fixture@1.0.0",
    )?;
    let source = SourceIdentity::from_bytes(SOURCE).ok_or("source")?;
    let toolchain =
        ContentId::<ToolchainDomain>::from_canonical_bytes(b"program fixture toolchain");
    let rows = || {
        ["caller.ts", "callee.ts", "omitted.ts"]
            .into_iter()
            .map(|path| NativeProgramSource {
                program_path: format!("workspace/package/{path}"),
                source,
                package_path: Some(path.into()),
            })
            .chain([NativeProgramSource {
                program_path: "@compiler/lib.es5.d.ts".into(),
                source: SourceIdentity::from_bytes(b"interface Object {}").expect("small library"),
                package_path: None,
            }])
            .collect()
    };
    let manifest =
        NativeProgramSourceManifest::from_sources(toolchain, rows()).map_err(|e| e.to_string())?;
    let coordinate = TypeScriptSourceCoordinate {
        program: manifest.program(),
        source: *source.identity,
        path: "workspace/package/callee.ts",
        declaration_start: 0,
        declaration_end: 10,
        name_start: 0,
    };
    let caller = program_call_image(&key, "caller.ts", SOURCE, Some(coordinate))?;
    let callee = program_call_image(&key, "callee.ts", SOURCE, None)?;
    for case in 0..9 {
        let mut current = vec![
            ("caller.ts", Some(source.identity)),
            ("callee.ts", Some(source.identity)),
            ("omitted.ts", Some(source.identity)),
        ];
        match case {
            1 => current[2].1 = Some(ContentId::from_canonical_bytes(b"edited omitted member")),
            2 => current[2].1 = None,
            3 => {
                current.pop();
            }
            // Discovered configuration scripts outside the configured program
            // remain independent source facts, not native membership holes.
            8 => current.push((
                "eslint.config.mjs",
                Some(ContentId::from_canonical_bytes(b"export default [];")),
            )),
            _ => {}
        }
        let mut program_rows: Vec<_> = rows();
        if case == 4 {
            program_rows.last_mut().ok_or("library")?.source =
                SourceIdentity::from_bytes(b"changed SDK bytes").ok_or("sdk")?;
        }
        if case == 6 {
            program_rows[1].package_path = Some("wrong.ts".into());
        }
        let current_toolchain = if case == 5 {
            ContentId::from_canonical_bytes(b"changed compiler")
        } else {
            toolchain
        };
        let current_program =
            NativeProgramSourceManifest::from_sources(current_toolchain, program_rows)
                .map_err(|e| e.to_string())?;
        let frontier = view_build::SourceAvailability::of(&sources(
            &key,
            backend_engine::SourceLanguage::TypeScript,
            &current,
        )?)
        .map_err(|e| e.to_string())?;
        let mut activation = activated(&[&caller, &callee])?;
        activation.native_program_sources = if case == 7 {
            None
        } else {
            Some(Arc::new(current_program))
        };
        let selected = frontier
            .select(
                &key,
                partial(2, if case == 8 { 4 } else { 3 })?,
                activation,
                &mut view_build::ImageRowResidence::default(),
            )
            .map_err(|e| e.to_string())?;
        let opened = selected
            .images()
            .map(|image| image.reopen().map_err(|e| e.to_string()))
            .collect::<Result<Vec<_>, _>>()?;
        let package = key.package_key();
        let view = projected_view(package, &opened)?;
        let target = view_build::semantic_symbol(package, version(2).identity());
        let programs = selected.program().into_iter().collect::<Vec<_>>();
        let facts = project_opened_reference_facts(
            &opened,
            &view,
            package,
            target,
            &current.iter().map(|(path, _)| (*path).to_owned()).collect(),
            &[],
            None,
            &frontier.retargeting_scopes(package, &BTreeMap::new()),
            &programs,
            !selected.has_program_manifest(),
        )
        .map_err(|e| e.to_string())?;
        let caller_symbol = view_build::semantic_symbol(package, version(1).identity());
        assert_eq!(
            facts.iter().any(|fact| fact.site == caller_symbol
                && fact.relation == backend_engine::SemanticLinkKind::Calls),
            matches!(case, 0 | 8),
            "case {case}: only current native membership proves the exact cross-source call"
        );
        let graph = project_opened_semantic_graph(
            &opened,
            &view,
            package,
            target,
            backend_engine::RowId::Symbol(target),
            true,
            &current.iter().map(|(path, _)| (*path).to_owned()).collect(),
            &frontier.retargeting_scopes(package, &BTreeMap::new()),
            &programs,
            !selected.has_program_manifest(),
        )
        .map_err(|e| e.to_string())?;
        let relation = backend_engine::GraphRelation::new(
            backend_engine::RowId::Symbol(caller_symbol),
            backend_engine::RowId::Symbol(target),
            backend_engine::SemanticLinkKind::Calls,
        );
        assert_eq!(
            graph.contains(&relation),
            matches!(case, 0 | 8),
            "case {case}: graph uses the same exact proof as references"
        );
        assert_eq!(
            selected.images().len(),
            2,
            "missing program proof does not discard current positive artifacts"
        );
    }
    Ok(())
}

#[test]
fn partial_program_cannot_borrow_another_programs_same_source_target_image() -> Result<(), String> {
    use backend_semantic::ir::{
        NativeProgramSource, NativeProgramSourceManifest, TypeScriptSourceCoordinate,
    };
    let key = publication_key(
        LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
        "pkg:npm/fixture@1.0.0",
    )?;
    let source = SourceIdentity::from_bytes(SOURCE).ok_or("source")?;
    let toolchain =
        ContentId::<ToolchainDomain>::from_canonical_bytes(b"program fixture toolchain");
    let program = |sdk: &[u8]| {
        NativeProgramSourceManifest::from_sources(
            toolchain,
            ["caller.ts", "callee.ts", "omitted.ts"]
                .into_iter()
                .map(|path| NativeProgramSource {
                    program_path: format!("workspace/package/{path}"),
                    source,
                    package_path: Some(path.to_owned()),
                })
                .chain([NativeProgramSource {
                    program_path: "@compiler/lib.fixture.d.ts".into(),
                    source: SourceIdentity::from_bytes(sdk).expect("bounded SDK fixture"),
                    package_path: None,
                }])
                .collect(),
        )
        .map_err(|e| e.to_string())
    };
    let first_program = Arc::new(program(b"original SDK")?);
    let second_program = Arc::new(program(b"different SDK")?);
    assert_ne!(first_program.program(), second_program.program());
    let caller = program_call_image(
        &key,
        "caller.ts",
        SOURCE,
        Some(TypeScriptSourceCoordinate {
            program: first_program.program(),
            source: *source.identity,
            path: "workspace/package/callee.ts",
            declaration_start: 0,
            declaration_end: 10,
            name_start: 0,
        }),
    )?;
    let target_bytes = program_call_image(&key, "callee.ts", SOURCE, None)?;
    let current = [
        ("caller.ts", Some(source.identity)),
        ("callee.ts", Some(source.identity)),
        ("omitted.ts", Some(source.identity)),
    ];
    let frontier = view_build::SourceAvailability::of(&sources(
        &key,
        backend_engine::SourceLanguage::TypeScript,
        &current,
    )?)
    .map_err(|e| e.to_string())?;
    for case in 0..3 {
        // These are two separately admitted artifact-selection fixtures, not
        // current installed compiler results. The caller's program omits its
        // target image in case 0; an unrelated program emits identical target
        // source/image bytes. That second owner cannot certify the first join.
        let mut first = if case == 1 {
            activated(&[&caller, &target_bytes])?
        } else {
            activated(&[&caller])?
        };
        first.native_program_sources = Some(Arc::clone(&first_program));
        let mut second = activated(&[&target_bytes])?;
        second.native_program_sources = Some(Arc::clone(if case == 2 {
            &first_program
        } else {
            &second_program
        }));
        let mut residence = view_build::ImageRowResidence::default();
        let selected = [
            frontier
                .select(
                    &key,
                    partial(if case == 1 { 2 } else { 1 }, 3)?,
                    first,
                    &mut residence,
                )
                .map_err(|e| e.to_string())?,
            frontier
                .select(&key, partial(1, 3)?, second, &mut residence)
                .map_err(|e| e.to_string())?,
        ];
        assert!(
            selected
                .iter()
                .all(|activation| activation.program().is_some())
        );
        let opened = selected
            .iter()
            .flat_map(|activation| activation.images())
            .map(|image| image.reopen().map_err(|e| e.to_string()))
            .collect::<Result<Vec<_>, _>>()?;
        let package = key.package_key();
        // Keep one presentation row per declaration even when the same
        // target image has two legitimate activation owners in case 1.
        let presentation = [&caller, &target_bytes]
            .into_iter()
            .map(|bytes| {
                backend_semantic::ir::SemanticImageView::reopen(bytes).map_err(|e| e.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let view = projected_view(package, &presentation)?;
        let target = view_build::semantic_symbol(package, version(2).identity());
        let caller_symbol = view_build::semantic_symbol(package, version(1).identity());
        let programs = selected
            .iter()
            .filter_map(|activation| activation.program())
            .collect::<Vec<_>>();
        let paths = current.iter().map(|(path, _)| (*path).to_owned()).collect();
        let scopes = frontier.retargeting_scopes(package, &BTreeMap::new());
        let facts = project_opened_reference_facts(
            &opened,
            &view,
            package,
            target,
            &paths,
            &[],
            None,
            &scopes,
            &programs,
            false,
        )
        .map_err(|e| e.to_string())?;
        let graph = project_opened_semantic_graph(
            &opened,
            &view,
            package,
            target,
            backend_engine::RowId::Symbol(target),
            true,
            &paths,
            &scopes,
            &programs,
            false,
        )
        .map_err(|e| e.to_string())?;
        assert_eq!(
            facts.iter().any(|fact| fact.site == caller_symbol
                && fact.relation == backend_engine::SemanticLinkKind::Calls),
            case != 0,
            "case {case}: only an image owned by the exact native program may certify the target"
        );
        assert_eq!(
            graph.contains(&backend_engine::GraphRelation::new(
                backend_engine::RowId::Symbol(caller_symbol),
                backend_engine::RowId::Symbol(target),
                backend_engine::SemanticLinkKind::Calls,
            )),
            case != 0,
            "case {case}: graph uses the same owning image proof"
        );
        assert_eq!(
            selected[1].images().len(),
            1,
            "unrelated positive target facts remain available"
        );
    }
    Ok(())
}

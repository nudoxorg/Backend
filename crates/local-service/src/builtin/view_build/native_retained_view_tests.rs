//! Complete native image stays available for stale view rows, not current references.
use super::*;
use crate::builtin::{
    ActivatedProductSemantics, IndexedProject, IndexedSources, ProductSourceRecord,
};
use backend_engine::application::{
    LocalCompilerHost, LocalHostDiscovery, LocalHostEnvironment, LocalHostVariable,
    OwnedPackageSource, OwnedPackageSourceSet,
};
use backend_engine::builtin::{ProductSemanticPublicationKey, SemanticPublicationCoverage};
use backend_library::interface::{
    CorrelationId, GenerateTarget, PackageCompileRequest, SemanticImageAuthority,
    SemanticImageSnapshot,
};
use backend_semantic::vocabulary::{LanguageProfile, PackageUrl, Stage, TypeScriptSource};
use backend_version::{ArtifactId, ContentId, SourceFactDomain};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs,
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

struct Root(PathBuf);
impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct Env(PathBuf);
impl LocalHostEnvironment for Env {
    fn value(&self, key: LocalHostVariable) -> Option<OsString> {
        (key == LocalHostVariable::NudoxDataRoot).then(|| self.0.clone().into_os_string())
    }
    fn search_path(&self) -> Option<OsString> {
        Some(OsString::new())
    }
}

#[test]
fn complete_native_image_retains_stale_view_row_but_not_current_query()
-> Result<(), Box<dyn std::error::Error>> {
    let root = Root(std::env::temp_dir().join(format!(
        "nudox-native-retained-view-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    )));
    let project = root.0.join("project");
    fs::create_dir_all(&project)?;
    let original = "export function foo() { return 1; }\n";
    let changed = "export function foo() { return 2; }\n";
    fs::write(project.join("foo.ts"), original)?;
    fs::write(
        project.join("tsconfig.json"),
        r#"{"compilerOptions":{"noLib":true,"types":[]},"files":["foo.ts"]}"#,
    )?;
    let client = LocalCompilerHost::new(
        Env(root.0.join("compiler")),
        LocalHostDiscovery::ExplicitOnly,
    )
    .open()?;
    let profile = LanguageProfile::TypeScript(TypeScriptSource::TypeScript);
    let coordinate = PackageUrl::parse("pkg:npm/fixture@1.0.0".to_owned())
        .map_err(|error| format!("coordinate: {error:?}"))?;
    let request = PackageCompileRequest::new(
        GenerateTarget {
            correlation: CorrelationId(225),
            profile,
            stage: Stage::LowerIr,
        },
        coordinate.clone(),
    )
    .map_err(|error| format!("profile: {error:?}"))?;
    let sources = vec![OwnedPackageSource::new("foo.ts", original)?].into_boxed_slice();
    let staged = client.compile_package_sources_staged(OwnedPackageSourceSet::new(
        request,
        project.clone(),
        sources,
    )?)?;
    assert_eq!(staged.artifacts().len(), 1);
    fs::write(project.join("foo.ts"), changed)?;
    let bytes = staged
        .semantic_output_object(0)
        .ok_or("image absent")?
        .bytes();
    let snapshot = SemanticImageSnapshot::try_from_reopened(
        SemanticImageAuthority {
            identity: ArtifactId::from_encoded_bytes(bytes),
            byte_len: u32::try_from(bytes.len())?,
        },
        bytes,
    )
    .map_err(|error| format!("snapshot: {error:?}"))?;
    let key = ProductSemanticPublicationKey::new(
        backend_engine::PackageReference::parse("fixture".to_owned())
            .map_err(|error| format!("package: {error:?}"))?,
        coordinate,
        profile,
    )?;
    let package = key.package_key();
    let id_a = ContentId::<SourceFactDomain>::from_canonical_bytes(original.as_bytes());
    let id_b = ContentId::<SourceFactDomain>::from_canonical_bytes(changed.as_bytes());
    let file_key = backend_engine::product_source_file_key(package.to_bytes(), "foo.ts");
    let record = ProductSourceRecord::file_with_retention(
        package.to_bytes(),
        "foo.ts",
        backend_engine::SourceLanguage::TypeScript,
        [1; 32],
        [2; 32],
        Vec::<backend_compile::SourceDeclaration>::new(),
        backend_engine::DeclarationRetention::Unavailable(
            backend_library::SourceUnavailableReason::Unparsed,
        ),
    )?
    .with_source_identity(id_b)?;
    let indexed_project = IndexedProject {
        package,
        label: "fixture".into(),
        files: Arc::from([file_key]),
    };
    let indexed = IndexedSources {
        projects: BTreeMap::from([(package.to_bytes(), indexed_project)]),
        files: vec![(file_key, record)],
        cargo_aliases: BTreeMap::new(),
        source_snapshot: None,
    };
    let availability = SourceAvailability::of(&indexed)?;
    let snapshots: Arc<[SemanticImageSnapshot]> = Arc::from([snapshot]);
    let activated = || ActivatedProductSemantics {
        native_program_sources: None,
        images: Arc::clone(&snapshots),
    };
    let retained = availability.select(
        &key,
        SemanticPublicationCoverage::Complete,
        activated(),
        &mut ImageRowResidence::default(),
    )?;
    assert_eq!(
        retained.images().len(),
        1,
        "complete old image remains for stale view projection"
    );
    let strict = availability.select_current(
        &key,
        SemanticPublicationCoverage::Complete,
        activated(),
        &mut ImageRowResidence::default(),
    )?;
    assert_eq!(
        strict.images().len(),
        0,
        "references reject old image as current evidence"
    );
    let image = retained
        .images()
        .next()
        .ok_or("retained image missing")?
        .reopen()?;
    let decision = freshness_decision(
        &BTreeMap::from([("foo.ts".to_owned(), id_a)]),
        &BTreeSet::from(["foo.ts".to_owned()]),
        Some(&BTreeMap::from([("foo.ts".to_owned(), id_b)])),
    );
    assert!(decision.stale_paths.contains("foo.ts"));
    let (base, _) = crate::builtin::initial_view()?;
    let projected = semantic::project_image_rows(
        &image,
        *blake3::hash(bytes).as_bytes(),
        indexed
            .projects
            .get(&package.to_bytes())
            .ok_or("project missing")?,
        profile,
        base.basis(),
        decision.image_stale("foo.ts", id_a),
        "foo.ts",
        &[],
        None,
        None,
    )?;
    let foo = projected
        .rows
        .iter()
        .find(|row| row.row.label.ends_with("::foo"))
        .ok_or("stale foo row missing")?;
    assert_eq!(
        foo.row.source,
        backend_library::SourceAvailability::NotCaptured
    );
    assert!(foo.row.document.iter().any(|fragment| matches!(fragment, backend_library::Fragment::Text(text) if text.to_ascii_lowercase().contains("stale"))));
    drop(client);
    Ok(())
}

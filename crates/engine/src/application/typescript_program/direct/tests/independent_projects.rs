//! Actual Angular03 authored bytes, hash-checked against its closed source admission.
//! The dependency environment is intentionally empty; these are linked TSZ root /
//! closure membership and ownership controls, not runtime acceptance claims.
use super::*;
use crate::application::typescript_host::TypeScriptProjectHost;
use std::{
    fs,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const CORPUS: &[(&str, &str)] = &[
    (
        "angular.json",
        include_str!("fixtures/angular-current42/angular.json"),
    ),
    (
        "package.json",
        include_str!("fixtures/angular-current42/package.json"),
    ),
    (
        "playwright.config.ts",
        include_str!("fixtures/angular-current42/playwright.config.ts"),
    ),
    (
        "src/app/app.component.ts",
        include_str!("fixtures/angular-current42/src/app/app.component.ts"),
    ),
    (
        "src/app/app.config.ts",
        include_str!("fixtures/angular-current42/src/app/app.config.ts"),
    ),
    (
        "src/app/app.routes.ts",
        include_str!("fixtures/angular-current42/src/app/app.routes.ts"),
    ),
    (
        "src/app/core/auth/auth.component.ts",
        include_str!("fixtures/angular-current42/src/app/core/auth/auth.component.ts"),
    ),
    (
        "src/app/core/auth/if-authenticated.directive.ts",
        include_str!("fixtures/angular-current42/src/app/core/auth/if-authenticated.directive.ts"),
    ),
    (
        "src/app/core/auth/services/jwt.service.spec.ts",
        include_str!("fixtures/angular-current42/src/app/core/auth/services/jwt.service.spec.ts"),
    ),
    (
        "src/app/core/auth/services/jwt.service.ts",
        include_str!("fixtures/angular-current42/src/app/core/auth/services/jwt.service.ts"),
    ),
    (
        "src/app/core/auth/services/user.service.spec.ts",
        include_str!("fixtures/angular-current42/src/app/core/auth/services/user.service.spec.ts"),
    ),
    (
        "src/app/core/auth/services/user.service.ts",
        include_str!("fixtures/angular-current42/src/app/core/auth/services/user.service.ts"),
    ),
    (
        "src/app/core/auth/user.model.ts",
        include_str!("fixtures/angular-current42/src/app/core/auth/user.model.ts"),
    ),
    (
        "src/app/core/interceptors/api.interceptor.ts",
        include_str!("fixtures/angular-current42/src/app/core/interceptors/api.interceptor.ts"),
    ),
    (
        "src/app/core/interceptors/error.interceptor.ts",
        include_str!("fixtures/angular-current42/src/app/core/interceptors/error.interceptor.ts"),
    ),
    (
        "src/app/core/interceptors/token.interceptor.ts",
        include_str!("fixtures/angular-current42/src/app/core/interceptors/token.interceptor.ts"),
    ),
    (
        "src/app/core/layout/footer.component.ts",
        include_str!("fixtures/angular-current42/src/app/core/layout/footer.component.ts"),
    ),
    (
        "src/app/core/layout/header.component.ts",
        include_str!("fixtures/angular-current42/src/app/core/layout/header.component.ts"),
    ),
    (
        "src/app/core/models/errors.model.ts",
        include_str!("fixtures/angular-current42/src/app/core/models/errors.model.ts"),
    ),
    (
        "src/app/core/models/loading-state.model.ts",
        include_str!("fixtures/angular-current42/src/app/core/models/loading-state.model.ts"),
    ),
    (
        "src/app/features/article/components/article-comment.component.ts",
        include_str!(
            "fixtures/angular-current42/src/app/features/article/components/article-comment.component.ts"
        ),
    ),
    (
        "src/app/features/article/components/article-list.component.ts",
        include_str!(
            "fixtures/angular-current42/src/app/features/article/components/article-list.component.ts"
        ),
    ),
    (
        "src/app/features/article/components/article-meta.component.ts",
        include_str!(
            "fixtures/angular-current42/src/app/features/article/components/article-meta.component.ts"
        ),
    ),
    (
        "src/app/features/article/components/article-preview.component.ts",
        include_str!(
            "fixtures/angular-current42/src/app/features/article/components/article-preview.component.ts"
        ),
    ),
    (
        "src/app/features/article/components/favorite-button.component.ts",
        include_str!(
            "fixtures/angular-current42/src/app/features/article/components/favorite-button.component.ts"
        ),
    ),
    (
        "src/app/features/article/models/article-list-config.model.ts",
        include_str!(
            "fixtures/angular-current42/src/app/features/article/models/article-list-config.model.ts"
        ),
    ),
    (
        "src/app/features/article/models/article.model.ts",
        include_str!("fixtures/angular-current42/src/app/features/article/models/article.model.ts"),
    ),
    (
        "src/app/features/article/models/comment.model.ts",
        include_str!("fixtures/angular-current42/src/app/features/article/models/comment.model.ts"),
    ),
    (
        "src/app/features/article/pages/article/article.component.ts",
        include_str!(
            "fixtures/angular-current42/src/app/features/article/pages/article/article.component.ts"
        ),
    ),
    (
        "src/app/features/article/pages/editor/editor.component.ts",
        include_str!(
            "fixtures/angular-current42/src/app/features/article/pages/editor/editor.component.ts"
        ),
    ),
    (
        "src/app/features/article/pages/home/home.component.ts",
        include_str!(
            "fixtures/angular-current42/src/app/features/article/pages/home/home.component.ts"
        ),
    ),
    (
        "src/app/features/article/services/articles.service.spec.ts",
        include_str!(
            "fixtures/angular-current42/src/app/features/article/services/articles.service.spec.ts"
        ),
    ),
    (
        "src/app/features/article/services/articles.service.ts",
        include_str!(
            "fixtures/angular-current42/src/app/features/article/services/articles.service.ts"
        ),
    ),
    (
        "src/app/features/article/services/comments.service.spec.ts",
        include_str!(
            "fixtures/angular-current42/src/app/features/article/services/comments.service.spec.ts"
        ),
    ),
    (
        "src/app/features/article/services/comments.service.ts",
        include_str!(
            "fixtures/angular-current42/src/app/features/article/services/comments.service.ts"
        ),
    ),
    (
        "src/app/features/article/services/tags.service.spec.ts",
        include_str!(
            "fixtures/angular-current42/src/app/features/article/services/tags.service.spec.ts"
        ),
    ),
    (
        "src/app/features/article/services/tags.service.ts",
        include_str!(
            "fixtures/angular-current42/src/app/features/article/services/tags.service.ts"
        ),
    ),
    (
        "src/app/features/profile/components/follow-button.component.ts",
        include_str!(
            "fixtures/angular-current42/src/app/features/profile/components/follow-button.component.ts"
        ),
    ),
    (
        "src/app/features/profile/components/profile-articles.component.ts",
        include_str!(
            "fixtures/angular-current42/src/app/features/profile/components/profile-articles.component.ts"
        ),
    ),
    (
        "src/app/features/profile/components/profile-favorites.component.ts",
        include_str!(
            "fixtures/angular-current42/src/app/features/profile/components/profile-favorites.component.ts"
        ),
    ),
    (
        "src/app/features/profile/models/profile.model.ts",
        include_str!("fixtures/angular-current42/src/app/features/profile/models/profile.model.ts"),
    ),
    (
        "src/app/features/profile/pages/profile/profile.component.ts",
        include_str!(
            "fixtures/angular-current42/src/app/features/profile/pages/profile/profile.component.ts"
        ),
    ),
    (
        "src/app/features/profile/profile.routes.ts",
        include_str!("fixtures/angular-current42/src/app/features/profile/profile.routes.ts"),
    ),
    (
        "src/app/features/profile/services/profile.service.spec.ts",
        include_str!(
            "fixtures/angular-current42/src/app/features/profile/services/profile.service.spec.ts"
        ),
    ),
    (
        "src/app/features/profile/services/profile.service.ts",
        include_str!(
            "fixtures/angular-current42/src/app/features/profile/services/profile.service.ts"
        ),
    ),
    (
        "src/app/features/settings/settings.component.ts",
        include_str!("fixtures/angular-current42/src/app/features/settings/settings.component.ts"),
    ),
    (
        "src/app/shared/components/list-errors.component.ts",
        include_str!(
            "fixtures/angular-current42/src/app/shared/components/list-errors.component.ts"
        ),
    ),
    (
        "src/app/shared/pipes/default-image.pipe.ts",
        include_str!("fixtures/angular-current42/src/app/shared/pipes/default-image.pipe.ts"),
    ),
    (
        "src/app/shared/pipes/markdown.pipe.ts",
        include_str!("fixtures/angular-current42/src/app/shared/pipes/markdown.pipe.ts"),
    ),
    (
        "src/main.ts",
        include_str!("fixtures/angular-current42/src/main.ts"),
    ),
    (
        "src/test-setup.ts",
        include_str!("fixtures/angular-current42/src/test-setup.ts"),
    ),
    (
        "tsconfig.app.json",
        include_str!("fixtures/angular-current42/tsconfig.app.json"),
    ),
    (
        "tsconfig.json",
        include_str!("fixtures/angular-current42/tsconfig.json"),
    ),
    (
        "tsconfig.spec.json",
        include_str!("fixtures/angular-current42/tsconfig.spec.json"),
    ),
    (
        "vitest.config.ts",
        include_str!("fixtures/angular-current42/vitest.config.ts"),
    ),
];
struct Fixture(PathBuf);
impl Fixture {
    fn captured() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "nudox-angular-project-set-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let fixture = Self(root.canonicalize().unwrap());
        for (path, source) in CORPUS {
            let path = fixture.0.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, source).unwrap();
        }
        fixture
    }
    fn sources(&self) -> Vec<PackageSource<'static>> {
        CORPUS
            .iter()
            .filter(|(path, _)| path.ends_with(".ts"))
            .map(|(path, source)| PackageSource::new(*path, *source).unwrap())
            .collect()
    }
    fn admit(&self) -> crate::application::typescript_host::AdmittedTypeScriptProject {
        TypeScriptProjectHost::direct(
            None,
            ToolchainProbeLimits::new(Duration::from_secs(5), NonZeroUsize::new(1024).unwrap())
                .unwrap(),
        )
        .admit(&self.0)
        .unwrap()
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

// A separate small native project exercises alias lowering as well as admission.
// Native target coordinates retain the querying project's identity; durable
// package ownership retains the producer-selected leaf. The query join must
// authenticate their shared source membership before joining those identities.
fn assert_native_cross_project_alias_ownership() {
    use crate::driver::{
        CompileControl, CompileRequest, CompileScratch, DeclarationScope, ResolvedToolchain,
        SemanticAuthorityInput, ToolchainSelection,
    };
    use backend_semantic::ir::{
        EntityKind, ExternalTarget, ForeignTargetOrigin, NativeProgramNode, NativeProgramSource,
        NativeProgramSourceManifest, NativeProgramSources, PackageLineage, SemanticCoreReader as _,
        SemanticImageIdentity, SemanticImageView, SourceIdentity, TYPESCRIPT_TSZ_SOURCE_ECOSYSTEM,
        TypeScriptSourceCoordinate, encode_full_semantic_image, full_semantic_image_len,
    };
    use backend_semantic::vocabulary::{LanguageProfile, Stage, TypeScriptSource};

    const PRIMARY: &str = "export function exported(value: number): number { return value + 1; }\n";
    const CALLER: &str = "import { exported as alias } from './src/primary';\nexport function read(): number { return alias(1); }\n";
    let fixture = Fixture::captured();
    fs::write(fixture.0.join("src/primary.ts"), PRIMARY).unwrap();
    fs::write(fixture.0.join("supplemental.ts"), CALLER).unwrap();
    fs::write(fixture.0.join("tsconfig.json"),
        r#"{"compilerOptions":{"noLib":true,"types":[],"module":"ESNext","moduleResolution":"bundler"},"files":["src/primary.ts","supplemental.ts"]}"#).unwrap();
    fs::write(
        fixture.0.join("tsconfig.app.json"),
        r#"{"extends":"./tsconfig.json","files":["src/primary.ts"]}"#,
    )
    .unwrap();
    let native_work = fixture.0.join("native-work");
    fs::create_dir(&native_work).unwrap();
    let admitted = fixture.admit();
    assert!(admitted.inputs().compiler_api().is_err());
    let sources = [
        PackageSource::new("src/primary.ts", PRIMARY).unwrap(),
        PackageSource::new("supplemental.ts", CALLER).unwrap(),
    ];
    let forest = build_native_forest_inputs(
        &admitted.inputs(),
        &sources,
        Instant::now() + Duration::from_secs(30),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(forest.project_set);
    let app = forest.root as usize;
    let conventional = forest
        .nodes
        .iter()
        .position(|node| node.config.as_ref().unwrap().0 == "tsconfig.json")
        .unwrap();
    assert!(matches!(forest.package_membership["src/primary.ts"],
        TypeScriptForestSourceMembership::Selected { program, .. } if program == app));
    assert!(matches!(forest.package_membership["supplemental.ts"],
        TypeScriptForestSourceMembership::Selected { program, .. } if program == conventional));
    let metadata = forest
        .nodes
        .iter()
        .map(|node| {
            (
                node.config.clone().unwrap(),
                node.references.clone(),
                *node.native.options.environment.as_bytes(),
            )
        })
        .collect::<Vec<_>>();
    let membership = forest.package_membership;
    let mut authorities = Vec::new();
    for node in forest.nodes {
        let mut authority = backend_frontend_typescript::TszProjectAuthority::new();
        authority
            .update(
                node.native.sources,
                node.native.options,
                &node.native.libraries,
            )
            .unwrap();
        authorities.push(authority);
    }
    let toolchain = ResolvedToolchain::native_typescript();
    assert!(
        toolchain.external().is_err(),
        "linked TSZ has no executable authority"
    );
    let manifests = authorities
        .iter()
        .enumerate()
        .map(|(index, authority)| {
            let project = authority.project().unwrap();
            NativeProgramSourceManifest::from_sources(
                toolchain.identity,
                project
                    .program()
                    .files
                    .iter()
                    .map(|file| {
                        let source = file.arena.get_source_file_at(file.source_file).unwrap();
                        NativeProgramSource {
                            program_path: file.file_name.clone(),
                            source: SourceIdentity::from_bytes(source.text.as_bytes()).unwrap(),
                            package_path: membership.iter().find_map(
                                |(path, member)| match member {
                                    TypeScriptForestSourceMembership::Selected {
                                        program,
                                        program_path,
                                    } if *program == index
                                        && program_path.as_ref() == file.file_name =>
                                    {
                                        Some(path.to_string())
                                    }
                                    _ => None,
                                },
                            ),
                        }
                    })
                    .collect(),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    assert_ne!(manifests[app].program(), manifests[conventional].program());
    let nodes = metadata
        .into_iter()
        .zip(manifests.iter())
        .map(
            |(((path, config), references, environment), program)| NativeProgramNode {
                config_path: path,
                config,
                references,
                environment,
                program: Some(program.clone()),
            },
        )
        .collect();
    let owners =
        NativeProgramSources::from_project_set(toolchain.identity, app as u32, nodes).unwrap();
    let mut encoded = BTreeMap::new();
    for (index, path, source) in [
        (app, "src/primary.ts", PRIMARY),
        (conventional, "supplemental.ts", CALLER),
    ] {
        let project = authorities[index].project().unwrap();
        let session = project
            .checked_query_session_unmetered_for_test()
            .unwrap()
            .authenticate_native_program_sources(&manifests[index])
            .unwrap();
        let cancelled = AtomicBool::new(false);
        let mut diagnostics = [0u8; 4096];
        let compiled = crate::driver::compile_ir(
            CompileRequest {
                profile: LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
                stage: Stage::LowerIr,
                source: source.as_bytes(),
                declaration_scope: DeclarationScope::new(
                    PackageLineage::new("fixture", "independent-alias").unwrap(),
                    path,
                )
                .unwrap(),
                toolchain: ToolchainSelection::ResolvedNative(toolchain),
                authority: SemanticAuthorityInput::TypeScriptTsz {
                    project,
                    session: &session,
                    source_path: path,
                },
                control: CompileControl {
                    deadline: Instant::now() + Duration::from_secs(30),
                    cancelled: &cancelled,
                },
            },
            CompileScratch {
                diagnostic_output: &mut diagnostics,
                native_work: &native_work,
            },
        )
        .unwrap();
        let mut bytes = vec![0; full_semantic_image_len(&compiled.ir).unwrap()];
        encode_full_semantic_image(&compiled.ir, &mut bytes).unwrap();
        encoded.insert(path, bytes);
    }
    let owners = owners
        .with_image_bindings(encoded.iter().map(|(path, bytes)| {
            (
                path.to_string(),
                *SemanticImageIdentity::from_encoded_bytes(bytes).as_ref(),
            )
        }))
        .unwrap();
    let cold = NativeProgramSources::decode(owners.as_bytes()).unwrap();
    let primary = SemanticImageView::reopen(&encoded["src/primary.ts"]).unwrap();
    let target = primary
        .canonical_entities()
        .find(|entity| {
            entity.kind == EntityKind::Function
                && entity.name.named_atom().and_then(|name| primary.atom(name))
                    == Some(b"exported".as_slice())
        })
        .unwrap();
    let extent = target.source.unwrap();
    let caller = SemanticImageView::reopen(&encoded["supplemental.ts"]).unwrap();
    let coordinates = caller
        .external_targets()
        .filter_map(|target| {
            let ExternalTarget::Foreign(target) = target else {
                return None;
            };
            let ForeignTargetOrigin::Universe { ecosystem } = target.origin else {
                return None;
            };
            if caller.atom(ecosystem) != Some(TYPESCRIPT_TSZ_SOURCE_ECOSYSTEM.as_bytes()) {
                return None;
            }
            caller
                .atom(target.path)
                .and_then(|raw| std::str::from_utf8(raw).ok())
                .and_then(TypeScriptSourceCoordinate::decode)
                .filter(|coordinate| coordinate.path == "src/primary.ts")
        })
        .collect::<Vec<_>>();
    assert!(
        !coordinates.is_empty(),
        "real alias import/call lowers an exact native target"
    );
    let actual_owner = cold
        .sources()
        .find(|row| row.package_path == Some("src/primary.ts"))
        .unwrap();
    for coordinate in coordinates {
        assert_eq!(
            coordinate.program,
            manifests[conventional].program(),
            "native query origin is preserved"
        );
        assert_ne!(
            coordinate.program, actual_owner.program,
            "query origin is not the target package owner"
        );
        assert_eq!(actual_owner.program, manifests[app].program());
        assert_eq!(coordinate.source, *actual_owner.source.identity);
        assert_eq!(coordinate.path, actual_owner.program_path);
        assert_eq!(
            (coordinate.declaration_start, coordinate.declaration_end),
            (extent.start(), extent.end())
        );
        assert_eq!(
            coordinate.name_start as usize,
            PRIMARY.find("exported").unwrap()
        );
        assert!(
            cold.sources().any(|row| row.program == coordinate.program
                && row.program_path == coordinate.path
                && *row.source.identity == coordinate.source
                && row.package_path.is_none()),
            "querying project authentically reads the exact target as a dependency"
        );
    }
    let conventional_project = authorities[conventional].project().unwrap();
    let app_project = authorities[app].project().unwrap();
    let session = conventional_project
        .checked_query_session_unmetered_for_test()
        .unwrap();
    let index = session.file_index("src/primary.ts").unwrap();
    let foreign_session = app_project
        .checked_query_session_unmetered_for_test()
        .unwrap();
    let foreign_file =
        &app_project.program().files[foreign_session.file_index("src/primary.ts").unwrap()];
    assert_eq!(
        session.native_source_identity_for_bound_file(index, foreign_file),
        None,
        "same path/text in a foreign leaf cannot replace the actual querying-program target"
    );
    assert_eq!(
        session.native_source_identity(index, b"export function exported() { return 99; }"),
        None
    );
    assert!(
        conventional_project
            .checked_query_session_unmetered_for_test()
            .unwrap()
            .authenticate_native_program_sources(&manifests[app])
            .is_err(),
        "the primary leaf manifest cannot impersonate the supplemental query origin"
    );
    assert!(cold.image_matches(
        "src/primary.ts",
        SemanticImageIdentity::from_encoded_bytes(&encoded["src/primary.ts"]).as_ref()
    ));
    assert!(!cold.image_matches(
        "src/primary.ts",
        SemanticImageIdentity::from_encoded_bytes(&encoded["supplemental.ts"]).as_ref()
    ));
}

#[test]
fn actual_angular_fifty_sources_keep_build_owner_and_admit_nine_in_real_conventional_project() {
    assert_native_cross_project_alias_ownership();
    let fixture = Fixture::captured();
    let admitted = fixture.admit();
    let inputs = admitted.inputs();
    assert!(
        inputs.compiler_api().is_err(),
        "no host compiler or Node authority"
    );
    let selected = select_config(&inputs).unwrap().unwrap();
    assert!(selected.path.ends_with("tsconfig.app.json") && selected.selected_build_config);
    let sources = fixture.sources();
    assert_eq!(sources.len(), 50);
    let forest = build_native_forest_inputs(
        &inputs,
        &sources,
        Instant::now() + Duration::from_secs(30),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(forest.project_set);
    assert!(
        !forest.configured_forest,
        "independent programs are not fake config references"
    );
    assert_eq!(forest.nodes.len(), 2);
    let app = forest
        .nodes
        .iter()
        .position(|node| node.config.as_ref().unwrap().0 == "tsconfig.app.json")
        .unwrap();
    let conventional = forest
        .nodes
        .iter()
        .position(|node| node.config.as_ref().unwrap().0 == "tsconfig.json")
        .unwrap();
    assert_eq!(
        forest.root as usize, app,
        "producer-selected primary is unchanged"
    );
    assert!(forest.nodes.iter().all(|node| node.references.is_empty()));
    let selected_count = |index: usize| {
        forest.nodes[index]
            .native
            .package_membership
            .values()
            .filter(|member| member.selected_program_path().is_some())
            .count()
    };
    assert_eq!(
        selected_count(app),
        41,
        "do not force nine excluded sources into the app program"
    );
    assert_eq!(
        selected_count(conventional),
        50,
        "all fifty are actual conventional-program members"
    );
    let owned_count = |index| {
        forest.package_membership.values().filter(|member|
        matches!(member, TypeScriptForestSourceMembership::Selected { program, .. } if *program == index)).count()
    };
    assert_eq!(owned_count(app), 41);
    assert_eq!(owned_count(conventional), 9);
    assert!(
        forest
            .package_membership
            .values()
            .all(|member| matches!(member, TypeScriptForestSourceMembership::Selected { .. }))
    );
    assert_ne!(
        forest.nodes[app].native.options.environment,
        forest.nodes[conventional].native.options.environment
    );
    for node in &forest.nodes {
        node.native
            .closure_witness
            .validate_current(admitted.witness())
            .unwrap();
    }
    fs::write(
        fixture.0.join("tsconfig.json"),
        "{\"compilerOptions\":{\"strict\":false}}",
    )
    .unwrap();
    assert!(
        forest.nodes.iter().all(|node| node
            .native
            .closure_witness
            .validate_current(admitted.witness())
            .is_err()),
        "effective base config edits cannot reuse either admitted project"
    );
}

#[test]
fn conventional_project_exclusions_remain_real_gaps_instead_of_being_counted_complete() {
    let fixture = Fixture::captured();
    fs::write(fixture.0.join("tsconfig.json"),
        r#"{"compilerOptions":{"noLib":true,"types":[],"experimentalDecorators":true,"moduleResolution":"bundler","module":"ESNext"},"exclude":["**/*.spec.ts"]}"#).unwrap();
    let admitted = fixture.admit();
    let sources = fixture.sources();
    let forest = build_native_forest_inputs(
        &admitted.inputs(),
        &sources,
        Instant::now() + Duration::from_secs(30),
        &AtomicBool::new(false),
    )
    .unwrap();
    let excluded = forest
        .package_membership
        .iter()
        .filter_map(|(path, member)| {
            matches!(
                member,
                TypeScriptForestSourceMembership::ExcludedFromConfiguredProgram
            )
            .then_some(path.as_ref())
        })
        .collect::<Vec<_>>();
    assert_eq!(excluded.len(), 6);
    assert!(excluded.iter().all(|path| path.ends_with(".spec.ts")));
    assert_eq!(
        forest.package_membership.len(),
        50,
        "the physical denominator is retained"
    );
}

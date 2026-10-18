//! Real direct-TSZ images through selected source, row/query, and cold-search seams.

#![allow(clippy::expect_used, clippy::too_many_lines)]

use super::super::{
    BuiltinIntent, BuiltinSemanticChange, BuiltinSourceChange, ProductDaemon,
    generation_residence::SemanticGenerationResidence, semantic_authority::SemanticAuthority,
};
use super::{image_rows::ImageRowResidence, query, semantic};
use backend_engine::application::{
    LocalCompilerHost, LocalHostDiscovery, LocalHostEnvironment, LocalHostVariable,
    OwnedPackageSource, OwnedPackageSourceSet,
};
use backend_engine::builtin::{
    PartialSemanticCoverage, ProductSemanticPublicationKey, ProductSemanticPublicationRecord,
    SemanticPublicationCoverage,
};
use backend_engine::{ProductSourceRecord, ViewRoot};
use backend_library::interface::{CorrelationId, GenerateTarget, PackageCompileRequest};
use backend_semantic::vocabulary::{LanguageProfile, PackageUrl, Stage, TypeScriptSource};
use backend_version::{ContentId, SourceFactDomain};
use std::{
    ffi::OsString,
    num::NonZeroU32,
    path::{Path, PathBuf},
};

struct DirectHost(PathBuf);

impl LocalHostEnvironment for DirectHost {
    fn value(&self, variable: LocalHostVariable) -> Option<OsString> {
        (variable == LocalHostVariable::NudoxDataRoot).then(|| self.0.clone().into_os_string())
    }

    fn search_path(&self) -> Option<OsString> {
        Some(OsString::new())
    }
}

fn open_owner(path: &Path) -> ProductDaemon {
    let descriptor = super::super::profile_descriptor(super::super::BuiltinProfile::Product)
        .expect("product descriptor");
    let dispatcher = super::super::builtin_dispatcher(Some([0x79; 32]), descriptor, 60_000)
        .expect("product dispatcher");
    crate::Locald::open_with_dispatcher_and_registry(
        path,
        super::super::BuiltinModel,
        super::super::genesis().expect("product genesis"),
        dispatcher,
        backend_engine::DaemonConfig::default(),
        super::super::product_relation_registry().expect("typed product relations"),
    )
    .expect("open durable product owner")
}

fn search_ids(coordinator: &crate::builtin::query::QueryCoordinator, name: &str) -> Vec<String> {
    let result = coordinator
        .search_local(
            crate::builtin::query::LocalQuery::prefix(name, 50).expect("bounded exact-name prefix"),
        )
        .expect("ordinary product search");
    assert_eq!(
        result.rows.len(),
        result.total_matches,
        "fixture fits one page"
    );
    let mut ids = result
        .rows
        .iter()
        .map(|hit| hit.row.id.stable_key())
        .collect::<Vec<_>>();
    ids.sort();
    ids
}

#[test]
fn direct_tsz_named_imports_share_authored_rows_and_survive_cold_search() {
    let root = tempfile::tempdir().expect("owned fixture root");
    let project = root.path().join("project");
    std::fs::create_dir_all(project.join("src")).expect("authored source root");
    std::fs::create_dir_all(project.join("node_modules/vendor")).expect("local vendor fixture");
    std::fs::write(
        project.join("node_modules/vendor/package.json"),
        r#"{"name":"vendor","version":"1.0.0","types":"index.d.ts"}"#,
    )
    .expect("real dependency package metadata");
    std::fs::write(
        project.join("node_modules/vendor/index.d.ts"),
        "export declare function remote(): number;\n",
    )
    .expect("real vendor declaration");
    std::fs::write(
        project.join("tsconfig.json"),
        r#"{
        "compilerOptions":{"noLib":true,"types":[],"strict":true,
            "module":"ESNext","moduleResolution":"bundler"},
        "files":["src/route.ts","src/scheduler.ts","src/fallback.ts"]
    }"#,
    )
    .expect("real configured TSZ program");
    let sources = [
        ("src/fallback.ts", "export const structuralOnly = 1;\n"),
        (
            "src/route.ts",
            concat!(
                "import { generateSchedule as schedule, scheduleVersion } from './scheduler';\n",
                "import { remote } from 'vendor';\n",
                "import * as api from './scheduler';\n",
                "export function POST(value: number): number { return schedule(value) + scheduleVersion + remote() + api.scheduleArrow(value); }\n",
                "export function shadow(schedule: number): number { return schedule; }\n",
            ),
        ),
        (
            "src/scheduler.ts",
            concat!(
                "export function generateSchedule(value: number): number { return value; }\n",
                "export const scheduleVersion = 1;\n",
                "export const scheduleArrow = (value: number): number => value;\n",
            ),
        ),
    ];
    for (path, source) in sources {
        std::fs::write(project.join(path), source).expect("actual authored bytes");
    }
    let profile = LanguageProfile::TypeScript(TypeScriptSource::TypeScript);
    let coordinate = PackageUrl::parse("pkg:npm/ts-import-boundary@1.0.0".to_owned())
        .expect("package coordinate");
    let client = LocalCompilerHost::new(
        DirectHost(root.path().join("compiler")),
        LocalHostDiscovery::ExplicitOnly,
    )
    .open()
    .expect("linked direct TSZ with empty PATH and no external compiler");
    let request = PackageCompileRequest::new(
        GenerateTarget {
            correlation: CorrelationId(194),
            profile,
            stage: Stage::LowerIr,
        },
        coordinate.clone(),
    )
    .expect("native TSZ request");
    // The third authored file deliberately has structural facts only. Partial
    // publication exercises exact twin unification rather than whole-file omission.
    let primary = sources
        .iter()
        .filter(|(path, _)| *path != "src/fallback.ts")
        .map(|(path, source)| OwnedPackageSource::new(path, source).expect("primary source"))
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let staged = client
        .compile_package_sources_staged(
            OwnedPackageSourceSet::new(request, project.clone(), primary)
                .expect("selected source set"),
        )
        .expect("actual direct-TSZ lowering and canonical image encoding");
    assert!(
        staged.native_program_sources().is_some(),
        "the selected images retain actual direct-TSZ program membership"
    );
    assert!(
        staged
            .native_program_sources()
            .expect("native program")
            .sources()
            .any(|member| {
                Path::new(member.row.program_path).ends_with("node_modules/vendor/index.d.ts")
                    && member.row.source.identity
                        == ContentId::<SourceFactDomain>::from_canonical_bytes(
                            b"export declare function remote(): number;\n",
                        )
            }),
        "vendor external comes from real admitted dependency bytes, not a missing module"
    );
    let produced = (0..staged.artifacts().len())
        .map(|ordinal| {
            let output = staged
                .semantic_output_object(ordinal)
                .expect("real encoded semantic artifact");
            let image = backend_semantic::ir::SemanticImageView::reopen(output.bytes())
                .expect("canonical TSZ image roundtrip");
            super::compiled_source_path(&image).expect("actual source provenance")
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        produced,
        ["src/route.ts".to_owned(), "src/scheduler.ts".to_owned()]
            .into_iter()
            .collect(),
        "only selected authored sources own published images"
    );
    let key = ProductSemanticPublicationKey::new(
        backend_engine::PackageReference::parse(project.to_str().expect("project UTF8").to_owned())
            .expect("local package"),
        coordinate,
        profile,
    )
    .expect("semantic publication key");
    let package = key.package_key();
    let workspace = root.path().join("workspace");
    let mut daemon = open_owner(&workspace);
    let mut authority = SemanticAuthority::open(&workspace).expect("real immutable authority");
    let observation = authority
        .observe(&key, *staged.input_witness().input_root(), 3)
        .expect("observe declared source scope");
    let attempt = authority
        .begin_candidate_attempt(&key, &observation)
        .expect("candidate");
    let claim = authority
        .publish_staged(&key, attempt, &staged, |_| Ok(()))
        .expect("persist real compiler images and native program membership")
        .0;

    let syntax = backend_frontend_typescript::syntax_frontend().expect("source frontend");
    let mut files = sources
        .iter()
        .map(|(path, source)| {
            let analysis = syntax
                .analyze(Path::new(path), source.as_bytes())
                .expect("authored facts");
            let identity = ContentId::<SourceFactDomain>::from_canonical_bytes(source.as_bytes());
            let file = ProductSourceRecord::file(
                package.to_bytes(),
                *path,
                backend_engine::SourceLanguage::TypeScript,
                *blake3::hash(source.as_bytes()).as_bytes(),
                *blake3::hash(b"typescript-import-regression-source-analysis").as_bytes(),
                analysis.declarations().clone(),
            )
            .expect("complete source facts")
            .with_source_identity(identity)
            .expect("exact captured source identity");
            (
                backend_engine::product_source_file_key(package.to_bytes(), path),
                file,
            )
        })
        .collect::<Vec<_>>();
    files.sort_by_key(|(id, _)| *id);
    let mut version = blake3::Hasher::new();
    for (path, source) in sources {
        version.update(path.as_bytes());
        version.update(&[0]);
        version.update(source.as_bytes());
    }
    let project_record = ProductSourceRecord::project(
        key.package().as_str(),
        *version.finalize().as_bytes(),
        files.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
    )
    .expect("selected authored project");
    let mut changes = vec![BuiltinSourceChange {
        key: package.to_bytes(),
        after: Some(project_record),
    }];
    changes.extend(files.into_iter().map(|(key, file)| BuiltinSourceChange {
        key,
        after: Some(file),
    }));
    let partial = SemanticPublicationCoverage::Partial(
        PartialSemanticCoverage::new(
            NonZeroU32::new(2).expect("completed"),
            NonZeroU32::new(3).expect("declared scope"),
        )
        .expect("honest partial coverage"),
    );
    let record = ProductSemanticPublicationRecord::Published {
        coverage: partial,
        claim,
    };
    let generation = key
        .for_generation_bytes(*claim.binding().identity.as_ref())
        .expect("immutable generation key");
    let marker = BuiltinIntent::index_with_semantics(
        package,
        key.package().as_str(),
        changes,
        vec![
            BuiltinSemanticChange {
                key: generation,
                after: Some(record.clone()),
            },
            BuiltinSemanticChange {
                key: key.clone(),
                after: Some(record),
            },
        ],
    )
    .expect("source plus selected semantic marker");
    authority
        .commit_product_selection_changes(vec![(key.clone(), claim)], Vec::new(), || {
            super::super::commands::commit_builtin_intent(&mut daemon, 1, &marker)
        })
        .expect("commit exact source/semantic selection");
    drop(staged);
    drop(authority);
    drop(daemon);

    // Actual durable reopen: no hand-built IR, rows, endpoint keys or query facts.
    let mut daemon = open_owner(&workspace);
    let mut authority = SemanticAuthority::open(&workspace).expect("cold semantic authority");
    authority
        .reconcile_workspace(&mut daemon)
        .expect("production cold-selection lifecycle");
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let selected = snapshot
        .relation::<super::super::BuiltinSemanticRelation>()
        .expect("semantic relation")
        .lookup(&key)
        .expect("selected record")
        .expect("present selection");
    assert!(matches!(
        selected,
        ProductSemanticPublicationRecord::Published {
            coverage: SemanticPublicationCoverage::Partial(_),
            ..
        }
    ));
    let indexed =
        super::super::read_indexed_sources(&snapshot).expect("authenticated source selection");
    let mut generations = SemanticGenerationResidence::default();
    authority.install_image_loader(&mut generations);
    let mut images = ImageRowResidence::default();
    let (base, _) =
        super::super::initial_view_for_workspace(&snapshot).expect("selected base view");
    let projected = semantic::rows_for_indexed_sources(
        &base,
        &indexed,
        &snapshot,
        &client,
        &workspace,
        semantic::ForeignPublication::Reject,
        &mut images,
        &mut generations,
    )
    .expect("production source-selected row projection");
    let corpus =
        query::semantic_query_corpus(&snapshot, &client, &indexed, &mut generations, &mut images)
            .expect("production semantic/structural corpus");
    let row_ids = projected
        .rows
        .iter()
        .map(|row| row.id.stable_key())
        .collect::<std::collections::BTreeSet<_>>();
    let fact_ids = corpus
        .facts()
        .iter()
        .map(|fact| fact.presentation().id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        row_ids, fact_ids,
        "row/query authored and external identities agree"
    );
    for name in ["schedule", "scheduleVersion", "remote", "api"] {
        let imports = corpus
            .facts()
            .iter()
            .filter(|fact| {
                let p = fact.presentation();
                p.name == name && p.kind == "import"
            })
            .collect::<Vec<_>>();
        assert_eq!(imports.len(), 1, "one authored import identity for {name}");
        let id = &imports[0].presentation().id;
        let rows = projected
            .rows
            .iter()
            .filter(|row| row.id.stable_key() == *id)
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 1, "one row for the query import identity");
        let location = rows[0].source.captured().expect("authored import location");
        assert_eq!(location.path(), "src/route.ts");
        assert_eq!(
            location.start_line(),
            if name == "remote" {
                2
            } else if name == "api" {
                3
            } else {
                1
            }
        );
    }
    assert!(
        corpus
            .facts()
            .iter()
            .any(|fact| fact.presentation().name == "remote"
                && fact.presentation().kind == "external"),
        "a real unselected vendor remains external"
    );
    assert!(
        !corpus.facts().iter().any(|fact| [
            "generateSchedule",
            "schedule",
            "scheduleVersion",
            "scheduleArrow"
        ]
        .contains(&fact.presentation().name.as_str())
            && fact.presentation().kind == "external"),
        "selected local targets add no external twin"
    );
    let target = corpus
        .facts()
        .iter()
        .find(|f| {
            f.presentation().name == "generateSchedule" && f.presentation().kind == "function"
        })
        .expect("actual local function")
        .presentation()
        .id
        .clone();
    let post = corpus
        .facts()
        .iter()
        .find(|f| f.presentation().name == "POST")
        .expect("authored caller");
    assert!(
        post.presentation().related.contains(&target),
        "aliased call reaches its real definition"
    );
    let arrow = corpus
        .facts()
        .iter()
        .find(|f| f.presentation().name == "scheduleArrow" && f.presentation().kind == "constant")
        .expect("selected authored function-value binding");
    assert!(
        post.presentation()
            .related
            .contains(&arrow.presentation().id),
        "called namespace value reaches the same selected binding, not a per-importer external"
    );
    assert!(
        corpus
            .facts()
            .iter()
            .any(|f| f.presentation().name == "api" && f.presentation().kind == "external"),
        "a whole module namespace remains honestly external; no blanket external filtering"
    );
    let shadow = corpus
        .facts()
        .iter()
        .find(|f| f.presentation().name == "shadow")
        .expect("shadow control");
    assert!(
        !shadow.presentation().related.contains(&target),
        "same spelling never borrows import target"
    );
    let view = ViewRoot::new_checked(
        base.recipe(),
        base.basis(),
        base.frontier(),
        projected.rows,
        super::super::coverage::view_coverage(
            &snapshot,
            &projected.activated,
            super::super::coverage::SemanticDeployment::Unconfigured,
            &generations,
        )
        .expect("production partial semantic and unconfigured vector coverage"),
        base.capability().expect("view capability"),
    )
    .expect("complete physical view of the explicitly partial semantic selection");
    let durable = root.path().join("search");
    let expected = {
        let mut search = crate::builtin::query::SearchSnapshotOwner::with_durable_root(&durable);
        let selected = search
            .select(
                snapshot.root(),
                view.clone(),
                view.capability().expect("capability"),
                super::super::admitted_coverage().expect("coverage"),
                corpus.clone(),
            )
            .expect("persist real search projection");
        let result = [
            "generateSchedule",
            "scheduleVersion",
            "structuralOnly",
            "remote",
            "scheduleArrow",
        ]
        .map(|name| search_ids(selected, name));
        // Full search can also match another import's shared statement or
        // signature. Assert actual authored identities, not incidental hit totals.
        for (index, name, expected) in [
            (0, "generateSchedule", 1),
            (1, "scheduleVersion", 2),
            (2, "structuralOnly", 1),
            (4, "scheduleArrow", 1),
        ] {
            let ids = corpus
                .facts()
                .iter()
                .filter(|fact| fact.presentation().name == name)
                .map(|fact| fact.presentation().id.clone())
                .collect::<Vec<_>>();
            assert_eq!(ids.len(), expected, "distinct authored targets for {name}");
            assert!(
                ids.iter().all(|id| result[index].contains(id)),
                "full search preserves every useful authored target for {name}"
            );
        }
        assert!(result[0].contains(&target), "actual function is searchable");
        result
    };
    let mut cold = crate::builtin::query::SearchSnapshotOwner::with_durable_root(&durable);
    let selected = cold
        .select(
            snapshot.root(),
            view.clone(),
            view.capability().expect("capability"),
            super::super::admitted_coverage().expect("coverage"),
            corpus,
        )
        .expect("cold search projection");
    let actual = [
        "generateSchedule",
        "scheduleVersion",
        "structuralOnly",
        "remote",
        "scheduleArrow",
    ]
    .map(|name| search_ids(selected, name));
    assert_eq!(
        actual, expected,
        "cold search keeps distinct authored and foreign identities"
    );
    assert_eq!(cold.projection_opens(), 1);
    assert_eq!(cold.projection_builds(), 0);
}

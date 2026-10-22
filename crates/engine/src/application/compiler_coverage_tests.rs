//! Actual captured Angular inputs, checked by linked TSZ with no Node/API or dependency install.
use crate::application::{
    LocalCompiler, LocalCompilerConfig, LocalCompilerControl, LocalCompilerScratch,
    LocalCompilerTimeout, LocalPackageRootSet, LocalToolchainSet, PackageAuthorityConfiguration,
    PackageSource, PackageSourceCoverageGapCause, PackageSourceSet, StagedSemanticPackage,
};
use backend_library::interface::{
    CorrelationId, GenerateTarget, PackageCompileRequest, PackageUrl,
};
use backend_semantic::{
    ir::{LinkKind, SemanticReader, SourceCoverageManifest, SourceCoverageRow},
    vocabulary::{LanguageProfile, Stage, TypeScriptSource},
};
use backend_store::journal::PublicationLimits;
use std::{
    collections::BTreeMap,
    fs,
    num::NonZeroUsize,
    path::PathBuf,
    sync::atomic::AtomicBool,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

struct Fixture {
    root: PathBuf,
    files: BTreeMap<String, String>,
}
impl Fixture {
    fn new() -> Self {
        let value: serde_json::Value =
            serde_json::from_str(include_str!("fixtures/angular-coverage50.json")).unwrap();
        let files: BTreeMap<String, String> =
            serde_json::from_value(value["files"].clone()).unwrap();
        let root = std::env::temp_dir().join(format!(
            "tsz-coverage50-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let fixture = Self {
            root: root.canonicalize().unwrap(),
            files,
        };
        for (path, text) in &fixture.files {
            fixture.write(path, text);
        }
        fixture.write(
            "package.json",
            r#"{"name":"angular-coverage50","version":"1.0.0"}"#,
        );
        fixture
    }
    fn empty() -> Self {
        let root = std::env::temp_dir().join(format!(
            "tsz-coverage-empty-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        Self {
            root: root.canonicalize().unwrap(),
            files: BTreeMap::new(),
        }
    }
    fn write(&self, path: &str, text: &str) {
        let path = self.root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    fn put(&mut self, path: &str, text: &str) {
        self.files.insert(path.to_owned(), text.to_owned());
        self.write(path, text);
    }
    fn compile(&self) -> StagedSemanticPackage {
        self.compile_profile(LanguageProfile::TypeScript(TypeScriptSource::TypeScript))
    }
    fn compile_profile(&self, profile: LanguageProfile) -> StagedSemanticPackage {
        let host = super::TypeScriptProjectHost::direct(
            None,
            super::ToolchainProbeLimits::new(
                Duration::from_secs(5),
                NonZeroUsize::new(1024).unwrap(),
            )
            .unwrap(),
        );
        let cancelled = AtomicBool::new(false);
        let artifacts = self.root.join("artifacts");
        let journal = self.root.join("journal");
        let work = self.root.join("work");
        fs::create_dir_all(&work).unwrap();
        let mut scratch = LocalCompilerScratch::with_fragment_capacity(
            NonZeroUsize::new(16 * 1024 * 1024).unwrap(),
        )
        .unwrap();
        let mut compiler = LocalCompiler::create_with_package_authority(
            LocalCompilerConfig {
                toolchains: LocalToolchainSet::validate(&[]).unwrap(),
                artifact_directory: &artifacts,
                journal_directory: &journal,
                native_work_directory: &work,
                control: LocalCompilerControl {
                    timeout: LocalCompilerTimeout::new(Duration::from_secs(120)).unwrap(),
                    cancelled: &cancelled,
                },
            },
            LocalPackageRootSet::EMPTY,
            PackageAuthorityConfiguration {
                typescript_project_host: Some(&host),
                ..PackageAuthorityConfiguration::UNAVAILABLE
            },
            PublicationLimits::new(NonZeroUsize::MIN, NonZeroUsize::MIN).unwrap(),
            &mut scratch,
        )
        .unwrap();
        let request = PackageCompileRequest::new(
            GenerateTarget {
                correlation: CorrelationId(5012),
                profile,
                stage: Stage::LowerIr,
            },
            PackageUrl::try_from("pkg:npm/angular-coverage50@1.0.0".to_owned()).unwrap(),
        )
        .unwrap();
        let sources = self
            .files
            .iter()
            .filter(|(path, _)| match profile {
                LanguageProfile::TypeScript(TypeScriptSource::TypeScript) => path.ends_with(".ts"),
                LanguageProfile::TypeScript(TypeScriptSource::Tsx) => path.ends_with(".tsx"),
                _ => false,
            })
            .map(|(path, text)| PackageSource::new(path, text).unwrap())
            .collect::<Vec<_>>();

        compiler
            .compile_package_sources_staged(
                PackageSourceSet::new(&request, &self.root, &sources).unwrap(),
                &mut |_| {},
            )
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

const EXCLUDED: [&str; 9] = [
    "playwright.config.ts",
    "src/app/core/auth/services/jwt.service.spec.ts",
    "src/app/core/auth/services/user.service.spec.ts",
    "src/app/features/article/services/articles.service.spec.ts",
    "src/app/features/article/services/comments.service.spec.ts",
    "src/app/features/article/services/tags.service.spec.ts",
    "src/app/features/profile/services/profile.service.spec.ts",
    "src/test-setup.ts",
    "vitest.config.ts",
];

#[test]
fn linked_tsz_angular50_records_actual_config_and_exact_nine_scope_observations() {
    let fixture = Fixture::new();
    let staged = fixture.compile();
    assert_eq!(staged.artifacts().len(), 41);
    assert_eq!(
        staged.input_witness().coverage().state(),
        backend_version::Coverage::Partial
    );
    let coverage = staged
        .source_coverage()
        .expect("producer retained observations")
        .catalog()
        .expect("complete explanation fits");
    assert_eq!((coverage.source_count(), coverage.completed()), (50, 41));
    assert_eq!(coverage.input_root(), staged.input_witness().input_root());
    let gaps=coverage.rows().iter().filter_map(|row|match row {SourceCoverageRow::Gap{path,source,byte_len,cause,..}=>{
        let bytes=fixture.files[path].as_bytes();
        assert_eq!(*source,*backend_version::ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(bytes).as_ref());
        assert_eq!(*byte_len as usize,bytes.len());
        assert_eq!(*cause,PackageSourceCoverageGapCause::TypeScriptSourceExcludedFromConfiguredProgram);Some(path.as_str())
    },_=>None}).collect::<Vec<_>>();
    assert_eq!(gaps, EXCLUDED);
    let configs=coverage.rows().iter().filter_map(|row|match row {SourceCoverageRow::Configuration{path,source,..}=>{
        assert_eq!(*source,*backend_version::ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(fixture.files[path].as_bytes()).as_ref());Some(path.as_str())
    },_=>None}).collect::<Vec<_>>();
    assert_eq!(configs, ["tsconfig.app.json"]);
    let program = staged.native_program_sources().unwrap();
    let (root, nodes) = program
        .nodes()
        .expect("configured application has an authenticated node even without references");
    assert_eq!(root, 0);
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].config_path, "tsconfig.app.json");
    assert_eq!(coverage.program(), &program.membership_identity());
    let cold = SourceCoverageManifest::decode(&coverage.encode()).unwrap();
    assert_eq!(cold, *coverage);
    assert!(
        !fixture.root.join("node_modules").exists(),
        "linked TSZ alone owns this observation"
    );
}

#[test]
fn linked_tsz_tsx_cycle_is_an_exact_file_gap_while_navbar_and_caller_publish() {
    let mut fixture = Fixture::empty();
    fixture.put(
        "tsconfig.json",
        r#"{"compilerOptions":{"noLib":true,"types":[],"module":"ESNext","moduleResolution":"bundler","jsx":"preserve"},"files":["src/app/layout.tsx","src/app/page.tsx","src/components/NavBar.tsx","src/types/react-node.tsx"]}"#,
    );
    let react_node = "export type ReactNode = { readonly children?: ReactNode };\n";
    let navbar = "export function NavBar() { return \"nav\"; }\n";
    let page = "import { NavBar } from \"../components/NavBar\";\nexport function Page() { return NavBar(); }\n";
    fixture.put("src/types/react-node.tsx", react_node);
    let layout = "import type { ReactNode } from \"../types/react-node\";\nexport type Layout = ReactNode;\n";
    fixture.put("src/app/layout.tsx", layout);
    fixture.put("src/components/NavBar.tsx", navbar);
    fixture.put("src/app/page.tsx", page);

    let staged = fixture.compile_profile(LanguageProfile::TypeScript(TypeScriptSource::Tsx));
    assert_eq!(staged.artifacts().len(), 3);
    assert_eq!(staged.coverage_gaps().len(), 1);
    assert_eq!(
        staged.input_witness().coverage().state(),
        backend_version::Coverage::Partial,
        "one refused TSX source must not claim complete profile coverage"
    );
    assert!(staged.artifacts().iter().any(|artifact| {
        artifact.source().identity
            == backend_version::ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(
                navbar.as_bytes(),
            )
    }));
    assert!(staged.artifacts().iter().any(|artifact| {
        artifact.source().identity
            == backend_version::ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(
                react_node.as_bytes(),
            )
    }));
    let page_ordinal = staged
        .artifacts()
        .iter()
        .position(|artifact| {
            artifact.source().identity
                == backend_version::ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(
                    page.as_bytes(),
                )
        })
        .expect("the healthy caller keeps a semantic artifact");
    assert!(!staged.artifacts().iter().any(|artifact| {
        artifact.source().identity
            == backend_version::ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(
                layout.as_bytes(),
            )
    }));

    let retained_navbar_call = staged
        .with_semantic_reader(page_ordinal, |reader, _| -> Result<bool, std::io::Error> {
            let links = reader.canonical_links().collect::<Vec<_>>();
            Ok(reader.link_occurrences().any(|(_, occurrence)| {
                let Some(span) = occurrence.source else {
                    return false;
                };
                let Some(name) = page
                    .as_bytes()
                    .get(span.start() as usize..span.end() as usize)
                else {
                    return false;
                };
                name == b"NavBar"
                    && links
                        .iter()
                        .any(|(id, link)| *id == occurrence.link && link.kind == LinkKind::Calls)
            }))
        })
        .expect("the healthy caller image reopens");
    assert!(
        retained_navbar_call,
        "the exact NavBar call remains in Page"
    );

    let gap = staged
        .coverage_gaps()
        .iter()
        .find(|gap| gap.relative_path() == "src/app/layout.tsx")
        .expect("the recursive source is named as the failed file");
    assert_eq!(
        gap.cause(),
        PackageSourceCoverageGapCause::SourceNotRepresentable
    );
    assert_eq!(
        gap.source().identity,
        backend_version::ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(
            layout.as_bytes(),
        )
    );
    assert_eq!(gap.source().byte_len as usize, layout.len());
    let coverage = staged
        .source_coverage()
        .expect("source omission is authenticated")
        .catalog()
        .expect("coverage explanation fits");
    assert_eq!((coverage.source_count(), coverage.completed()), (4, 3));
    assert!(coverage.rows().iter().any(|row| {
        matches!(row, SourceCoverageRow::Gap { path, source, byte_len, cause, .. }
            if path == "src/app/layout.tsx"
                && source == gap.source().identity.as_ref()
                && *byte_len as usize == layout.len()
                && *cause == PackageSourceCoverageGapCause::SourceNotRepresentable)
    }));
}

#[test]
fn linked_tsz_coverage_follows_imports_instead_of_spec_filename_and_keeps_changed_gap_identity() {
    let mut fixture = Fixture::new();
    let first = fixture.compile();
    let path = EXCLUDED[0];
    let text = "export const changedToolingConfig = 7;\n";
    fixture.files.insert(path.into(), text.into());
    fixture.write(path, text);
    let changed = fixture.compile();
    assert_eq!(changed.artifacts().len(), 41);
    assert_ne!(
        first.source_coverage().unwrap().identity(),
        changed.source_coverage().unwrap().identity()
    );
    assert!(changed.source_coverage().unwrap().catalog().unwrap().rows().iter().any(|row|matches!(row,SourceCoverageRow::Gap{path:seen,source,..} if seen==path && *source==*backend_version::ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(text.as_bytes()).as_ref())));
    // Import a previously excluded spec. Its filename does not decide TSZ membership.
    let imported = EXCLUDED[1];
    let text = "export const includedSpec = 1;\n";
    fixture.files.insert(imported.into(), text.into());
    fixture.write(imported, text);
    let main = format!(
        "{}\nimport './app/core/auth/services/jwt.service.spec';\n",
        fixture.files["src/main.ts"]
    );
    fixture.files.insert("src/main.ts".into(), main.clone());
    fixture.write("src/main.ts", &main);
    let selected = fixture.compile();
    assert_eq!(selected.artifacts().len(), 42);
    assert!(
        !selected
            .coverage_gaps()
            .iter()
            .any(|gap| gap.relative_path() == imported)
    );
    assert!(
        selected
            .native_program_sources()
            .unwrap()
            .sources()
            .any(|row| row.package_path == Some(imported))
    );
}

#[test]
fn linked_tsz_explanation_byte_capacity_keeps_successful_partial_artifacts_and_exact_basis() {
    let mut fixture = Fixture::new();
    let angular = fixture.files["angular.json"].clone();
    fixture.files.clear();
    fixture.files.insert("angular.json".into(), angular);
    fixture.files.insert(
        "tsconfig.json".into(),
        r#"{"compilerOptions":{"target":"ES2022","noLib":true}}"#.into(),
    );
    fixture.files.insert(
        "tsconfig.app.json".into(),
        r#"{"extends":"./tsconfig.json","files":["src/main.ts"]}"#.into(),
    );
    fixture
        .files
        .insert("src/main.ts".into(), "export const main = 1;".into());
    // Actual source universe is below the independent existing 4096 resolver
    // ceiling. Deep but valid paths exceed only this explanation's byte budget.
    let prefix = std::iter::repeat_n("x".repeat(190), 20)
        .collect::<Vec<_>>()
        .join("/");
    for n in 0..2300 {
        fixture.files.insert(
            format!("{prefix}/outside-{n:04}.ts"),
            "export const outside = 1;".into(),
        );
    }
    for (path, text) in &fixture.files {
        fixture.write(path, text);
    }
    let staged = fixture.compile();
    assert_eq!(staged.artifacts().len(), 1);
    assert_eq!(staged.coverage_gaps().len(), 2300);
    assert_eq!(
        staged.input_witness().coverage().state(),
        backend_version::Coverage::Partial
    );
    let recording = staged.source_coverage().unwrap();
    assert_eq!(
        recording.capacity_reason(),
        Some(backend_semantic::ir::SourceCoverageCapacityReason::EncodedBytes)
    );
    assert!(recording.catalog().is_none());
    assert_eq!((recording.source_count(), recording.completed()), (2301, 1));
    assert_eq!(recording.input_root(), staged.input_witness().input_root());
    assert_eq!(
        recording.program(),
        &staged
            .native_program_sources()
            .unwrap()
            .membership_identity()
    );
    let cold =
        backend_semantic::ir::SourceCoverageObservation::decode(&recording.encode()).unwrap();
    assert_eq!(&cold, recording);
}

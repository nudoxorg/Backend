use super::*;
use backend_semantic::ir::SemanticCoreReader;

#[test]
fn native_private_reexport_publication_admission_baseline() {
    let mut fixture = Fixture::empty();
    fixture.put("tsconfig.json", r#"{"compilerOptions":{"noLib":true,"types":[],"module":"ESNext","moduleResolution":"bundler","jsx":"preserve"},"files":["ui/button.ts","ui/index.ts","page.tsx"]}"#);
    fixture.put("ui/button.ts", "function Button() { return 1; }\n");
    fixture.put(
        "ui/index.ts",
        "export { Button as Action } from './button';\n",
    );
    let page = "// λ\nimport { Action as Confirm } from './ui';\nexport function Page() { return <><Confirm /><Confirm /></>; }\n";
    fixture.put("page.tsx", page);

    let staged = fixture.compile_profile(LanguageProfile::TypeScript(TypeScriptSource::Tsx));
    eprintln!(
        "native staged artifacts={} coverage={:?} gaps={:?}",
        staged.artifacts().len(),
        staged.input_witness().coverage().state(),
        staged.coverage_gaps()
    );
    assert_eq!(staged.artifacts().len(), 1);
    assert!(staged.coverage_gaps().is_empty());
    let observations = staged
        .with_semantic_reader(0, |reader, _| -> Result<Vec<_>, std::io::Error> {
            use backend_semantic::ir::{
                Confidence, ExternalTarget, ForeignTargetOrigin, LinkTarget, SemanticCoreReader,
                TYPESCRIPT_TSZ_SOURCE_ECOSYSTEM, TypeScriptSourceCoordinate,
            };
            let mut found = Vec::new();
            for (_, occurrence) in reader.link_occurrences() {
                let Some(span) = occurrence.source else {
                    continue;
                };
                if page
                    .as_bytes()
                    .get(span.start() as usize..span.end() as usize)
                    != Some(&b"Confirm"[..])
                {
                    continue;
                }
                let Some(link) = reader.link(occurrence.link) else {
                    continue;
                };
                let Some(owner) = reader
                    .core_entity(link.from)
                    .and_then(|entity| entity.name.named_atom())
                    .and_then(|name| reader.atom(name))
                else {
                    continue;
                };
                if owner != b"Page" {
                    continue;
                }
                let LinkTarget::External(id) = link.target else {
                    continue;
                };
                let Some(ExternalTarget::Foreign(foreign)) = reader.external(id) else {
                    continue;
                };
                let ForeignTargetOrigin::Universe { ecosystem } = foreign.origin else {
                    continue;
                };
                if reader.atom(ecosystem) != Some(TYPESCRIPT_TSZ_SOURCE_ECOSYSTEM.as_bytes()) {
                    continue;
                }
                let Some(coordinate) = reader
                    .atom(foreign.path)
                    .and_then(|bytes| std::str::from_utf8(bytes).ok())
                    .and_then(TypeScriptSourceCoordinate::decode)
                else {
                    continue;
                };
                found.push((
                    span.start(),
                    span.end(),
                    link.kind,
                    occurrence.confidence,
                    coordinate.path.to_owned(),
                    coordinate.name_start,
                ));
            }
            assert!(
                found
                    .iter()
                    .all(|item| item.2 == LinkKind::Reads && item.3 == Confidence::Compiler)
            );
            Ok(found)
        })
        .expect("staged native Page image opens");
    eprintln!("native staged Page observations: {observations:?}");
    assert_eq!(observations.len(), 2);
    assert!(
        observations
            .iter()
            .all(|item| item.4 == "ui/button.ts" && item.5 == 9)
    );

    let host = super::super::TypeScriptProjectHost::direct(
        None,
        super::super::ToolchainProbeLimits::new(
            Duration::from_secs(5),
            NonZeroUsize::new(1024).unwrap(),
        )
        .unwrap(),
    );
    let cancelled = AtomicBool::new(false);
    let artifact_dir = fixture.root.join("published-artifacts");
    let journal_dir = fixture.root.join("published-journal");
    let work = fixture.root.join("published-work");
    fs::create_dir(&work).unwrap();
    let mut scratch =
        LocalCompilerScratch::with_fragment_capacity(NonZeroUsize::new(16 * 1024 * 1024).unwrap())
            .unwrap();
    let mut compiler = LocalCompiler::create_with_package_authority(
        LocalCompilerConfig {
            toolchains: LocalToolchainSet::validate(&[]).unwrap(),
            artifact_directory: &artifact_dir,
            journal_directory: &journal_dir,
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
            correlation: CorrelationId(5013),
            profile: LanguageProfile::TypeScript(TypeScriptSource::Tsx),
            stage: Stage::LowerIr,
        },
        PackageUrl::try_from("pkg:npm/private-reexport@1.0.0".to_owned()).unwrap(),
    )
    .unwrap();
    let sources = [PackageSource::new("page.tsx", page).unwrap()];
    let published = compiler
        .compile_package_sources(
            PackageSourceSet::new(&request, &fixture.root, &sources).unwrap(),
            &mut |_| {},
        )
        .expect("native local journal publication outcome");
    assert_eq!(published.images.len(), 1);
    assert!(published.coverage_gaps().is_empty());
    let image = published.images[0]
        .reopen()
        .expect("published Page image reopens");
    let page_references = image
        .link_occurrences()
        .filter(|(_, occurrence)| {
            let Some(span) = occurrence.source else {
                return false;
            };
            let Some(link) = image.link(occurrence.link) else {
                return false;
            };
            page.as_bytes()
                .get(span.start() as usize..span.end() as usize)
                == Some(&b"Confirm"[..])
                && link.kind == LinkKind::Reads
                && occurrence.confidence == backend_semantic::ir::Confidence::Compiler
                && image
                    .core_entity(link.from)
                    .and_then(|owner| owner.name.named_atom())
                    .and_then(|name| image.atom(name))
                    == Some(&b"Page"[..])
        })
        .count();
    assert_eq!(page_references, 2);
    eprintln!(
        "native local compiler journal published 1 image, 0 gaps, {page_references} Page Reads; service selection not tested"
    );
}

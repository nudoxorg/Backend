//! Native TSZ baseline for renamed JSX uses through a barrel re-export.
//! This deliberately bypasses ingestion, FrontendSet and SyntaxFrontend.

use backend_engine::driver::{
    CompileControl, CompileRequest, CompileScratch, DeclarationScope, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainSelection, compile_ir,
};
use backend_frontend_typescript::{
    TszEnvironmentFingerprint, TszFileInput, TszProjectAuthority, TszProjectExecutionBudget,
    TszProjectModuleRequestKind, TszProjectModuleResolution, TszProjectModuleResolutionTarget,
    TszProjectOptions, TszProjectSemanticOptions, checker_options_from_compiler_api_json,
};
use backend_semantic::ir::{
    Confidence, ExternalTarget, ForeignTargetOrigin, LinkKind, LinkTarget, PackageLineage,
    SemanticImageView, SemanticReader, TYPESCRIPT_TSZ_SOURCE_ECOSYSTEM, TypeScriptSourceCoordinate,
    encode_full_semantic_image, full_semantic_image_len,
};
use backend_semantic::vocabulary::{LanguageProfile, Stage, TypeScriptSource};
use std::{
    error::Error,
    path::Path,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

const BUTTON: &str = "export function Button() { return 1; }\n";
const BARREL: &str = "export { Button as Action } from './button';\n";
const PAGE: &str = "// λ makes byte offsets non-ASCII\nimport { Action as Confirm } from './ui';\nexport function Page() { return <><Confirm /><Confirm /></>; }\n";

type TestResult<T> = Result<T, Box<dyn Error>>;

#[derive(Debug, PartialEq, Eq)]
struct ObservedUse {
    owner: String,
    relation: LinkKind,
    confidence: Confidence,
    source_file: Option<String>,
    target_ecosystem: Option<String>,
    start: usize,
    end: usize,
    target_path: Option<String>,
    target_name_start: Option<u32>,
    target_source: Option<[u8; 32]>,
}

struct Baseline {
    uses: Vec<ObservedUse>,
    diagnostics: Vec<(String, u32)>,
    target_identity: [u8; 32],
}

fn run(
    page: &str,
    button: &str,
    barrel: &str,
    import_target: TszProjectModuleResolutionTarget,
) -> TestResult<Baseline> {
    let cancel = AtomicBool::new(false);
    let deadline = Instant::now() + Duration::from_secs(60);
    let budget = TszProjectExecutionBudget::new(deadline, &cancel, 50_000_000);
    let mut checker = checker_options_from_compiler_api_json(
        r#"{"compilerOptions":{"jsx":"preserve","noLib":true}}"#,
    )?;
    checker.no_lib = true;
    let resolution =
        |importer: &str, specifier: &str, request_kind, target| TszProjectModuleResolution {
            importer_path: importer.to_owned(),
            specifier: specifier.to_owned(),
            request_kind,
            resolution_mode: None,
            target,
        };
    let options = TszProjectOptions {
        checker,
        semantic_options: TszProjectSemanticOptions::declaration_scoped(),
        module_resolutions: vec![
            resolution(
                "ui/index.ts",
                "./button",
                TszProjectModuleRequestKind::EsmReExport,
                TszProjectModuleResolutionTarget::File {
                    path: "ui/button.ts".to_owned(),
                },
            ),
            resolution(
                "page.tsx",
                "./ui",
                TszProjectModuleRequestKind::EsmImport,
                import_target,
            ),
        ],
        program_diagnostics: Vec::new(),
        file_formats: Vec::new(),
        environment: TszEnvironmentFingerprint::from_sha256([0x71; 32]),
    };
    let inputs = [
        ("ui/button.ts", button),
        ("ui/index.ts", barrel),
        ("page.tsx", page),
    ]
    .into_iter()
    .map(|(path, source)| TszFileInput {
        path: path.to_owned(),
        source: source.to_owned(),
    })
    .collect();
    let mut authority = TszProjectAuthority::new();
    authority.update_with_execution_checkpoint(inputs, options, &[], &budget)?;
    let project = authority
        .project()
        .ok_or("missing admitted native project")?;
    let diagnostic_codes = project
        .diagnostics()
        .map(|d| (d.file.clone(), d.code))
        .collect();
    eprintln!(
        "native TSZ diagnostics: {:?}",
        project
            .diagnostics()
            .map(|diagnostic| (
                diagnostic.file.clone(),
                diagnostic.code,
                diagnostic.message_text.clone()
            ))
            .collect::<Vec<_>>()
    );
    let session = project.checked_query_session(&budget)?;
    let target_index = session.file_index("ui/button.ts")?;
    let target_identity = session
        .native_source_identity(target_index, button.as_bytes())
        .ok_or("target bytes do not match the checked TSZ session")?;
    assert_eq!(
        target_identity,
        *backend_version::ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(
            button.as_bytes()
        )
    );
    assert_eq!(
        session.native_source_identity(target_index, b"different target bytes"),
        None
    );
    let mut diagnostics = [0; 4096];
    let output = compile_ir(
        CompileRequest {
            profile: LanguageProfile::TypeScript(TypeScriptSource::Tsx),
            stage: Stage::LowerIr,
            source: page.as_bytes(),
            declaration_scope: DeclarationScope::new(
                PackageLineage::new("npm", "native-tsx-fixture")
                    .map_err(|e| format!("lineage: {e:?}"))?,
                "page.tsx",
            )
            .map_err(|e| format!("declaration scope: {e:?}"))?,
            toolchain: ToolchainSelection::ResolvedNative(ResolvedToolchain::native_typescript()),
            authority: SemanticAuthorityInput::TypeScriptTsz {
                project,
                session: &session,
                source_path: "page.tsx",
            },
            control: CompileControl {
                deadline,
                cancelled: &cancel,
            },
        },
        CompileScratch {
            diagnostic_output: &mut diagnostics,
            native_work: Path::new("/tmp"),
        },
    )
    .map_err(|error| format!("native TSZ compile_ir failed: {error:?}"))?;
    let ir = &output.ir;
    let uses = observe(ir, page)?;
    let mut bytes = vec![0; full_semantic_image_len(ir)?];
    encode_full_semantic_image(ir, &mut bytes)?;
    let reopened = SemanticImageView::reopen(&bytes)?;
    assert_eq!(
        uses,
        observe(&reopened, page)?,
        "typed occurrences survive encode/reopen"
    );
    eprintln!("native TSZ observations: {uses:#?}");
    Ok(Baseline {
        uses,
        diagnostics: diagnostic_codes,
        target_identity,
    })
}

fn observe(ir: &impl SemanticReader, page: &str) -> TestResult<Vec<ObservedUse>> {
    let mut uses = Vec::new();
    for (_, occurrence) in ir.link_occurrences() {
        let Some(span) = occurrence.source else {
            continue;
        };
        let start = span.start() as usize;
        let end = span.end() as usize;
        if page.get(start..end) != Some("Confirm") {
            continue;
        }
        let link = ir.link(occurrence.link).ok_or("occurrence link missing")?;
        let owner = ir
            .core_entity(link.from)
            .and_then(|item| item.name.named_atom())
            .and_then(|name| ir.atom(name))
            .map(|name| String::from_utf8_lossy(name).into_owned())
            .unwrap_or_default();
        let mut ecosystem = None;
        let coordinate = match link.target {
            LinkTarget::External(id) => match ir.external(id) {
                Some(ExternalTarget::Foreign(foreign)) => {
                    if let ForeignTargetOrigin::Universe { ecosystem: atom } = foreign.origin {
                        ecosystem = ir
                            .atom(atom)
                            .and_then(|value| std::str::from_utf8(value).ok())
                            .map(str::to_owned);
                    }
                    ir.atom(foreign.path)
                        .and_then(|bytes| std::str::from_utf8(bytes).ok())
                        .and_then(TypeScriptSourceCoordinate::decode)
                }
                _ => None,
            },
            _ => None,
        };
        uses.push(ObservedUse {
            owner,
            relation: link.kind,
            confidence: occurrence.confidence,
            source_file: ir
                .atom(span.file())
                .and_then(|value| std::str::from_utf8(value).ok())
                .map(str::to_owned),
            target_ecosystem: ecosystem,
            start,
            end,
            target_path: coordinate.map(|value| value.path.to_owned()),
            target_name_start: coordinate.map(|value| value.name_start),
            target_source: coordinate.map(|value| value.source),
        });
    }
    uses.sort_by(|left, right| {
        (left.start, left.end, &left.owner).cmp(&(right.start, right.end, &right.owner))
    });
    Ok(uses)
}

#[test]
fn native_mixed_ts_tsx_renamed_barrel_jsx_occurrences() -> TestResult<()> {
    let baseline = run(
        PAGE,
        BUTTON,
        BARREL,
        TszProjectModuleResolutionTarget::File {
            path: "ui/index.ts".to_owned(),
        },
    )?;
    assert_diagnostics(&baseline, &[]);
    let target_identity = baseline.target_identity;
    let uses = baseline.uses;
    let page_uses = uses
        .iter()
        .filter(|item| item.owner == "Page" && item.target_path.as_deref() == Some("ui/button.ts"))
        .collect::<Vec<_>>();
    assert_eq!(
        page_uses.len(),
        2,
        "native Page/Confirm occurrences: {uses:#?}"
    );
    assert_ne!(page_uses[0].start, page_uses[1].start);
    for item in page_uses {
        assert_eq!(&PAGE[item.start..item.end], "Confirm");
        assert_eq!(
            item.target_name_start,
            BUTTON.find("Button").map(|value| value as u32)
        );
        assert_eq!(item.target_source, Some(target_identity));
        assert_eq!(item.source_file.as_deref(), Some("page.tsx"));
        assert_eq!(
            item.target_ecosystem.as_deref(),
            Some(TYPESCRIPT_TSZ_SOURCE_ECOSYSTEM)
        );
        assert_eq!(item.relation, LinkKind::Reads);
        assert_eq!(item.confidence, Confidence::Compiler);
    }
    Ok(())
}

#[test]
fn native_jsx_occurrence_offsets_remain_absolute_after_long_unicode_prefix() -> TestResult<()> {
    let page = format!("// λ\n{}{}", "// padding\n".repeat(500), PAGE);
    let baseline = run(&page, BUTTON, BARREL, barrel_target())?;
    assert_diagnostics(&baseline, &[]);
    let uses = baseline.uses;
    let page_uses = uses
        .iter()
        .filter(|item| item.owner == "Page" && item.target_path.as_deref() == Some("ui/button.ts"))
        .collect::<Vec<_>>();
    assert_eq!(page_uses.len(), 2, "long-source Page uses: {uses:#?}");
    for item in page_uses {
        assert!(item.start > 4096);
        assert_eq!(&page[item.start..item.end], "Confirm");
    }
    Ok(())
}

fn assert_no_button_use(
    label: &str,
    page: &str,
    button: &str,
    target: TszProjectModuleResolutionTarget,
    expected_diagnostics: &[u32],
) -> TestResult<()> {
    let baseline = run(page, button, BARREL, target)?;
    assert_diagnostics(&baseline, expected_diagnostics);
    let uses = baseline.uses;
    assert!(!uses.iter().any(|item| item.owner == "Page" && item.target_path.as_deref() == Some("ui/button.ts")), "{label}: false native target: {uses:#?}");
    Ok(())
}

fn assert_diagnostics(baseline: &Baseline, expected: &[u32]) {
    let actual = baseline
        .diagnostics
        .iter()
        .filter(|(_, code)| *code != 2318)
        .map(|(_, code)| *code)
        .collect::<Vec<_>>();
    assert_eq!(
        actual, expected,
        "unexpected native checker diagnostics; noLib TS2318 is excluded"
    );
}

fn barrel_target() -> TszProjectModuleResolutionTarget {
    TszProjectModuleResolutionTarget::File {
        path: "ui/index.ts".into(),
    }
}

#[test]
fn native_jsx_no_use_is_not_a_page_reference() -> TestResult<()> {
    assert_no_button_use(
        "no-use",
        "import { Action as Confirm } from './ui';\nexport function Page() { return 1; }\n",
        BUTTON,
        barrel_target(),
        &[],
    )
}

#[test]
fn native_jsx_unresolved_import_does_not_bind_button() -> TestResult<()> {
    assert_no_button_use(
        "unresolved",
        PAGE,
        BUTTON,
        TszProjectModuleResolutionTarget::Unresolved,
        &[2307],
    )
}

#[test]
fn native_jsx_type_only_import_does_not_bind_runtime_button() -> TestResult<()> {
    assert_no_button_use(
        "type-only",
        "import type { Action as Confirm } from './ui';\nexport function Page() { return <Confirm />; }\n",
        BUTTON,
        barrel_target(),
        &[1361],
    )
}

#[test]
fn native_jsx_private_button_is_not_exported_by_barrel() -> TestResult<()> {
    assert_no_button_use(
        "private",
        PAGE,
        "function Button() { return 1; }\n",
        barrel_target(),
        &[2305, 2306],
    )
}

#[test]
fn native_jsx_parameter_shadow_does_not_bind_imported_button() -> TestResult<()> {
    assert_no_button_use(
        "shadow",
        "import { Action as Confirm } from './ui';\nexport function Page(Confirm: () => number) { return <Confirm />; }\n",
        BUTTON,
        barrel_target(),
        &[],
    )
}

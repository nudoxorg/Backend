//! Cross-file TypeScript method value reads retarget through the import specifier.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::{Duration, Instant},
};

use backend_engine::driver::{
    CompileControl, CompileOutput, CompileRequest, CompileScratch, NativeTool, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainSelection, compile,
};
use backend_frontend_typescript::legacy::{Checker, CheckerError, Report};
use backend_semantic::ir::{
    DecodedOccurrence, EntityKind, ForeignOrigin, FragmentView, OccurrenceConfidence,
    OccurrenceTarget, ReferenceKind,
};
use backend_semantic::vocabulary::{LanguageProfile, Stage, TypeScriptSource};

static FIXTURE_ID: AtomicUsize = AtomicUsize::new(0);
static CANCELLED: AtomicBool = AtomicBool::new(false);

const WORKOUT_SERVICE: &[u8] = b"export class WorkoutService {\n  note = 1;\n  setNote() {}\n}\n";

const WEEKS: &[u8] = b"import { WorkoutService } from \"./workout.service\";
export function group(service: WorkoutService) { const bound = service.setNote; service.setNote(); }
";

fn fixture_root() -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "nudox-ts-method-value-{}-{}",
        std::process::id(),
        FIXTURE_ID.fetch_add(1, Ordering::Relaxed),
    ));
    fs::create_dir_all(&root).expect("fixture root");
    fs::write(root.join("workout.service.ts"), WORKOUT_SERVICE).expect("workout.service.ts");
    root
}

fn checker_missing(error: &CheckerError) -> bool {
    matches!(
        error,
        CheckerError::Spawn { .. }
            | CheckerError::ToolingUnavailable { .. }
            | CheckerError::ModuleUnavailable { .. }
    )
}

fn compile_weeks<'output>(
    source: &'static [u8],
    report: &Report,
    diagnostic: &mut [u8],
    output: &'output mut [u8],
) -> FragmentView<'output> {
    let toolchain = ResolvedToolchain::from_version(
        NativeTool::TypeScriptCompiler,
        Path::new("/bin/true"),
        b"typescript-method-value-fixture",
    )
    .expect("toolchain");
    compile(
        CompileRequest {
            profile: LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            stage: Stage::LowerIr,
            source,
            declaration_scope: backend_engine::driver::DeclarationScope::fixture(),
            toolchain: ToolchainSelection::ResolvedNative(toolchain),
            authority: SemanticAuthorityInput::TypeScript { report },
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(120),
                cancelled: &CANCELLED,
            },
        },
        CompileScratch {
            diagnostic_output: diagnostic,
            native_work: Path::new("/tmp"),
        },
        CompileOutput {
            fragment_output: output,
        },
    )
    .expect("lower weeks.ts")
    .fragment
}

fn occurrences<'a>(view: &'a FragmentView<'a>) -> Vec<DecodedOccurrence<'a>> {
    view.occurrences().into_iter().flatten().flatten().collect()
}

fn entities(view: &FragmentView<'_>) -> Vec<(u32, Vec<u8>, EntityKind)> {
    let atoms: Vec<_> = view
        .atoms()
        .map(|atom| (atom.ordinal.raw, atom.bytes.to_vec()))
        .collect();
    view.entities()
        .map(|entity| {
            (
                entity.entity.raw,
                atoms
                    .iter()
                    .find(|(ordinal, _)| *ordinal == entity.name.raw)
                    .map(|(_, bytes)| bytes.clone())
                    .unwrap_or_default(),
                entity.kind,
            )
        })
        .collect()
}

fn named_owner(view: &FragmentView<'_>, name: &[u8], kind: EntityKind) -> u32 {
    entities(view)
        .into_iter()
        .find(|(_, entity_name, entity_kind)| entity_name == name && *entity_kind == kind)
        .map(|(owner, _, _)| owner)
        .expect("named owner")
}

fn owner_decl_start(source: &[u8], owner_name: &str) -> u32 {
    let needle = format!("function {owner_name}");
    let start = source
        .windows(needle.len())
        .position(|window| window == needle.as_bytes())
        .expect("owner declaration in source");
    u32::try_from(start).expect("owner start")
}

fn occurrence_span_text<'a>(
    source: &'a [u8],
    owner_start: u32,
    row: &DecodedOccurrence<'a>,
) -> Option<&'a str> {
    let start = owner_start + row.occurrence.span.start;
    let end = owner_start + row.occurrence.span.end;
    let start = usize::try_from(start).ok()?;
    let end = usize::try_from(end).ok()?;
    core::str::from_utf8(source.get(start..end)?).ok()
}

#[test]
fn cross_file_method_value_is_oracle_package_function_read() {
    let root = fixture_root();
    let report = match Checker::default().run_in_package(TypeScriptSource::TypeScript, WEEKS, &root)
    {
        Ok(report) => report,
        Err(error) if checker_missing(&error) => {
            eprintln!("checker unavailable: {error}");
            return;
        }
        Err(error) => panic!("checker failed: {error}"),
    };

    let value_reference = report
        .references
        .iter()
        .find(|reference| {
            reference.name.as_deref() == Some("setNote")
                && !reference.is_field
                && reference.overload_index.is_none()
        })
        .unwrap_or_else(|| {
            panic!(
                "checker did not resolve cross-file method value `setNote`: {:#?}",
                report.references
            )
        });
    assert_eq!(value_reference.module.as_deref(), Some("./workout.service"));
    assert_eq!(value_reference.is_field, false);
    assert_eq!(value_reference.name.as_deref(), Some("setNote"));

    let mut diagnostic = [0_u8; 4096];
    let mut output = vec![0_u8; 8 * 1024 * 1024];
    let view = compile_weeks(WEEKS, &report, &mut diagnostic, &mut output);
    let occs = occurrences(&view);
    let group_owner = named_owner(&view, b"group", EntityKind::Function);
    let group_start = owner_decl_start(WEEKS, "group");

    let value_rows: Vec<_> = occs
        .iter()
        .filter(|row| {
            row.owner.raw == group_owner
                && row.occurrence.kind == ReferenceKind::VariableUse
                && row.occurrence.confidence == OccurrenceConfidence::Oracle
                && matches!(
                    row.occurrence.target,
                    OccurrenceTarget::Foreign(backend_semantic::ir::ForeignKey {
                        origin: ForeignOrigin::Package(_),
                        path: "setNote",
                        display: "setNote",
                        kind: Some(EntityKind::Function),
                    })
                )
        })
        .collect();
    assert_eq!(
        value_rows.len(),
        1,
        "expected exactly one oracle package method value read, saw {:#?}",
        occs
    );
    let row = value_rows[0];
    let OccurrenceTarget::Foreign(key) = row.occurrence.target else {
        unreachable!();
    };
    let ForeignOrigin::Package(lineage) = key.origin else {
        panic!("expected package origin");
    };
    assert_eq!(lineage.ecosystem, "npm");
    assert_eq!(lineage.name, "./workout.service");
    assert_eq!(
        occurrence_span_text(WEEKS, group_start, row),
        Some("setNote"),
        "method value span must cover the property name token only"
    );

    let call_rows: Vec<_> = occs
        .iter()
        .filter(|row| {
            row.owner.raw == group_owner
                && row.occurrence.kind == ReferenceKind::FunctionCall
                && row.occurrence.confidence == OccurrenceConfidence::Oracle
                && matches!(
                    row.occurrence.target,
                    OccurrenceTarget::Foreign(backend_semantic::ir::ForeignKey {
                        origin: ForeignOrigin::Package(_),
                        path: "setNote",
                        display: "setNote",
                        kind: None,
                    })
                )
        })
        .collect();
    assert_eq!(
        call_rows.len(),
        1,
        "expected exactly one oracle package method call, saw {:#?}",
        occs
    );
    let call_row = call_rows[0];
    let OccurrenceTarget::Foreign(call_key) = call_row.occurrence.target else {
        unreachable!();
    };
    let ForeignOrigin::Package(call_lineage) = call_key.origin else {
        panic!("expected package origin");
    };
    assert_eq!(call_lineage.ecosystem, "npm");
    assert_eq!(call_lineage.name, "./workout.service");
    assert_eq!(call_key.path, "setNote");
    assert_eq!(call_key.display, "setNote");
    assert_eq!(call_key.kind, None);
}

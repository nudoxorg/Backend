//! Cross-file TypeScript class field reads retarget through the import specifier.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::{Duration, Instant},
};

use backend_engine::driver::{
    CompileControl, CompileOutput, CompileRequest, CompileScratch, NativeTool,
    ResolvedToolchain, SemanticAuthorityInput, ToolchainSelection, compile,
};
use backend_frontend_typescript::legacy::{Checker, CheckerError, Report};
use backend_semantic::ir::{
    DecodedOccurrence, EntityKind, ForeignOrigin, FragmentView, OccurrenceConfidence,
    OccurrenceTarget, ReferenceKind,
};
use backend_semantic::vocabulary::{LanguageProfile, Stage, TypeScriptSource};

static FIXTURE_ID: AtomicUsize = AtomicUsize::new(0);
static CANCELLED: AtomicBool = AtomicBool::new(false);

const WORKOUT_SERVICE: &str = "export class WorkoutService {\n  note = 1;\n  setNote() {}\n}\n";

const WEEKS: &str = "import { WorkoutService } from \"./workout.service\";\n\nexport class Weeks {\n  service = new WorkoutService();\n  drive() {\n    const _local = this.note;\n    let _f = this.service.note;\n    this.service.setNote();\n  }\n  note = 2;\n}\n";

fn fixture_root() -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "nudox-ts-field-read-{}-{}",
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

fn leak_weeks() -> &'static [u8] {
    Box::leak(WEEKS.as_bytes().to_vec().into_boxed_slice())
}

fn compile_weeks(source: &'static [u8], report: &Report) -> FragmentView<'static> {
    let toolchain = ResolvedToolchain::from_version(
        NativeTool::TypeScriptCompiler,
        Path::new("/bin/true"),
        b"typescript-field-read-fixture",
    )
    .expect("toolchain");
    let diagnostic: &'static mut [u8] = Box::leak(Box::new([0; 4096]));
    let output: &'static mut [u8] = Box::leak(vec![0; 8 * 1024 * 1024].into_boxed_slice());
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

fn owner_decl_start(source: &str, owner_name: &str) -> u32 {
    let start = source
        .find(owner_name)
        .expect("owner declaration in source");
    u32::try_from(start).expect("owner start")
}

fn occurrence_span_text<'a>(
    source: &'a str,
    owner_start: u32,
    row: &DecodedOccurrence<'a>,
) -> Option<&'a str> {
    let start = owner_start + row.occurrence.span.start;
    let end = owner_start + row.occurrence.span.end;
    let start = usize::try_from(start).ok()?;
    let end = usize::try_from(end).ok()?;
    source.get(start..end)
}

#[test]
fn cross_file_class_field_read_is_oracle_package_field_access() {
    let root = fixture_root();
    let report = match Checker::default().run_in_package(
        TypeScriptSource::TypeScript,
        WEEKS.as_bytes(),
        &root,
    ) {
        Ok(report) => report,
        Err(error) if checker_missing(&error) => {
            eprintln!("checker unavailable: {error}");
            return;
        }
        Err(error) => panic!("checker failed: {error}"),
    };

    let field_reference = report
        .references
        .iter()
        .find(|reference| reference.name.as_deref() == Some("note") && reference.is_field)
        .unwrap_or_else(|| {
            panic!(
                "checker did not resolve cross-file class field `note` with a module: {:#?}",
                report.references
            )
        });
    assert!(
        field_reference.module.is_some(),
        "checker field reference must name the foreign module: {field_reference:#?}"
    );

    let source = leak_weeks();
    let view = compile_weeks(source, &report);
    let occs = occurrences(&view);
    let drive_owner = named_owner(&view, b"drive", EntityKind::Function);
    let drive_start = owner_decl_start(WEEKS, "drive()");

    let package_fields: Vec<_> = occs
        .iter()
        .filter(|row| {
            row.occurrence.kind == ReferenceKind::FieldAccess
                && row.occurrence.confidence == OccurrenceConfidence::Oracle
                && matches!(
                    row.occurrence.target,
                    OccurrenceTarget::Foreign(backend_semantic::ir::ForeignKey {
                        origin: ForeignOrigin::Package(_),
                        path,
                        display,
                        kind: Some(EntityKind::Field),
                    }) if path == "note" && display == "note"
                )
        })
        .collect();
    assert_eq!(
        package_fields.len(),
        1,
        "expected exactly one oracle package field read, saw {:#?}",
        occs
    );
    let row = package_fields[0];
    assert_eq!(row.owner.raw, drive_owner, "field read owner must be drive");
    let OccurrenceTarget::Foreign(key) = row.occurrence.target else {
        unreachable!();
    };
    let ForeignOrigin::Package(lineage) = key.origin else {
        panic!("expected package origin");
    };
    assert_eq!(lineage.ecosystem, "npm");
    assert_eq!(lineage.name, "./workout.service");
    assert_eq!(
        occurrence_span_text(WEEKS, drive_start, row),
        Some("note"),
        "field read span must cover the property name token only"
    );

    let local_owner = named_owner(&view, b"_local", EntityKind::Constant);
    let local_start = owner_decl_start(WEEKS, "_local");
    let local_this_note = occs.iter().any(|row| {
        row.occurrence.kind == ReferenceKind::FieldAccess
            && matches!(row.occurrence.target, OccurrenceTarget::Local(_))
            && occurrence_span_text(WEEKS, local_start, row) == Some("note")
            && row.owner.raw == local_owner
    });
    assert!(local_this_note, "same-file this.note must stay Local");

    assert!(
        !occs.iter().any(|row| {
            matches!(
                row.occurrence.target,
                OccurrenceTarget::Foreign(backend_semantic::ir::ForeignKey {
                    kind: Some(EntityKind::Field),
                    ..
                })
            ) && row.occurrence.kind == ReferenceKind::FieldAccess
                && occurrence_span_text(WEEKS, drive_start, row) == Some("setNote")
        }),
        "method call setNote must not become a package field key"
    );
}

//! Cross-file TypeScript const and let value reads retarget through the import specifier.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
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

const DEMO: &[u8] = b"export namespace Demo {
  export const note = 1;
  export let count = 2;
  export function setNote() {}
}
";

const READ: &[u8] = b"import { Demo } from \"./demo\";
export function read() {
  const value = Demo.note;
  const current = Demo.count;
  const bound = Demo.setNote;
  Demo.setNote();
}
";

fn fixture_root() -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "nudox-ts-const-value-{}-{}",
        std::process::id(),
        FIXTURE_ID.fetch_add(1, Ordering::Relaxed),
    ));
    fs::create_dir_all(&root).expect("fixture root");
    fs::write(root.join("demo.ts"), DEMO).expect("demo.ts");
    root
}

fn compile_read<'output>(
    source: &'static [u8],
    report: &Report,
    diagnostic: &mut [u8],
    output: &'output mut [u8],
) -> FragmentView<'output> {
    let toolchain = ResolvedToolchain::from_version(
        NativeTool::TypeScriptCompiler,
        Path::new("/bin/true"),
        b"typescript-const-value-fixture",
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
                cancelled: &std::sync::atomic::AtomicBool::new(false),
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
    .expect("lower read.ts")
    .fragment
}

fn occurrences<'a>(view: &'a FragmentView<'a>) -> Vec<DecodedOccurrence<'a>> {
    view.occurrences().into_iter().flatten().flatten().collect()
}

fn named_owner(view: &FragmentView<'_>, name: &[u8], kind: EntityKind) -> u32 {
    let atoms: Vec<_> = view
        .atoms()
        .map(|atom| (atom.ordinal.raw, atom.bytes.to_vec()))
        .collect();
    view.entities()
        .find(|entity| {
            atoms
                .iter()
                .find(|(ordinal, _)| *ordinal == entity.name.raw)
                .map(|(_, bytes)| bytes.as_slice() == name && entity.kind == kind)
                .unwrap_or(false)
        })
        .map(|entity| entity.entity.raw)
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
fn cross_file_const_and_let_values_are_oracle_package_reads() {
    let root = fixture_root();
    let report = Checker::default()
        .run_in_package(TypeScriptSource::TypeScript, READ, &root)
        .unwrap_or_else(|error: CheckerError| panic!("checker failed: {error}"));

    let note_reference = report
        .references
        .iter()
        .find(|reference| {
            reference.name.as_deref() == Some("note")
                && !reference.is_field
                && reference.overload_index.is_none()
        })
        .unwrap_or_else(|| {
            panic!(
                "checker did not resolve cross-file const `note`: {:#?}",
                report.references
            )
        });
    assert_eq!(note_reference.module.as_deref(), Some("./demo"));
    assert!(note_reference.is_const);
    assert!(!note_reference.is_variable);

    let count_reference = report
        .references
        .iter()
        .find(|reference| {
            reference.name.as_deref() == Some("count")
                && !reference.is_field
                && reference.overload_index.is_none()
        })
        .unwrap_or_else(|| {
            panic!(
                "checker did not resolve cross-file let `count`: {:#?}",
                report.references
            )
        });
    assert_eq!(count_reference.module.as_deref(), Some("./demo"));
    assert!(count_reference.is_variable);
    assert!(!count_reference.is_const);

    let set_note_value = report
        .references
        .iter()
        .find(|reference| {
            reference.name.as_deref() == Some("setNote")
                && !reference.is_field
                && reference.overload_index.is_none()
        })
        .unwrap_or_else(|| {
            panic!(
                "checker did not resolve cross-file function value `setNote`: {:#?}",
                report.references
            )
        });
    assert!(!set_note_value.is_const);
    assert!(!set_note_value.is_variable);
    assert!(!set_note_value.is_field);

    let mut diagnostic = [0_u8; 4096];
    let mut output = vec![0_u8; 8 * 1024 * 1024];
    let view = compile_read(READ, &report, &mut diagnostic, &mut output);
    let occs = occurrences(&view);
    let read_owner = named_owner(&view, b"read", EntityKind::Function);
    let read_start = owner_decl_start(READ, "read");

    let note_rows: Vec<_> = occs
        .iter()
        .filter(|row| {
            row.owner.raw == read_owner
                && row.occurrence.kind == ReferenceKind::VariableUse
                && row.occurrence.confidence == OccurrenceConfidence::Oracle
                && matches!(
                    row.occurrence.target,
                    OccurrenceTarget::Foreign(backend_semantic::ir::ForeignKey {
                        origin: ForeignOrigin::Package(_),
                        path: "note",
                        display: "note",
                        kind: Some(EntityKind::Constant),
                    })
                )
        })
        .collect();
    assert_eq!(
        note_rows.len(),
        1,
        "expected exactly one oracle package const value read, saw {:#?}",
        occs
    );
    let note_row = note_rows[0];
    let OccurrenceTarget::Foreign(note_key) = note_row.occurrence.target else {
        unreachable!();
    };
    let ForeignOrigin::Package(note_lineage) = note_key.origin else {
        panic!("expected package origin");
    };
    assert_eq!(note_lineage.ecosystem, "npm");
    assert_eq!(note_lineage.name, "./demo");
    assert_eq!(
        occurrence_span_text(READ, read_start, note_row),
        Some("note"),
        "const value span must cover the property name token only"
    );

    let count_rows: Vec<_> = occs
        .iter()
        .filter(|row| {
            row.owner.raw == read_owner
                && row.occurrence.kind == ReferenceKind::VariableUse
                && row.occurrence.confidence == OccurrenceConfidence::Oracle
                && matches!(
                    row.occurrence.target,
                    OccurrenceTarget::Foreign(backend_semantic::ir::ForeignKey {
                        origin: ForeignOrigin::Package(_),
                        path: "count",
                        display: "count",
                        kind: Some(EntityKind::Static),
                    })
                )
        })
        .collect();
    assert_eq!(
        count_rows.len(),
        1,
        "expected exactly one oracle package let value read, saw {:#?}",
        occs
    );
    let count_row = count_rows[0];
    let OccurrenceTarget::Foreign(count_key) = count_row.occurrence.target else {
        unreachable!();
    };
    let ForeignOrigin::Package(count_lineage) = count_key.origin else {
        panic!("expected package origin");
    };
    assert_eq!(count_lineage.ecosystem, "npm");
    assert_eq!(count_lineage.name, "./demo");
    assert_eq!(
        occurrence_span_text(READ, read_start, count_row),
        Some("count"),
        "let value span must cover the property name token only"
    );

    let value_rows: Vec<_> = occs
        .iter()
        .filter(|row| {
            row.owner.raw == read_owner
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
    let value_row = value_rows[0];
    let OccurrenceTarget::Foreign(value_key) = value_row.occurrence.target else {
        unreachable!();
    };
    let ForeignOrigin::Package(value_lineage) = value_key.origin else {
        panic!("expected package origin");
    };
    assert_eq!(value_lineage.ecosystem, "npm");
    assert_eq!(value_lineage.name, "./demo");
    assert_eq!(
        occurrence_span_text(READ, read_start, value_row),
        Some("setNote"),
        "method value span must cover the property name token only"
    );

    let call_rows: Vec<_> = occs
        .iter()
        .filter(|row| {
            row.owner.raw == read_owner
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
    assert_eq!(call_lineage.name, "./demo");
    assert_eq!(call_key.path, "setNote");
    assert_eq!(call_key.display, "setNote");
    assert_eq!(call_key.kind, None);
}

//! Cross-file Java enum-constant reads retarget through the declaring enum's maven namespace key.

use std::{
    path::Path,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::{Duration, Instant},
};

use backend_engine::driver::{
    CompileControl, CompileOutput, CompileRequest, CompileScratch, NativeTool, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainSelection, compile,
};
use backend_frontend_java::legacy::{
    JavaRelease as HarnessRelease,
    harness::{Harness, HarnessRequest, JavaSource, JdkToolchain},
};
use backend_semantic::ir::{
    DecodedOccurrence, EntityKind, ForeignOrigin, FragmentView, OccurrenceConfidence,
    OccurrenceTarget, ReferenceKind,
};
use backend_semantic::vocabulary::{JavaRelease, LanguageProfile, Stage};

static FIXTURE_ID: AtomicUsize = AtomicUsize::new(0);

const STATUS: &str = "package demo;\npublic enum Status { Active, Done }\n";

const DRIVE: &str =
    "package demo;\npublic final class Drive {\n  Status run() { return Status.Active; }\n}\n";

const LOCAL: &str =
    "package demo;\nenum Status { Active, Done }\npublic final class Drive {\n  Status run() { return Status.Active; }\n}\n";

fn jdk_ready() -> Option<JdkToolchain<'static>> {
    let root = std::env::var_os("NUDOX_JDK")?;
    JdkToolchain::from_owned_root(root.into()).ok()
}

fn compile_drive(image: &[u8], source: &[u8]) -> Vec<u8> {
    let toolchain = ResolvedToolchain::from_version(
        NativeTool::JavaCompiler,
        Path::new("/bin/true"),
        b"java-enum-constant-fixture",
    )
    .expect("toolchain");
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0_u8; 262_144];
    let mut output = vec![0_u8; 262_144];
    let work = std::env::temp_dir().join(format!(
        "nudox-java-enum-constant-{}-{}",
        std::process::id(),
        FIXTURE_ID.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::create_dir_all(&work).expect("work dir");
    compile(
        CompileRequest {
            profile: LanguageProfile::Java(JavaRelease::Java21),
            stage: Stage::LowerIr,
            source,
            declaration_scope: backend_engine::driver::DeclarationScope::fixture(),
            toolchain: ToolchainSelection::ResolvedNative(toolchain),
            authority: SemanticAuthorityInput::Java { image },
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(120),
                cancelled: &cancelled,
            },
        },
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: &work,
        },
        CompileOutput {
            fragment_output: &mut output,
        },
    )
    .expect("lower Drive.java");
    let length = output
        .get(8..12)
        .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
        .map(u32::from_le_bytes)
        .expect("header length") as usize;
    output.get(..length).expect("fragment bytes").to_vec()
}

fn lane(bytes: &[u8]) -> (FragmentView<'_>, Vec<DecodedOccurrence<'_>>) {
    let view = FragmentView::validate(bytes).expect("validate fragment");
    let occurrences = view
        .occurrences()
        .into_iter()
        .flatten()
        .flatten()
        .collect();
    (view, occurrences)
}

fn owner_name(view: &FragmentView<'_>, owner: backend_semantic::ir::EntityId) -> Vec<u8> {
    let atom = view
        .entities()
        .find(|entity| entity.entity.raw == owner.raw)
        .expect("owner entity")
        .name
        .raw;
    view.atoms()
        .nth(atom as usize)
        .map(|atom| atom.bytes.to_vec())
        .expect("owner name atom")
}

fn owner_decl_start(source: &str, owner_name: &str) -> u32 {
    let start = source.find(owner_name).expect("owner in source");
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

fn authority_image(
    jdk: &JdkToolchain<'_>,
    sources: &[JavaSource<'_>],
) -> Vec<u8> {
    let mut harness = Harness::new().expect("harness");
    harness.prepare(jdk).expect("prepare doclet");
    let mut image = Vec::new();
    harness
        .image(
            jdk,
            HarnessRequest {
                sources,
                classpath: &[],
                release: HarnessRelease::Java21,
            },
            &mut image,
        )
        .expect("authority image");
    image
}

#[test]
fn cross_file_enum_constant_read_targets_declaring_enum_namespace_key() {
    let Some(jdk) = jdk_ready() else {
        eprintln!("skip: NUDOX_JDK unset");
        return;
    };

    let sources = [
        JavaSource {
            name: Path::new("demo/Drive.java"),
            bytes: DRIVE.as_bytes(),
        },
        JavaSource {
            name: Path::new("demo/Status.java"),
            bytes: STATUS.as_bytes(),
        },
    ];
    let image = authority_image(&jdk, &sources);
    let bytes = compile_drive(&image, DRIVE.as_bytes());
    let (view, occs) = lane(&bytes);

    let run_owner = view
        .entities()
        .find(|entity| {
            entity.kind == EntityKind::Function
                && view
                    .atoms()
                    .nth(entity.name.raw as usize)
                    .is_some_and(|atom| atom.bytes == b"run")
        })
        .expect("run method")
        .entity;
    let run_start = owner_decl_start(DRIVE, "Status run");

    let namespace_reads: Vec<_> = occs
        .iter()
        .filter(|row| {
            row.occurrence.kind == ReferenceKind::VariableUse
                && row.occurrence.confidence == OccurrenceConfidence::Oracle
                && matches!(
                    row.occurrence.target,
                    OccurrenceTarget::Foreign(backend_semantic::ir::ForeignKey {
                        origin: ForeignOrigin::Namespace {
                            ecosystem: "maven",
                            namespace: "demo.Status",
                        },
                        path,
                        display,
                        kind: Some(EntityKind::Variant),
                    }) if path == "Active" && display == "Active"
                )
        })
        .collect();
    assert_eq!(
        namespace_reads.len(),
        1,
        "expected exactly one maven demo.Status Active variant read, saw {:#?}",
        occs
            .iter()
            .map(|row| (row.occurrence.kind, row.occurrence.target))
            .collect::<Vec<_>>()
    );

    let row = namespace_reads[0];
    assert_eq!(row.owner, run_owner, "enum read owner must be run");
    assert_eq!(
        occurrence_span_text(DRIVE, run_start, row),
        Some("Active"),
        "enum constant span must cover Active only"
    );

    assert!(
        !occs.iter().any(|row| {
            matches!(
                row.occurrence.target,
                OccurrenceTarget::Foreign(backend_semantic::ir::ForeignKey {
                    path,
                    display,
                    ..
                }) if path == "Done" || display == "Done"
            )
        }),
        "Status.Done must not be emitted"
    );
}

#[test]
fn same_file_enum_constant_read_stays_local() {
    let Some(jdk) = jdk_ready() else {
        eprintln!("skip: NUDOX_JDK unset");
        return;
    };

    let sources = [JavaSource {
        name: Path::new("demo/Drive.java"),
        bytes: LOCAL.as_bytes(),
    }];
    let image = authority_image(&jdk, &sources);
    let bytes = compile_drive(&image, LOCAL.as_bytes());
    let (_, occs) = lane(&bytes);

    let local_active = occs.iter().any(|row| {
        row.occurrence.kind == ReferenceKind::VariableUse
            && row.occurrence.confidence == OccurrenceConfidence::Oracle
            && matches!(row.occurrence.target, OccurrenceTarget::Local(_))
            && row
                .occurrence
                .span
                .end
                .checked_sub(row.occurrence.span.start) == Some(6)
    });
    assert!(local_active, "same-file Status.Active must stay Local");

    assert!(
        !occs.iter().any(|row| {
            matches!(
                row.occurrence.target,
                OccurrenceTarget::Foreign(backend_semantic::ir::ForeignKey {
                    origin: ForeignOrigin::Namespace { .. },
                    ..
                })
            ) && row.occurrence.kind == ReferenceKind::VariableUse
        }),
        "same-file enum constant must not use a namespace key"
    );
}

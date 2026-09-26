//! Cross-file TypeScript enum member reads retarget through the import specifier.

use std::{
    path::Path,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use backend_engine::driver::{
    CompileControl, CompileOutput, CompileRequest, CompileScratch, NativeTool,
    ResolvedToolchain, SemanticAuthorityInput, ToolchainSelection, compile,
};
use backend_frontend_typescript::legacy::{Reference, Report};
use backend_semantic::ir::{
    DecodedOccurrence, EntityKind, ForeignOrigin, FragmentView, OccurrenceConfidence,
    OccurrenceTarget, ReferenceKind,
};
use backend_semantic::vocabulary::{LanguageProfile, Stage, TypeScriptSource};

static CANCELLED: AtomicBool = AtomicBool::new(false);

const USER_TS: &str = "import { Color } from \"./color\";\nexport enum Other { Green = 2 }\nexport function pick(): boolean { return Color.Red === 1; }\nexport function wrong(): boolean { return Other.Red === 1; }\n";

const SAME_FILE_TS: &str = "export enum Color { Red = 1 }\nexport function pick(): boolean { return Color.Red === 1; }\n";

fn report(source: &[u8], references: Vec<Reference>) -> Report {
    let digest = backend_frontend_typescript::legacy::source_digest(source)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Report {
        schema_version: 1,
        source_digest: digest,
        declaration_file: false,
        diagnostics: Box::new([]),
        declarations: Box::new([]),
        references: references.into_boxed_slice(),
        narrowings: Box::new([]),
    }
}

fn token_offset(source: &str, site: &str, token: &str) -> u32 {
    let site_at = source.find(site).expect("reference site");
    let offset = site
        .find(token)
        .expect("token inside site");
    u32::try_from(site_at + offset).expect("token offset")
}

fn compile_source(source: &'static [u8], authority: &Report) -> FragmentView<'static> {
    let toolchain = ResolvedToolchain::from_version(
        NativeTool::TypeScriptCompiler,
        Path::new("/bin/true"),
        b"typescript-enum-cross-file-fixture",
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
            authority: SemanticAuthorityInput::TypeScript { report: authority },
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
    .expect("lower source")
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

fn package_red_keys<'a>(occs: &'a [DecodedOccurrence<'a>]) -> Vec<&'a DecodedOccurrence<'a>> {
    occs.iter()
        .filter(|row| {
            row.occurrence.kind == ReferenceKind::FieldAccess
                && row.occurrence.confidence == OccurrenceConfidence::Oracle
                && matches!(
                    row.occurrence.target,
                    OccurrenceTarget::Foreign(backend_semantic::ir::ForeignKey {
                        origin: ForeignOrigin::Package(_),
                        path,
                        display,
                        kind: Some(EntityKind::Variant),
                    }) if path == "Red" && display == "Red"
                )
        })
        .collect()
}

#[test]
fn cross_file_enum_member_is_oracle_package_variant_access() {
    let source = Box::leak(USER_TS.as_bytes().to_vec().into_boxed_slice());
    let red_start = token_offset(USER_TS, "return Color.Red === 1", "Red");
    let color_start = token_offset(USER_TS, "return Color.Red === 1", "Color");
    let authority = report(
        source,
        vec![
            Reference {
                start: color_start,
                end: color_start + 5,
                target_start: None,
                target_end: None,
                module: Some("./color".into()),
                name: Some("Color".into()),
                overload_index: None,
                is_field: false,
                is_enum_member: false,
            },
            Reference {
                start: red_start,
                end: red_start + 3,
                target_start: None,
                target_end: None,
                module: Some("./color".into()),
                name: Some("Red".into()),
                overload_index: None,
                is_field: false,
                is_enum_member: true,
            },
        ],
    );

    let view = compile_source(source, &authority);
    let occs = occurrences(&view);
    let pick_owner = named_owner(&view, b"pick", EntityKind::Function);
    let pick_start = owner_decl_start(USER_TS, "function pick");

    let package_red = package_red_keys(&occs);
    assert_eq!(
        package_red.len(),
        1,
        "expected exactly one oracle package enum member read, saw {:#?}",
        occs
    );
    let row = package_red[0];
    assert_eq!(row.owner.raw, pick_owner, "enum member read owner must be pick");
    let OccurrenceTarget::Foreign(key) = row.occurrence.target else {
        unreachable!();
    };
    let ForeignOrigin::Package(lineage) = key.origin else {
        panic!("expected package origin");
    };
    assert_eq!(lineage.ecosystem, "npm");
    assert_eq!(lineage.name, "./color");
    assert_eq!(key.path, "Red");
    assert_eq!(key.display, "Red");
    assert_eq!(
        occurrence_span_text(USER_TS, pick_start, row),
        Some("Red"),
        "enum member read span must cover the property name token only"
    );

    let wrong_owner = named_owner(&view, b"wrong", EntityKind::Function);
    let wrong_start = owner_decl_start(USER_TS, "wrong()");
    assert!(
        !occs.iter().any(|row| {
            row.owner.raw == wrong_owner
                && occurrence_span_text(USER_TS, wrong_start, row) == Some("Red")
                && matches!(
                    row.occurrence.target,
                    OccurrenceTarget::Foreign(backend_semantic::ir::ForeignKey {
                        origin: ForeignOrigin::Package(lineage),
                        ..
                    }) if lineage.name == "./color"
                )
        }),
        "Other.Red must not become a package key for ./color"
    );
}

#[test]
fn same_file_enum_member_stays_local_without_package_red() {
    let source = Box::leak(SAME_FILE_TS.as_bytes().to_vec().into_boxed_slice());
    let authority = report(source, vec![]);
    let view = compile_source(source, &authority);
    let occs = occurrences(&view);
    let pick_owner = named_owner(&view, b"pick", EntityKind::Function);

    let local_red: Vec<_> = occs
        .iter()
        .filter(|row| {
            row.owner.raw == pick_owner
                && row.occurrence.kind == ReferenceKind::FieldAccess
                && matches!(row.occurrence.target, OccurrenceTarget::Local(_))
                && row.occurrence.confidence == OccurrenceConfidence::Index
        })
        .collect();
    assert_eq!(local_red.len(), 1, "same-file Color.Red is one local field access");

    assert_eq!(
        package_red_keys(&occs).len(),
        0,
        "same-file Color.Red must not emit a package key named Red"
    );
}

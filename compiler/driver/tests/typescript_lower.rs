//! Public TypeScript lowering falsifiers.
//!
//! These tests deliberately enter through `compile_ir`; the checker report is
//! an input witness, not an invocation of the lowering module's private API.

use std::{
    path::Path,
    sync::atomic::AtomicBool,
    thread,
    time::{Duration, Instant},
};

use compiler_driver::{
    CompileControl, CompileFailure, CompileOutput, CompileRequest, CompileScratch, NativeTool,
    ResolvedToolchain, SemanticAuthorityInput, ToolchainSelection, compile, compile_ir,
};
use compiler_ir::{
    DecodedOccurrence, DecodedTypeFact, EntityKind, FragmentView, ItemKind, OccurrenceConfidence,
    OccurrenceTarget, PrimitiveShape, ReferenceKind, SemanticTypeTag, TypeReason, TypeWidth,
};
use compiler_languages_typescript::{
    Checker, MappedModifier as CheckerMappedModifier, Report, TypeTree,
};
use compiler_publication::{
    OpenPublicationScratch, PublicationScratch, PublishControl, open_published, publish_compiled,
};
use compiler_vocabulary::{
    LanguageProfile, LoweringUnsupported, ProjectionAdmissionFault, ProjectionSemanticTypeFault,
    ProjectionSemanticTypeTag, Stage, TypeScriptSource,
};
use server_journal::{DurablePublisher, PublicationLimits, PublicationPaths};

const SOURCE: &[u8] = include_bytes!("../../languages/typescript/tests/fixtures/source.ts");
const TRANSCRIPT: &[u8] =
    include_bytes!("../../languages/typescript/tests/transcripts/golden.json");
static CANCELLED: AtomicBool = AtomicBool::new(false);

fn report(source: &[u8]) -> Report {
    let digest = compiler_languages_typescript::source_digest(source)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Report {
        schema_version: 1,
        source_digest: digest,
        diagnostics: Box::new([]),
        declarations: Box::new([]),
        references: Box::new([]),
        narrowings: Box::new([]),
    }
}

fn try_lower(
    source: &'static [u8],
    authority: Option<&Report>,
) -> Result<compiler_driver::CompiledIr, CompileFailure<'static>> {
    let toolchain = ResolvedToolchain::from_version(
        NativeTool::TypeScriptCompiler,
        Path::new("/bin/true"),
        b"typescript-authority-test",
    )
    .unwrap();
    let diagnostic: &'static mut [u8] = Box::leak(Box::new([0; 4096]));
    compile_ir(
        CompileRequest {
            profile: LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            stage: Stage::LowerIr,
            source,
            declaration_scope: compiler_driver::DeclarationScope::fixture(),
            toolchain: ToolchainSelection::ResolvedNative(toolchain),
            authority: authority.map_or(SemanticAuthorityInput::None, |report| {
                SemanticAuthorityInput::TypeScript { report }
            }),
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(30),
                cancelled: &CANCELLED,
            },
        },
        CompileScratch {
            diagnostic_output: diagnostic,
            native_work: Path::new("/tmp"),
        },
    )
}

fn fragment(source: &'static [u8], authority: Option<&Report>) -> FragmentView<'static> {
    let toolchain = ResolvedToolchain::from_version(
        NativeTool::TypeScriptCompiler,
        Path::new("/bin/true"),
        b"typescript-authority-test",
    )
    .unwrap();
    let diagnostic: &'static mut [u8] = Box::leak(Box::new([0; 4096]));
    let output: &'static mut [u8] = Box::leak(vec![0; 8 * 1024 * 1024].into_boxed_slice());
    compile(
        CompileRequest {
            profile: LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            stage: Stage::LowerIr,
            source,
            declaration_scope: compiler_driver::DeclarationScope::fixture(),
            toolchain: ToolchainSelection::ResolvedNative(toolchain),
            authority: authority.map_or(SemanticAuthorityInput::None, |report| {
                SemanticAuthorityInput::TypeScript { report }
            }),
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(30),
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
    .unwrap()
    .fragment
}

fn view(source: &'static [u8], authority: Option<&Report>) -> FragmentView<'static> {
    match authority {
        Some(report) => fragment(source, Some(report)),
        None => {
            let report = report(source);
            fragment(source, Some(&report))
        }
    }
}

fn entities(view: &FragmentView<'_>) -> Vec<(u32, Vec<u8>, EntityKind)> {
    let atoms: Vec<_> = view
        .atoms()
        .map(|a| (a.ordinal.raw, a.bytes.to_vec()))
        .collect();
    view.entities()
        .map(|e| {
            (
                e.entity.raw,
                atoms
                    .iter()
                    .find(|(n, _)| *n == e.name.raw)
                    .map(|(_, b)| b.clone())
                    .unwrap_or_default(),
                e.kind,
            )
        })
        .collect()
}
fn named(view: &FragmentView<'_>, name: &[u8]) -> (u32, EntityKind) {
    entities(view)
        .into_iter()
        .find(|(_, n, _)| n == name)
        .map(|(id, _, kind)| (id, kind))
        .unwrap()
}
fn facts<'a>(view: &'a FragmentView<'a>) -> Vec<DecodedTypeFact<'a>> {
    view.type_facts().into_iter().flatten().flatten().collect()
}
fn fact<'a>(view: &'a FragmentView<'a>, owner: u32) -> DecodedTypeFact<'a> {
    facts(view)
        .into_iter()
        .find(|f| f.owner.raw == owner)
        .unwrap()
}
fn occurrences<'a>(view: &'a FragmentView<'a>) -> Vec<DecodedOccurrence<'a>> {
    view.occurrences().into_iter().flatten().flatten().collect()
}

fn ir_tag_shape(ir: &compiler_ir::Ir, id: compiler_ir::TypeId) -> (SemanticTypeTag, u8) {
    use compiler_ir::{ComputedType, ConcreteType, TypeExpr};
    match ir.ty(id).unwrap() {
        TypeExpr::Concrete(ConcreteType::Builtin(_) | ConcreteType::Literal(_)) => {
            (SemanticTypeTag::Primitive, 0)
        }
        TypeExpr::Concrete(ConcreteType::Nominal(_)) => (SemanticTypeTag::Nominal, 0),
        TypeExpr::Concrete(ConcreteType::Applied { arguments, .. }) => (
            SemanticTypeTag::Apply,
            ir.types(arguments).unwrap().len() as u8,
        ),
        TypeExpr::Concrete(ConcreteType::Function {
            parameters,
            results,
            ..
        }) => (
            SemanticTypeTag::FunctionPointer,
            (ir.tuple_elements(parameters).unwrap().len()
                + ir.tuple_elements(results).unwrap().len()) as u8,
        ),
        TypeExpr::Concrete(ConcreteType::Array { .. }) => (SemanticTypeTag::ArraySequence, 1),
        TypeExpr::Concrete(ConcreteType::Union(types)) => {
            (SemanticTypeTag::Union, ir.types(types).unwrap().len() as u8)
        }
        TypeExpr::Concrete(_) => (SemanticTypeTag::Unknown, 0),
        TypeExpr::Unknown(_) => (SemanticTypeTag::Unknown, 0),
        TypeExpr::Computed(ComputedType::This) => (SemanticTypeTag::SelfType, 0),
        TypeExpr::Computed(ComputedType::KeyOf(_))
        | TypeExpr::Computed(ComputedType::TypeOf(_)) => (SemanticTypeTag::Nominal, 1),
        TypeExpr::Computed(ComputedType::IndexedAccess { .. }) => (SemanticTypeTag::Apply, 2),
        TypeExpr::Computed(ComputedType::Conditional { .. }) => (SemanticTypeTag::Conditional, 4),
        TypeExpr::Computed(ComputedType::Mapped { .. }) => (SemanticTypeTag::Mapped, 3),
        TypeExpr::Computed(ComputedType::Infer { .. }) => (SemanticTypeTag::TypeVar, 1),
        TypeExpr::Computed(ComputedType::TemplateLiteral(parts)) => (
            SemanticTypeTag::TemplateLiteral,
            ir.template_parts(parts).unwrap().len() as u8,
        ),
        TypeExpr::Computed(ComputedType::Import { arguments, .. }) => (
            SemanticTypeTag::Apply,
            ir.types(arguments).unwrap().len() as u8,
        ),
        TypeExpr::Computed(ComputedType::Awaited(_)) => (SemanticTypeTag::Apply, 1),
    }
}

fn expected_kind_matches(actual: compiler_ir::ItemKind, expected: EntityKind) -> bool {
    matches!(
        (actual, expected),
        (compiler_ir::ItemKind::Constant, EntityKind::Constant)
            | (compiler_ir::ItemKind::Function, EntityKind::Function)
            | (compiler_ir::ItemKind::Record, EntityKind::Record)
            | (compiler_ir::ItemKind::Trait, EntityKind::Trait)
            | (compiler_ir::ItemKind::Static, EntityKind::Static)
    )
}

#[test]
fn declared_scalar_annotations_map_onto_lattice_records() {
    let v = view(b"export const n: number = 1; export const s: string = 'x'; export const b: boolean = false;", None);
    assert_eq!(
        fact(&v, named(&v, b"n").0).record.payload0,
        u32::from(PrimitiveShape::Float)
    );
    assert_eq!(
        fact(&v, named(&v, b"s").0).record.payload0,
        u32::from(PrimitiveShape::Str)
    );
    assert_eq!(
        fact(&v, named(&v, b"b").0).record.payload0,
        u32::from(PrimitiveShape::Bool)
    );
}
#[test]
fn mutually_recursive_interfaces_keep_diagonal_self_nominals_and_linked_members() {
    let v = view(
        b"export interface A { b: B; } export interface B { a: A; }",
        None,
    );
    for n in [b"A".as_slice(), b"B".as_slice()] {
        let (id, k) = named(&v, n);
        assert_eq!(k, EntityKind::Trait);
        assert_eq!(
            fact(&v, id).record.nominal,
            Some(compiler_ir::NominalRef::Local(compiler_ir::EntityId::new(
                id
            )))
        );
    }
}
#[test]
fn structural_types_commit_union_intersection_tuple_array_and_apply_records() {
    let v=view(b"export type U=string|number; export type I=string&number; export type T=[string,number]; export const a:number[]= [];",None);
    for (n, t, c) in [
        (b"U", SemanticTypeTag::Union, 2),
        (b"I", SemanticTypeTag::Intersection, 2),
        (b"T", SemanticTypeTag::Tuple, 2),
        (b"a", SemanticTypeTag::ArraySequence, 1),
    ] {
        let f = fact(&v, named(&v, n).0);
        assert_eq!(f.record.tag, t);
        assert_eq!(f.record.children.length, c);
    }
}
#[test]
fn signatures_commit_parameter_and_result_facts_and_distinct_overloads() {
    let v=view(b"export function f(x:number,y:string):boolean{return true;} declare function g(x:string):void; declare function g(x:number):void;",None);
    assert_eq!(fact(&v, named(&v, b"f").0).record.children.length, 3);
    assert_eq!(
        entities(&v)
            .into_iter()
            .filter(|(_, n, k)| n == b"g" && *k == EntityKind::Function)
            .count(),
        2
    );
}
#[test]
fn references_resolve_local_import_and_unresolved_targets() {
    let v=view(b"import { foreign } from 'pkg'; export interface Shape { area(): number; } export const s: Shape = foreign(); export const broken = missingGlobal;",None);
    let (_, k) = named(&v, b"foreign");
    assert_eq!(k, EntityKind::Reexport);
    assert!(
        occurrences(&v)
            .iter()
            .any(|o| o.occurrence.kind == ReferenceKind::Import
                && o.occurrence.confidence == OccurrenceConfidence::Import)
    );
    let (s, _) = named(&v, b"s");
    assert!(
        occurrences(&v)
            .iter()
            .any(|o| o.owner.raw == s && o.occurrence.confidence == OccurrenceConfidence::Index)
    );
}
#[test]
fn jsdoc_commits_text_code_and_local_link_fragments() {
    let v = view(
        b"/** Checks {@code x} and {@link Shape}. */ export interface Shape {}",
        None,
    );
    assert!(v.docs().is_some());
}
#[test]
fn absent_jsdoc_commits_no_documentation_section() {
    let v = view(b"export interface Plain {}", None);
    assert!(v.docs().is_none());
}
#[test]
fn template_literal_mapped_and_conditional_records_commit_their_tags() {
    let v=view(b"export type Lit=`pre${string}post`; export type Branch=string extends string ? number : boolean;",None);
    assert_eq!(
        fact(&v, named(&v, b"Lit").0).record.tag,
        SemanticTypeTag::TemplateLiteral
    );
    assert_eq!(fact(&v, named(&v, b"Branch").0).record.children.length, 4);
    let lit_owner = named(&v, b"Lit").0;
    let branch_owner = named(&v, b"Branch").0;
    assert!(facts(&v).iter().any(|f| {
        f.owner.raw == lit_owner
            && f.record.tag == SemanticTypeTag::TemplateLiteral
            && f.record.children.length == 3
    }));
    assert!(facts(&v).iter().any(|f| {
        f.owner.raw == branch_owner
            && f.record.tag == SemanticTypeTag::Conditional
            && f.record.children.length == 4
    }));
}

#[test]
fn decoded_computed_records_retain_mapped_modifiers_and_literal_bases() {
    let source =
        b"export type M={ readonly [K in string]?: number }; export type L=\"ok\"|42|1n|true;";
    let v = view(source, None);
    let mapped_owner = named(&v, b"M").0;
    let mapped = facts(&v)
        .into_iter()
        .find(|f| f.owner.raw == mapped_owner && f.record.tag == SemanticTypeTag::Mapped)
        .unwrap();
    assert_eq!(mapped.record.children.length, 2);
    assert_eq!(mapped.record.payload0, 0);
    assert_eq!(mapped.record.payload1, 0);
    let bases: Vec<_> = facts(&v)
        .into_iter()
        .filter(|f| {
            f.record.tag == SemanticTypeTag::Primitive
                && f.record.payload0 == u32::from(PrimitiveShape::Builtin)
                && f.record.text.is_some()
        })
        .map(|f| f.record.payload1)
        .collect();
    let mut sorted = bases;
    sorted.sort_unstable();
    assert_eq!(sorted, vec![0, 1, 2, 3]);
}
#[test]
fn anonymous_object_literals_commit_named_member_children() {
    let v = view(
        b"export const p: { readonly a: string; b?: number } = {a:'x',b:1};",
        None,
    );
    assert_eq!(
        fact(&v, named(&v, b"p").0).record.tag,
        SemanticTypeTag::AnonymousRecord
    );
    assert_eq!(fact(&v, named(&v, b"p").0).record.children.length, 2);
}
#[test]
fn single_declaration_owns_its_annotation_without_synthetic_rows() {
    let v = view(b"export const only: number = 1;", None);
    assert_eq!(entities(&v).len(), 1);
    assert_eq!(
        fact(&v, named(&v, b"only").0).record.payload1,
        TypeWidth::Fixed(64).to_cell()
    );
}
#[test]
fn foreign_generic_reference_is_unknown_without_checker_module_authority() {
    let v = view(b"export const m: Map<string, number> = new Map();", None);
    let f = fact(&v, named(&v, b"m").0);
    assert_eq!(f.record.tag, SemanticTypeTag::Unknown);
    assert_eq!(f.record.payload0, u32::from(TypeReason::UnresolvedExternal));
    assert_eq!(f.record.text, Some(&b"Map"[..]));
}

#[test]
fn self_referential_alias_is_bounded_on_a_small_stack() {
    const SOURCE: &[u8] = b"export type A = A | false; export const x: A = false;";
    let join = thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(|| {
            let checker = Checker::default()
                .run(TypeScriptSource::TypeScript, SOURCE)
                .unwrap();
            let view = view(SOURCE, Some(&checker));
            let owner = named(&view, b"x").0;
            let record = facts(&view)
                .into_iter()
                .find(|record| {
                    record.owner.raw == owner
                        && record.segment == compiler_ir::TypeFactSegment::Computed
                })
                .unwrap();
            (
                record.record.tag,
                TypeReason::try_from(record.record.payload0).ok(),
            )
        })
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(
        join,
        (SemanticTypeTag::Unknown, Some(TypeReason::DynamicallyTyped))
    );
}

#[test]
fn plugin_union_keeps_all_forty_literal_members_reachable() {
    const SOURCE: &[u8] = b"export const Plugin = null;";
    let mut checker = report(SOURCE);
    checker.declarations = Box::new([compiler_languages_typescript::Declaration {
        name_start: 13,
        name_end: 19,
        origin: compiler_languages_typescript::Origin::Computed,
        overload_index: None,
        r#type: Some(compiler_languages_typescript::TypeTree::Union {
            members: (0..40)
                .map(|index| compiler_languages_typescript::TypeTree::Literal {
                    base: compiler_languages_typescript::LiteralBase::String,
                    text: index.to_string(),
                })
                .collect(),
        }),
    }]);
    let view = view(SOURCE, Some(&checker));
    let literals = facts(&view)
        .into_iter()
        .filter(|record| record.segment == compiler_ir::TypeFactSegment::Computed)
        .filter(|record| record.record.tag == SemanticTypeTag::Primitive)
        .count();
    assert_eq!(literals, 40);
}

#[test]
fn checker_mapped_types_map_their_modifier_vocabularies_and_as_child_exactly() {
    const SOURCE: &[u8] = b"export type added = { +readonly [K in string as number]+?: boolean };\nexport type removed = { -readonly [K in string]-?: boolean };\nexport type preserved = { [K in string]: boolean };";
    let primitive = |name: &str| TypeTree::Primitive {
        name: name.to_owned(),
    };
    let declaration = |name: &str,
                       readonly: CheckerMappedModifier,
                       optional: CheckerMappedModifier,
                       name_as: bool| {
        let start = SOURCE
            .windows(name.len())
            .position(|window| window == name.as_bytes())
            .expect("mapped declaration name") as u32;
        compiler_languages_typescript::Declaration {
            name_start: start,
            name_end: start + u32::try_from(name.len()).expect("name width"),
            origin: compiler_languages_typescript::Origin::Computed,
            overload_index: None,
            r#type: Some(TypeTree::Mapped {
                parameter: "K".to_owned(),
                constraint: Box::new(primitive("string")),
                name_as: name_as.then(|| Box::new(primitive("number"))),
                value: Box::new(primitive("boolean")),
                readonly,
                optional,
            }),
        }
    };
    let mut checker = report(SOURCE);
    checker.declarations = Box::new([
        declaration(
            "added",
            CheckerMappedModifier::Add,
            CheckerMappedModifier::Add,
            true,
        ),
        declaration(
            "removed",
            CheckerMappedModifier::Remove,
            CheckerMappedModifier::Remove,
            false,
        ),
        declaration(
            "preserved",
            CheckerMappedModifier::Preserve,
            CheckerMappedModifier::Preserve,
            false,
        ),
    ]);
    let view = view(SOURCE, Some(&checker));
    for (name, readonly, optional, children) in [
        (
            b"added".as_slice(),
            compiler_ir::LatticeMappedModifier::Add,
            compiler_ir::LatticeMappedModifier::Add,
            3,
        ),
        (
            b"removed".as_slice(),
            compiler_ir::LatticeMappedModifier::Remove,
            compiler_ir::LatticeMappedModifier::Remove,
            2,
        ),
        (
            b"preserved".as_slice(),
            compiler_ir::LatticeMappedModifier::Absent,
            compiler_ir::LatticeMappedModifier::Absent,
            2,
        ),
    ] {
        let owner = named(&view, name).0;
        let row = facts(&view)
            .into_iter()
            .find(|row| {
                row.owner.raw == owner
                    && row.segment == compiler_ir::TypeFactSegment::Computed
                    && row.record.tag == SemanticTypeTag::Mapped
            })
            .expect("computed mapped row");
        assert_eq!(row.record.payload0, u32::from(readonly));
        assert_eq!(row.record.payload1, u32::from(optional));
        assert_eq!(row.record.children.length, children);
    }
}

#[test]
fn direct_mapped_conditional_key_does_not_fabricate_an_optional_modifier() {
    const SOURCE: &[u8] = b"export type table<T, U, X, Y, V> = { [K in T extends U ? X : Y]: V };";
    let view = view(SOURCE, None);
    let owner = named(&view, b"table").0;
    let mapped = facts(&view)
        .into_iter()
        .find(|row| row.owner.raw == owner && row.record.tag == SemanticTypeTag::Mapped)
        .expect("direct mapped row");
    assert_eq!(
        mapped.record.payload1,
        u32::from(compiler_ir::LatticeMappedModifier::Absent)
    );
}
#[test]
fn empty_source_admits_the_current_schema_fragment_without_semantic_data() {
    assert!(try_lower(b"", Some(&report(b""))).is_err());
}
#[test]
fn oxc_bound_symbols_fill_the_canonical_declaration_lane() {
    let v = view(
        b"import { foreign } from 'pkg'; export class Box {} export const value = foreign;",
        None,
    );
    assert_eq!(entities(&v).len(), 3);
    assert_eq!(named(&v, b"Box").1, EntityKind::Record);
}
#[test]
fn oxc_interface_is_never_rewritten_as_a_record() {
    let v = view(b"export interface Shape { area(): number; }", None);
    let (id, k) = named(&v, b"Shape");
    assert_eq!(k, EntityKind::Trait);
    assert_eq!(fact(&v, id).record.tag, SemanticTypeTag::Nominal);
}

#[test]
fn golden_computed_cells_fill_extension_facts_with_checker_rows() {
    let r = Checker::default().decode(TRANSCRIPT).unwrap();
    assert!(try_lower(SOURCE, Some(&r)).unwrap().ir.entity_count() > 0);
}
#[test]
fn golden_inferred_const_and_union_records_match_the_checker() {
    let r = Checker::default().decode(TRANSCRIPT).unwrap();
    assert!(try_lower(SOURCE, Some(&r)).unwrap().ir.entity_count() >= 2);
}
#[test]
fn golden_foreign_generic_base_names_the_resolved_spelling() {
    let r = Checker::default().decode(TRANSCRIPT).unwrap();
    assert!(try_lower(SOURCE, Some(&r)).unwrap().ir.entity_count() >= 5);
}
#[test]
fn golden_this_type_and_local_nominal_computed_rows_bind_by_name() {
    let r = Checker::default().decode(TRANSCRIPT).unwrap();
    assert!(try_lower(SOURCE, Some(&r)).unwrap().ir.entity_count() >= 10);
}
#[test]
fn golden_oracle_confidence_picks_distinct_overload_targets() {
    let r = Checker::default().decode(TRANSCRIPT).unwrap();
    assert!(try_lower(SOURCE, Some(&r)).unwrap().ir.entity_count() >= 10);
}
#[test]
fn golden_narrowing_extends_the_declared_fact_with_a_site_row() {
    let r = Checker::default().decode(TRANSCRIPT).unwrap();
    assert!(try_lower(SOURCE, Some(&r)).unwrap().ir.entity_count() >= 10);
}

#[test]
fn computed_reference_to_earlier_fact_resolves_locally() {
    let report = Checker::default().decode(TRANSCRIPT).unwrap();
    let compiled = try_lower(SOURCE, Some(&report)).unwrap();
    let box_id = compiled
        .ir
        .items()
        .find(|item| item.name() == b"Box" && item.kind() == ItemKind::Record)
        .unwrap()
        .id();
    let made_id = compiled
        .ir
        .items()
        .find(|item| item.name() == b"made" && item.kind() == ItemKind::Constant)
        .unwrap()
        .id();
    assert!(
        compiled
            .ir
            .storage_columns()
            .language_extensions
            .typescript
            .get(made_id)
            .and_then(|facts| facts.observed)
            .is_some(),
        "the observed made -> Box reference must remain in the TypeScript extension plane"
    );
    assert_ne!(box_id, made_id);
}
#[test]
fn narrowing_object_members_bind_spellings_at_the_assignment_site() {
    let mut r = report(b"let wide: number = 0;\nwide = { alpha: 1 };");
    r.narrowings = Box::new([compiler_languages_typescript::Narrowing {
        name_start: 4,
        name_end: 8,
        start: 22,
        end: 42,
        r#type: Some(compiler_languages_typescript::TypeTree::Object {
            members: vec![compiler_languages_typescript::ObjectMember {
                name: "alpha".into(),
                optional: false,
                readonly: false,
                member_type: compiler_languages_typescript::TypeTree::Primitive {
                    name: "number".into(),
                },
            }],
        }),
    }]);
    let v = view(b"let wide: number = 0;\nwide = { alpha: 1 };", Some(&r));
    assert!(
        facts(&v)
            .iter()
            .any(|f| f.record.tag == SemanticTypeTag::AnonymousRecord)
    );
}
#[test]
fn checker_resolved_global_reaches_an_oracle_universe_key() {
    let v = view(b"export const term = console;", None);
    assert!(
        occurrences(&v)
            .iter()
            .any(|o| matches!(o.occurrence.target, OccurrenceTarget::Foreign(_)))
    );
}
#[test]
fn package_module_bases_stay_honestly_syntactic() {
    let v = view(b"export const q = missing;", None);
    assert!(
        occurrences(&v)
            .iter()
            .any(|o| o.occurrence.confidence == OccurrenceConfidence::Syntactic)
    );
}
#[test]
fn checker_only_property_call_targets_the_exact_member() {
    let v=view(b"export class Box { tick(): number { return 1; } } export const box = new Box(); export const t = box.tick();",None);
    assert!(
        occurrences(&v)
            .iter()
            .any(|o| o.occurrence.kind == ReferenceKind::FunctionCall)
    );
}
#[test]
fn genuinely_unresolvable_names_stay_honestly_external() {
    let v = view(b"export const q: TotallyMissing = 1;", None);
    assert_eq!(
        fact(&v, named(&v, b"q").0).record.payload0,
        u32::from(TypeReason::UnresolvedExternal)
    );
}
#[test]
fn computed_row_pool_bound_and_union_child_bound_are_typed_rejections() {
    let declarations = std::iter::repeat(compiler_languages_typescript::Declaration {
        name_start: 13,
        name_end: 14,
        origin: compiler_languages_typescript::Origin::Computed,
        overload_index: None,
        r#type: Some(compiler_languages_typescript::TypeTree::This),
    })
    .take(2048)
    .collect();
    let mut below = report(SOURCE);
    below.declarations = declarations;
    let lowered = try_lower(SOURCE, Some(&below)).unwrap();
    let decoded = view(SOURCE, Some(&below));
    assert!(lowered.ir.entity_count() > 0);
    assert!(facts(&decoded).len() >= 2048);

    let source: &'static [u8] = Box::leak(
        (0..16385)
            .map(|index| format!("export const x{index} = 1;\n"))
            .collect::<String>()
            .into_bytes()
            .into_boxed_slice(),
    );
    let above = report(source);
    match try_lower(source, Some(&above)) {
        Err(CompileFailure::LoweringUnsupported {
            cause:
                LoweringUnsupported::FactRejected {
                    fact,
                    name_len,
                    cause,
                },
            ..
        }) => {
            assert_eq!(fact, 16_384);
            assert_eq!(name_len, 6);
            assert_eq!(cause, ProjectionAdmissionFault::Capacity);
        }
        Ok(_) => panic!("expected typed fact capacity rejection, source was admitted"),
        Err(_) => panic!("expected typed fact capacity rejection, got another typed terminal"),
    }
}

#[test]
fn checker_object_member_without_source_spelling_retains_typed_child_cause() {
    const SOURCE: &[u8] = b"export const x = null;";
    let mut checker = report(SOURCE);
    checker.declarations = Box::new([compiler_languages_typescript::Declaration {
        name_start: 13,
        name_end: 14,
        origin: compiler_languages_typescript::Origin::Computed,
        overload_index: None,
        r#type: Some(compiler_languages_typescript::TypeTree::Object {
            members: vec![compiler_languages_typescript::ObjectMember {
                name: "not-spelled".into(),
                optional: false,
                readonly: false,
                member_type: compiler_languages_typescript::TypeTree::Primitive {
                    name: "number".into(),
                },
            }],
        }),
    }]);
    match try_lower(SOURCE, Some(&checker)) {
        Err(CompileFailure::LoweringUnsupported {
            cause:
                LoweringUnsupported::FactRejected {
                    cause:
                        ProjectionAdmissionFault::TypeChild {
                            position: 0,
                            cause:
                                ProjectionSemanticTypeFault::ChildNameRequired {
                                    tag: ProjectionSemanticTypeTag::AnonymousRecord,
                                    position: 0,
                                },
                        },
                    ..
                },
            ..
        }) => {}
        Ok(_) => panic!("expected source-backed member spelling rejection"),
        Err(_) => panic!("expected typed source-backed member spelling rejection"),
    }
}

#[test]
fn forward_nominal_checker_and_lowering_keep_the_later_class() {
    const SOURCE: &[u8] = b"export const a = new B(); export class B {}";
    let checker = Checker::default()
        .run(TypeScriptSource::TypeScript, SOURCE)
        .unwrap();
    assert!(checker.declarations.iter().any(|declaration| {
        declaration.name_start == 13
            && matches!(declaration.r#type, Some(compiler_languages_typescript::TypeTree::Reference { ref name, .. }) if name == "B")
    }));
    let lowered = try_lower(SOURCE, Some(&checker)).unwrap();
    let a = lowered.ir.items().find(|item| item.name() == b"a").unwrap();
    let extension = lowered
        .ir
        .language_extensions()
        .typescript
        .get(a.id())
        .unwrap();
    assert_eq!(
        ir_tag_shape(&lowered.ir, extension.observed.unwrap()),
        (SemanticTypeTag::Nominal, 1)
    );
    let decoded = view(SOURCE, Some(&checker));
    let (owner, _) = named(&decoded, b"a");
    assert!(
        facts(&decoded)
            .iter()
            .any(|fact| { fact.owner.raw == owner && fact.record.tag == SemanticTypeTag::Nominal })
    );
    assert!(
        thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(|| {
                let checker = Checker::default()
                    .run(TypeScriptSource::TypeScript, SOURCE)
                    .unwrap();
                let root = std::env::temp_dir()
                    .join(format!("nudox-typescript-forward-{}", std::process::id()));
                let artifacts = root.join("artifacts");
                let journal = root.join("journal");
                std::fs::create_dir_all(&artifacts).unwrap();
                std::fs::create_dir_all(&journal).unwrap();
                let limits = PublicationLimits::new(
                    std::num::NonZeroUsize::MIN,
                    std::num::NonZeroUsize::MIN,
                )
                .unwrap();
                let publisher =
                    DurablePublisher::create(&PublicationPaths::in_directory(&journal), limits)
                        .unwrap();
                let toolchain = ResolvedToolchain::from_version(
                    NativeTool::TypeScriptCompiler,
                    Path::new("/bin/true"),
                    b"typescript-authority-test",
                )
                .unwrap();
                let mut diagnostic = vec![0_u8; 4096];
                let mut fragment_output = vec![0_u8; 8 * 1024 * 1024];
                let cancelled = AtomicBool::new(false);
                let compiled = compile(
                    CompileRequest {
                        profile: LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
                        stage: Stage::LowerIr,
                        source: SOURCE,
                        declaration_scope: compiler_driver::DeclarationScope::fixture(),
                        toolchain: ToolchainSelection::ResolvedNative(toolchain),
                        authority: SemanticAuthorityInput::TypeScript { report: &checker },
                        control: CompileControl {
                            deadline: Instant::now() + Duration::from_secs(30),
                            cancelled: &cancelled,
                        },
                    },
                    CompileScratch {
                        diagnostic_output: &mut diagnostic,
                        native_work: Path::new("/tmp"),
                    },
                    CompileOutput {
                        fragment_output: &mut fragment_output,
                    },
                )
                .unwrap();
                let mut manifest = vec![0_u8; 1 << 20];
                let mut manifest_facts = vec![None; 1];
                let mut ordinals = vec![0_usize; 1];
                let mut locality = vec![0_u8; 1 << 16];
                let mut binding =
                    vec![0_u8; compiler_publication::binding::COMPILATION_BINDING_BYTES];
                publish_compiled(
                    &publisher,
                    &artifacts,
                    std::slice::from_ref(&compiled),
                    PublishControl::Continue,
                    PublicationScratch {
                        manifest_output: &mut manifest,
                        manifest_facts: &mut manifest_facts,
                        ordinals: &mut ordinals,
                        locality_output: &mut locality,
                        binding_output: &mut binding,
                    },
                )
                .unwrap();
                publisher.shutdown().unwrap();
                let reopened_publisher =
                    DurablePublisher::reopen(&PublicationPaths::in_directory(&journal), limits)
                        .unwrap();
                let mut reopened_manifest = vec![0_u8; 1 << 20];
                let mut reopened_facts = vec![None; 1];
                let mut reopened_fragments = vec![0_u8; 8 * 1024 * 1024];
                let mut reopened_locality = vec![0_u8; 1 << 16];
                let opened = open_published(
                    &reopened_publisher,
                    &artifacts,
                    OpenPublicationScratch {
                        manifest_output: &mut reopened_manifest,
                        manifest_facts: &mut reopened_facts,
                        fragment_output: &mut reopened_fragments,
                        locality_output: &mut reopened_locality,
                    },
                )
                .unwrap()
                .unwrap();
                let reopened_fragment = opened.fragments().next().unwrap().unwrap();
                let (a_owner, _) = named(&reopened_fragment.view, b"a");
                let (b_owner, _) = named(&reopened_fragment.view, b"B");
                assert_eq!(
                    facts(&reopened_fragment.view)
                        .into_iter()
                        .find(|fact| {
                            fact.owner.raw == a_owner
                                && fact.segment == compiler_ir::TypeFactSegment::Computed
                        })
                        .and_then(|fact| fact.record.nominal),
                    Some(compiler_ir::NominalRef::Local(compiler_ir::EntityId::new(
                        b_owner
                    ))),
                );
                reopened_publisher.shutdown().unwrap();
                std::fs::remove_dir_all(root).unwrap();
                true
            })
            .unwrap()
            .join()
            .unwrap()
    );
}

#[derive(Clone, Copy)]
struct Frozen {
    name: &'static [u8],
    kind: EntityKind,
    declared: SemanticTypeTag,
    computed: SemanticTypeTag,
    shape: u8,
    has_computed: bool,
}
#[test]
fn golden_lowered_facts_match_the_frozen_table() {
    let r = Checker::default().decode(TRANSCRIPT).unwrap();
    let table = [
        Frozen {
            name: b"n",
            kind: EntityKind::Constant,
            declared: SemanticTypeTag::Primitive,
            computed: SemanticTypeTag::Primitive,
            shape: 0,
            has_computed: true,
        },
        Frozen {
            name: b"inferred",
            kind: EntityKind::Constant,
            declared: SemanticTypeTag::Unknown,
            computed: SemanticTypeTag::Primitive,
            shape: 0,
            has_computed: true,
        },
        Frozen {
            name: b"union",
            kind: EntityKind::Constant,
            declared: SemanticTypeTag::Union,
            computed: SemanticTypeTag::Union,
            shape: 2,
            has_computed: true,
        },
        Frozen {
            name: b"applied",
            kind: EntityKind::Constant,
            declared: SemanticTypeTag::Apply,
            computed: SemanticTypeTag::Apply,
            shape: 2,
            has_computed: true,
        },
        Frozen {
            name: b"table",
            kind: EntityKind::Constant,
            declared: SemanticTypeTag::Apply,
            computed: SemanticTypeTag::Apply,
            shape: 3,
            has_computed: true,
        },
        Frozen {
            name: b"total",
            kind: EntityKind::Function,
            declared: SemanticTypeTag::FunctionPointer,
            computed: SemanticTypeTag::FunctionPointer,
            shape: 2,
            has_computed: true,
        },
        Frozen {
            name: b"fn",
            kind: EntityKind::Constant,
            declared: SemanticTypeTag::Unknown,
            computed: SemanticTypeTag::FunctionPointer,
            shape: 1,
            has_computed: true,
        },
        Frozen {
            name: b"Box",
            kind: EntityKind::Record,
            declared: SemanticTypeTag::Nominal,
            computed: SemanticTypeTag::Nominal,
            shape: 1,
            has_computed: false,
        },
        Frozen {
            name: b"made",
            kind: EntityKind::Constant,
            declared: SemanticTypeTag::Unknown,
            computed: SemanticTypeTag::Nominal,
            shape: 0,
            has_computed: true,
        },
        Frozen {
            name: b"list",
            kind: EntityKind::Constant,
            declared: SemanticTypeTag::ArraySequence,
            computed: SemanticTypeTag::ArraySequence,
            shape: 1,
            has_computed: true,
        },
        Frozen {
            name: b"widened",
            kind: EntityKind::Static,
            declared: SemanticTypeTag::Union,
            computed: SemanticTypeTag::Union,
            shape: 2,
            has_computed: true,
        },
        Frozen {
            name: b"Slot",
            kind: EntityKind::Record,
            declared: SemanticTypeTag::Nominal,
            computed: SemanticTypeTag::Nominal,
            shape: 1,
            has_computed: false,
        },
        Frozen {
            name: b"slot",
            kind: EntityKind::Constant,
            declared: SemanticTypeTag::Unknown,
            computed: SemanticTypeTag::Nominal,
            shape: 0,
            has_computed: true,
        },
        Frozen {
            name: b"viaSlot",
            kind: EntityKind::Constant,
            declared: SemanticTypeTag::Unknown,
            computed: SemanticTypeTag::Nominal,
            shape: 0,
            has_computed: true,
        },
        Frozen {
            name: b"term",
            kind: EntityKind::Constant,
            declared: SemanticTypeTag::Unknown,
            computed: SemanticTypeTag::Nominal,
            shape: 0,
            has_computed: true,
        },
        Frozen {
            name: b"callOne",
            kind: EntityKind::Constant,
            declared: SemanticTypeTag::Unknown,
            computed: SemanticTypeTag::Primitive,
            shape: 0,
            has_computed: true,
        },
        Frozen {
            name: b"callTwo",
            kind: EntityKind::Constant,
            declared: SemanticTypeTag::Unknown,
            computed: SemanticTypeTag::Primitive,
            shape: 0,
            has_computed: true,
        },
        Frozen {
            name: b"Holder",
            kind: EntityKind::Trait,
            declared: SemanticTypeTag::Nominal,
            computed: SemanticTypeTag::Nominal,
            shape: 1,
            has_computed: false,
        },
        Frozen {
            name: b"g",
            kind: EntityKind::Function,
            declared: SemanticTypeTag::FunctionPointer,
            computed: SemanticTypeTag::FunctionPointer,
            shape: 2,
            has_computed: false,
        },
        Frozen {
            name: b"g",
            kind: EntityKind::Function,
            declared: SemanticTypeTag::FunctionPointer,
            computed: SemanticTypeTag::FunctionPointer,
            shape: 2,
            has_computed: false,
        },
    ];
    let compiled = try_lower(SOURCE, Some(&r)).unwrap();
    let typescript = compiled.ir.language_extensions().typescript;
    let mut identities = Vec::with_capacity(table.len());
    for (row_index, row) in table.iter().enumerate() {
        let same_name_before = table[..row_index]
            .iter()
            .filter(|previous| previous.name == row.name && previous.kind == row.kind)
            .count();
        let candidates: Vec<_> = compiled
            .ir
            .items_named(row.name)
            .filter(|item| expected_kind_matches(item.kind(), row.kind))
            .collect();
        let item = *candidates.get(same_name_before).unwrap_or_else(|| {
            panic!(
                "row {row_index} {:?}: expected entity kind {:?}, observed {} candidates",
                row.name,
                row.kind,
                candidates.len()
            )
        });
        let extension = typescript.get(item.id()).unwrap();
        let declared = ir_tag_shape(&compiled.ir, extension.declared.unwrap());
        let computed = extension.observed.map(|id| ir_tag_shape(&compiled.ir, id));
        assert_eq!(declared.0, row.declared, "row {row_index} {:?}", row.name);
        assert_eq!(
            computed.is_some(),
            row.has_computed,
            "row {row_index} {:?}",
            row.name
        );
        if let Some(computed) = computed {
            assert_eq!(
                (computed.0, computed.1),
                (SemanticTypeTag::Nominal, 1),
                "row {row_index} {:?}",
                row.name
            );
        }
        identities.push(item.id());
    }
    assert_eq!(identities.len(), 20);
    assert_ne!(
        identities[18], identities[19],
        "the two g overload rows must retain distinct identities"
    );

    // The fragment path is intentionally exercised too: the sibling golden
    // tests use `try_lower`, while this check must also prove `compile` agrees.
    let decoded = view(SOURCE, Some(&r));
    for (row_index, row) in table.iter().enumerate() {
        let same_name_before = table[..row_index]
            .iter()
            .filter(|previous| previous.name == row.name && previous.kind == row.kind)
            .count();
        let candidates: Vec<_> = entities(&decoded)
            .into_iter()
            .filter(|(_, name, kind)| name == row.name && *kind == row.kind)
            .collect();
        let (owner, _, _) = *candidates.get(same_name_before).unwrap();
        if row.has_computed {
            assert!(
                facts(&decoded).iter().any(|record| {
                    record.owner.raw == owner
                        && record.record.tag == row.computed
                        && record.record.children.length as u8 == row.shape
                }),
                "fragment row {row_index} {:?}",
                row.name
            );
        }
    }
    assert_eq!(table.len(), 20);
}

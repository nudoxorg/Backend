//! Canonical seven-plane sparse language-extension preparation.
//!
//! Each plane stays named and typed. Fact keys are framed through the shared
//! typed/terminal remaps, while sparse bindings are the authority that keeps
//! equal interned facts on multiple declarations distinct without inventing a
//! raw fact-ordinal tie breaker.

use alloc::{vec, vec::Vec};

use crate::{
    ArenaRange, CSharpFacts, CSharpNullability, CSharpPartialRole, CSharpReferenceKind,
    ClangFacts, ClangStorageClass, FactAvailability, GoFacts, Ir, JavaFacts,
    Language, LanguageExtensionColumnView, LanguageExtensionsView, PythonFacts,
    PythonParameterKind, RustFacts, RustOwnership, SemanticImageAuthority, SourceSpan,
    TypeScriptFacts,
};

use super::super::typed::TypedDependencyPlan;
use super::{
    ExtensionBinding, ExtensionPlanePlan, ExtensionPlanFault, ExtensionPlans, FullPlanError,
    TerminalPools,
};

impl ExtensionPlans {
    pub(crate) fn build(
        ir: &Ir,
        typed: &TypedDependencyPlan<'_>,
        terminal: &TerminalPools,
    ) -> Result<Self, FullPlanError> {
        let columns = ir.language_extensions();
        let typescript = plan_plane(
            columns.typescript,
            Language::TypeScript,
            typed,
            terminal,
            append_typescript,
        )?;
        let csharp = plan_plane(columns.csharp, Language::CSharp, typed, terminal, append_csharp)?;
        let go = plan_plane(columns.go, Language::Go, typed, terminal, append_go)?;
        let rust = plan_plane(columns.rust, Language::Rust, typed, terminal, append_rust)?;
        let python = plan_plane(columns.python, Language::Python, typed, terminal, append_python)?;
        let java = plan_plane(columns.java, Language::Java, typed, terminal, append_java)?;
        let clang = plan_plane(columns.clang, Language::Clang, typed, terminal, append_clang)?;
        let plans = Self { typescript, csharp, go, rust, python, java, clang };
        validate_plane_authority(ir, columns, &plans, typed.canonical())?;
        Ok(plans)
    }
}

fn plan_plane<Facts: Copy, Space>(
    plane: LanguageExtensionColumnView<'_, Facts, Space>,
    language: Language,
    typed: &TypedDependencyPlan<'_>,
    terminal: &TerminalPools,
    append: impl Fn(&mut Vec<u8>, Facts, &TypedDependencyPlan<'_>, &TerminalPools) -> Result<(), FullPlanError>,
) -> Result<ExtensionPlanePlan, FullPlanError> {
    let fact_count = plane.facts.len();
    let fact_count_u32 = u32::try_from(fact_count)
        .map_err(|_| ExtensionPlanFault::GeometryOverflow { language, rows: fact_count })?;
    let entities = &typed.canonical().core.entities;
    let mut binding_counts = vec![0_u32; fact_count];
    for entity in entities.iter().copied() {
        if let Some(id) = plane.ids.get(entity) {
            let raw_fact = id.raw;
            let index = usize::try_from(raw_fact).map_err(|_| {
                ExtensionPlanFault::GeometryOverflow { language, rows: fact_count }
            })?;
            let count = binding_counts.get_mut(index).ok_or(ExtensionPlanFault::SparseBinding {
                language,
                entity,
                fact: raw_fact,
                count: fact_count_u32,
            })?;
            *count = count.checked_add(1).ok_or(ExtensionPlanFault::GeometryOverflow {
                language,
                rows: entities.len(),
            })?;
        }
    }
    let mut binding_offsets = Vec::with_capacity(fact_count.checked_add(1).ok_or(
        ExtensionPlanFault::GeometryOverflow { language, rows: fact_count },
    )?);
    binding_offsets.push(0_u32);
    for count in binding_counts.iter().copied() {
        let previous = *binding_offsets.last().ok_or(ExtensionPlanFault::GeometryOverflow {
            language,
            rows: fact_count,
        })?;
        binding_offsets.push(previous.checked_add(count).ok_or(ExtensionPlanFault::GeometryOverflow {
            language,
            rows: fact_count,
        })?);
    }
    let binding_len = usize::try_from(*binding_offsets.last().ok_or(
        ExtensionPlanFault::GeometryOverflow { language, rows: fact_count },
    )?).map_err(|_| ExtensionPlanFault::GeometryOverflow { language, rows: fact_count })?;
    let mut binding_entities = vec![0_u32; binding_len];
    let mut positions = binding_offsets[..fact_count].to_vec();
    for entity in entities.iter().copied() {
        let Some(id) = plane.ids.get(entity) else { continue };
        let raw_fact = id.raw;
        let index = usize::try_from(raw_fact).map_err(|_| {
            ExtensionPlanFault::GeometryOverflow { language, rows: fact_count }
        })?;
        let position = positions.get_mut(index).ok_or(ExtensionPlanFault::SparseBinding {
            language,
            entity,
            fact: raw_fact,
            count: fact_count_u32,
        })?;
        let end = *binding_offsets.get(index.checked_add(1).ok_or(
            ExtensionPlanFault::GeometryOverflow { language, rows: fact_count },
        )?).ok_or(ExtensionPlanFault::SparseBinding {
            language,
            entity,
            fact: raw_fact,
            count: fact_count_u32,
        })?;
        if *position >= end {
            return Err(ExtensionPlanFault::SparseBinding {
                language,
                entity,
                fact: raw_fact,
                count: fact_count_u32,
            }
            .into());
        }
        let canonical_entity = typed.canonical().entity(entity)?;
        let destination = binding_entities.get_mut(usize::try_from(*position).map_err(|_| {
            ExtensionPlanFault::GeometryOverflow { language, rows: fact_count }
        })?).ok_or(ExtensionPlanFault::SparseBinding {
            language,
            entity,
            fact: raw_fact,
            count: fact_count_u32,
        })?;
        *destination = canonical_entity;
        *position = position.checked_add(1).ok_or(ExtensionPlanFault::GeometryOverflow {
            language,
            rows: fact_count,
        })?;
    }
    let mut key_bytes = Vec::new();
    let mut key_ranges = Vec::with_capacity(fact_count);
    for (index, facts) in plane.facts.iter().copied().enumerate() {
        let fact = u32::try_from(index)
            .map_err(|_| ExtensionPlanFault::GeometryOverflow { language, rows: fact_count })?;
        let start = key_bytes.len();
        append(&mut key_bytes, facts, typed, terminal)?;
        let end = key_bytes.len();
        key_ranges.push(ArenaRange {
            start: u32::try_from(start)
                .map_err(|_| ExtensionPlanFault::KeyLengthOverflow { language, fact })?,
            len: u32::try_from(end.checked_sub(start).ok_or(
                ExtensionPlanFault::KeyLengthOverflow { language, fact },
            )?)
            .map_err(|_| ExtensionPlanFault::KeyLengthOverflow { language, fact })?,
        });
        let bindings = binding_range(&binding_offsets, &binding_entities, index, language, fact)?;
        if bindings.is_empty() {
            return Err(ExtensionPlanFault::UnboundFact { language, fact }.into());
        }
    }
    validate_key_ranges(&key_bytes, &key_ranges, language)?;
    let mut order = (0..fact_count)
        .map(|raw| u32::try_from(raw).map_err(|_| ExtensionPlanFault::GeometryOverflow { language, rows: fact_count }))
        .collect::<Result<Vec<_>, _>>()?;
    order.sort_unstable_by(|left, right| {
        fact_key_after_validation(&key_bytes, &key_ranges, *left)
            .cmp(fact_key_after_validation(&key_bytes, &key_ranges, *right))
            .then_with(|| binding_slice_after_validation(&binding_offsets, &binding_entities, *left)
                .cmp(binding_slice_after_validation(&binding_offsets, &binding_entities, *right)))
    });
    for pair in order.windows(2) {
        let [left, right] = pair else { continue };
        if fact_key_after_validation(&key_bytes, &key_ranges, *left)
            == fact_key_after_validation(&key_bytes, &key_ranges, *right)
            && binding_slice_after_validation(&binding_offsets, &binding_entities, *left)
                == binding_slice_after_validation(&binding_offsets, &binding_entities, *right)
        {
            return Err(ExtensionPlanFault::DuplicateCanonicalKey {
                language,
                first: *left,
                second: *right,
            }
            .into());
        }
    }
    let mut remap = vec![0_u32; fact_count];
    for (canonical_fact, raw) in order.iter().copied().enumerate() {
        remap[usize::try_from(raw).map_err(|_| ExtensionPlanFault::GeometryOverflow {
            language,
            rows: fact_count,
        })?] = u32::try_from(canonical_fact).map_err(|_| ExtensionPlanFault::GeometryOverflow {
            language,
            rows: fact_count,
        })?;
    }
    let mut bindings = Vec::with_capacity(binding_len);
    for raw_fact in 0..fact_count {
        let raw = u32::try_from(raw_fact)
            .map_err(|_| ExtensionPlanFault::GeometryOverflow { language, rows: fact_count })?;
        let canonical_fact = *remap.get(raw_fact).ok_or(ExtensionPlanFault::MissingFact {
            language,
            fact: raw,
            count: fact_count_u32,
        })?;
        for entity in binding_range(&binding_offsets, &binding_entities, raw_fact, language, raw)? {
            bindings.push(ExtensionBinding { entity: *entity, fact: canonical_fact });
        }
    }
    bindings.sort_unstable_by_key(|binding| (binding.entity, binding.fact));
    Ok(ExtensionPlanePlan { order, bindings, key_bytes, key_ranges })
}

fn validate_plane_authority(
    ir: &Ir,
    columns: LanguageExtensionsView<'_>,
    plans: &ExtensionPlans,
    canonical: &super::super::canonical::CanonicalFullPlan<'_>,
) -> Result<(), FullPlanError> {
    for entity in canonical.core.entities.iter().copied() {
        let present = [
            (Language::TypeScript, columns.typescript.get(entity).is_some()),
            (Language::CSharp, columns.csharp.get(entity).is_some()),
            (Language::Go, columns.go.get(entity).is_some()),
            (Language::Rust, columns.rust.get(entity).is_some()),
            (Language::Python, columns.python.get(entity).is_some()),
            (Language::Java, columns.java.get(entity).is_some()),
            (Language::Clang, columns.clang.get(entity).is_some()),
        ];
        let mut selected = None;
        for (language, has_facts) in present {
            if !has_facts { continue; }
            if let Some(first) = selected {
                return Err(ExtensionPlanFault::MultiplePlanes { entity, first, second: language }.into());
            }
            selected = Some(language);
        }
        let facts = ir.semantic_entity(entity).ok_or(ExtensionPlanFault::GeometryOverflow {
            language: Language::Rust,
            rows: ir.entity_count(),
        })?;
        let present = selected.is_some();
        if matches!(facts.authority.language_extension, FactAvailability::Captured) != present {
            return Err(ExtensionPlanFault::Authority {
                entity,
                claimed: facts.authority.language_extension,
                present,
            }
            .into());
        }
    }
    for (language, has_facts) in [
        (Language::TypeScript, !plans.typescript.bindings.is_empty()),
        (Language::CSharp, !plans.csharp.bindings.is_empty()),
        (Language::Go, !plans.go.bindings.is_empty()),
        (Language::Rust, !plans.rust.bindings.is_empty()),
        (Language::Python, !plans.python.bindings.is_empty()),
        (Language::Java, !plans.java.bindings.is_empty()),
        (Language::Clang, !plans.clang.bindings.is_empty()),
    ] {
        if !has_facts { continue; }
        let authority = columns.authority;
        if !matches!(authority, SemanticImageAuthority::Language(profile) if profile.language() == language) {
            return Err(ExtensionPlanFault::Profile { language, authority }.into());
        }
    }
    Ok(())
}

fn binding_range<'a>(
    offsets: &[u32],
    entities: &'a [u32],
    raw_fact: usize,
    language: Language,
    fact: u32,
) -> Result<&'a [u32], ExtensionPlanFault> {
    let count = offsets
        .len()
        .checked_sub(1)
        .ok_or(ExtensionPlanFault::GeometryOverflow { language, rows: 0 })?;
    let count = u32::try_from(count)
        .map_err(|_| ExtensionPlanFault::GeometryOverflow { language, rows: offsets.len() })?;
    let start = *offsets.get(raw_fact).ok_or(ExtensionPlanFault::MissingFact {
        language, fact, count,
    })?;
    let end = *offsets.get(raw_fact.checked_add(1).ok_or(ExtensionPlanFault::MissingFact {
        language, fact, count,
    })?).ok_or(ExtensionPlanFault::MissingFact {
        language, fact, count,
    })?;
    entities.get(usize::try_from(start).map_err(|_| ExtensionPlanFault::MissingFact { language, fact, count })?
        ..usize::try_from(end).map_err(|_| ExtensionPlanFault::MissingFact { language, fact, count })?)
        .ok_or(ExtensionPlanFault::MissingFact { language, fact, count })
}

fn validate_key_ranges(
    bytes: &[u8], ranges: &[ArenaRange], language: Language,
) -> Result<(), ExtensionPlanFault> {
    for (index, range) in ranges.iter().copied().enumerate() {
        let fact = u32::try_from(index).map_err(|_| ExtensionPlanFault::GeometryOverflow {
            language, rows: ranges.len(),
        })?;
        let start = usize::try_from(range.start)
            .map_err(|_| ExtensionPlanFault::KeyLengthOverflow { language, fact })?;
        let length = usize::try_from(range.len)
            .map_err(|_| ExtensionPlanFault::KeyLengthOverflow { language, fact })?;
        if start.checked_add(length).filter(|end| *end <= bytes.len()).is_none() {
            return Err(ExtensionPlanFault::KeyLengthOverflow { language, fact });
        }
    }
    Ok(())
}

#[allow(clippy::as_conversions, reason = "validated u32 ranges fit the supported address space")]
fn fact_key_after_validation<'a>(bytes: &'a [u8], ranges: &[ArenaRange], raw: u32) -> &'a [u8] {
    let range = ranges[raw as usize];
    let start = range.start as usize;
    &bytes[start..start + range.len as usize]
}

#[allow(clippy::as_conversions, reason = "validated u32 sparse offsets fit the supported address space")]
fn binding_slice_after_validation<'a>(offsets: &[u32], entities: &'a [u32], raw: u32) -> &'a [u32] {
    &entities[offsets[raw as usize] as usize..offsets[raw as usize + 1] as usize]
}

fn append_typescript(
    out: &mut Vec<u8>, facts: TypeScriptFacts, typed: &TypedDependencyPlan<'_>, _: &TerminalPools,
) -> Result<(), FullPlanError> {
    out.push(extension_tag_typescript());
    out.extend_from_slice(&typed.canonical_type_parameters(facts.type_parameters)?.to_le_bytes());
    append_optional_type(out, facts.declared, typed)?;
    append_optional_type(out, facts.observed, typed)
}

fn append_csharp(
    out: &mut Vec<u8>, facts: CSharpFacts, typed: &TypedDependencyPlan<'_>, _: &TerminalPools,
) -> Result<(), FullPlanError> {
    out.push(extension_tag_csharp());
    out.push(csharp_nullability_code(facts.nullability));
    out.push(csharp_reference_code(facts.reference_kind));
    out.extend_from_slice(&typed.canonical_type_parameters(facts.constraints)?.to_le_bytes());
    out.push(bool_code(facts.effects.is_async));
    out.push(bool_code(facts.effects.is_iterator));
    out.push(bool_code(facts.effects.is_extension));
    out.extend_from_slice(&typed.canonical_atom_list(facts.attributes)?.to_le_bytes());
    out.push(csharp_partial_code(facts.partial));
    append_optional_source(out, facts.xml_provenance, typed)
}

fn append_go(
    out: &mut Vec<u8>, facts: GoFacts, typed: &TypedDependencyPlan<'_>, terminal: &TerminalPools,
) -> Result<(), FullPlanError> {
    out.push(extension_tag_go());
    out.extend_from_slice(&typed.canonical_type_list(facts.signature.parameters)?.to_le_bytes());
    out.extend_from_slice(&typed.canonical_type_list(facts.signature.results)?.to_le_bytes());
    out.push(bool_code(facts.signature.variadic));
    out.extend_from_slice(&typed.canonical_type_parameters(facts.type_parameters)?.to_le_bytes());
    out.extend_from_slice(&terminal.member(facts.fields)?.to_le_bytes());
    out.extend_from_slice(&terminal.member(facts.method_set)?.to_le_bytes());
    out.extend_from_slice(&typed.canonical_atom_list(facts.build_constraints)?.to_le_bytes());
    out.extend_from_slice(&typed.canonical_atom_list(facts.constant_value)?.to_le_bytes());
    out.extend_from_slice(&facts.constant_group.to_le_bytes());
    out.extend_from_slice(&facts.constant_flags.to_le_bytes());
    Ok(())
}

fn append_rust(
    out: &mut Vec<u8>, facts: RustFacts, typed: &TypedDependencyPlan<'_>, _: &TerminalPools,
) -> Result<(), FullPlanError> {
    out.push(extension_tag_rust());
    out.push(rust_ownership_code(facts.ownership));
    out.extend_from_slice(&typed.canonical_atom_list(facts.lifetimes)?.to_le_bytes());
    out.extend_from_slice(&typed.canonical_type_parameters(facts.where_clauses)?.to_le_bytes());
    out.extend_from_slice(&typed.canonical_atom_list(facts.macros)?.to_le_bytes());
    Ok(())
}

fn append_python(
    out: &mut Vec<u8>, facts: PythonFacts, typed: &TypedDependencyPlan<'_>, _: &TerminalPools,
) -> Result<(), FullPlanError> {
    out.push(extension_tag_python());
    out.extend_from_slice(&typed.canonical_atom_list(facts.decorators)?.to_le_bytes());
    out.push(python_parameter_code(facts.parameter_kind));
    out.push(confidence_code(facts.dynamic_confidence));
    Ok(())
}

fn append_java(
    out: &mut Vec<u8>, facts: JavaFacts, typed: &TypedDependencyPlan<'_>, terminal: &TerminalPools,
) -> Result<(), FullPlanError> {
    out.push(extension_tag_java());
    out.extend_from_slice(&typed.canonical_type_list(facts.throws)?.to_le_bytes());
    out.extend_from_slice(&typed.canonical_atom_list(facts.annotations)?.to_le_bytes());
    out.extend_from_slice(&terminal.member(facts.overloads)?.to_le_bytes());
    out.extend_from_slice(&terminal.member(facts.record_components)?.to_le_bytes());
    Ok(())
}

fn append_clang(
    out: &mut Vec<u8>, facts: ClangFacts, typed: &TypedDependencyPlan<'_>, _: &TerminalPools,
) -> Result<(), FullPlanError> {
    out.push(extension_tag_clang());
    out.push(bool_code(facts.qualifiers.is_const));
    out.push(bool_code(facts.qualifiers.is_volatile));
    out.push(bool_code(facts.qualifiers.is_restrict));
    out.push(clang_storage_code(facts.storage));
    append_optional_u32(out, facts.layout.size_bits);
    append_optional_u32(out, facts.layout.align_bits);
    out.extend_from_slice(&typed.canonical_type_parameters(facts.templates)?.to_le_bytes());
    out.extend_from_slice(&typed.canonical_atom_list(facts.includes)?.to_le_bytes());
    Ok(())
}

fn append_optional_type(out: &mut Vec<u8>, value: Option<crate::TypeId>, typed: &TypedDependencyPlan<'_>) -> Result<(), FullPlanError> {
    match value {
        Some(value) => { out.push(present_tag()); out.extend_from_slice(&typed.canonical_type(value)?.to_le_bytes()); }
        None => { out.push(absent_tag()); out.extend_from_slice(&0_u32.to_le_bytes()); }
    }
    Ok(())
}

fn append_optional_source(out: &mut Vec<u8>, value: Option<SourceSpan>, typed: &TypedDependencyPlan<'_>) -> Result<(), FullPlanError> {
    match value {
        Some(value) => {
            out.push(present_tag());
            out.extend_from_slice(&typed.canonical().atom(value.file())?.to_le_bytes());
            out.extend_from_slice(&value.start().to_le_bytes());
            out.extend_from_slice(&value.end().to_le_bytes());
        }
        None => { out.push(absent_tag()); out.extend_from_slice(&[0; 12]); }
    }
    Ok(())
}

fn append_optional_u32(out: &mut Vec<u8>, value: Option<u32>) {
    match value {
        Some(value) => { out.push(present_tag()); out.extend_from_slice(&value.to_le_bytes()); }
        None => { out.push(absent_tag()); out.extend_from_slice(&0_u32.to_le_bytes()); }
    }
}

const fn extension_tag_typescript() -> u8 { 0 }
const fn extension_tag_csharp() -> u8 { 1 }
const fn extension_tag_go() -> u8 { 2 }
const fn extension_tag_rust() -> u8 { 3 }
const fn extension_tag_python() -> u8 { 4 }
const fn extension_tag_java() -> u8 { 5 }
const fn extension_tag_clang() -> u8 { 6 }
const fn absent_tag() -> u8 { 0 }
const fn present_tag() -> u8 { 1 }
const fn bool_code(value: bool) -> u8 { if value { 1 } else { 0 } }
const fn csharp_nullability_code(value: CSharpNullability) -> u8 { match value { CSharpNullability::Oblivious => 0, CSharpNullability::NonNullable => 1, CSharpNullability::Nullable => 2 } }
const fn csharp_reference_code(value: CSharpReferenceKind) -> u8 { match value { CSharpReferenceKind::Value => 0, CSharpReferenceKind::In => 1, CSharpReferenceKind::Ref => 2, CSharpReferenceKind::Out => 3 } }
const fn csharp_partial_code(value: CSharpPartialRole) -> u8 { match value { CSharpPartialRole::None => 0, CSharpPartialRole::Definition => 1, CSharpPartialRole::Implementation => 2 } }
const fn rust_ownership_code(value: RustOwnership) -> u8 { match value { RustOwnership::Value => 0, RustOwnership::SharedBorrow => 1, RustOwnership::MutableBorrow => 2, RustOwnership::Moved => 3 } }
const fn python_parameter_code(value: PythonParameterKind) -> u8 { match value { PythonParameterKind::PositionalOnly => 0, PythonParameterKind::PositionalOrKeyword => 1, PythonParameterKind::VariadicPositional => 2, PythonParameterKind::KeywordOnly => 3, PythonParameterKind::VariadicKeyword => 4 } }
const fn confidence_code(value: crate::Confidence) -> u8 { match value { crate::Confidence::Syntactic => 0, crate::Confidence::Heuristic => 1, crate::Confidence::Indexed => 2, crate::Confidence::Imported => 3, crate::Confidence::Compiler => 4 } }
const fn clang_storage_code(value: ClangStorageClass) -> u8 { match value { ClangStorageClass::None => 0, ClangStorageClass::Auto => 1, ClangStorageClass::Static => 2, ClangStorageClass::Extern => 3, ClangStorageClass::Register => 4, ClangStorageClass::ThreadLocal => 5 } }

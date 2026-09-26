//! Traverses one direct libclang translation unit into caller-bounded semantic fact slots.
//! The visitor retains declarations, recursive types, references, documentation, includes, and diagnostics.
//! Every capacity boundary is explicit and the traversal has no scanner or process fallback.

use core::{
    ffi::c_void,
    ptr,
    sync::atomic::{AtomicBool, Ordering},
};

use std::collections::HashMap;

use clang_sys::{
    CXChildVisitResult, CXCursor, CXCursorKind as NativeCursorKind, CXDiagnostic,
    CXDiagnosticSeverity as NativeDiagnosticSeverity, CXType, CXTypeKind,
};

use crate::legacy::{
    ClangInput, ClangScratch, CollectError, ScratchLane,
    facts::{
        BuiltinClass, ClangFacts, DeclarationFact, DeclarationId, DeclarationKind, DefinitionState,
        DiagnosticFact, DiagnosticSeverity, IncludeFact, IntegerRank, MethodVirtuality,
        OverrideFact, ReferenceFact, ReferenceKind, ReferenceTarget, SourceDependencyKind,
        SourceSpan, StorageClass, SymbolIdentity, TypeEdge, TypeFact, TypeId, TypeKind,
        TypeQualifiers, TypeRelation,
    },
    ffi::{self, TranslationUnit},
};

mod collector;

/// Maximum number of parameter cursors deferred until the owner walk completes.
///
/// The stash receives one entry per visited parameter cursor — prototypes and
/// definitions both defer, and a macro-generated header can declare more
/// parameter cursors than declaration slots (measured: `eigen`'s
/// `Eigen/src/misc/lapacke.h` alone visits 32,769+ parameter cursors). The
/// bound therefore uses the same historical four-times ratio as the recursive
/// type, type-edge, and reference lanes, so the transient deferral never
/// fires before the committed declaration lane it feeds.
const PARAMETER_STASH_CAPACITY: usize = 4 * crate::legacy::facts::MAX_CLANG_FACTS;

/// One parameter cursor deferred until its owner's committed definition state is final.
#[derive(Clone, Copy)]
struct StashedParameter {
    /// The semantic-parent identity shared by a prototype and its definition.
    owner: Option<SymbolIdentity>,
    /// Whether the visiting declaration is a definition. A prototype's
    /// parameters stash `false` even when a definition committed earlier.
    owner_is_definition: bool,
    /// The visiting declaration's extent. Distinct declarations never share
    /// one, so this separates duplicate-prototype sets that share an owner
    /// identity and flag.
    parent_span: Option<SourceSpan>,
    /// The native cursor replayed after the walk to emit the selected run only.
    cursor: CXCursor,
}

/// One maximal, owner-homogeneous run of stashed parameter cursors in visit order.
struct ParameterRun {
    /// The semantic-parent identity shared by every parameter in the run.
    owner: Option<SymbolIdentity>,
    /// The visiting declaration's extent shared by every parameter in the run.
    parent_span: Option<SourceSpan>,
    /// Inclusive first stashed index.
    start: usize,
    /// Exclusive last stashed index.
    end: usize,
    /// Whether the visiting declaration is a definition.
    is_definition: bool,
}

/// Collects complete direct libclang facts into the caller-provided typed scratch arrays.
///
/// # Errors
///
/// Returns an exact native, source-coordinate, or caller-capacity failure. Missing libclang is
/// never replaced with an alternative parser or a partial scanner result.
pub fn collect<'scratch>(
    input: ClangInput<'_>,
    scratch: ClangScratch<'scratch>,
) -> Result<ClangFacts<'scratch>, CollectError> {
    collect_inner(input, scratch, None)
}

/// Collects direct libclang facts while observing an exact caller-owned cancellation flag.
///
/// # Errors
///
/// Returns [`CollectError::Cancelled`] before native loading or at the next cursor boundary when
/// the caller has set `cancellation`; every other failure has the same meaning as [`collect`].
pub fn collect_cancellable<'scratch>(
    input: ClangInput<'_>,
    scratch: ClangScratch<'scratch>,
    cancellation: &'_ AtomicBool,
) -> Result<ClangFacts<'scratch>, CollectError> {
    collect_inner(input, scratch, Some(cancellation))
}

/// Runs one collector transaction with an optional caller-controlled cancellation authority.
fn collect_inner<'scratch>(
    input: ClangInput<'_>,
    scratch: ClangScratch<'scratch>,
    cancellation: Option<&AtomicBool>,
) -> Result<ClangFacts<'scratch>, CollectError> {
    preflight(input)?;
    if cancelled(cancellation) {
        return Err(CollectError::Cancelled);
    }
    let unit = ffi::parse(input)?;
    let mut collector = Collector::new(&unit, scratch, cancellation);
    collector.collect_diagnostics()?;
    let state = ptr::from_mut(&mut collector).cast::<c_void>();
    unit.visit(visit, state);
    collector.finish()
}

/// Rejects source facts that cannot safely reach the native authority before library discovery.
fn preflight(input: ClangInput<'_>) -> Result<(), CollectError> {
    if input.source().contains(&0) {
        return Err(CollectError::SourceContainsNul);
    }
    u32::try_from(input.source().len()).map_err(|_| CollectError::SourceLengthTooLarge {
        observed: input.source().len(),
    })?;
    Ok(())
}

/// Reads one optional cancellation flag with acquire ordering at a native work boundary.
fn cancelled(cancellation: Option<&AtomicBool>) -> bool {
    cancellation.is_some_and(|flag| flag.load(Ordering::Acquire))
}

/// Mutable fact writers and their exact typed counts for one synchronous native traversal.
struct Collector<'unit, 'scratch> {
    unit: &'unit TranslationUnit,
    scratch: ClangScratch<'scratch>,
    declarations: usize,
    types: usize,
    type_edges: usize,
    references: usize,
    diagnostics: usize,
    includes: usize,
    overrides: usize,
    parameters: Vec<StashedParameter>,
    cancellation: Option<&'unit AtomicBool>,
    failure: Option<CollectError>,
}

/// C callback that never permits a Rust failure to cross the native ABI boundary.
extern "C" fn visit(cursor: CXCursor, _parent: CXCursor, data: *mut c_void) -> CXChildVisitResult {
    let stop = ffi::callback_state(data, |collector: &mut Collector<'_, '_>| {
        collector.observe(cursor)
    })
    .unwrap_or(true);
    if stop {
        clang_sys::CXChildVisit_Break
    } else {
        clang_sys::CXChildVisit_Recurse
    }
}

/// Appends one fact to a caller-provided typed region while preserving exact capacity facts.
fn push<Fact>(
    slots: &mut [Fact],
    used: &mut usize,
    lane: ScratchLane,
    fact: Fact,
) -> Result<(), CollectError> {
    let Some(slot) = slots.get_mut(*used) else {
        let required = used.checked_add(1).ok_or(CollectError::SlotCountOverflow {
            lane,
            capacity: slots.len(),
        })?;
        return Err(CollectError::ScratchCapacity {
            lane,
            capacity: slots.len(),
            required,
        });
    };
    *slot = fact;
    *used = used.checked_add(1).ok_or(CollectError::SlotCountOverflow {
        lane,
        capacity: slots.len(),
    })?;
    Ok(())
}

/// Borrows an initialized prefix while retaining an impossible internal discrepancy as typed data.
fn prefix<Fact>(slots: &[Fact], used: usize, lane: ScratchLane) -> Result<&[Fact], CollectError> {
    slots.get(..used).ok_or(CollectError::ScratchCapacity {
        lane,
        capacity: slots.len(),
        required: used,
    })
}

/// Classifies libclang's direct declaration cursor kinds without name or token heuristics.
const fn declaration_kind(kind: NativeCursorKind) -> DeclarationKind {
    match kind {
        clang_sys::CXCursor_Namespace => DeclarationKind::Namespace,
        clang_sys::CXCursor_MacroDefinition => DeclarationKind::Macro,
        clang_sys::CXCursor_StructDecl
        | clang_sys::CXCursor_UnionDecl
        | clang_sys::CXCursor_ClassDecl => DeclarationKind::Record,
        clang_sys::CXCursor_EnumDecl => DeclarationKind::Enumeration,
        clang_sys::CXCursor_EnumConstantDecl => DeclarationKind::Enumerator,
        clang_sys::CXCursor_FunctionDecl => DeclarationKind::Function,
        clang_sys::CXCursor_CXXMethod | clang_sys::CXCursor_ConversionFunction => {
            DeclarationKind::Method
        }
        clang_sys::CXCursor_Constructor => DeclarationKind::Constructor,
        clang_sys::CXCursor_Destructor => DeclarationKind::Destructor,
        clang_sys::CXCursor_FieldDecl => DeclarationKind::Field,
        clang_sys::CXCursor_VarDecl => DeclarationKind::Variable,
        clang_sys::CXCursor_ParmDecl => DeclarationKind::Parameter,
        clang_sys::CXCursor_TemplateTypeParameter
        | clang_sys::CXCursor_NonTypeTemplateParameter
        | clang_sys::CXCursor_TemplateTemplateParameter => DeclarationKind::TemplateParameter,
        clang_sys::CXCursor_TypedefDecl | clang_sys::CXCursor_TypeAliasDecl => {
            DeclarationKind::TypeAlias
        }
        clang_sys::CXCursor_FunctionTemplate
        | clang_sys::CXCursor_ClassTemplate
        | clang_sys::CXCursor_ClassTemplatePartialSpecialization => DeclarationKind::Template,
        _ => DeclarationKind::Unknown,
    }
}

/// Classifies native reference cursors without token spelling or source-text inference.
const fn reference_kind(kind: NativeCursorKind) -> Option<ReferenceKind> {
    match kind {
        clang_sys::CXCursor_DeclRefExpr | clang_sys::CXCursor_VariableRef => {
            Some(ReferenceKind::Value)
        }
        clang_sys::CXCursor_TypeRef => Some(ReferenceKind::Type),
        clang_sys::CXCursor_TemplateRef => Some(ReferenceKind::Template),
        clang_sys::CXCursor_MemberRef | clang_sys::CXCursor_MemberRefExpr => {
            Some(ReferenceKind::Member)
        }
        clang_sys::CXCursor_CallExpr => Some(ReferenceKind::Call),
        clang_sys::CXCursor_MacroExpansion => Some(ReferenceKind::MacroExpansion),
        _ => None,
    }
}

/// Classifies direct include and module-import cursor authority without source-text inspection.
const fn source_dependency_kind(kind: NativeCursorKind) -> Option<SourceDependencyKind> {
    match kind {
        clang_sys::CXCursor_InclusionDirective => Some(SourceDependencyKind::Include),
        clang_sys::CXCursor_ModuleImportDecl => Some(SourceDependencyKind::ModuleImport),
        _ => None,
    }
}

/// Classifies direct native type kinds while retaining unsupported kinds as `Unknown` facts.
const fn type_kind(kind: CXTypeKind, declaration: Option<SymbolIdentity>) -> TypeKind {
    match kind {
        clang_sys::CXType_Pointer => TypeKind::Pointer,
        clang_sys::CXType_BlockPointer => TypeKind::BlockPointer,
        clang_sys::CXType_MemberPointer => TypeKind::MemberPointer,
        clang_sys::CXType_LValueReference => TypeKind::LvalueReference,
        clang_sys::CXType_RValueReference => TypeKind::RvalueReference,
        clang_sys::CXType_ConstantArray
        | clang_sys::CXType_IncompleteArray
        | clang_sys::CXType_VariableArray
        | clang_sys::CXType_DependentSizedArray => TypeKind::Array,
        clang_sys::CXType_FunctionNoProto | clang_sys::CXType_FunctionProto => TypeKind::Function,
        clang_sys::CXType_Record
        | clang_sys::CXType_Enum
        | clang_sys::CXType_Typedef
        | clang_sys::CXType_Elaborated => TypeKind::Named,
        clang_sys::CXType_Unexposed if declaration.is_some() => TypeKind::Named,
        clang_sys::CXType_Void
        | clang_sys::CXType_Bool
        | clang_sys::CXType_Char_U
        | clang_sys::CXType_UChar
        | clang_sys::CXType_Char16
        | clang_sys::CXType_Char32
        | clang_sys::CXType_UShort
        | clang_sys::CXType_UInt
        | clang_sys::CXType_ULong
        | clang_sys::CXType_ULongLong
        | clang_sys::CXType_UInt128
        | clang_sys::CXType_Char_S
        | clang_sys::CXType_SChar
        | clang_sys::CXType_WChar
        | clang_sys::CXType_Short
        | clang_sys::CXType_Int
        | clang_sys::CXType_Long
        | clang_sys::CXType_LongLong
        | clang_sys::CXType_Int128
        | clang_sys::CXType_Float
        | clang_sys::CXType_Double
        | clang_sys::CXType_LongDouble
        | clang_sys::CXType_NullPtr
        | clang_sys::CXType_Complex
        | clang_sys::CXType_Vector => TypeKind::Builtin,
        _ => TypeKind::Unknown,
    }
}

/// Projects libclang's declaration-definition predicate into its closed fact type.
const fn definition_state(is_definition: bool) -> DefinitionState {
    if is_definition {
        DefinitionState::Definition
    } else {
        DefinitionState::Declaration
    }
}

/// Projects libclang's closed storage class into the canonical lattice.
/// Private-extern and OpenCL work-group-local declarations keep static
/// storage because both bind internal-linkage storage; `ThreadLocal` has no
/// native query on the enabled symbol surface and is therefore unreachable.
const fn storage_class(class: clang_sys::CX_StorageClass) -> StorageClass {
    match class {
        clang_sys::CX_SC_Auto => StorageClass::Auto,
        clang_sys::CX_SC_Static
        | clang_sys::CX_SC_PrivateExtern
        | clang_sys::CX_SC_OpenCLWorkGroupLocal => StorageClass::Static,
        clang_sys::CX_SC_Extern => StorageClass::Extern,
        clang_sys::CX_SC_Register => StorageClass::Register,
        // `Invalid` and every newer native class keep no storage fact.
        _ => StorageClass::None,
    }
}

/// Wraps one native integer scalar with its exact signedness and C rank.
const fn integer_class(signed: bool, rank: IntegerRank) -> Option<BuiltinClass> {
    Some(BuiltinClass::Integer { signed, rank })
}

/// Classifies libclang's closed builtin type kinds without name or spelling
/// inspection, retaining exotic builtins as `Other`.
const fn builtin_class(kind: CXTypeKind, canonical_kind: CXTypeKind) -> Option<BuiltinClass> {
    match kind {
        clang_sys::CXType_Void => Some(BuiltinClass::Void),
        clang_sys::CXType_Bool => Some(BuiltinClass::Bool),
        clang_sys::CXType_Char_U => Some(BuiltinClass::PlainCharUnsigned),
        clang_sys::CXType_Char_S => Some(BuiltinClass::PlainCharSigned),
        clang_sys::CXType_SChar => Some(BuiltinClass::SignedChar),
        clang_sys::CXType_UChar => Some(BuiltinClass::UnsignedChar),
        clang_sys::CXType_Char16 => Some(BuiltinClass::Utf16CodeUnit),
        clang_sys::CXType_Char32 => Some(BuiltinClass::Utf32CodeUnit),
        clang_sys::CXType_WChar => match canonical_kind {
            clang_sys::CXType_SChar
            | clang_sys::CXType_Short
            | clang_sys::CXType_Int
            | clang_sys::CXType_Long
            | clang_sys::CXType_LongLong
            | clang_sys::CXType_Int128 => Some(BuiltinClass::WideCharSigned),
            clang_sys::CXType_UChar
            | clang_sys::CXType_UShort
            | clang_sys::CXType_UInt
            | clang_sys::CXType_ULong
            | clang_sys::CXType_ULongLong
            | clang_sys::CXType_UInt128 => Some(BuiltinClass::WideCharUnsigned),
            _ => Some(BuiltinClass::WideCharSignednessUnavailable),
        },
        clang_sys::CXType_Short => integer_class(true, IntegerRank::Short),
        clang_sys::CXType_UShort => integer_class(false, IntegerRank::Short),
        clang_sys::CXType_Int => integer_class(true, IntegerRank::Int),
        clang_sys::CXType_UInt => integer_class(false, IntegerRank::Int),
        clang_sys::CXType_Long => integer_class(true, IntegerRank::Long),
        clang_sys::CXType_ULong => integer_class(false, IntegerRank::Long),
        clang_sys::CXType_LongLong => integer_class(true, IntegerRank::LongLong),
        clang_sys::CXType_ULongLong => integer_class(false, IntegerRank::LongLong),
        clang_sys::CXType_Int128 => integer_class(true, IntegerRank::Int128),
        clang_sys::CXType_UInt128 => integer_class(false, IntegerRank::Int128),
        clang_sys::CXType_Float | clang_sys::CXType_Double | clang_sys::CXType_LongDouble => {
            Some(BuiltinClass::Float)
        }
        clang_sys::CXType_NullPtr | clang_sys::CXType_Complex | clang_sys::CXType_Vector => {
            Some(BuiltinClass::Other)
        }
        _ => None,
    }
}

/// Narrows one native byte measurement into exact bit facts. Negative
/// results are libclang's incomplete, dependent, and invalid layout errors,
/// which keep the cell honestly empty.
fn measured_bits(bytes: i64) -> Option<u32> {
    if bytes <= 0 {
        return None;
    }
    let bits = bytes.checked_mul(8)?;
    u32::try_from(bits).ok()
}

/// Projects libclang's closed diagnostic severity without converting it through strings.
const fn diagnostic_severity(severity: NativeDiagnosticSeverity) -> DiagnosticSeverity {
    match severity {
        clang_sys::CXDiagnostic_Ignored => DiagnosticSeverity::Ignored,
        clang_sys::CXDiagnostic_Note => DiagnosticSeverity::Note,
        clang_sys::CXDiagnostic_Warning => DiagnosticSeverity::Warning,
        clang_sys::CXDiagnostic_Error => DiagnosticSeverity::Error,
        clang_sys::CXDiagnostic_Fatal => DiagnosticSeverity::Fatal,
        _ => DiagnosticSeverity::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CollectError, DeclarationKind, ScratchLane, TypeKind, declaration_kind, push, type_kind,
    };
    use crate::legacy::facts::SymbolIdentity;

    /// Holds the exact result category required by one deterministic collector unit test.
    #[derive(Debug, thiserror::Error)]
    enum TestError {
        /// A collector result differed from the required closed error fact.
        #[error("unexpected collector result: {0:?}")]
        Unexpected(CollectError),
        /// A fact classifier differed from the direct native kind law.
        #[error("native classifier did not preserve the required fact")]
        Classification,
    }

    #[test]
    fn capacity_error_preserves_exact_fact_lane_and_required_slot() -> Result<(), TestError> {
        let mut slots = [0_u8];
        let mut used = 0;
        push(&mut slots, &mut used, ScratchLane::Types, 1).map_err(TestError::Unexpected)?;
        match push(&mut slots, &mut used, ScratchLane::Types, 2) {
            Err(
                error @ CollectError::ScratchCapacity {
                    lane: ScratchLane::Types,
                    capacity: 1,
                    required: 2,
                },
            ) => {
                let _ = error;
                Ok(())
            }
            Err(error) => Err(TestError::Unexpected(error)),
            Ok(()) => Err(TestError::Classification),
        }
    }

    #[test]
    fn native_kind_classifiers_preserve_templates_and_recursive_types() -> Result<(), TestError> {
        let template = declaration_kind(clang_sys::CXCursor_FunctionTemplate);
        let pointer = type_kind(clang_sys::CXType_Pointer, None);
        let block_pointer = type_kind(clang_sys::CXType_BlockPointer, None);
        let member_pointer = type_kind(clang_sys::CXType_MemberPointer, None);
        let named = type_kind(
            clang_sys::CXType_Unexposed,
            Some(SymbolIdentity {
                bytes: [1; crate::legacy::SYMBOL_IDENTITY_BYTES],
            }),
        );
        if template == DeclarationKind::Template
            && pointer == TypeKind::Pointer
            && block_pointer == TypeKind::BlockPointer
            && member_pointer == TypeKind::MemberPointer
            && named == TypeKind::Named
        {
            Ok(())
        } else {
            Err(TestError::Classification)
        }
    }
}

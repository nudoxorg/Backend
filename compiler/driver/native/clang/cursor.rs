//! Defines cursor behavior for the direct Clang semantic frontend of `compiler-driver`.
//! The closed match from libclang cursor kinds onto the canonical semantic kind registry
//! makes a wrong-kind mapping unrepresentable: every emitted fact names the kind its cursor
//! branch constructed, and no cursor kind outside the map can produce an entity fact.

use std::{ffi::CStr, ffi::c_int, ffi::c_uint, ptr};

use super::{
    error::ClangError,
    ffi,
    protocol::{
        ClangReferenceKind, ClangSourceSpan, ClangTypeUseKind, ClangTypeUseResolution, FactId,
        FactRole, SemanticKind,
    },
    traversal::{VisitIssue, VisitState},
};

/// Visits one cursor and emits its typed facts, if its kind maps onto the canonical registry.
pub(crate) fn visit_one<'source, 'path, 'scratch, 'cancel>(
    state: &mut VisitState<'source, 'path, 'scratch, 'cancel>,
    cursor: ffi::CxCursor,
    parent: ffi::CxCursor,
) -> Result<(), VisitIssue> {
    // SAFETY: the cursor belongs to the live translation unit.
    let kind = unsafe { ffi::clang_get_cursor_kind(cursor) };
    if !is_main_file(cursor) && kind != ffi::CX_CURSOR_MACRO_EXPANSION {
        return Ok(());
    }
    match kind {
        ffi::CX_CURSOR_STRUCT_DECL | ffi::CX_CURSOR_UNION_DECL | ffi::CX_CURSOR_CLASS_DECL => {
            let Some(name) = name_span(cursor) else {
                return Ok(());
            };
            let owner = extent_span(state.source, cursor).unwrap_or(name);
            emit_entity(state, cursor, SemanticKind::Record, name, owner, None)?;
        }
        ffi::CX_CURSOR_FUNCTION_DECL
        | ffi::CX_CURSOR_CXX_METHOD
        | ffi::CX_CURSOR_CONSTRUCTOR
        | ffi::CX_CURSOR_DESTRUCTOR
        | ffi::CX_CURSOR_FUNCTION_TEMPLATE => {
            let Some(name) = name_span(cursor) else {
                return Ok(());
            };
            let owner = extent_span(state.source, cursor).unwrap_or(name);
            emit_entity(
                state,
                cursor,
                SemanticKind::Function,
                name,
                owner,
                Some(ClangTypeUseKind::FunctionSignature),
            )?;
        }
        ffi::CX_CURSOR_FIELD_DECL => {
            let Some(name) = name_span(cursor) else {
                return Ok(());
            };
            let owner = owner_for(state, cursor)?.unwrap_or(name);
            emit_entity(
                state,
                cursor,
                SemanticKind::Field,
                name,
                owner,
                Some(ClangTypeUseKind::Field),
            )?;
        }
        ffi::CX_CURSOR_PARM_DECL => {
            let Some(name) = name_span(cursor) else {
                return Ok(());
            };
            let owner = owner_for(state, cursor)?.unwrap_or(name);
            emit_entity(
                state,
                cursor,
                SemanticKind::Parameter,
                name,
                owner,
                Some(ClangTypeUseKind::Parameter),
            )?;
        }
        ffi::CX_CURSOR_VAR_DECL => {
            let Some(name) = name_span(cursor) else {
                return Ok(());
            };
            let owner = owner_for(state, cursor)?.unwrap_or(name);
            let semantic_kind = if is_const_qualified(cursor) {
                SemanticKind::Constant
            } else {
                SemanticKind::Static
            };
            emit_entity(
                state,
                cursor,
                semantic_kind,
                name,
                owner,
                Some(ClangTypeUseKind::Variable),
            )?;
        }
        ffi::CX_CURSOR_ENUM_DECL => {
            let Some(name) = name_span(cursor) else {
                return Ok(());
            };
            let owner = extent_span(state.source, cursor).unwrap_or(name);
            emit_entity(state, cursor, SemanticKind::Enum, name, owner, None)?;
        }
        ffi::CX_CURSOR_ENUM_CONSTANT_DECL => {
            let Some(name) = name_span(cursor) else {
                return Ok(());
            };
            let owner = owner_for(state, cursor)?.unwrap_or(name);
            emit_entity(state, cursor, SemanticKind::Variant, name, owner, None)?;
        }
        ffi::CX_CURSOR_TYPEDEF_DECL => {
            let Some(name) = name_span(cursor) else {
                return Ok(());
            };
            let owner = extent_span(state.source, cursor).unwrap_or(name);
            emit_entity(state, cursor, SemanticKind::Alias, name, owner, None)?;
        }
        ffi::CX_CURSOR_NAMESPACE => {
            let Some(name) = name_span(cursor) else {
                return Ok(());
            };
            let owner = extent_span(state.source, cursor).unwrap_or(name);
            emit_entity(state, cursor, SemanticKind::Module, name, owner, None)?;
        }
        ffi::CX_CURSOR_TYPE_REF => {
            emit_type_ref(state, cursor, parent)?;
        }
        ffi::CX_CURSOR_DECL_REF_EXPR => {
            emit_reference(state, cursor)?;
        }
        ffi::CX_CURSOR_MEMBER_REF_EXPR => {
            emit_reference(state, cursor)?;
        }
        ffi::CX_CURSOR_MACRO_EXPANSION => {
            emit_reference(state, cursor)?;
        }
        _ => {}
    }
    Ok(())
}

/// Converts an interned identity capacity issue into the exact typed error lens.
fn issue_from_error(error: ClangError<'_>) -> VisitIssue {
    match error {
        ClangError::BindingCapacity { limit, observed } => {
            VisitIssue::BindingCapacity { limit, observed }
        }
        ClangError::LibclangIdentityUnavailable => VisitIssue::IdentityUnavailable,
        ClangError::LibclangFactScratchTooSmall {
            provided, required, ..
        } => VisitIssue::FactScratchCapacity { provided, required },
        ClangError::LibclangIdentityScratchTooSmall {
            provided, required, ..
        } => VisitIssue::IdentityScratchCapacity { provided, required },
        ClangError::LibclangTokenCapacity {
            observed, limit, ..
        } => VisitIssue::TokenCapacity { observed, limit },
        ClangError::FactCountOverflow => VisitIssue::FactCountOverflow,
        _ => VisitIssue::FactCountOverflow,
    }
}

/// Interns the cursor identity, journals the entity fact, and emits its builtin type use.
fn emit_entity<'source, 'path, 'scratch, 'cancel>(
    state: &mut VisitState<'source, 'path, 'scratch, 'cancel>,
    cursor: ffi::CxCursor,
    semantic_kind: SemanticKind,
    name: ClangSourceSpan,
    owner: ClangSourceSpan,
    type_use: Option<ClangTypeUseKind>,
) -> Result<(), VisitIssue> {
    let (identity, _) = state
        .identities
        .intern(
            cursor,
            FactRole::Symbol,
            Some(semantic_kind),
            name,
            state.source_name,
        )
        .map_err(issue_from_error)?;
    check_identity(state, identity)?;
    let Some(name_text) = source_text(state.source, name) else {
        return Err(VisitIssue::FactCountOverflow);
    };
    state.emit_entity_fact(super::protocol::EntityFact {
        name: name_text,
        kind: semantic_kind,
        span: name,
        owner,
        identity,
    })?;
    if let Some(type_use) = type_use {
        emit_builtin_type(state, cursor, type_use, owner)?;
    }
    Ok(())
}

/// Owner extent of one nested declaration from the active ancestor path.
fn owner_for<'source, 'path, 'scratch, 'cancel>(
    state: &VisitState<'source, 'path, 'scratch, 'cancel>,
    _cursor: ffi::CxCursor,
) -> Result<Option<ClangSourceSpan>, VisitIssue> {
    Ok(state.path.owner())
}

/// Rejects identities at or above the exact admitted caller capacity.
fn check_identity<'source, 'path, 'scratch, 'cancel>(
    state: &VisitState<'source, 'path, 'scratch, 'cancel>,
    identity: FactId,
) -> Result<(), VisitIssue> {
    let raw = usize::try_from(identity.ordinal).map_err(|_| VisitIssue::FactCountOverflow)?;
    if raw >= state.identities.max_entries() {
        let observed = raw.checked_add(1).ok_or(VisitIssue::FactCountOverflow)?;
        return Err(VisitIssue::BindingCapacity {
            limit: state.identities.max_entries(),
            observed,
        });
    }
    Ok(())
}

/// Emits one type-use fact naming the parent declaration context.
fn emit_type_ref<'source, 'path, 'scratch, 'cancel>(
    state: &mut VisitState<'source, 'path, 'scratch, 'cancel>,
    cursor: ffi::CxCursor,
    parent: ffi::CxCursor,
) -> Result<(), VisitIssue> {
    let Some(kind) = type_use_context_kind(parent) else {
        return Ok(());
    };
    let Some(span) = reference_name_span(cursor).or_else(|| extent_span(state.source, cursor))
    else {
        return Ok(());
    };
    // SAFETY: the cursor belongs to the live translation unit; a null return is checked.
    let target = unsafe { ffi::clang_get_cursor_referenced(cursor) };
    if parent_is_null(target) {
        return Ok(());
    }
    if !is_main_file(target) {
        return Err(VisitIssue::ExternalIdentityUnavailable);
    }
    let Some(target_span) = name_span(target).or_else(|| extent_span(state.source, target)) else {
        return Ok(());
    };
    let (identity, _) = state
        .identities
        .intern(target, FactRole::Type, None, target_span, state.source_name)
        .map_err(issue_from_error)?;
    check_identity(state, identity)?;
    let Some(name) = source_text(state.source, span) else {
        return Ok(());
    };
    let owner = owner_for(state, parent)?.unwrap_or(span);
    state.emit_type_use_fact(super::protocol::TypeUseFact {
        name,
        kind,
        resolution: ClangTypeUseResolution::Declaration {
            target: target_span,
            identity,
        },
        span,
        owner,
    })
}

/// Emits one builtin type-use fact located by bounded tokenization before the name.
fn emit_builtin_type<'source, 'path, 'scratch, 'cancel>(
    state: &mut VisitState<'source, 'path, 'scratch, 'cancel>,
    cursor: ffi::CxCursor,
    kind: ClangTypeUseKind,
    owner: ClangSourceSpan,
) -> Result<(), VisitIssue> {
    // SAFETY: the cursor belongs to the live translation unit.
    let type_ = if kind == ClangTypeUseKind::FunctionSignature {
        unsafe { ffi::clang_get_cursor_result_type(cursor) }
    } else {
        unsafe { ffi::clang_get_cursor_type(cursor) }
    };
    // SAFETY: the type belongs to the live translation unit.
    let canonical = unsafe { ffi::clang_get_canonical_type(type_) };
    let canonical_kind = canonical.kind;
    if !is_builtin(canonical_kind) {
        return Ok(());
    }
    // SAFETY: the type belongs to the live translation unit; the string is disposed on
    // every path below exactly once.
    let spelling = unsafe { ffi::clang_get_type_spelling(canonical) };
    // SAFETY: the string is live for this borrow and disposed at the end of the block.
    let last = {
        let pointer = unsafe { ffi::clang_get_c_string(spelling) };
        if pointer.is_null() {
            None
        } else {
            // SAFETY: libclang returns a NUL-terminated string for a live handle.
            last_word(unsafe { CStr::from_ptr(pointer) }.to_bytes())
        }
    };
    let Some(last_word) = last else {
        // SAFETY: the string is live and is disposed exactly once here.
        unsafe { ffi::clang_dispose_string(spelling) };
        return Ok(());
    };
    let span = token_span_before_name(
        state.translation_unit,
        state.source,
        cursor,
        last_word,
        state.token_limit,
    );
    // SAFETY: the string is live and is disposed exactly once here.
    unsafe { ffi::clang_dispose_string(spelling) };
    let Some(span) = span? else {
        return Ok(());
    };
    let Some(name) = source_text(state.source, span) else {
        return Ok(());
    };
    state.emit_type_use_fact(super::protocol::TypeUseFact {
        name,
        kind,
        resolution: ClangTypeUseResolution::Builtin,
        span,
        owner,
    })
}

/// Emits one reference fact resolved through the cursor authority.
fn emit_reference<'source, 'path, 'scratch, 'cancel>(
    state: &mut VisitState<'source, 'path, 'scratch, 'cancel>,
    cursor: ffi::CxCursor,
) -> Result<(), VisitIssue> {
    let Some(use_span) = reference_name_span(cursor).or_else(|| extent_span(state.source, cursor))
    else {
        return Ok(());
    };
    // SAFETY: the cursor belongs to the live translation unit; a null return is checked.
    let target = unsafe { ffi::clang_get_cursor_referenced(cursor) };
    if parent_is_null(target) {
        return Ok(());
    }
    let kind = reference_kind(state, cursor, target);
    if !is_main_file(target) {
        // A foreign macro definition has no admitted identity in the caller's source.
        // Its expansion remains a valid preprocessing observation, but omitting that
        // typed fact is safer than turning a package header into a caller identity error.
        if kind == ClangReferenceKind::MacroInvocation {
            return Ok(());
        }
        return Err(VisitIssue::ExternalIdentityUnavailable);
    }
    let Some(resolved_fallback) = name_span(target).or_else(|| extent_span(state.source, target))
    else {
        return Ok(());
    };
    let (identity, resolved_span) = state
        .identities
        .intern(
            target,
            FactRole::Symbol,
            None,
            resolved_fallback,
            state.source_name,
        )
        .map_err(issue_from_error)?;
    check_identity(state, identity)?;
    let Some(target_name) = source_text(state.source, use_span) else {
        return Ok(());
    };
    let owner = owner_for(state, cursor)?.unwrap_or(use_span);
    state.emit_reference_fact(super::protocol::ReferenceFact {
        target: target_name,
        kind,
        use_span,
        resolved_span,
        owner,
        identity,
    })
}

/// Classifies from the resolved declaration, adding call context only for call-capable targets.
fn reference_kind<'source, 'path, 'scratch, 'cancel>(
    state: &VisitState<'source, 'path, 'scratch, 'cancel>,
    cursor: ffi::CxCursor,
    target: ffi::CxCursor,
) -> ClangReferenceKind {
    // SAFETY: both cursors belong to the live translation unit.
    let use_kind = unsafe { ffi::clang_get_cursor_kind(cursor) };
    // SAFETY: both cursors belong to the live translation unit.
    let target_kind = unsafe { ffi::clang_get_cursor_kind(target) };
    if target_kind == ffi::CX_CURSOR_MACRO_DEFINITION || use_kind == ffi::CX_CURSOR_MACRO_EXPANSION
    {
        return ClangReferenceKind::MacroInvocation;
    }
    match target_kind {
        ffi::CX_CURSOR_FIELD_DECL => ClangReferenceKind::FieldAccess,
        ffi::CX_CURSOR_CXX_METHOD | ffi::CX_CURSOR_CONSTRUCTOR | ffi::CX_CURSOR_DESTRUCTOR
            if matches!(
                state.path.nearest_call_kind(),
                Some(ffi::CX_CURSOR_CALL_EXPR | ffi::CX_CURSOR_CXX_MEMBER_CALL_EXPR)
            ) =>
        {
            ClangReferenceKind::MethodCall
        }
        ffi::CX_CURSOR_FUNCTION_DECL | ffi::CX_CURSOR_FUNCTION_TEMPLATE
            if state.path.nearest_call_kind().is_some() =>
        {
            ClangReferenceKind::FunctionCall
        }
        _ => ClangReferenceKind::VariableUse,
    }
}

/// Closed use-site role of a type reference from its declaring parent cursor.
fn type_use_context_kind(cursor: ffi::CxCursor) -> Option<ClangTypeUseKind> {
    // SAFETY: the cursor belongs to the live translation unit.
    let kind = unsafe { ffi::clang_get_cursor_kind(cursor) };
    match kind {
        ffi::CX_CURSOR_FIELD_DECL => Some(ClangTypeUseKind::Field),
        ffi::CX_CURSOR_FUNCTION_DECL
        | ffi::CX_CURSOR_CXX_METHOD
        | ffi::CX_CURSOR_CONSTRUCTOR
        | ffi::CX_CURSOR_DESTRUCTOR
        | ffi::CX_CURSOR_FUNCTION_TEMPLATE => Some(ClangTypeUseKind::FunctionSignature),
        ffi::CX_CURSOR_PARM_DECL => Some(ClangTypeUseKind::Parameter),
        ffi::CX_CURSOR_VAR_DECL => Some(ClangTypeUseKind::Variable),
        _ => None,
    }
}

/// Reports whether a cursor's assigned type is const-qualified.
fn is_const_qualified(cursor: ffi::CxCursor) -> bool {
    // SAFETY: the cursor belongs to the live translation unit.
    let type_ = unsafe { ffi::clang_get_cursor_type(cursor) };
    // SAFETY: the type belongs to the live translation unit.
    unsafe { ffi::clang_is_const_qualified_type(type_) != 0 }
}

/// Reports whether the canonical type kind is a builtin primitive.
fn is_builtin(kind: c_int) -> bool {
    matches!(
        kind,
        ffi::CX_TYPE_VOID
            | ffi::CX_TYPE_BOOL
            | ffi::CX_TYPE_CHAR_U
            | ffi::CX_TYPE_UCHAR
            | ffi::CX_TYPE_CHAR16
            | ffi::CX_TYPE_CHAR32
            | ffi::CX_TYPE_USHORT
            | ffi::CX_TYPE_UINT
            | ffi::CX_TYPE_ULONG
            | ffi::CX_TYPE_ULONG_LONG
            | ffi::CX_TYPE_UINT128
            | ffi::CX_TYPE_CHAR_S
            | ffi::CX_TYPE_SCHAR
            | ffi::CX_TYPE_WCHAR
            | ffi::CX_TYPE_SHORT
            | ffi::CX_TYPE_INT
            | ffi::CX_TYPE_LONG
            | ffi::CX_TYPE_LONG_LONG
            | ffi::CX_TYPE_INT128
            | ffi::CX_TYPE_FLOAT
            | ffi::CX_TYPE_DOUBLE
            | ffi::CX_TYPE_LONG_DOUBLE
    )
}

/// Final alphanumeric word of a type spelling.
fn last_word(bytes: &[u8]) -> Option<&[u8]> {
    let mut end = bytes.len();
    while end != 0 && !bytes[end - 1].is_ascii_alphanumeric() {
        end -= 1;
    }
    let mut start = end;
    while start != 0 && bytes[start - 1].is_ascii_alphanumeric() {
        start -= 1;
    }
    (start < end).then_some(&bytes[start..end])
}

/// Locates the exact span of the token spelling `wanted` immediately before the cursor name,
/// under the caller-derived token capacity.
fn token_span_before_name(
    translation_unit: ffi::CxTranslationUnit,
    source: &[u8],
    cursor: ffi::CxCursor,
    wanted: &[u8],
    token_limit: c_uint,
) -> Result<Option<ClangSourceSpan>, VisitIssue> {
    // SAFETY: the cursor belongs to the live translation unit.
    let range = unsafe { ffi::clang_get_cursor_extent(cursor) };
    let mut tokens: *mut ffi::CxToken = ptr::null_mut();
    let mut count: c_uint = 0;
    // SAFETY: the translation unit is live and the out-pointers are reserved stack slots.
    unsafe { ffi::clang_tokenize(translation_unit, range, &mut tokens, &mut count) };
    if tokens.is_null() {
        return Ok(None);
    }
    if count > token_limit {
        // SAFETY: the tokens were produced live and are disposed exactly once here.
        unsafe { ffi::clang_dispose_tokens(translation_unit, tokens, count) };
        return Err(VisitIssue::TokenCapacity {
            observed: count as usize,
            limit: token_limit as usize,
        });
    }
    let Some(name_start) = name_span(cursor).map(|span| span.start) else {
        // SAFETY: the tokens were produced live and are disposed exactly once here.
        unsafe { ffi::clang_dispose_tokens(translation_unit, tokens, count) };
        return Ok(None);
    };
    let mut result = None;
    for index in 0..count {
        // SAFETY: the index is below the live token count returned by libclang.
        let token = unsafe { *tokens.add(index as usize) };
        // SAFETY: the token is live for this query.
        let token_kind = unsafe { ffi::clang_get_token_kind(token) };
        if token_kind == ffi::CX_TOKEN_COMMENT {
            continue;
        }
        // SAFETY: the token is live and the returned string is disposed exactly once.
        let spelling = unsafe { ffi::clang_get_token_spelling(translation_unit, token) };
        // SAFETY: the string is live for this borrow and disposed at the end of the block.
        let matches = {
            let pointer = unsafe { ffi::clang_get_c_string(spelling) };
            // SAFETY: libclang returns a NUL-terminated string for a live handle.
            !pointer.is_null() && unsafe { CStr::from_ptr(pointer) }.to_bytes() == wanted
        };
        // SAFETY: the string is live and is disposed exactly once here.
        unsafe { ffi::clang_dispose_string(spelling) };
        if !matches {
            continue;
        }
        // SAFETY: the token is live for this query.
        let token_range = unsafe { ffi::clang_get_token_extent(translation_unit, token) };
        let Some(span) = range_span(source, token_range) else {
            continue;
        };
        if span.end <= name_start {
            result = Some(span);
            break;
        }
    }
    // SAFETY: the tokens were produced live and are disposed exactly once here.
    unsafe { ffi::clang_dispose_tokens(translation_unit, tokens, count) };
    Ok(result)
}

/// Reports whether a cursor belongs to the translation unit's main file.
fn is_main_file(cursor: ffi::CxCursor) -> bool {
    // SAFETY: the cursor belongs to the live translation unit.
    let location = unsafe { ffi::clang_get_cursor_location(cursor) };
    // SAFETY: the location belongs to the live translation unit.
    unsafe { ffi::clang_location_is_from_main_file(location) != 0 }
}

/// Reports whether a cursor is the null cursor.
fn parent_is_null(cursor: ffi::CxCursor) -> bool {
    // SAFETY: the cursor is a value libclang produced.
    unsafe { ffi::clang_cursor_is_null(cursor) != 0 }
}

/// Exact name range of a declaration cursor.
fn name_span(cursor: ffi::CxCursor) -> Option<ClangSourceSpan> {
    // SAFETY: the cursor belongs to the live translation unit.
    let range = unsafe { ffi::clang_cursor_get_spelling_name_range(cursor, 0, 0) };
    // SAFETY: the range belongs to the live translation unit.
    if unsafe { ffi::clang_range_is_null(range) } != 0 {
        return None;
    }
    extent_span_unchecked(range)
}

/// Exact single-piece reference name range of a reference cursor.
fn reference_name_span(cursor: ffi::CxCursor) -> Option<ClangSourceSpan> {
    // SAFETY: the cursor belongs to the live translation unit.
    let range = unsafe {
        ffi::clang_get_cursor_reference_name_range(cursor, ffi::CX_NAME_RANGE_WANT_SINGLE_PIECE, 0)
    };
    // SAFETY: the range belongs to the live translation unit.
    if unsafe { ffi::clang_range_is_null(range) } != 0 {
        None
    } else {
        extent_span_unchecked(range)
    }
}

/// Exact byte-unit extent of a cursor clamped to the caller source.
pub(crate) fn extent_span(source: &[u8], cursor: ffi::CxCursor) -> Option<ClangSourceSpan> {
    // SAFETY: the cursor belongs to the live translation unit.
    let range = unsafe { ffi::clang_get_cursor_extent(cursor) };
    range_span(source, range)
}

/// Converts a name or reference range into a span without a source-bound check.
fn extent_span_unchecked(range: ffi::CxSourceRange) -> Option<ClangSourceSpan> {
    // This helper is used only for name/reference ranges and is converted to source bounds
    // by `source_text` at the call site.
    // SAFETY: the range belongs to the live translation unit.
    let start = unsafe { ffi::clang_get_range_start(range) };
    // SAFETY: the range belongs to the live translation unit.
    let end = unsafe { ffi::clang_get_range_end(range) };
    let mut start_offset = 0_u32;
    let mut end_offset = 0_u32;
    // SAFETY: the locations belong to the live translation unit and the out-pointers are
    // reserved stack slots.
    unsafe {
        ffi::clang_get_file_location(
            start,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut start_offset,
        );
    }
    // SAFETY: the locations belong to the live translation unit and the out-pointers are
    // reserved stack slots.
    unsafe {
        ffi::clang_get_file_location(
            end,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut end_offset,
        );
    }
    (start_offset <= end_offset).then_some(ClangSourceSpan {
        start: start_offset,
        end: end_offset,
    })
}

/// Converts a range into a span bounded by the exact caller source length.
fn range_span(source: &[u8], range: ffi::CxSourceRange) -> Option<ClangSourceSpan> {
    let span = extent_span_unchecked(range)?;
    (span.start as usize <= span.end as usize && span.end as usize <= source.len()).then_some(span)
}

/// Borrows exact UTF-8 source text for a span.
pub(crate) fn source_text(source: &[u8], span: ClangSourceSpan) -> Option<&str> {
    std::str::from_utf8(source.get(span.start as usize..span.end as usize)?).ok()
}

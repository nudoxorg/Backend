//! Defines the reviewed direct libclang C ABI subset for `compiler-driver`.
//!
//! This is the capability's single unsafe module. It declares only the stable libclang
//! entry points, opaque handles, and fixed ABI values this frontend consumes; the names and
//! values are the documented libclang C interface (previously supplied by `clang-sys`, whose
//! own build script cannot express this crate's graceful typed-unavailable link mode).
//! Every call site owns its safety proof next to the call; guards dispose exactly the
//! handles they construct.

#![allow(
    unsafe_code,
    reason = "the reviewed direct libclang C ABI binding owns exactly the declared subset; safe wrappers elsewhere in the module refuse to compile without the linked authority"
)]

use std::ffi::{c_char, c_int, c_uint, c_ulong, c_void};

/// Opaque libclang index handle.
pub(crate) type CxIndex = *mut c_void;
/// Opaque libclang translation-unit handle.
pub(crate) type CxTranslationUnit = *mut c_void;
/// Opaque libclang diagnostic handle.
pub(crate) type CxDiagnostic = *mut c_void;
/// Opaque libclang child diagnostic-set handle.
pub(crate) type CxDiagnosticSet = *mut c_void;
/// Opaque caller-data pointer carried through the traversal callback.
pub(crate) type CxClientData = *mut c_void;
/// Opaque libclang file handle.
pub(crate) type CxFile = *mut c_void;
/// Traversal callback result code.
pub(crate) type CxChildVisitResult = c_int;
/// Traversal callback signature invoked synchronously by libclang.
pub(crate) type CxCursorVisitor =
    extern "C" fn(CxCursor, CxCursor, CxClientData) -> CxChildVisitResult;

/// libclang string handle owning its bytes until disposal.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct CxString {
    /// Opaque libclang-owned payload.
    pub(crate) data: *const c_void,
    /// Opaque libclang-owned flags.
    pub(crate) private_flags: c_uint,
}

/// libclang cursor value carrying its kind and opaque native state.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct CxCursor {
    /// Stable cursor-kind discriminant.
    pub(crate) kind: c_int,
    /// Opaque libclang state.
    pub(crate) xdata: c_int,
    /// Opaque libclang state pointers.
    pub(crate) data: [*const c_void; 3],
}

impl Default for CxCursor {
    fn default() -> Self {
        Self {
            kind: 0,
            xdata: 0,
            data: [std::ptr::null(); 3],
        }
    }
}

/// libclang type value carrying its kind discriminant and opaque state.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct CxType {
    /// Stable type-kind discriminant.
    pub(crate) kind: c_int,
    /// Opaque libclang state pointers.
    pub(crate) data: [*mut c_void; 2],
}

/// libclang source location carrying opaque file state and a physical offset.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct CxSourceLocation {
    /// Opaque libclang file state pointers.
    pub(crate) ptr_data: [*const c_void; 2],
    /// Opaque libclang location integer.
    pub(crate) int_data: c_uint,
}

/// libclang source range carrying opaque file state and physical endpoints.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct CxSourceRange {
    /// Opaque libclang file state pointers.
    pub(crate) ptr_data: [*const c_void; 2],
    /// Physical start datum.
    pub(crate) begin_int_data: c_uint,
    /// Physical end datum.
    pub(crate) end_int_data: c_uint,
}

/// libclang token value.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct CxToken {
    /// Opaque libclang token integers.
    pub(crate) int_data: [c_uint; 4],
    /// Opaque libclang token pointer.
    pub(crate) ptr_data: *mut c_void,
}

/// libclang unsaved-file descriptor binding a name to an in-memory byte authority.
#[repr(C)]
pub(crate) struct CxUnsavedFile {
    /// Name registered for the buffer inside the translation unit.
    pub(crate) filename: *const c_char,
    /// Exact buffer bytes; the only content authority.
    pub(crate) contents: *const c_char,
    /// Exact buffer byte count.
    pub(crate) length: c_ulong,
}

/// One libclang translation-unit resource-usage entry.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct CxTUResourceUsageEntry {
    /// Closed resource-category discriminant.
    pub(crate) kind: c_int,
    /// Reported category amount.
    pub(crate) amount: c_ulong,
}

/// libclang translation-unit resource-usage report owning its entries until disposal.
#[repr(C)]
pub(crate) struct CxTUResourceUsage {
    /// Owning translation unit.
    pub(crate) data: CxTranslationUnit,
    /// Entry count.
    pub(crate) num_entries: c_uint,
    /// Entry array owned until disposal.
    pub(crate) entries: *mut CxTUResourceUsageEntry,
}

/// `CXChildVisit_Break`: stop the traversal.
pub(crate) const CX_CHILD_VISIT_BREAK: CxChildVisitResult = 0;
/// `CXChildVisit_Recurse`: visit this cursor's children.
pub(crate) const CX_CHILD_VISIT_RECURSE: CxChildVisitResult = 2;

/// `CXError_Success`: the parse call succeeded.
pub(crate) const CX_ERROR_SUCCESS: c_int = 0;

/// `CXDiagnostic_Note`.
pub(crate) const CX_DIAGNOSTIC_NOTE: c_int = 1;
/// `CXDiagnostic_Warning`.
pub(crate) const CX_DIAGNOSTIC_WARNING: c_int = 2;
/// `CXDiagnostic_Error`.
pub(crate) const CX_DIAGNOSTIC_ERROR: c_int = 3;
/// `CXDiagnostic_Fatal`.
pub(crate) const CX_DIAGNOSTIC_FATAL: c_int = 4;

/// `CXNameRange_WantSinglePiece`.
pub(crate) const CX_NAME_RANGE_WANT_SINGLE_PIECE: c_uint = 0x4;

/// `CXTranslationUnit_DetailedPreprocessingRecord`.
pub(crate) const CX_TRANSLATION_UNIT_DETAILED_PREPROCESSING_RECORD: c_uint = 1;

/// `CXToken_Comment`.
pub(crate) const CX_TOKEN_COMMENT: c_int = 4;

/// `CXCursor_StructDecl`.
pub(crate) const CX_CURSOR_STRUCT_DECL: c_int = 2;
/// `CXCursor_UnionDecl`.
pub(crate) const CX_CURSOR_UNION_DECL: c_int = 3;
/// `CXCursor_ClassDecl`.
pub(crate) const CX_CURSOR_CLASS_DECL: c_int = 4;
/// `CXCursor_EnumDecl`.
pub(crate) const CX_CURSOR_ENUM_DECL: c_int = 5;
/// `CXCursor_FieldDecl`.
pub(crate) const CX_CURSOR_FIELD_DECL: c_int = 6;
/// `CXCursor_EnumConstantDecl`.
pub(crate) const CX_CURSOR_ENUM_CONSTANT_DECL: c_int = 7;
/// `CXCursor_FunctionDecl`.
pub(crate) const CX_CURSOR_FUNCTION_DECL: c_int = 8;
/// `CXCursor_VarDecl`.
pub(crate) const CX_CURSOR_VAR_DECL: c_int = 9;
/// `CXCursor_ParmDecl`.
pub(crate) const CX_CURSOR_PARM_DECL: c_int = 10;
/// `CXCursor_TypedefDecl`.
pub(crate) const CX_CURSOR_TYPEDEF_DECL: c_int = 20;
/// `CXCursor_CXXMethod`.
pub(crate) const CX_CURSOR_CXX_METHOD: c_int = 21;
/// `CXCursor_Namespace`.
pub(crate) const CX_CURSOR_NAMESPACE: c_int = 22;
/// `CXCursor_Constructor`.
pub(crate) const CX_CURSOR_CONSTRUCTOR: c_int = 24;
/// `CXCursor_Destructor`.
pub(crate) const CX_CURSOR_DESTRUCTOR: c_int = 25;
/// `CXCursor_FunctionTemplate`.
pub(crate) const CX_CURSOR_FUNCTION_TEMPLATE: c_int = 30;
/// `CXCursor_TypeRef`.
pub(crate) const CX_CURSOR_TYPE_REF: c_int = 43;
/// `CXCursor_DeclRefExpr`.
pub(crate) const CX_CURSOR_DECL_REF_EXPR: c_int = 101;
/// `CXCursor_MemberRefExpr`.
pub(crate) const CX_CURSOR_MEMBER_REF_EXPR: c_int = 102;
/// `CXCursor_CallExpr`.
pub(crate) const CX_CURSOR_CALL_EXPR: c_int = 103;
/// `CXCursor_CXXMemberCallExpr`.
pub(crate) const CX_CURSOR_CXX_MEMBER_CALL_EXPR: c_int = 104;
/// `CXCursor_MacroDefinition`.
pub(crate) const CX_CURSOR_MACRO_DEFINITION: c_int = 501;
/// `CXCursor_MacroExpansion`.
pub(crate) const CX_CURSOR_MACRO_EXPANSION: c_int = 502;

/// `CXType_Void`.
pub(crate) const CX_TYPE_VOID: c_int = 2;
/// `CXType_Bool`.
pub(crate) const CX_TYPE_BOOL: c_int = 3;
/// `CXType_Char_U`.
pub(crate) const CX_TYPE_CHAR_U: c_int = 4;
/// `CXType_UChar`.
pub(crate) const CX_TYPE_UCHAR: c_int = 5;
/// `CXType_Char16`.
pub(crate) const CX_TYPE_CHAR16: c_int = 6;
/// `CXType_Char32`.
pub(crate) const CX_TYPE_CHAR32: c_int = 7;
/// `CXType_UShort`.
pub(crate) const CX_TYPE_USHORT: c_int = 8;
/// `CXType_UInt`.
pub(crate) const CX_TYPE_UINT: c_int = 9;
/// `CXType_ULong`.
pub(crate) const CX_TYPE_ULONG: c_int = 10;
/// `CXType_ULongLong`.
pub(crate) const CX_TYPE_ULONG_LONG: c_int = 11;
/// `CXType_UInt128`.
pub(crate) const CX_TYPE_UINT128: c_int = 12;
/// `CXType_Char_S`.
pub(crate) const CX_TYPE_CHAR_S: c_int = 13;
/// `CXType_SChar`.
pub(crate) const CX_TYPE_SCHAR: c_int = 14;
/// `CXType_WChar`.
pub(crate) const CX_TYPE_WCHAR: c_int = 15;
/// `CXType_Short`.
pub(crate) const CX_TYPE_SHORT: c_int = 16;
/// `CXType_Int`.
pub(crate) const CX_TYPE_INT: c_int = 17;
/// `CXType_Long`.
pub(crate) const CX_TYPE_LONG: c_int = 18;
/// `CXType_LongLong`.
pub(crate) const CX_TYPE_LONG_LONG: c_int = 19;
/// `CXType_Int128`.
pub(crate) const CX_TYPE_INT128: c_int = 20;
/// `CXType_Float`.
pub(crate) const CX_TYPE_FLOAT: c_int = 21;
/// `CXType_Double`.
pub(crate) const CX_TYPE_DOUBLE: c_int = 22;
/// `CXType_LongDouble`.
pub(crate) const CX_TYPE_LONG_DOUBLE: c_int = 23;
/// libclang `CXType_Invalid`: returned by pointee queries on non-pointer types.
pub(crate) const CX_TYPE_INVALID: c_int = 0;

/// Creates a libclang index.
///
/// # Safety
/// The returned handle owns native state until [`clang_disposeIndex`]; a null return means
/// libclang refused construction.
pub(crate) unsafe fn clang_create_index(
    exclude_declarations_from_pch: c_int,
    display_diagnostics: c_int,
) -> CxIndex {
    unsafe { clang_createIndex(exclude_declarations_from_pch, display_diagnostics) }
}

/// Disposes an index created by [`clang_create_index`].
///
/// # Safety
/// The handle must be live and must not be used afterwards.
pub(crate) unsafe fn clang_dispose_index(index: CxIndex) {
    unsafe { clang_disposeIndex(index) }
}

/// Parses one translation unit from arguments and at most one unsaved buffer.
///
/// # Safety
/// All pointers must reference live, NUL-free C strings and buffers for the duration of the
/// call; `translation_unit` must be a reserved out-pointer.
#[allow(
    clippy::too_many_arguments,
    reason = "the wrapper preserves the exact eight-parameter stable libclang C ABI; grouping the parameters would misrepresent the called interface"
)]
pub(crate) unsafe fn clang_parse_translation_unit2(
    index: CxIndex,
    source_filename: *const c_char,
    arguments: *const *const c_char,
    argument_count: c_int,
    unsaved_files: *mut CxUnsavedFile,
    unsaved_file_count: c_uint,
    options: c_uint,
    translation_unit: *mut CxTranslationUnit,
) -> c_int {
    unsafe {
        clang_parseTranslationUnit2(
            index,
            source_filename,
            arguments,
            argument_count,
            unsaved_files,
            unsaved_file_count,
            options,
            translation_unit,
        )
    }
}

/// Disposes a translation unit parsed by libclang.
///
/// # Safety
/// The handle must be live and must not be used afterwards.
pub(crate) unsafe fn clang_dispose_translation_unit(translation_unit: CxTranslationUnit) {
    unsafe { clang_disposeTranslationUnit(translation_unit) }
}

/// Returns the translation unit's root cursor.
///
/// # Safety
/// The translation unit must be live.
pub(crate) unsafe fn clang_get_translation_unit_cursor(
    translation_unit: CxTranslationUnit,
) -> CxCursor {
    unsafe { clang_getTranslationUnitCursor(translation_unit) }
}

/// Visits the direct children of one cursor with the callback.
///
/// # Safety
/// The cursor must belong to a live translation unit; the callback and its client data must
/// remain valid until libclang returns.
pub(crate) unsafe fn clang_visit_children(
    parent: CxCursor,
    visitor: CxCursorVisitor,
    client_data: CxClientData,
) -> c_uint {
    unsafe { clang_visitChildren(parent, visitor, client_data) }
}

/// Returns a cursor's kind discriminant.
///
/// # Safety
/// The cursor must belong to a live translation unit.
pub(crate) unsafe fn clang_get_cursor_kind(cursor: CxCursor) -> c_int {
    unsafe { clang_getCursorKind(cursor) }
}

/// Returns the canonical declaration of a cursor.
///
/// # Safety
/// The cursor must belong to a live translation unit.
pub(crate) unsafe fn clang_get_canonical_cursor(cursor: CxCursor) -> CxCursor {
    unsafe { clang_getCanonicalCursor(cursor) }
}

/// Returns the unified symbol resolution string of a cursor.
///
/// # Safety
/// The cursor must belong to a live translation unit; the returned string is disposed exactly once.
pub(crate) unsafe fn clang_get_cursor_usr(cursor: CxCursor) -> CxString {
    unsafe { clang_getCursorUSR(cursor) }
}

/// Borrows a libclang string's bytes.
///
/// # Safety
/// The string must be live and its borrowed bytes must not outlive it.
pub(crate) unsafe fn clang_get_c_string(string: CxString) -> *const c_char {
    unsafe { clang_getCString(string) }
}

/// Disposes a libclang string exactly once.
///
/// # Safety
/// The string must be live and must not be used afterwards.
pub(crate) unsafe fn clang_dispose_string(string: CxString) {
    unsafe { clang_disposeString(string) }
}

/// Returns the linked library's self-describing version string.
///
/// # Safety
/// The returned string is disposed exactly once by the caller.
pub(crate) unsafe fn clang_get_clang_version() -> CxString {
    unsafe { clang_getClangVersion() }
}

/// Returns the diagnostic count of a translation unit.
///
/// # Safety
/// The translation unit must be live.
pub(crate) unsafe fn clang_get_num_diagnostics(translation_unit: CxTranslationUnit) -> c_uint {
    unsafe { clang_getNumDiagnostics(translation_unit) }
}

/// Returns one top-level diagnostic of a translation unit.
///
/// # Safety
/// The translation unit must be live; a null return is possible and must be checked.
pub(crate) unsafe fn clang_get_diagnostic(
    translation_unit: CxTranslationUnit,
    index: c_uint,
) -> CxDiagnostic {
    unsafe { clang_getDiagnostic(translation_unit, index) }
}

/// Returns the child diagnostic set owned by one diagnostic.
///
/// # Safety
/// The diagnostic must be live; the returned set may be null.
pub(crate) unsafe fn clang_get_child_diagnostics(diagnostic: CxDiagnostic) -> CxDiagnosticSet {
    unsafe { clang_getChildDiagnostics(diagnostic) }
}

/// Returns the child count of a diagnostic set.
///
/// # Safety
/// The set must be live.
pub(crate) unsafe fn clang_get_num_diagnostics_in_set(set: CxDiagnosticSet) -> c_uint {
    unsafe { clang_getNumDiagnosticsInSet(set) }
}

/// Returns one diagnostic of a set.
///
/// # Safety
/// The set must be live; a null return is possible and must be checked.
pub(crate) unsafe fn clang_get_diagnostic_in_set(
    set: CxDiagnosticSet,
    index: c_uint,
) -> CxDiagnostic {
    unsafe { clang_getDiagnosticInSet(set, index) }
}

/// Disposes one diagnostic.
///
/// # Safety
/// The handle must be live and must not be used afterwards.
pub(crate) unsafe fn clang_dispose_diagnostic(diagnostic: CxDiagnostic) {
    unsafe { clang_disposeDiagnostic(diagnostic) }
}

/// Disposes one child diagnostic set.
///
/// # Safety
/// The handle must be live and must not be used afterwards.
pub(crate) unsafe fn clang_dispose_diagnostic_set(set: CxDiagnosticSet) {
    unsafe { clang_disposeDiagnosticSet(set) }
}

/// Returns a diagnostic's severity discriminant.
///
/// # Safety
/// The diagnostic must be live.
pub(crate) unsafe fn clang_get_diagnostic_severity(diagnostic: CxDiagnostic) -> c_int {
    unsafe { clang_getDiagnosticSeverity(diagnostic) }
}

/// Returns a diagnostic's source location.
///
/// # Safety
/// The diagnostic must be live.
pub(crate) unsafe fn clang_get_diagnostic_location(diagnostic: CxDiagnostic) -> CxSourceLocation {
    unsafe { clang_getDiagnosticLocation(diagnostic) }
}

/// Returns a cursor's source location.
///
/// # Safety
/// The cursor must belong to a live translation unit.
pub(crate) unsafe fn clang_get_cursor_location(cursor: CxCursor) -> CxSourceLocation {
    unsafe { clang_getCursorLocation(cursor) }
}

/// Reports whether a location belongs to the translation unit's main file.
///
/// # Safety
/// The location must belong to a live translation unit.
pub(crate) unsafe fn clang_location_is_from_main_file(location: CxSourceLocation) -> c_int {
    unsafe { clang_Location_isFromMainFile(location) }
}

/// Decomposes a location into its expansion file, line, column, and offset.
///
/// # Safety
/// The location must belong to a live translation unit; out-pointers must be writable.
pub(crate) unsafe fn clang_get_expansion_location(
    location: CxSourceLocation,
    file: *mut CxFile,
    line: *mut c_uint,
    column: *mut c_uint,
    offset: *mut c_uint,
) {
    unsafe { clang_getExpansionLocation(location, file, line, column, offset) }
}

/// Decomposes a location into its file-relative file, line, column, and offset.
///
/// # Safety
/// The location must belong to a live translation unit; out-pointers must be writable.
pub(crate) unsafe fn clang_get_file_location(
    location: CxSourceLocation,
    file: *mut CxFile,
    line: *mut c_uint,
    column: *mut c_uint,
    offset: *mut c_uint,
) {
    unsafe { clang_getFileLocation(location, file, line, column, offset) }
}

/// Returns a cursor's assigned type.
///
/// # Safety
/// The cursor must belong to a live translation unit.
pub(crate) unsafe fn clang_get_cursor_type(cursor: CxCursor) -> CxType {
    unsafe { clang_getCursorType(cursor) }
}

/// Returns a function-like cursor's result type.
///
/// # Safety
/// The cursor must belong to a live translation unit.
pub(crate) unsafe fn clang_get_cursor_result_type(cursor: CxCursor) -> CxType {
    unsafe { clang_getCursorResultType(cursor) }
}

/// Returns a type's canonical form.
///
/// # Safety
/// The type must belong to a live translation unit.
pub(crate) unsafe fn clang_get_canonical_type(type_: CxType) -> CxType {
    unsafe { clang_getCanonicalType(type_) }
}

/// Reports whether a type is const-qualified.
///
/// # Safety
/// The type must belong to a live translation unit.
pub(crate) unsafe fn clang_is_const_qualified_type(type_: CxType) -> c_uint {
    unsafe { clang_isConstQualifiedType(type_) }
}

/// Returns a pointer type's pointee.
///
/// # Safety
/// The type must belong to a live translation unit.
pub(crate) unsafe fn clang_get_pointee_type(type_: CxType) -> CxType {
    unsafe { clang_getPointeeType(type_) }
}

/// Returns a type's spelling.
///
/// # Safety
/// The type must belong to a live translation unit; the returned string is disposed exactly once.
pub(crate) unsafe fn clang_get_type_spelling(type_: CxType) -> CxString {
    unsafe { clang_getTypeSpelling(type_) }
}

/// Returns a cursor's full source extent.
///
/// # Safety
/// The cursor must belong to a live translation unit.
pub(crate) unsafe fn clang_get_cursor_extent(cursor: CxCursor) -> CxSourceRange {
    unsafe { clang_getCursorExtent(cursor) }
}

/// Returns a range's start location.
///
/// # Safety
/// The range must belong to a live translation unit.
pub(crate) unsafe fn clang_get_range_start(range: CxSourceRange) -> CxSourceLocation {
    unsafe { clang_getRangeStart(range) }
}

/// Returns a range's end location.
///
/// # Safety
/// The range must belong to a live translation unit.
pub(crate) unsafe fn clang_get_range_end(range: CxSourceRange) -> CxSourceLocation {
    unsafe { clang_getRangeEnd(range) }
}

/// Reports whether a range is null.
///
/// # Safety
/// The range must belong to a live translation unit.
pub(crate) unsafe fn clang_range_is_null(range: CxSourceRange) -> c_int {
    unsafe { clang_Range_isNull(range) }
}

/// Returns the spelling name range of a cursor.
///
/// # Safety
/// The cursor must belong to a live translation unit.
pub(crate) unsafe fn clang_cursor_get_spelling_name_range(
    cursor: CxCursor,
    piece_index: c_uint,
    options: c_uint,
) -> CxSourceRange {
    unsafe { clang_Cursor_getSpellingNameRange(cursor, piece_index, options) }
}

/// Returns the reference name range of a cursor.
///
/// # Safety
/// The cursor must belong to a live translation unit.
pub(crate) unsafe fn clang_get_cursor_reference_name_range(
    cursor: CxCursor,
    name_range_flags: c_uint,
    piece_index: c_uint,
) -> CxSourceRange {
    unsafe { clang_getCursorReferenceNameRange(cursor, name_range_flags, piece_index) }
}

/// Returns the declaration a cursor references.
///
/// # Safety
/// The cursor must belong to a live translation unit; a null return is possible and must be checked.
pub(crate) unsafe fn clang_get_cursor_referenced(cursor: CxCursor) -> CxCursor {
    unsafe { clang_getCursorReferenced(cursor) }
}

/// Compares two cursors for equality.
///
/// # Safety
/// Both cursors must belong to live translation units.
pub(crate) unsafe fn clang_equal_cursors(left: CxCursor, right: CxCursor) -> c_uint {
    unsafe { clang_equalCursors(left, right) }
}

/// Reports whether a cursor is the null cursor.
///
/// # Safety
/// The cursor must be a value libclang produced.
pub(crate) unsafe fn clang_cursor_is_null(cursor: CxCursor) -> c_int {
    unsafe { clang_Cursor_isNull(cursor) }
}

/// Tokenizes a source range into caller-disposed token storage.
///
/// # Safety
/// The translation unit must be live; out-pointers must be writable and the returned tokens
/// must be disposed exactly once with [`clang_dispose_tokens`].
pub(crate) unsafe fn clang_tokenize(
    translation_unit: CxTranslationUnit,
    range: CxSourceRange,
    tokens: *mut *mut CxToken,
    token_count: *mut c_uint,
) {
    unsafe { clang_tokenize_range(translation_unit, range, tokens, token_count) }
}

/// Disposes tokens returned by [`clang_tokenize`].
///
/// # Safety
/// The tokens must be live and must not be used afterwards.
pub(crate) unsafe fn clang_dispose_tokens(
    translation_unit: CxTranslationUnit,
    tokens: *mut CxToken,
    token_count: c_uint,
) {
    unsafe { clang_disposeTokens(translation_unit, tokens, token_count) }
}

/// Returns a token's kind discriminant.
///
/// # Safety
/// The token must belong to a live tokenization.
pub(crate) unsafe fn clang_get_token_kind(token: CxToken) -> c_int {
    unsafe { clang_getTokenKind(token) }
}

/// Returns a token's spelling.
///
/// # Safety
/// The token and translation unit must be live; the returned string is disposed exactly once.
pub(crate) unsafe fn clang_get_token_spelling(
    translation_unit: CxTranslationUnit,
    token: CxToken,
) -> CxString {
    unsafe { clang_getTokenSpelling(translation_unit, token) }
}

/// Returns a token's source extent.
///
/// # Safety
/// The token and translation unit must be live.
pub(crate) unsafe fn clang_get_token_extent(
    translation_unit: CxTranslationUnit,
    token: CxToken,
) -> CxSourceRange {
    unsafe { clang_getTokenExtent(translation_unit, token) }
}

/// Returns a translation unit's resource-usage report.
///
/// # Safety
/// The translation unit must be live; the report is disposed exactly once with
/// [`clang_dispose_tu_resource_usage`].
pub(crate) unsafe fn clang_get_tu_resource_usage(
    translation_unit: CxTranslationUnit,
) -> CxTUResourceUsage {
    unsafe { clang_getCXTUResourceUsage(translation_unit) }
}

/// Disposes a resource-usage report.
///
/// # Safety
/// The report must be live and must not be used afterwards.
pub(crate) unsafe fn clang_dispose_tu_resource_usage(usage: CxTUResourceUsage) {
    unsafe { clang_disposeCXTUResourceUsage(usage) }
}

unsafe extern "C" {
    fn clang_createIndex(
        exclude_declarations_from_pch: c_int,
        display_diagnostics: c_int,
    ) -> CxIndex;
    fn clang_disposeIndex(index: CxIndex);
    fn clang_parseTranslationUnit2(
        index: CxIndex,
        source_filename: *const c_char,
        arguments: *const *const c_char,
        argument_count: c_int,
        unsaved_files: *mut CxUnsavedFile,
        unsaved_file_count: c_uint,
        options: c_uint,
        translation_unit: *mut CxTranslationUnit,
    ) -> c_int;
    fn clang_disposeTranslationUnit(translation_unit: CxTranslationUnit);
    fn clang_getTranslationUnitCursor(translation_unit: CxTranslationUnit) -> CxCursor;
    fn clang_visitChildren(
        parent: CxCursor,
        visitor: CxCursorVisitor,
        client_data: CxClientData,
    ) -> c_uint;
    fn clang_getCursorKind(cursor: CxCursor) -> c_int;
    fn clang_getCanonicalCursor(cursor: CxCursor) -> CxCursor;
    fn clang_getCursorUSR(cursor: CxCursor) -> CxString;
    fn clang_getCString(string: CxString) -> *const c_char;
    fn clang_disposeString(string: CxString);
    fn clang_getClangVersion() -> CxString;
    fn clang_getNumDiagnostics(translation_unit: CxTranslationUnit) -> c_uint;
    fn clang_getDiagnostic(translation_unit: CxTranslationUnit, index: c_uint) -> CxDiagnostic;
    fn clang_getChildDiagnostics(diagnostic: CxDiagnostic) -> CxDiagnosticSet;
    fn clang_getNumDiagnosticsInSet(set: CxDiagnosticSet) -> c_uint;
    fn clang_getDiagnosticInSet(set: CxDiagnosticSet, index: c_uint) -> CxDiagnostic;
    fn clang_disposeDiagnostic(diagnostic: CxDiagnostic);
    fn clang_disposeDiagnosticSet(set: CxDiagnosticSet);
    fn clang_getDiagnosticSeverity(diagnostic: CxDiagnostic) -> c_int;
    fn clang_getDiagnosticLocation(diagnostic: CxDiagnostic) -> CxSourceLocation;
    fn clang_getCursorLocation(cursor: CxCursor) -> CxSourceLocation;
    fn clang_Location_isFromMainFile(location: CxSourceLocation) -> c_int;
    fn clang_getExpansionLocation(
        location: CxSourceLocation,
        file: *mut CxFile,
        line: *mut c_uint,
        column: *mut c_uint,
        offset: *mut c_uint,
    );
    fn clang_getFileLocation(
        location: CxSourceLocation,
        file: *mut CxFile,
        line: *mut c_uint,
        column: *mut c_uint,
        offset: *mut c_uint,
    );
    fn clang_getCursorType(cursor: CxCursor) -> CxType;
    fn clang_getCursorResultType(cursor: CxCursor) -> CxType;
    fn clang_getCanonicalType(type_: CxType) -> CxType;
    fn clang_isConstQualifiedType(type_: CxType) -> c_uint;
    fn clang_getPointeeType(type_: CxType) -> CxType;
    fn clang_getTypeSpelling(type_: CxType) -> CxString;
    fn clang_getCursorExtent(cursor: CxCursor) -> CxSourceRange;
    fn clang_getRangeStart(range: CxSourceRange) -> CxSourceLocation;
    fn clang_getRangeEnd(range: CxSourceRange) -> CxSourceLocation;
    fn clang_Range_isNull(range: CxSourceRange) -> c_int;
    fn clang_Cursor_getSpellingNameRange(
        cursor: CxCursor,
        piece_index: c_uint,
        options: c_uint,
    ) -> CxSourceRange;
    fn clang_getCursorReferenceNameRange(
        cursor: CxCursor,
        name_range_flags: c_uint,
        piece_index: c_uint,
    ) -> CxSourceRange;
    fn clang_getCursorReferenced(cursor: CxCursor) -> CxCursor;
    fn clang_equalCursors(left: CxCursor, right: CxCursor) -> c_uint;
    fn clang_Cursor_isNull(cursor: CxCursor) -> c_int;
    #[link_name = "clang_tokenize"]
    fn clang_tokenize_range(
        translation_unit: CxTranslationUnit,
        range: CxSourceRange,
        tokens: *mut *mut CxToken,
        token_count: *mut c_uint,
    );
    fn clang_disposeTokens(
        translation_unit: CxTranslationUnit,
        tokens: *mut CxToken,
        token_count: c_uint,
    );
    fn clang_getTokenKind(token: CxToken) -> c_int;
    fn clang_getTokenSpelling(translation_unit: CxTranslationUnit, token: CxToken) -> CxString;
    fn clang_getTokenExtent(translation_unit: CxTranslationUnit, token: CxToken) -> CxSourceRange;
    fn clang_getCXTUResourceUsage(translation_unit: CxTranslationUnit) -> CxTUResourceUsage;
    fn clang_disposeCXTUResourceUsage(usage: CxTUResourceUsage);
}

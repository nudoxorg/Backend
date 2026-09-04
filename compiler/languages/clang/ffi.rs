//! Contains the one reviewed unsafe boundary around the libclang C API.
//! It turns library loading, translation-unit ownership, locations, and native strings into typed values.
//! No raw libclang pointer, string, or callback state escapes this module.

use core::{
    ffi::{CStr, c_char, c_int, c_uint, c_ulong, c_void},
    ptr,
};
use std::{ffi::CString, path::Path};

use clang_sys::{
    CXChildVisitResult, CXCursor, CXDiagnostic, CXErrorCode, CXFile, CXIndex, CXSourceLocation,
    CXSourceRange, CXString, CXTranslationUnit, CXType, CXUnsavedFile,
};

use crate::{
    CollectError, CompilationCommand, CompilationDatabase, DatabaseError, NativeApi, NativeFailure,
    ParseFailure,
    facts::{SourceSpan, SymbolIdentity},
    input::ClangInput,
};

/// Reads the native compilation database and retains only bounded, owned command cells.
pub(crate) fn load_database(directory: &Path) -> Result<CompilationDatabase, DatabaseError> {
    let directory = CString::new(directory.to_string_lossy().as_bytes())
        .map_err(|_| DatabaseError::DirectoryContainsNul)?;
    if !clang_sys::is_loaded() {
        clang_sys::load().map_err(|_| DatabaseError::Native)?;
    }
    if !clang_sys::clang_CompilationDatabase_fromDirectory::is_loaded()
        || !clang_sys::clang_CompilationDatabase_getAllCompileCommands::is_loaded()
        || !clang_sys::clang_CompilationDatabase_dispose::is_loaded()
        || !clang_sys::clang_CompileCommands_getSize::is_loaded()
        || !clang_sys::clang_CompileCommands_getCommand::is_loaded()
        || !clang_sys::clang_CompileCommands_dispose::is_loaded()
        || !clang_sys::clang_CompileCommand_getArg::is_loaded()
        || !clang_sys::clang_CompileCommand_getDirectory::is_loaded()
        || !clang_sys::clang_CompileCommand_getNumArgs::is_loaded()
        || !clang_sys::clang_getCString::is_loaded()
        || !clang_sys::clang_disposeString::is_loaded()
    {
        return Err(DatabaseError::Native);
    }
    let mut status = clang_sys::CXCompilationDatabase_NoError;
    // SAFETY: the path is a live NUL-terminated string for this synchronous call.
    let database = unsafe {
        clang_sys::clang_CompilationDatabase_fromDirectory(directory.as_ptr(), &raw mut status)
    };
    if database.is_null() {
        return Err(DatabaseError::Absent);
    }
    // SAFETY: database is live and exclusively owned here.
    let commands = unsafe { clang_sys::clang_CompilationDatabase_getAllCompileCommands(database) };
    let size = unsafe { clang_sys::clang_CompileCommands_getSize(commands) };
    let mut result = Vec::new();
    for index in 0..size {
        let command = unsafe { clang_sys::clang_CompileCommands_getCommand(commands, index) };
        let count = unsafe { clang_sys::clang_CompileCommand_getNumArgs(command) } as usize;
        if count > crate::MAX_DATABASE_ARGUMENTS {
            unsafe { clang_sys::clang_CompileCommands_dispose(commands) };
            unsafe { clang_sys::clang_CompilationDatabase_dispose(database) };
            return Err(DatabaseError::ArgumentCapacity {
                required: count,
                capacity: crate::MAX_DATABASE_ARGUMENTS,
            });
        }
        let mut arguments = Vec::with_capacity(count);
        for argument in 0..count {
            let value = unsafe { clang_sys::clang_CompileCommand_getArg(command, argument as u32) };
            let text = native_text(value)?;
            arguments.push(CString::new(text).map_err(|_| DatabaseError::StringContainsNul)?);
        }
        let file_name = arguments.last().ok_or(DatabaseError::Native)?.clone();
        let directory =
            native_text(unsafe { clang_sys::clang_CompileCommand_getDirectory(command) })?;
        result.push(CompilationCommand {
            file_name,
            arguments,
            directory: CString::new(directory).map_err(|_| DatabaseError::StringContainsNul)?,
        });
    }
    unsafe { clang_sys::clang_CompileCommands_dispose(commands) };
    unsafe { clang_sys::clang_CompilationDatabase_dispose(database) };
    Ok(CompilationDatabase { commands: result })
}

/// A live libclang index and translation unit with the caller's main source file identity.
pub(crate) struct TranslationUnit {
    index: CXIndex,
    unit: CXTranslationUnit,
    main_file: CXFile,
    source_len: u32,
}

/// A source coordinate pair not yet narrowed to the canonical u32 representation.
#[derive(Clone, Copy)]
struct NativeSpan {
    start: u64,
    end: u64,
}

/// Loads and checks the direct libclang authority for the current thread.
pub(crate) fn parse(input: ClangInput<'_>) -> Result<TranslationUnit, CollectError> {
    if !clang_sys::is_loaded() {
        clang_sys::load().map_err(|detail| NativeFailure {
            detail: detail.into_boxed_str(),
        })?;
    }
    for api in REQUIRED_APIS {
        if !api.is_loaded() {
            return Err(CollectError::MissingApi { api: api.kind() });
        }
    }
    {
        // Parses the exact caller bytes through an unsaved libclang translation unit.
        if input.source().contains(&0) {
            return Err(CollectError::SourceContainsNul);
        }
        let source_len =
            c_ulong::try_from(input.source().len()).map_err(|_| CollectError::SourceTooLarge {
                observed: input.source().len(),
            })?;
        let canonical_source_len = u32::try_from(input.source().len()).map_err(|_| {
            CollectError::SourceLengthTooLarge {
                observed: input.source().len(),
            }
        })?;
        let resolved_file_name = resolved_file_name(input)?;
        let mut unit = ptr::null_mut();
        let mut unsaved = CXUnsavedFile {
            Filename: resolved_file_name.as_ptr(),
            Contents: input.source().as_ptr().cast::<c_char>(),
            Length: source_len,
        };
        let (arguments, argument_count) = parse_arguments(input)?;
        let argument_count = c_int::try_from(argument_count).map_err(|_| CollectError::Parse {
            failure: ParseFailure::InvalidArguments,
        })?;
        // SAFETY: this function verified this symbol; filename and unsaved content stay live for the
        // call; the output pointer is exclusive and initialized only by libclang.
        let index = unsafe {
            clang_sys::clang_createIndex(
                EXCLUDE_DECLARATIONS_FROM_PREAMBLE,
                HIDE_NATIVE_DIAGNOSTICS,
            )
        };
        if index.is_null() {
            return Err(CollectError::IndexUnavailable);
        }
        // SAFETY: this function verified this symbol; all pointer arguments describe live stack or
        // caller storage; libclang copies/owns its translation unit before the call returns.
        let status = unsafe {
            clang_sys::clang_parseTranslationUnit2(
                index,
                resolved_file_name.as_ptr(),
                arguments.as_ptr(),
                argument_count,
                &raw mut unsaved,
                ONE_UNSAVED_SOURCE,
                clang_sys::CXTranslationUnit_DetailedPreprocessingRecord,
                &raw mut unit,
            )
        };
        if status != clang_sys::CXError_Success {
            // SAFETY: createIndex returned this live index and this path has not transferred it.
            unsafe { clang_sys::clang_disposeIndex(index) };
            return Err(CollectError::Parse {
                failure: parse_failure(status),
            });
        }
        if unit.is_null() {
            // SAFETY: createIndex returned this live index and this path has not transferred it.
            unsafe { clang_sys::clang_disposeIndex(index) };
            return Err(CollectError::Parse {
                failure: ParseFailure::Failure,
            });
        }
        // SAFETY: unit is live, and the input C string lives for this immediate lookup.
        let main_file = unsafe { clang_sys::clang_getFile(unit, resolved_file_name.as_ptr()) };
        if main_file.is_null() {
            // SAFETY: libclang returned the live unit and index on this successful parse path.
            unsafe { clang_sys::clang_disposeTranslationUnit(unit) };
            // SAFETY: the unit was released before its owning index.
            unsafe { clang_sys::clang_disposeIndex(index) };
            return Err(CollectError::MainFileUnavailable);
        }
        Ok(TranslationUnit {
            index,
            unit,
            main_file,
            source_len: canonical_source_len,
        })
    }
}

impl TranslationUnit {
    /// Returns the root cursor of this live translation unit.
    pub(crate) fn root(&self) -> CXCursor {
        // SAFETY: self owns a live translation unit until Drop.
        unsafe { clang_sys::clang_getTranslationUnitCursor(self.unit) }
    }

    pub(crate) fn cursor_kind(cursor: CXCursor) -> clang_sys::CXCursorKind {
        // SAFETY: cursor was supplied by this live translation unit.
        unsafe { clang_sys::clang_getCursorKind(cursor) }
    }

    /// Returns the canonical declaration cursor's USR identity for declaration deduplication.
    pub(crate) fn canonical_identity(cursor: CXCursor) -> Option<SymbolIdentity> {
        // SAFETY: cursor was supplied by this live translation unit.
        let canonical = unsafe { clang_sys::clang_getCanonicalCursor(cursor) };
        Self::cursor_identity(canonical)
    }

    /// Returns whether libclang supplied an actual spelling for this cursor.
    pub(crate) fn cursor_spelling_is_empty(cursor: CXCursor) -> bool {
        // SAFETY: cursor was supplied by this live translation unit; the CXString is disposed
        // exactly once below.
        let spelling = unsafe { clang_sys::clang_getCursorSpelling(cursor) };
        let pointer = unsafe { clang_sys::clang_getCString(spelling) };
        let empty = pointer.is_null() || unsafe { CStr::from_ptr(pointer) }.to_bytes().is_empty();
        // SAFETY: spelling is owned by this function and has not been disposed before this point.
        unsafe { clang_sys::clang_disposeString(spelling) };
        empty
    }

    /// Returns the template declaration kind associated with a cursor, if libclang exposes one.
    pub(crate) fn template_cursor_kind(cursor: CXCursor) -> clang_sys::CXCursorKind {
        // SAFETY: cursor was supplied by this live translation unit.
        unsafe { clang_sys::clang_getTemplateCursorKind(cursor) }
    }

    pub(crate) fn is_null_cursor(cursor: CXCursor) -> bool {
        // SAFETY: cursor is a value returned by this live libclang invocation.
        unsafe { clang_sys::clang_Cursor_isNull(cursor) != 0 }
    }

    pub(crate) fn is_definition(cursor: CXCursor) -> bool {
        // SAFETY: cursor was supplied by this live translation unit.
        unsafe { clang_sys::clang_isCursorDefinition(cursor) != 0 }
    }

    pub(crate) fn is_declaration(kind: clang_sys::CXCursorKind) -> bool {
        // SAFETY: kind was returned by this loaded libclang instance.
        unsafe { clang_sys::clang_isDeclaration(kind) != 0 }
    }

    /// Visits every child cursor with the exact caller-owned callback state pointer.
    pub(crate) fn visit(
        &self,
        visitor: extern "C" fn(CXCursor, CXCursor, *mut c_void) -> CXChildVisitResult,
        state: *mut c_void,
    ) {
        // SAFETY: self owns the unit; visitor and state remain live for this synchronous call.
        unsafe { clang_sys::clang_visitChildren(self.root(), visitor, state) };
    }

    /// Maps a cursor's main-file extent to an exact canonical source span.
    pub(crate) fn cursor_span(&self, cursor: CXCursor) -> Result<Option<SourceSpan>, CollectError> {
        // SAFETY: cursor was supplied by this translation unit during a synchronous traversal.
        let range = unsafe { clang_sys::clang_getCursorExtent(cursor) };
        self.range_span(range)
    }

    /// Maps a native range to an exact main-source span when its endpoints share the main file.
    pub(crate) fn range_span(
        &self,
        range: CXSourceRange,
    ) -> Result<Option<SourceSpan>, CollectError> {
        // SAFETY: range was obtained from this translation unit; both calls are pure native reads.
        let start = unsafe { clang_sys::clang_getRangeStart(range) };
        // SAFETY: range was obtained from this translation unit; both calls are pure native reads.
        let end = unsafe { clang_sys::clang_getRangeEnd(range) };
        self.locations_span(start, end)
    }

    /// Maps a native location to a zero-width main-source span when it belongs to this source.
    pub(crate) fn location_span(
        &self,
        location: CXSourceLocation,
    ) -> Result<Option<SourceSpan>, CollectError> {
        self.locations_span(location, location)
    }

    /// Returns a cursor USR as a domain-separated identity without retaining a native string.
    pub(crate) fn cursor_identity(cursor: CXCursor) -> Option<SymbolIdentity> {
        // SAFETY: cursor was supplied by this live translation unit; the returned CXString is
        // disposed exactly once by native_identity below.
        let usr = unsafe { clang_sys::clang_getCursorUSR(cursor) };
        native_identity(usr, b"nudox.clang.usr.v1")
    }

    /// Returns the semantic-parent identity when libclang can resolve it.
    pub(crate) fn semantic_parent(cursor: CXCursor) -> Option<SymbolIdentity> {
        // SAFETY: cursor was supplied by this live translation unit.
        let parent = unsafe { clang_sys::clang_getCursorSemanticParent(cursor) };
        Self::cursor_identity(parent)
    }

    /// Returns a raw documentation range for a cursor without retaining any libclang memory.
    pub(crate) fn documentation_span(
        &self,
        cursor: CXCursor,
    ) -> Result<Option<SourceSpan>, CollectError> {
        // SAFETY: cursor was supplied by this live translation unit.
        let range = unsafe { clang_sys::clang_Cursor_getCommentRange(cursor) };
        self.range_span(range)
    }

    pub(crate) fn name_span(&self, cursor: CXCursor) -> Result<Option<SourceSpan>, CollectError> {
        // SAFETY: cursor was supplied by this live translation unit; the named option constant
        // requests its first spelling range without template or qualifier expansion.
        let range = unsafe {
            clang_sys::clang_Cursor_getSpellingNameRange(
                cursor,
                SPELLING_NAME_FIRST_PIECE,
                SPELLING_NAME_OPTIONS_NONE,
            )
        };
        self.range_span(range)
    }

    /// Returns the direct referenced cursor for a native use cursor.
    pub(crate) fn referenced(cursor: CXCursor) -> CXCursor {
        // SAFETY: cursor was supplied by this live translation unit.
        unsafe { clang_sys::clang_getCursorReferenced(cursor) }
    }

    pub(crate) fn method_virtuality(cursor: CXCursor) -> (bool, bool) {
        // SAFETY: cursor was supplied by this live translation unit.
        let virtual_ = unsafe { clang_sys::clang_CXXMethod_isVirtual(cursor) != 0 };
        // SAFETY: cursor was supplied by this live translation unit.
        let pure = unsafe { clang_sys::clang_CXXMethod_isPureVirtual(cursor) != 0 };
        (virtual_, pure)
    }

    pub(crate) fn overridden_cursors(cursor: CXCursor) -> OverriddenCursors {
        let mut cursors = ptr::null_mut();
        let mut count = 0;
        // SAFETY: output cells are local, and cursor belongs to this live translation unit.
        unsafe {
            clang_sys::clang_getOverriddenCursors(cursor, &raw mut cursors, &raw mut count);
        }
        OverriddenCursors { cursors, count }
    }

    /// Returns the direct native type attached to a declaration cursor.
    pub(crate) fn cursor_type(cursor: CXCursor) -> CXType {
        // SAFETY: cursor was supplied by this live translation unit.
        unsafe { clang_sys::clang_getCursorType(cursor) }
    }

    pub(crate) const fn type_kind(type_: CXType) -> clang_sys::CXTypeKind {
        type_.kind
    }

    pub(crate) fn type_qualifiers(type_: CXType) -> (bool, bool, bool) {
        // SAFETY: type_ was obtained from this live translation unit.
        let is_const = unsafe { clang_sys::clang_isConstQualifiedType(type_) != 0 };
        // SAFETY: type_ was obtained from this live translation unit.
        let is_volatile = unsafe { clang_sys::clang_isVolatileQualifiedType(type_) != 0 };
        // SAFETY: type_ was obtained from this live translation unit.
        let is_restrict = unsafe { clang_sys::clang_isRestrictQualifiedType(type_) != 0 };
        (is_const, is_volatile, is_restrict)
    }

    /// Returns the cursor's directly classified storage binding.
    pub(crate) fn storage_class(cursor: CXCursor) -> clang_sys::CX_StorageClass {
        // SAFETY: cursor was supplied by this live translation unit.
        unsafe { clang_sys::clang_Cursor_getStorageClass(cursor) }
    }

    /// Returns the type's byte size as reported by libclang, where negative
    /// values are libclang's incomplete, dependent, or invalid layout errors.
    pub(crate) fn type_size_of(type_: CXType) -> i64 {
        // SAFETY: type_ was obtained from this live translation unit.
        unsafe { clang_sys::clang_Type_getSizeOf(type_) }
    }

    /// Returns the type's byte alignment under the same negative-error law.
    pub(crate) fn type_align_of(type_: CXType) -> i64 {
        // SAFETY: type_ was obtained from this live translation unit.
        unsafe { clang_sys::clang_Type_getAlignOf(type_) }
    }

    pub(crate) fn type_declaration(type_: CXType) -> Option<SymbolIdentity> {
        // SAFETY: type_ was obtained from this live translation unit.
        let declaration = unsafe { clang_sys::clang_getTypeDeclaration(type_) };
        Self::cursor_identity(declaration)
    }

    pub(crate) fn pointee_type(type_: CXType) -> CXType {
        // SAFETY: type_ was obtained from this live translation unit.
        unsafe { clang_sys::clang_getPointeeType(type_) }
    }

    pub(crate) fn array_element_type(type_: CXType) -> CXType {
        // SAFETY: type_ was obtained from this live translation unit.
        unsafe { clang_sys::clang_getArrayElementType(type_) }
    }

    pub(crate) fn array_len(type_: CXType) -> Option<u64> {
        // SAFETY: type_ was obtained from this live translation unit.
        let length = unsafe { clang_sys::clang_getArraySize(type_) };
        u64::try_from(length).ok()
    }

    pub(crate) fn function_result_type(type_: CXType) -> CXType {
        // SAFETY: type_ was obtained from this live translation unit.
        unsafe { clang_sys::clang_getResultType(type_) }
    }

    pub(crate) fn function_argument_count(type_: CXType) -> Option<u32> {
        // SAFETY: type_ was obtained from this live translation unit.
        let count = unsafe { clang_sys::clang_getNumArgTypes(type_) };
        u32::try_from(count).ok()
    }

    pub(crate) fn function_argument_type(type_: CXType, index: u32) -> CXType {
        // SAFETY: type_ was obtained from this live translation unit and index is bounded by its
        // immediately preceding function_argument_count result.
        unsafe { clang_sys::clang_getArgType(type_, index) }
    }

    pub(crate) fn template_argument_count(type_: CXType) -> Option<u32> {
        // SAFETY: type_ was obtained from this live translation unit.
        let count = unsafe { clang_sys::clang_Type_getNumTemplateArguments(type_) };
        u32::try_from(count).ok()
    }

    pub(crate) fn template_argument_type(type_: CXType, index: u32) -> CXType {
        // SAFETY: type_ was obtained from this live translation unit and index is bounded by its
        // immediately preceding template_argument_count result.
        unsafe { clang_sys::clang_Type_getTemplateArgumentAsType(type_, index) }
    }

    /// Returns whether a cursor's extent belongs to the caller's main source authority.
    pub(crate) fn is_local(&self, cursor: CXCursor) -> Result<bool, CollectError> {
        Ok(self.cursor_span(cursor)?.is_some())
    }

    /// Streams native diagnostics to a typed closure while every handle remains valid.
    pub(crate) fn diagnostics(
        &self,
        mut receive: impl FnMut(CXDiagnostic) -> Result<(), CollectError>,
    ) -> Result<(), CollectError> {
        // SAFETY: self owns the translation unit for the complete enumeration.
        let count = unsafe { clang_sys::clang_getNumDiagnostics(self.unit) };
        for index in 0..count {
            // SAFETY: index is strictly below libclang's reported diagnostic count.
            let diagnostic = unsafe { clang_sys::clang_getDiagnostic(self.unit, index) };
            receive(diagnostic)?;
            // SAFETY: libclang returned this diagnostic exactly once for this enumeration.
            unsafe { clang_sys::clang_disposeDiagnostic(diagnostic) };
        }
        Ok(())
    }

    /// Returns one native diagnostic's stable spelling identity without retaining its text.
    pub(crate) fn diagnostic_identity(diagnostic: CXDiagnostic) -> Option<SymbolIdentity> {
        // SAFETY: diagnostic remains live for the caller's diagnostics closure.
        let spelling = unsafe { clang_sys::clang_getDiagnosticSpelling(diagnostic) };
        native_identity(spelling, b"nudox.clang.diagnostic.v1")
    }

    pub(crate) fn diagnostic_severity(diagnostic: CXDiagnostic) -> clang_sys::CXDiagnosticSeverity {
        // SAFETY: diagnostic remains live for the caller's diagnostics closure.
        unsafe { clang_sys::clang_getDiagnosticSeverity(diagnostic) }
    }

    pub(crate) fn diagnostic_location(diagnostic: CXDiagnostic) -> CXSourceLocation {
        // SAFETY: diagnostic remains live for the caller's diagnostics closure.
        unsafe { clang_sys::clang_getDiagnosticLocation(diagnostic) }
    }

    pub(crate) fn diagnostic_category(diagnostic: CXDiagnostic) -> u32 {
        // SAFETY: diagnostic remains live for the caller's diagnostics closure.
        unsafe { clang_sys::clang_getDiagnosticCategory(diagnostic) }
    }

    /// Returns the opaque identity of an include file resolved by libclang.
    pub(crate) fn included_file_identity(cursor: CXCursor) -> Option<SymbolIdentity> {
        // SAFETY: cursor was supplied by this live translation unit.
        let file = unsafe { clang_sys::clang_getIncludedFile(cursor) };
        if file.is_null() {
            return None;
        }
        // SAFETY: file belongs to this live translation unit and its name string is disposed by
        // native_identity exactly once.
        let name = unsafe { clang_sys::clang_getFileName(file) };
        native_identity(name, b"nudox.clang.include.v1")
    }

    pub(crate) fn imported_module_identity(cursor: CXCursor) -> Option<SymbolIdentity> {
        // SAFETY: cursor was supplied by this live translation unit.
        let module = unsafe { clang_sys::clang_Cursor_getModule(cursor) };
        if module.is_null() {
            return None;
        }
        // SAFETY: module belongs to this live translation unit and its temporary full-name string
        // is disposed exactly once by native_identity.
        let name = unsafe { clang_sys::clang_Module_getFullName(module) };
        native_identity(name, b"nudox.clang.module.v1")
    }

    /// Narrows two locations to one main-file source span.
    fn locations_span(
        &self,
        start: CXSourceLocation,
        end: CXSourceLocation,
    ) -> Result<Option<SourceSpan>, CollectError> {
        let Some(start) = Self::location(start) else {
            return Ok(None);
        };
        let Some(end) = Self::location(end) else {
            return Ok(None);
        };
        if !same_file(start.file, end.file)
            || !same_file(start.file, self.main_file)
            || start.offset > end.offset
            || start.offset > self.source_len
            || end.offset > self.source_len
        {
            return Ok(None);
        }
        let span = NativeSpan {
            start: u64::from(start.offset),
            end: u64::from(end.offset),
        };
        let start = u32::try_from(span.start).map_err(|_| CollectError::CoordinateTooLarge {
            coordinate: span.start,
        })?;
        let end = u32::try_from(span.end).map_err(|_| CollectError::CoordinateTooLarge {
            coordinate: span.end,
        })?;
        Ok(Some(SourceSpan { start, end }))
    }

    /// Extracts one native expansion-location file and byte offset.
    fn location(location: CXSourceLocation) -> Option<NativeLocation> {
        let mut file = ptr::null_mut();
        let mut line = 0;
        let mut column = 0;
        let mut offset = 0;
        // SAFETY: all output pointers are initialized local cells and location came from this TU.
        unsafe {
            clang_sys::clang_getExpansionLocation(
                location,
                &raw mut file,
                &raw mut line,
                &raw mut column,
                &raw mut offset,
            );
        }
        (!file.is_null()).then_some(NativeLocation { file, offset })
    }
}

/// Owns libclang's returned override array until exactly one native disposal.
pub(crate) struct OverriddenCursors {
    cursors: *mut CXCursor,
    count: c_uint,
}

impl OverriddenCursors {
    pub(crate) fn as_slice(&self) -> &[CXCursor] {
        if self.cursors.is_null() {
            return &[];
        }
        // SAFETY: libclang returned `count` contiguous cursors owned by this guard.
        unsafe { core::slice::from_raw_parts(self.cursors, self.count as usize) }
    }
}

impl Drop for OverriddenCursors {
    fn drop(&mut self) {
        if !self.cursors.is_null() {
            // SAFETY: this guard uniquely owns the array returned by libclang.
            unsafe { clang_sys::clang_disposeOverriddenCursors(self.cursors) };
        }
    }
}

pub(crate) fn callback_state<State, Output>(
    data: *mut c_void,
    with: impl FnOnce(&mut State) -> Output,
) -> Option<Output> {
    // SAFETY: collect passes one exclusive pointer to its live State for the synchronous native
    // visit; libclang does not retain callback data after clang_visitChildren returns.
    let state = unsafe { data.cast::<State>().as_mut() };
    state.map(with)
}

impl Drop for TranslationUnit {
    /// Releases the native unit before its owning index in the only valid libclang order.
    fn drop(&mut self) {
        // SAFETY: both handles were created by this object and are disposed exactly once here.
        unsafe { clang_sys::clang_disposeTranslationUnit(self.unit) };
        // SAFETY: the unit is already released, and the index is still owned by this object.
        unsafe { clang_sys::clang_disposeIndex(self.index) };
    }
}

/// A native source file and byte offset extracted from one libclang location.
struct NativeLocation {
    file: CXFile,
    offset: c_uint,
}

/// The precise native API groups that this collector requires for complete facts.
const REQUIRED_APIS: [RequiredApi; 8] = [
    RequiredApi::TranslationUnit,
    RequiredApi::Traversal,
    RequiredApi::Locations,
    RequiredApi::Identities,
    RequiredApi::Types,
    RequiredApi::Documentation,
    RequiredApi::Diagnostics,
    RequiredApi::Includes,
];

/// Requests libclang's first, unexpanded cursor spelling range.
const SPELLING_NAME_FIRST_PIECE: c_uint = 0;
/// Declares no libclang spelling-range expansion options.
const SPELLING_NAME_OPTIONS_NONE: c_uint = 0;
/// Keeps declarations from the preamble in the direct native cursor graph.
const EXCLUDE_DECLARATIONS_FROM_PREAMBLE: c_int = 0;
/// Suppresses libclang's direct stderr diagnostics; typed facts capture diagnostics instead.
const HIDE_NATIVE_DIAGNOSTICS: c_int = 0;
/// Supplies exactly one unsaved source authority to the native translation-unit constructor.
const ONE_UNSAVED_SOURCE: c_uint = 1;

/// A compact check for a required group of dynamically loaded libclang symbols.
#[derive(Clone, Copy)]
enum RequiredApi {
    /// Translation-unit construction and destruction.
    TranslationUnit,
    /// Cursor-tree traversal.
    Traversal,
    /// Location and file comparison APIs.
    Locations,
    /// USR and reference APIs.
    Identities,
    /// Recursive type APIs.
    Types,
    /// Documentation comment APIs.
    Documentation,
    /// Diagnostic APIs.
    Diagnostics,
    /// Include directive APIs.
    Includes,
}

impl RequiredApi {
    /// Returns the public typed API family associated with this internal symbol group.
    const fn kind(self) -> NativeApi {
        match self {
            Self::TranslationUnit => NativeApi::TranslationUnit,
            Self::Traversal => NativeApi::Traversal,
            Self::Locations => NativeApi::Locations,
            Self::Identities => NativeApi::Identities,
            Self::Types => NativeApi::Types,
            Self::Documentation => NativeApi::Documentation,
            Self::Diagnostics => NativeApi::Diagnostics,
            Self::Includes => NativeApi::Includes,
        }
    }

    /// Checks every exact dynamically loaded native symbol in this API family.
    fn is_loaded(self) -> bool {
        match self {
            Self::TranslationUnit => {
                clang_sys::clang_createIndex::is_loaded()
                    && clang_sys::clang_disposeIndex::is_loaded()
                    && clang_sys::clang_parseTranslationUnit2::is_loaded()
                    && clang_sys::clang_disposeTranslationUnit::is_loaded()
                    && clang_sys::clang_getTranslationUnitCursor::is_loaded()
                    && clang_sys::clang_getFile::is_loaded()
            }
            Self::Traversal => clang_sys::clang_visitChildren::is_loaded(),
            Self::Locations => {
                clang_sys::clang_getCursorExtent::is_loaded()
                    && clang_sys::clang_getRangeStart::is_loaded()
                    && clang_sys::clang_getRangeEnd::is_loaded()
                    && clang_sys::clang_getExpansionLocation::is_loaded()
                    && clang_sys::clang_File_isEqual::is_loaded()
            }
            Self::Identities => {
                clang_sys::clang_Cursor_isNull::is_loaded()
                    && clang_sys::clang_getCursorKind::is_loaded()
                    && clang_sys::clang_isCursorDefinition::is_loaded()
                    && clang_sys::clang_isDeclaration::is_loaded()
                    && clang_sys::clang_getCursorUSR::is_loaded()
                    && clang_sys::clang_getCanonicalCursor::is_loaded()
                    && clang_sys::clang_getCursorSpelling::is_loaded()
                    && clang_sys::clang_getTemplateCursorKind::is_loaded()
                    && clang_sys::clang_Cursor_getSpellingNameRange::is_loaded()
                    && clang_sys::clang_getCursorSemanticParent::is_loaded()
                    && clang_sys::clang_getCursorReferenced::is_loaded()
                    && clang_sys::clang_CXXMethod_isVirtual::is_loaded()
                    && clang_sys::clang_CXXMethod_isPureVirtual::is_loaded()
                    && clang_sys::clang_getOverriddenCursors::is_loaded()
                    && clang_sys::clang_disposeOverriddenCursors::is_loaded()
                    && clang_sys::clang_Cursor_getStorageClass::is_loaded()
                    && clang_sys::clang_getCString::is_loaded()
                    && clang_sys::clang_disposeString::is_loaded()
            }
            Self::Types => {
                clang_sys::clang_getCursorType::is_loaded()
                    && clang_sys::clang_getTypeDeclaration::is_loaded()
                    && clang_sys::clang_isConstQualifiedType::is_loaded()
                    && clang_sys::clang_isVolatileQualifiedType::is_loaded()
                    && clang_sys::clang_isRestrictQualifiedType::is_loaded()
                    && clang_sys::clang_getPointeeType::is_loaded()
                    && clang_sys::clang_getArrayElementType::is_loaded()
                    && clang_sys::clang_getArraySize::is_loaded()
                    && clang_sys::clang_getResultType::is_loaded()
                    && clang_sys::clang_getNumArgTypes::is_loaded()
                    && clang_sys::clang_getArgType::is_loaded()
                    && clang_sys::clang_Type_getNumTemplateArguments::is_loaded()
                    && clang_sys::clang_Type_getTemplateArgumentAsType::is_loaded()
                    && clang_sys::clang_Type_getSizeOf::is_loaded()
                    && clang_sys::clang_Type_getAlignOf::is_loaded()
            }
            Self::Documentation => clang_sys::clang_Cursor_getCommentRange::is_loaded(),
            Self::Diagnostics => {
                clang_sys::clang_getNumDiagnostics::is_loaded()
                    && clang_sys::clang_getDiagnostic::is_loaded()
                    && clang_sys::clang_disposeDiagnostic::is_loaded()
                    && clang_sys::clang_getDiagnosticLocation::is_loaded()
                    && clang_sys::clang_getDiagnosticSeverity::is_loaded()
                    && clang_sys::clang_getDiagnosticCategory::is_loaded()
                    && clang_sys::clang_getDiagnosticSpelling::is_loaded()
            }
            Self::Includes => {
                clang_sys::clang_getIncludedFile::is_loaded()
                    && clang_sys::clang_getFileName::is_loaded()
                    && clang_sys::clang_Cursor_getModule::is_loaded()
                    && clang_sys::clang_Module_getFullName::is_loaded()
            }
        }
    }
}

/// Produces the small fixed native command vector implied by one typed profile.
fn parse_arguments(
    input: ClangInput<'_>,
) -> Result<([*const c_char; crate::MAX_DATABASE_ARGUMENTS + 2], usize), CollectError> {
    let mut arguments = [ptr::null(); crate::MAX_DATABASE_ARGUMENTS + 2];
    let values = if let Some(values) = input.database_arguments() {
        values.get(1..).ok_or(CollectError::Parse {
            failure: ParseFailure::InvalidArguments,
        })?
    } else {
        &[
            c"-x",
            input.dialect_argument(),
            input.standard_argument(),
            c"-fparse-all-comments",
            c"-ferror-limit=0",
        ]
    };
    let extra = usize::from(input.database_working_directory().is_some()) * 2;
    if values
        .len()
        .checked_add(extra)
        .is_none_or(|len| len > arguments.len())
    {
        return Err(CollectError::ScratchCapacity {
            lane: crate::ScratchLane::Arguments,
            capacity: arguments.len(),
            required: values.len(),
        });
    }
    let mut count = 0;
    if let Some(directory) = input.database_working_directory() {
        arguments[count] = c"-working-directory".as_ptr();
        arguments[count + 1] = directory.as_ptr();
        count += 2;
    }
    // Database callers must provide the true `file` cell.  libclang's database view can
    // report the output cell after `-o` as the file name, so identity is removed by exact
    // value rather than by position.
    let file_name = input.file_name().to_bytes();
    for value in values {
        if value.to_bytes() == file_name {
            continue;
        }
        if count == arguments.len() {
            return Err(CollectError::ScratchCapacity {
                lane: crate::ScratchLane::Arguments,
                capacity: arguments.len(),
                required: count + 1,
            });
        }
        arguments[count] = value.as_ptr();
        count += 1;
    }
    Ok((arguments, count))
}

/// Resolves a database-relative source path using the database command's explicit directory.
fn resolved_file_name(input: ClangInput<'_>) -> Result<CString, CollectError> {
    let Some(directory) = input.database_working_directory() else {
        return Ok(input.file_name().to_owned());
    };
    let directory = std::str::from_utf8(directory.to_bytes()).map_err(|_| CollectError::Parse {
        failure: ParseFailure::InvalidArguments,
    })?;
    let file_name =
        std::str::from_utf8(input.file_name().to_bytes()).map_err(|_| CollectError::Parse {
            failure: ParseFailure::InvalidArguments,
        })?;
    let path = Path::new(file_name);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        Path::new(directory).join(path)
    };
    CString::new(path.to_string_lossy().as_bytes()).map_err(|_| CollectError::Parse {
        failure: ParseFailure::InvalidArguments,
    })
}

/// Maps libclang's closed parse status to the public typed error without discarding unknown codes.
const fn parse_failure(status: CXErrorCode) -> ParseFailure {
    match status {
        clang_sys::CXError_Failure | clang_sys::CXError_Success => ParseFailure::Failure,
        clang_sys::CXError_Crashed => ParseFailure::Crashed,
        clang_sys::CXError_InvalidArguments => ParseFailure::InvalidArguments,
        clang_sys::CXError_ASTReadError => ParseFailure::AstRead,
        other => ParseFailure::Unknown { raw: other },
    }
}

/// Compares native file handles without requiring a path-string allocation or comparison.
fn same_file(left: CXFile, right: CXFile) -> bool {
    if left.is_null() || right.is_null() {
        return false;
    }
    // SAFETY: both file handles were obtained from the same live translation unit.
    unsafe { clang_sys::clang_File_isEqual(left, right) != 0 }
}

/// Hashes one transient native `CXString` and disposes it exactly once before returning.
fn native_identity(string: CXString, domain: &[u8]) -> Option<SymbolIdentity> {
    // SAFETY: a CXString may be queried until it is disposed exactly once at function end.
    let pointer = unsafe { clang_sys::clang_getCString(string) };
    let identity = if pointer.is_null() {
        None
    } else {
        // SAFETY: libclang documents live CXString bytes as NUL-terminated until disposal.
        let bytes = unsafe { CStr::from_ptr(pointer) }.to_bytes();
        (!bytes.is_empty()).then(|| SymbolIdentity::from_native(domain, bytes))
    };
    // SAFETY: string is owned by this function and has not been disposed before this point.
    unsafe { clang_sys::clang_disposeString(string) };
    identity
}

fn native_text(string: CXString) -> Result<String, DatabaseError> {
    let pointer = unsafe { clang_sys::clang_getCString(string) };
    let result = if pointer.is_null() {
        Err(DatabaseError::InvalidUtf8)
    } else {
        let bytes = unsafe { CStr::from_ptr(pointer) }.to_bytes();
        std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| DatabaseError::InvalidUtf8)
    };
    unsafe { clang_sys::clang_disposeString(string) };
    result
}

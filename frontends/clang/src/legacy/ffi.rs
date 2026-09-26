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

use crate::legacy::{
    CollectError, CompilationCommand, CompilationDatabase, DatabaseError, NativeApi, NativeFailure,
    ParseFailure,
    facts::{SourceSpan, SymbolIdentity},
    input::ClangInput,
};

mod unit;

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
        if count > crate::legacy::MAX_DATABASE_ARGUMENTS {
            unsafe { clang_sys::clang_CompileCommands_dispose(commands) };
            unsafe { clang_sys::clang_CompilationDatabase_dispose(database) };
            return Err(DatabaseError::ArgumentCapacity {
                required: count,
                capacity: crate::legacy::MAX_DATABASE_ARGUMENTS,
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
    /// Compilation-database working directory used as the package root for
    /// cross-file identities. Absent for synthetic profiles without a database.
    compile_root: Option<CString>,
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
            compile_root: input
                .database_working_directory()
                .map(|directory| directory.to_owned()),
        })
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
/// Requests a reference name as one contiguous piece: the whole written
/// spelling of the referenced entity, never a split across pieces.
const REFERENCE_NAME_SINGLE_PIECE: clang_sys::CXNameRefFlags =
    clang_sys::CXNameRange_WantSinglePiece;
/// Under the single-piece flag the one returned piece is piece zero.
const REFERENCE_NAME_PIECE_ZERO: c_uint = 0;
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
                    && clang_sys::clang_getCursorReferenceNameRange::is_loaded()
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
                    && clang_sys::clang_getEnumDeclIntegerType::is_loaded()
                    && clang_sys::clang_getCanonicalType::is_loaded()
                    && clang_sys::clang_getTypeDeclaration::is_loaded()
                    && clang_sys::clang_isConstQualifiedType::is_loaded()
                    && clang_sys::clang_isVolatileQualifiedType::is_loaded()
                    && clang_sys::clang_isRestrictQualifiedType::is_loaded()
                    && clang_sys::clang_getPointeeType::is_loaded()
                    && clang_sys::clang_Type_getClassType::is_loaded()
                    && clang_sys::clang_getArrayElementType::is_loaded()
                    && clang_sys::clang_getArraySize::is_loaded()
                    && clang_sys::clang_getResultType::is_loaded()
                    && clang_sys::clang_getNumArgTypes::is_loaded()
                    && clang_sys::clang_getArgType::is_loaded()
                    && clang_sys::clang_isFunctionTypeVariadic::is_loaded()
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
) -> Result<
    (
        [*const c_char; crate::legacy::MAX_DATABASE_ARGUMENTS + 2],
        usize,
    ),
    CollectError,
> {
    let mut arguments = [ptr::null(); crate::legacy::MAX_DATABASE_ARGUMENTS + 2];
    let values = if let Some(values) = input.database_arguments() {
        if values.is_empty() {
            return Err(CollectError::Parse {
                failure: ParseFailure::InvalidArguments,
            });
        }
        // Only a leading compiler executable is positional. A real argv[0] is
        // a program path and can never begin with `-`, while every flag does,
        // so a prepared flag-first vector — system include arguments and
        // per-extension defaults, exactly what `ClangProject::arguments`
        // supplies — must survive intact. Dropping the first element
        // unconditionally ate the leading `-isystem` flag, turned its include
        // directory into a stray positional input, and shifted every
        // following flag one slot left.
        match values.first() {
            Some(first) if !first.to_bytes().starts_with(b"-") => &values[1..],
            _ => values,
        }
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
            lane: crate::legacy::ScratchLane::Arguments,
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
                lane: crate::legacy::ScratchLane::Arguments,
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

/// Hashes a native file name made relative to the compilation root, or `None`
/// when the file lives outside that root. The path is never lossily converted:
/// a non-UTF-8 native name simply yields no identity.
fn native_relative_identity(
    string: CXString,
    root: &[u8],
    domain: &[u8],
) -> Option<SymbolIdentity> {
    let pointer = unsafe { clang_sys::clang_getCString(string) };
    let identity = if pointer.is_null() {
        None
    } else {
        // SAFETY: libclang documents live CXString bytes as NUL-terminated until disposal.
        let bytes = unsafe { CStr::from_ptr(pointer) }.to_bytes();
        relative_path(bytes, root).and_then(|relative| {
            (!relative.is_empty()).then(|| SymbolIdentity::from_native(domain, relative))
        })
    };
    // SAFETY: string is owned by this function and has not been disposed before this point.
    unsafe { clang_sys::clang_disposeString(string) };
    identity
}

/// Strips one compilation-root prefix at a path-separator boundary, returning
/// the package-relative tail.
///
/// The root is normalized by trimming every trailing separator, so a
/// compilation database whose `directory` carries a trailing slash
/// (`/work/pkg/`) still yields the same tail as the separator-free spelling.
/// A path that is not a true separator-boundary descendant of the root — for
/// example a source referencing a sibling of a non-ancestor build directory —
/// yields `None` rather than a fabricated or partial tail.
fn relative_path<'bytes>(path: &'bytes [u8], root: &[u8]) -> Option<&'bytes [u8]> {
    let root = trim_trailing_separators(root);
    if root.is_empty() {
        return None;
    }
    let relative = path.strip_prefix(root)?;
    let first = *relative.first()?;
    if first != b'/' && first != b'\\' {
        return None;
    }
    let relative = trim_leading_separators(relative);
    (!relative.is_empty()).then_some(relative)
}

/// Trims every leading `/` or `\` from one native path tail.
fn trim_leading_separators(mut bytes: &[u8]) -> &[u8] {
    while let Some((first, rest)) = bytes.split_first() {
        if *first == b'/' || *first == b'\\' {
            bytes = rest;
        } else {
            break;
        }
    }
    bytes
}

/// Trims every trailing `/` or `\` from one compilation root.
fn trim_trailing_separators(mut bytes: &[u8]) -> &[u8] {
    while let Some((last, rest)) = bytes.split_last() {
        if *last == b'/' || *last == b'\\' {
            bytes = rest;
        } else {
            break;
        }
    }
    bytes
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

#[cfg(test)]
mod tests {
    use super::relative_path;

    #[test]
    fn relative_path_strips_only_a_boundary_prefixed_root() {
        assert_eq!(
            relative_path(b"/work/pkg/src/main.c", b"/work/pkg"),
            Some(&b"src/main.c"[..])
        );
        assert_eq!(relative_path(b"/work/pkg", b"/work/pkg"), None);
        assert_eq!(relative_path(b"/work/pkgx/src.c", b"/work/pkg"), None);
        assert_eq!(relative_path(b"/nix/store/hash/src.c", b"/work/pkg"), None);
        assert_eq!(relative_path(b"/work/pkg/src.c", b""), None);
    }

    #[test]
    fn relative_path_normalizes_a_trailing_separator_root() {
        assert_eq!(
            relative_path(b"/work/pkg/src/main.c", b"/work/pkg/"),
            Some(&b"src/main.c"[..])
        );
        assert_eq!(
            relative_path(b"/work/pkg/src/main.c", b"/work/pkg//"),
            Some(&b"src/main.c"[..])
        );
        assert_eq!(relative_path(b"/work/pkg", b"/work/pkg/"), None);
    }

    #[test]
    fn relative_path_rejects_a_non_ancestor_build_directory() {
        // A compilation database `directory` may be a build directory that is
        // not an ancestor of the referenced source. No partial tail may be
        // fabricated from the shared bytes.
        assert_eq!(
            relative_path(b"/work/pkg/src/x.c", b"/work/pkg/build"),
            None
        );
        assert_eq!(relative_path(b"/work/other/x.c", b"/work/pkg/build"), None);
        assert_eq!(
            relative_path(b"/work/pkg/build/x.c", b"/work/pkg/build"),
            Some(&b"x.c"[..])
        );
        assert_eq!(
            relative_path(b"/work/pkg/build/src/x.c", b"/work/pkg/build"),
            Some(&b"src/x.c"[..])
        );
    }
}

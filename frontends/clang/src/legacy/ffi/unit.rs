//! Reads cursors, types, and source spans from one live libclang translation unit.

use super::*;

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

    /// Whether the lexically enclosing declaration of a cursor is a
    /// definition. Parameter cursors ask their visiting declaration — the
    /// prototype or definition being walked — rather than the committed
    /// owner state, so definition-before-prototype and duplicate-prototype
    /// orders attribute identically.
    pub(crate) fn lexical_parent_is_definition(cursor: CXCursor) -> bool {
        // SAFETY: cursor was supplied by this live translation unit.
        let parent = unsafe { clang_sys::clang_getCursorLexicalParent(cursor) };
        Self::is_definition(parent)
    }

    /// The extent of the lexically enclosing declaration, when it maps to
    /// the main source file. Distinct visiting declarations have distinct
    /// extents, so this separates prototype from definition parameter sets
    /// even when they share an owner identity and flag.
    pub(crate) fn lexical_parent_span(
        &self,
        cursor: CXCursor,
    ) -> Result<Option<SourceSpan>, CollectError> {
        // SAFETY: cursor was supplied by this live translation unit.
        let parent = unsafe { clang_sys::clang_getCursorLexicalParent(cursor) };
        self.cursor_span(parent)
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

    /// Maps a cursor's reference-name range to an exact main-source span.
    ///
    /// For a use cursor this is the written spelling of the referenced entity
    /// at the site — the member token of a member access, the identifier of a
    /// declaration reference — without nested-name qualifiers or template
    /// arguments, requested as one contiguous piece. A call expression
    /// degenerates: libclang returns the whole expression extent as its
    /// reference-name range, so the caller treats a range identical to the
    /// use extent as unusable and reads the cursor's spelling-name range
    /// (exactly the callee token) instead. A cursor class with no written
    /// name in the main source yields libclang's zero-length range, which
    /// maps to `None`; the caller's final fallback keeps the whole use
    /// extent so no reference is dropped by the narrowing.
    pub(crate) fn reference_name_span(
        &self,
        cursor: CXCursor,
    ) -> Result<Option<SourceSpan>, CollectError> {
        // SAFETY: cursor was supplied by this live translation unit; the flag
        // requests the name as one contiguous piece, and piece zero is the
        // whole piece under that flag.
        let range = unsafe {
            clang_sys::clang_getCursorReferenceNameRange(
                cursor,
                REFERENCE_NAME_SINGLE_PIECE,
                REFERENCE_NAME_PIECE_ZERO,
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

    /// Returns libclang's exact enumeration underlying integer type. An
    /// invalid result remains invalid and is rejected by the collector's
    /// normal recursive type admission rather than guessed from enum size.
    pub(crate) fn enum_underlying_type(cursor: CXCursor) -> CXType {
        // SAFETY: cursor was supplied by this live translation unit.
        unsafe { clang_sys::clang_getEnumDeclIntegerType(cursor) }
    }

    pub(crate) const fn type_kind(type_: CXType) -> clang_sys::CXTypeKind {
        type_.kind
    }

    /// Returns the native canonical type. This is used only to retain the
    /// signedness authority hidden behind a `wchar_t` spelling.
    pub(crate) fn canonical_type(type_: CXType) -> CXType {
        // SAFETY: type_ was obtained from this live translation unit.
        unsafe { clang_sys::clang_getCanonicalType(type_) }
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

    /// Whether a C++ member pointer's owning class is a dependent
    /// (template-parameter) class.
    ///
    /// libclang signals dependence through the documented negative layout
    /// errors (`CXTypeLayoutError_Dependent`), and this query is the same
    /// family the collector already runs on every type before its children,
    /// so it is measured to be safe on exactly the inputs the direct class
    /// query cannot survive.
    pub(crate) fn member_pointer_class_is_dependent(type_: CXType) -> bool {
        // SAFETY: type_ was obtained from this live translation unit.
        let alignment = unsafe { clang_sys::clang_Type_getAlignOf(type_) };
        alignment == i64::from(clang_sys::CXTypeLayoutError_Dependent)
    }

    /// Returns the owning class type for a C++ member pointer. The direct
    /// libclang operation is the only authority for this operand; source
    /// spelling cannot distinguish overloads or nested owners reliably.
    ///
    /// The caller must first exclude dependent member pointers with
    /// [`Self::member_pointer_class_is_dependent`]: for `T::*` under a
    /// template parameter this libclang stores the class operand as a
    /// nested-name-specifier rather than a type, and the direct query
    /// dereferences that as a `Type*` — a hard native crash, measured on the
    /// `catch2/single_include/catch2/catch.hpp` corpus entry, whose first
    /// visited member pointer is the dependent `void (C::*)()`.
    pub(crate) fn member_pointer_class_type(type_: CXType) -> CXType {
        // SAFETY: type_ was obtained from this live translation unit.
        unsafe { clang_sys::clang_Type_getClassType(type_) }
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

    /// Returns libclang's direct C-family variadic predicate for a function
    /// type. It retains C's unbounded trailing `...` independently from a
    /// typed source rest parameter.
    pub(crate) fn function_is_variadic(type_: CXType) -> bool {
        // SAFETY: type_ was obtained from this live translation unit.
        unsafe { clang_sys::clang_isFunctionTypeVariadic(type_) != 0 }
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

    /// Returns the package-relative identity of the file declaring a cursor's target.
    ///
    /// Only a path that lives underneath the compilation root yields an
    /// identity: an absolute system or store path can never become a stable
    /// cross-fragment key, so those targets honestly report no file.
    pub(crate) fn cursor_file_identity(&self, cursor: CXCursor) -> Option<SymbolIdentity> {
        let root = self.compile_root.as_deref()?;
        // SAFETY: cursor was supplied by this live translation unit.
        let range = unsafe { clang_sys::clang_getCursorExtent(cursor) };
        // SAFETY: range was obtained from this translation unit; the calls are pure native reads.
        let location = unsafe { clang_sys::clang_getRangeStart(range) };
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
        if file.is_null() {
            return None;
        }
        // SAFETY: file belongs to this live translation unit and its name string is disposed by
        // native_relative_identity exactly once.
        let name = unsafe { clang_sys::clang_getFileName(file) };
        native_relative_identity(name, root.to_bytes(), b"nudox.clang.file.v1")
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

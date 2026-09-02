//! Defines parse behavior for the direct Clang semantic frontend of `compiler-driver`.
//! One call parses the exact caller bytes as one unsaved buffer, streams diagnostics,
//! traverses the cursor tree under bounded caller scratch, and commits journaled facts only
//! after the translation unit is admitted — a caller never observes a prefix of facts from a
//! failed translation unit.

use std::{
    ffi::{CStr, CString, c_char, c_int, c_uint, c_ulong},
    mem::{align_of, size_of},
    path::Path,
    ptr,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

use super::{
    error::ClangError,
    ffi,
    identity::{
        IDENTITY_MIN_TABLE_SLOTS, IDENTITY_SLOT_BYTES, IdentityInterner, kind_code, read_span,
        read_u32, write_span,
    },
    protocol::{
        ClangDiagnostic, ClangDiagnosticSeverity, ClangFact, ClangPhase, ClangReferenceKind,
        ClangSourceLanguage, ClangSourceSpan, ClangTypeUseKind, ClangTypeUseResolution, FactRole,
        SemanticKind,
    },
    source::diagnostic_span,
    traversal::{
        TRAVERSAL_MIN_FRAMES, TRAVERSAL_SCRATCH_DIVISOR, TraversalPath, VisitIssue, VisitState,
        visit_cursor,
    },
};

/// Fixed byte width of one journaled fact record.
pub(crate) const FACT_RECORD_BYTES: usize = 32;
/// Bytes of caller identity scratch charged per token for the token capacity bound.
pub(crate) const TOKEN_SCRATCH_BYTES_PER_TOKEN: usize = 64;

/// Journal tag of one canonical entity fact.
const FACT_TAG_ENTITY: u8 = 1;
/// Journal tag of one type-use fact.
const FACT_TAG_TYPE_USE: u8 = 2;
/// Journal tag of one reference fact.
const FACT_TAG_REFERENCE: u8 = 3;
/// Journal tag of one diagnostic fact.
const FACT_TAG_DIAGNOSTIC: u8 = 4;

/// Journal byte offset of the tag cell.
const RECORD_TAG: usize = 0;
/// Journal byte offset of the tag-specific subcode cell.
const RECORD_SUBCODE: usize = 1;
/// Journal byte offset of the identity role cell.
const RECORD_ROLE: usize = 2;
/// Journal byte offset of the resolution flag cell.
const RECORD_RESOLUTION: usize = 3;
/// Journal byte offset of the identity ordinal cell.
const RECORD_ORDINAL: usize = 4;
/// Journal byte offset of the first span.
const RECORD_SPAN_A: usize = 8;
/// Journal byte offset of the second span.
const RECORD_SPAN_B: usize = 16;
/// Journal byte offset of the third span or the diagnostic line.
const RECORD_SPAN_C: usize = 24;
/// Journal byte offset of the diagnostic column.
const RECORD_DIAGNOSTIC_COLUMN: usize = 28;

/// Typed analysis context assembled by the entry point.
pub(crate) struct AnalysisContext<'input, 'source> {
    /// Directory searched for package headers via one `-I` argument.
    pub(crate) include_root: &'input Path,
    /// Exact source name registered for the unsaved buffer.
    pub(crate) source_name: &'input Path,
    /// Closed source dialect parsed with the matching `-x` argument.
    pub(crate) source_language: ClangSourceLanguage,
    /// Exact caller source bytes; the only content authority.
    pub(crate) source: &'source [u8],
    /// Exact library version byte count observed during the authority check.
    pub(crate) library_version_bytes: usize,
}

/// Work and resource ledger of one admitted analysis.
pub(crate) struct AnalysisSummary {
    /// Admitted canonical entity count.
    pub(crate) entities: u32,
    /// Admitted type-use count.
    pub(crate) type_uses: u32,
    /// Admitted reference count.
    pub(crate) references: u32,
    /// Admitted diagnostic count.
    pub(crate) diagnostics: u32,
    /// First retained main-file error diagnostic when the source was rejected.
    pub(crate) first_diagnostic: Option<ClangDiagnostic>,
    /// Whether the native authority rejected the translation unit.
    pub(crate) parse_failed: bool,
    /// Largest reported per-category native resource amount.
    pub(crate) max_resource_category_bytes: usize,
    /// Aggregate reported native resource amount.
    pub(crate) resource_aggregate_bytes: u128,
    /// Number of nonzero native resource categories.
    pub(crate) resource_categories: usize,
    /// Exact cursor-visit work.
    pub(crate) cursor_visits: usize,
    /// Exact parent-comparison work.
    pub(crate) parent_queries: usize,
    /// Exact identity hash-probe work.
    pub(crate) identity_probes: usize,
    /// Identity scratch high-water mark.
    pub(crate) caller_scratch_high_water: usize,
    /// Fact journal high-water mark.
    pub(crate) fact_scratch_high_water: usize,
    /// Exact library version byte count.
    pub(crate) library_version_bytes: usize,
    /// Exact journal bytes written.
    pub(crate) journal_used: usize,
}

/// Transactional fact journal over one caller scratch region.
pub(crate) struct FactJournal<'scratch> {
    scratch: &'scratch mut [u8],
    used: usize,
    high_water: usize,
}

impl<'scratch> FactJournal<'scratch> {
    /// Journals facts over the exact caller scratch region.
    pub(crate) fn new(scratch: &'scratch mut [u8]) -> Self {
        Self {
            scratch,
            used: 0,
            high_water: 0,
        }
    }

    /// Encodes one typed fact or reports the exact capacity shortfall.
    pub(crate) fn push(&mut self, fact: ClangFact<'_>) -> Result<(), VisitIssue> {
        let end = self
            .used
            .checked_add(FACT_RECORD_BYTES)
            .ok_or(VisitIssue::FactCountOverflow)?;
        if end > self.scratch.len() {
            return Err(VisitIssue::FactScratchCapacity {
                provided: self.scratch.len(),
                required: end,
            });
        }
        let record = &mut self.scratch[self.used..end];
        record.fill(0);
        match fact {
            ClangFact::Entity(item) => {
                record[RECORD_TAG] = FACT_TAG_ENTITY;
                record[RECORD_SUBCODE] = kind_code(item.kind);
                record[RECORD_ROLE] = item.identity.role.code();
                record[RECORD_ORDINAL..RECORD_ORDINAL + 4]
                    .copy_from_slice(&item.identity.ordinal.to_le_bytes());
                write_span(record, RECORD_SPAN_A, item.span);
                write_span(record, RECORD_SPAN_B, item.owner);
            }
            ClangFact::TypeUse(item) => {
                record[RECORD_TAG] = FACT_TAG_TYPE_USE;
                record[RECORD_SUBCODE] = type_use_kind_code(item.kind);
                record[RECORD_RESOLUTION] = match item.resolution {
                    ClangTypeUseResolution::Builtin => 0,
                    ClangTypeUseResolution::Declaration { identity, .. } => {
                        record[RECORD_ROLE] = identity.role.code();
                        record[RECORD_ORDINAL..RECORD_ORDINAL + 4]
                            .copy_from_slice(&identity.ordinal.to_le_bytes());
                        1
                    }
                };
                write_span(record, RECORD_SPAN_A, item.span);
                write_span(record, RECORD_SPAN_B, item.owner);
                if let ClangTypeUseResolution::Declaration { target, .. } = item.resolution {
                    write_span(record, RECORD_SPAN_C, target);
                }
            }
            ClangFact::Reference(item) => {
                record[RECORD_TAG] = FACT_TAG_REFERENCE;
                record[RECORD_SUBCODE] = reference_kind_code(item.kind);
                record[RECORD_ROLE] = item.identity.role.code();
                record[RECORD_ORDINAL..RECORD_ORDINAL + 4]
                    .copy_from_slice(&item.identity.ordinal.to_le_bytes());
                write_span(record, RECORD_SPAN_A, item.use_span);
                write_span(record, RECORD_SPAN_B, item.resolved_span);
                write_span(record, RECORD_SPAN_C, item.owner);
            }
            ClangFact::Diagnostic(item) => {
                record[RECORD_TAG] = FACT_TAG_DIAGNOSTIC;
                record[RECORD_SUBCODE] = diagnostic_severity_code(item.severity);
                record[RECORD_SPAN_C..RECORD_SPAN_C + 4].copy_from_slice(&item.line.to_le_bytes());
                record[RECORD_DIAGNOSTIC_COLUMN..RECORD_DIAGNOSTIC_COLUMN + 4]
                    .copy_from_slice(&item.column.to_le_bytes());
                write_span(record, RECORD_SPAN_A, item.span);
            }
            ClangFact::Documentation(_) => return Err(VisitIssue::FactCountOverflow),
        }
        self.used = end;
        self.high_water = self.high_water.max(self.used);
        Ok(())
    }
}

/// Parses one source buffer and traverses it under bounded scratch, returning its summary.
/// Fact commitment is a separate transactional step over the same journal scratch.
pub(crate) fn analyze<'input, 'source>(
    context: AnalysisContext<'input, 'source>,
    cancellation: Option<&AtomicBool>,
    deadline: Option<Instant>,
    identity_scratch: &mut [u8],
    fact_scratch: &mut [u8],
) -> Result<AnalysisSummary, ClangError<'input>> {
    let AnalysisContext {
        include_root,
        source_name,
        source_language,
        source,
        library_version_bytes,
    } = context;
    if source.contains(&0) {
        return Err(ClangError::LibclangSourceNul);
    }
    if cancellation.is_some_and(|flag| flag.load(Ordering::Acquire)) {
        return Err(ClangError::Cancelled {
            phase: ClangPhase::Analysis,
        });
    }
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        return Err(ClangError::DeadlineExceeded {
            phase: ClangPhase::Analysis,
        });
    }
    let caller_identity_scratch_bytes = identity_scratch.len();
    let minimum_scratch = minimum_identity_scratch().ok_or(ClangError::FactCountOverflow)?;
    let ScratchPartition {
        identity: identity_scratch,
        traversal: traversal_scratch,
    } = partition_identity_scratch(identity_scratch).ok_or(
        ClangError::LibclangIdentityScratchTooSmall {
            source_name,
            provided: caller_identity_scratch_bytes,
            required: minimum_scratch,
        },
    )?;
    IdentityInterner::preflight(identity_scratch.len()).ok_or(
        ClangError::LibclangIdentityScratchTooSmall {
            source_name,
            provided: identity_scratch.len(),
            required: IDENTITY_MIN_TABLE_SLOTS * IDENTITY_SLOT_BYTES + 1,
        },
    )?;
    if fact_scratch.len() < FACT_RECORD_BYTES {
        return Err(ClangError::LibclangFactScratchTooSmall {
            source_name,
            provided: fact_scratch.len(),
            required: FACT_RECORD_BYTES,
        });
    }
    let filename = CString::new(
        source_name
            .to_str()
            .ok_or(ClangError::LibclangArgumentNul)?,
    )
    .map_err(|_| ClangError::LibclangArgumentNul)?;
    let root = CString::new(
        include_root
            .to_str()
            .ok_or(ClangError::LibclangArgumentNul)?,
    )
    .map_err(|_| ClangError::LibclangArgumentNul)?;
    let language_name = match source_language {
        ClangSourceLanguage::C => "c",
        ClangSourceLanguage::Cxx => "c++",
    };
    let language_switch = CString::new("-x").map_err(|_| ClangError::LibclangArgumentNul)?;
    let language_arg = CString::new(language_name).map_err(|_| ClangError::LibclangArgumentNul)?;
    let syntax_only = CString::new("-fsyntax-only").map_err(|_| ClangError::LibclangArgumentNul)?;
    let include = CString::new("-I").map_err(|_| ClangError::LibclangArgumentNul)?;
    let arguments = [
        language_switch.as_ptr(),
        language_arg.as_ptr(),
        syntax_only.as_ptr(),
        include.as_ptr(),
        root.as_ptr(),
    ];
    let argument_count =
        c_int::try_from(arguments.len()).map_err(|_| ClangError::FactCountOverflow)?;
    let source_length =
        c_ulong::try_from(source.len()).map_err(|_| ClangError::SourceCoordinateTooLarge {
            coordinate: source.len(),
        })?;
    let mut unsaved = ffi::CxUnsavedFile {
        filename: filename.as_ptr(),
        contents: source.as_ptr().cast::<c_char>(),
        length: source_length,
    };

    // SAFETY: all pointers in this call point to NUL-free, live C strings or to the
    // caller-owned source buffer. The unsaved file and argument array stay alive until
    // libclang has returned from parsing.
    let index = unsafe { ffi::clang_create_index(0, 0) };
    if index.is_null() {
        return Err(ClangError::LibclangIndexCreate);
    }
    let index_guard = IndexGuard(index);
    let mut translation_unit: ffi::CxTranslationUnit = ptr::null_mut();
    // SAFETY: `index_guard` owns a valid index and `translation_unit` is an out-pointer
    // reserved for libclang. The arrays above remain alive for the duration of the call.
    let parse_code = unsafe {
        ffi::clang_parse_translation_unit2(
            index_guard.0,
            filename.as_ptr(),
            arguments.as_ptr(),
            argument_count,
            &mut unsaved,
            1,
            ffi::CX_TRANSLATION_UNIT_DETAILED_PREPROCESSING_RECORD,
            &mut translation_unit,
        )
    };
    if translation_unit.is_null() {
        return Err(ClangError::LibclangParse {
            source_name,
            code: parse_code as u32,
        });
    }
    let translation_unit = TranslationUnit(translation_unit);

    let mut journal = FactJournal::new(fact_scratch);
    let (diagnostics, first_diagnostic) =
        stream_diagnostics(translation_unit.0, source, source_name, &mut journal)?;
    let parse_failed = parse_code != ffi::CX_ERROR_SUCCESS
        || first_diagnostic.is_some_and(|diagnostic| {
            matches!(diagnostic.severity, ClangDiagnosticSeverity::Error)
        });
    let resources =
        resource_metrics(translation_unit.0).map_err(|issue| issue.into_error(source_name))?;
    if parse_failed {
        return Ok(AnalysisSummary {
            entities: 0,
            type_uses: 0,
            references: 0,
            diagnostics,
            first_diagnostic,
            parse_failed: true,
            max_resource_category_bytes: resources.max_category_bytes,
            resource_aggregate_bytes: resources.aggregate_bytes,
            resource_categories: resources.categories,
            cursor_visits: 0,
            parent_queries: 0,
            identity_probes: 0,
            caller_scratch_high_water: 0,
            fact_scratch_high_water: journal.high_water,
            library_version_bytes,
            journal_used: journal.used,
        });
    }

    let token_limit =
        token_limit(caller_identity_scratch_bytes).ok_or(ClangError::LibclangTokenCapacity {
            source_name,
            observed: caller_identity_scratch_bytes / TOKEN_SCRATCH_BYTES_PER_TOKEN,
            limit: c_uint::MAX as usize,
        })?;
    // SAFETY: the translation unit is live for the whole traversal.
    let root_cursor = unsafe { ffi::clang_get_translation_unit_cursor(translation_unit.0) };
    let mut state = VisitState {
        translation_unit: translation_unit.0,
        source,
        source_name,
        cancellation,
        journal,
        identities: IdentityInterner::new(identity_scratch).map_err(|(provided, required)| {
            ClangError::LibclangIdentityScratchTooSmall {
                source_name,
                provided,
                required,
            }
        })?,
        entities: 0,
        type_uses: 0,
        references: 0,
        cursor_visits: 0,
        token_limit,
        path: TraversalPath::new(root_cursor, traversal_scratch)
            .map_err(|issue| issue.into_error(source_name))?,
        issue: None,
    };
    // SAFETY: `root_cursor` belongs to `translation_unit`; the callback and state pointer
    // remain valid until libclang returns. The callback itself stops descending at the
    // caller-admitted traversal capacity and cannot let a Rust panic cross into C.
    unsafe {
        ffi::clang_visit_children(
            root_cursor,
            visit_cursor,
            (&mut state as *mut VisitState<'_, '_, '_, '_>).cast(),
        );
    }
    if let Some(issue) = state.issue {
        return Err(issue.into_error(source_name));
    }
    if cancellation.is_some_and(|flag| flag.load(Ordering::Acquire)) {
        return Err(ClangError::Cancelled {
            phase: ClangPhase::Analysis,
        });
    }
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        return Err(ClangError::DeadlineExceeded {
            phase: ClangPhase::Analysis,
        });
    }
    let resources =
        resource_metrics(translation_unit.0).map_err(|issue| issue.into_error(source_name))?;
    let parent_queries = state.path.parent_queries();
    let path_high_water = state
        .path
        .high_water()
        .map_err(|issue| issue.into_error(source_name))?;
    let path_high_water = state
        .identities
        .scratch_offset(path_high_water)
        .ok_or(ClangError::FactCountOverflow)?;
    let caller_scratch_high_water = state.identities.high_water().max(path_high_water);
    Ok(AnalysisSummary {
        entities: state.entities,
        type_uses: state.type_uses,
        references: state.references,
        diagnostics,
        first_diagnostic,
        parse_failed: false,
        max_resource_category_bytes: resources.max_category_bytes,
        resource_aggregate_bytes: resources.aggregate_bytes,
        resource_categories: resources.categories,
        cursor_visits: state.cursor_visits,
        parent_queries,
        identity_probes: state.identities.probes(),
        caller_scratch_high_water,
        fact_scratch_high_water: state.journal.high_water,
        library_version_bytes,
        journal_used: state.journal.used,
    })
}

/// Commits journaled facts through the emit closure after the unit was admitted.
pub(crate) fn commit_facts<'source>(
    source: &'source [u8],
    scratch: &[u8],
    used: usize,
    emit: &mut impl FnMut(ClangFact<'source>),
) -> Result<(), ClangError<'static>> {
    if used > scratch.len() || !used.is_multiple_of(FACT_RECORD_BYTES) {
        return Err(ClangError::FactCountOverflow);
    }
    for (ordinal, record) in scratch[..used].chunks_exact(FACT_RECORD_BYTES).enumerate() {
        let record = <&[u8; FACT_RECORD_BYTES]>::try_from(record)
            .map_err(|_| ClangError::FactCountOverflow)?;
        let fact = decode_record(ordinal, record, source)?;
        if let ClangFact::Entity(entity) = fact {
            emit(ClangFact::Entity(entity));
            if entity.kind != SemanticKind::Parameter {
                if let Some(raw) = preceding_doc(source, entity.span.start as usize) {
                    emit(ClangFact::Documentation(super::protocol::DoxygenDocFact {
                        raw,
                        owner: entity.identity,
                        has_return: raw.contains("\\return"),
                        deprecated: raw.find("\\deprecated").map(|start| {
                            let text = &raw[start + "\\deprecated".len()..];
                            text.trim()
                        }),
                    }));
                }
            }
        } else {
            emit(fact);
        }
    }
    Ok(())
}

/// Borrows the contiguous Doxygen comment immediately preceding a declaration name.
fn preceding_doc(source: &[u8], name_start: usize) -> Option<&str> {
    let prefix = source.get(..name_start)?;
    let declaration_line = prefix
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |line| line + 1);
    let before_declaration = prefix.get(..declaration_line)?.trim_ascii_end();
    let line_end = before_declaration.len();
    let mut line_start = before_declaration
        .get(..line_end)?
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |line| line + 1);
    if before_declaration
        .get(line_start..line_end)?
        .starts_with(b"///")
    {
        while line_start != 0 {
            let previous_end = line_start - 1;
            let previous_start = before_declaration
                .get(..previous_end)?
                .iter()
                .rposition(|byte| *byte == b'\n')
                .map_or(0, |line| line + 1);
            if !before_declaration
                .get(previous_start..previous_end)?
                .starts_with(b"///")
            {
                break;
            }
            line_start = previous_start;
        }
        return std::str::from_utf8(&before_declaration[line_start..line_end]).ok();
    }
    if before_declaration.ends_with(b"*/") {
        let block_end = before_declaration.len();
        let marker = before_declaration
            .windows(3)
            .rposition(|window| window == b"/**")?;
        let block = before_declaration.get(marker..block_end)?;
        let first_close = block.windows(2).position(|window| window == b"*/")?;
        if first_close + 2 != block.len() {
            return None;
        }
        return std::str::from_utf8(block).ok();
    }
    None
}

/// Decodes one journal record into its typed fact or reports the exact ordinal.
fn decode_record<'source>(
    ordinal: usize,
    record: &[u8; FACT_RECORD_BYTES],
    source: &'source [u8],
) -> Result<ClangFact<'source>, ClangError<'static>> {
    let invalid = || ClangError::JournalRecordInvalid { ordinal };
    let identity = || -> Result<super::protocol::FactId, ClangError<'static>> {
        let role = FactRole::from_code(record[RECORD_ROLE]).ok_or_else(invalid)?;
        Ok(super::protocol::FactId {
            role,
            ordinal: read_u32(record, RECORD_ORDINAL),
        })
    };
    let borrowed_span = |offset: usize| -> Result<ClangSourceSpan, ClangError<'static>> {
        let span = read_span(record, offset);
        if span.start as usize <= span.end as usize && span.end as usize <= source.len() {
            Ok(span)
        } else {
            Err(invalid())
        }
    };
    let borrowed = |span: ClangSourceSpan| -> Result<&'source str, ClangError<'static>> {
        super::cursor::source_text(source, span).ok_or_else(invalid)
    };
    match record[RECORD_TAG] {
        FACT_TAG_ENTITY => {
            let kind =
                SemanticKind::try_from(u16::from(record[RECORD_SUBCODE])).map_err(|_| invalid())?;
            let span = borrowed_span(RECORD_SPAN_A)?;
            let name = borrowed(span)?;
            Ok(ClangFact::Entity(super::protocol::EntityFact {
                name,
                kind,
                span,
                owner: borrowed_span(RECORD_SPAN_B)?,
                identity: identity()?,
            }))
        }
        FACT_TAG_TYPE_USE => {
            let kind = type_use_kind(record[RECORD_SUBCODE]).ok_or_else(invalid)?;
            let resolution = match record[RECORD_RESOLUTION] {
                0 => ClangTypeUseResolution::Builtin,
                1 => ClangTypeUseResolution::Declaration {
                    target: borrowed_span(RECORD_SPAN_C)?,
                    identity: identity()?,
                },
                _ => return Err(invalid()),
            };
            let span = borrowed_span(RECORD_SPAN_A)?;
            let name = borrowed(span)?;
            Ok(ClangFact::TypeUse(super::protocol::TypeUseFact {
                name,
                kind,
                resolution,
                span,
                owner: borrowed_span(RECORD_SPAN_B)?,
            }))
        }
        FACT_TAG_REFERENCE => {
            let kind = reference_kind(record[RECORD_SUBCODE]).ok_or_else(invalid)?;
            let use_span = borrowed_span(RECORD_SPAN_A)?;
            let target = borrowed(use_span)?;
            Ok(ClangFact::Reference(super::protocol::ReferenceFact {
                target,
                kind,
                use_span,
                resolved_span: borrowed_span(RECORD_SPAN_B)?,
                owner: borrowed_span(RECORD_SPAN_C)?,
                identity: identity()?,
            }))
        }
        FACT_TAG_DIAGNOSTIC => {
            let severity = diagnostic_severity(record[RECORD_SUBCODE]).ok_or_else(invalid)?;
            Ok(ClangFact::Diagnostic(ClangDiagnostic {
                severity,
                line: read_u32(record, RECORD_SPAN_C),
                column: read_u32(record, RECORD_DIAGNOSTIC_COLUMN),
                span: borrowed_span(RECORD_SPAN_A)?,
            }))
        }
        _ => Err(invalid()),
    }
}

fn type_use_kind_code(kind: ClangTypeUseKind) -> u8 {
    match kind {
        ClangTypeUseKind::Field => 1,
        ClangTypeUseKind::FunctionSignature => 2,
        ClangTypeUseKind::Parameter => 3,
        ClangTypeUseKind::Variable => 4,
    }
}

fn type_use_kind(code: u8) -> Option<ClangTypeUseKind> {
    match code {
        1 => Some(ClangTypeUseKind::Field),
        2 => Some(ClangTypeUseKind::FunctionSignature),
        3 => Some(ClangTypeUseKind::Parameter),
        4 => Some(ClangTypeUseKind::Variable),
        _ => None,
    }
}

fn reference_kind_code(kind: ClangReferenceKind) -> u8 {
    match kind {
        ClangReferenceKind::FunctionCall => 1,
        ClangReferenceKind::MethodCall => 2,
        ClangReferenceKind::VariableUse => 3,
        ClangReferenceKind::FieldAccess => 4,
        ClangReferenceKind::MacroInvocation => 5,
    }
}

fn reference_kind(code: u8) -> Option<ClangReferenceKind> {
    match code {
        1 => Some(ClangReferenceKind::FunctionCall),
        2 => Some(ClangReferenceKind::MethodCall),
        3 => Some(ClangReferenceKind::VariableUse),
        4 => Some(ClangReferenceKind::FieldAccess),
        5 => Some(ClangReferenceKind::MacroInvocation),
        _ => None,
    }
}

fn diagnostic_severity_code(severity: ClangDiagnosticSeverity) -> u8 {
    match severity {
        ClangDiagnosticSeverity::Error => 1,
        ClangDiagnosticSeverity::Warning => 2,
        ClangDiagnosticSeverity::Note => 3,
    }
}

fn diagnostic_severity(code: u8) -> Option<ClangDiagnosticSeverity> {
    match code {
        1 => Some(ClangDiagnosticSeverity::Error),
        2 => Some(ClangDiagnosticSeverity::Warning),
        3 => Some(ClangDiagnosticSeverity::Note),
        _ => None,
    }
}

/// Minimum total identity scratch the interner and traversal path can operate with.
fn minimum_identity_scratch() -> Option<usize> {
    IDENTITY_MIN_TABLE_SLOTS
        .checked_mul(IDENTITY_SLOT_BYTES)
        .and_then(|bytes| {
            TRAVERSAL_MIN_FRAMES
                .checked_mul(size_of::<super::traversal::TraversalFrame>())
                .and_then(|frames| {
                    frames.checked_add(align_of::<super::traversal::TraversalFrame>() - 1)
                })
                .and_then(|frames| bytes.checked_add(frames))
        })
}

/// Caller scratch partitioned into its identity and traversal-path regions.
struct ScratchPartition<'scratch> {
    /// Identity interner region; the first caller bytes.
    identity: &'scratch mut [u8],
    /// Traversal ancestor-path region; the remaining caller bytes.
    traversal: &'scratch mut [u8],
}

/// Partitions identity scratch into interner and traversal-path regions.
fn partition_identity_scratch(scratch: &mut [u8]) -> Option<ScratchPartition<'_>> {
    let proportional = scratch.len() / TRAVERSAL_SCRATCH_DIVISOR;
    let minimum_path = TRAVERSAL_MIN_FRAMES
        .checked_mul(size_of::<super::traversal::TraversalFrame>())?
        .checked_add(align_of::<super::traversal::TraversalFrame>() - 1)?;
    let path_bytes = proportional.max(minimum_path);
    let identity_bytes = scratch.len().checked_sub(path_bytes)?;
    (identity_bytes > IDENTITY_MIN_TABLE_SLOTS * IDENTITY_SLOT_BYTES).then(|| {
        let (identity, traversal) = scratch.split_at_mut(identity_bytes);
        ScratchPartition {
            identity,
            traversal,
        }
    })
}

/// Caller-derived token capacity for bounded tokenization.
fn token_limit(scratch_len: usize) -> Option<c_uint> {
    let capacity = scratch_len / TOKEN_SCRATCH_BYTES_PER_TOKEN;
    c_uint::try_from(capacity).ok()
}

/// Verifies the linked library against the expected version family and returns the
/// observed version byte count.
pub(crate) fn verify_library_version<'input>(
    expected: &'input [u8],
) -> Result<usize, ClangError<'input>> {
    // SAFETY: `clang_getClangVersion` returns a libclang-owned string that remains valid
    // until it is disposed below. No pointer escapes this function.
    let version = unsafe { ffi::clang_get_clang_version() };
    // SAFETY: the string is live for this borrow and disposed at the end of the block.
    let observed = {
        let pointer = unsafe { ffi::clang_get_c_string(version) };
        if pointer.is_null() {
            &[][..]
        } else {
            // SAFETY: libclang returns a NUL-terminated string for a live handle.
            unsafe { CStr::from_ptr(pointer) }.to_bytes()
        }
    };
    let observed_len = observed.len();
    let matches = version_family(expected)
        .zip(version_family(observed))
        .is_some_and(|(expected, observed)| expected == observed);
    // SAFETY: the string is live and is disposed exactly once here.
    unsafe { ffi::clang_dispose_string(version) };
    if !matches {
        return Err(ClangError::ToolVersionMismatch {
            expected_bytes: expected.len(),
            observed: observed_len,
        });
    }
    Ok(observed_len)
}

/// Parses the first three numeric components of a version string.
fn version_family(bytes: &[u8]) -> Option<(u32, u32, u32)> {
    let mut numbers = [0_u32; 3];
    let mut found = 0_usize;
    let mut index = 0_usize;
    while index < bytes.len() && found < numbers.len() {
        while index < bytes.len() && !bytes[index].is_ascii_digit() {
            index += 1;
        }
        if index == bytes.len() {
            break;
        }
        let mut value = 0_u32;
        let mut digits = 0_usize;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            value = value
                .checked_mul(10)?
                .checked_add(u32::from(bytes[index] - b'0'))?;
            digits += 1;
            index += 1;
        }
        if digits != 0 {
            numbers[found] = value;
            found += 1;
        }
    }
    (found == numbers.len()).then_some((numbers[0], numbers[1], numbers[2]))
}

/// Index handle disposed exactly once.
struct IndexGuard(ffi::CxIndex);

impl Drop for IndexGuard {
    fn drop(&mut self) {
        // SAFETY: this guard is constructed only from a non-null handle and owns the index
        // until this destructor runs.
        unsafe { ffi::clang_dispose_index(self.0) };
    }
}

/// Translation-unit handle disposed exactly once.
struct TranslationUnit(ffi::CxTranslationUnit);

impl Drop for TranslationUnit {
    fn drop(&mut self) {
        // SAFETY: this guard is constructed only from a non-null handle and owns it until
        // this destructor runs.
        unsafe { ffi::clang_dispose_translation_unit(self.0) };
    }
}

/// Diagnostic handle disposed exactly once.
struct DiagnosticGuard(ffi::CxDiagnostic);

impl Drop for DiagnosticGuard {
    fn drop(&mut self) {
        // SAFETY: this guard owns a non-null diagnostic returned by libclang.
        unsafe { ffi::clang_dispose_diagnostic(self.0) };
    }
}

/// Child diagnostic-set handle disposed exactly once.
struct DiagnosticSetGuard(ffi::CxDiagnosticSet);

impl Drop for DiagnosticSetGuard {
    fn drop(&mut self) {
        // SAFETY: this guard owns a non-null child set returned by libclang.
        unsafe { ffi::clang_dispose_diagnostic_set(self.0) };
    }
}

/// Streams every main-file diagnostic (with children) into the journal.
fn stream_diagnostics<'input>(
    translation_unit: ffi::CxTranslationUnit,
    source: &[u8],
    source_name: &'input Path,
    journal: &mut FactJournal<'_>,
) -> Result<(u32, Option<ClangDiagnostic>), ClangError<'input>> {
    // SAFETY: the translation unit is live for the whole diagnostic streaming.
    let count = unsafe { ffi::clang_get_num_diagnostics(translation_unit) };
    let mut emitted = 0_u32;
    let mut first = None;
    for index in 0..count {
        // SAFETY: the translation unit is live and a null return is checked.
        let diagnostic = unsafe { ffi::clang_get_diagnostic(translation_unit, index) };
        if diagnostic.is_null() {
            continue;
        }
        let _guard = DiagnosticGuard(diagnostic);
        emitted = emitted
            .checked_add(stream_one_diagnostic(
                diagnostic,
                source,
                source_name,
                journal,
                &mut first,
            )?)
            .ok_or(ClangError::FactCountOverflow)?;

        // Child diagnostics carry notes attached to a warning or error (for example, a
        // deprecation note). They are owned by the returned set; keeping the set guard
        // alive makes each child pointer valid while it is streamed. Header-owned
        // children are filtered by the same location check as top-level diagnostics.
        // SAFETY: the diagnostic is live and a null return is checked.
        let children = unsafe { ffi::clang_get_child_diagnostics(diagnostic) };
        if !children.is_null() {
            let children = DiagnosticSetGuard(children);
            // SAFETY: the child set is live.
            let child_count = unsafe { ffi::clang_get_num_diagnostics_in_set(children.0) };
            for child_index in 0..child_count {
                // SAFETY: the child set is live and a null return is checked.
                let child = unsafe { ffi::clang_get_diagnostic_in_set(children.0, child_index) };
                if child.is_null() {
                    continue;
                }
                emitted = emitted
                    .checked_add(stream_one_diagnostic(
                        child,
                        source,
                        source_name,
                        journal,
                        &mut first,
                    )?)
                    .ok_or(ClangError::FactCountOverflow)?;
            }
            drop(children);
        }
    }
    Ok((emitted, first))
}

/// Streams one diagnostic and returns whether it produced a fact.
fn stream_one_diagnostic<'input>(
    diagnostic: ffi::CxDiagnostic,
    source: &[u8],
    source_name: &'input Path,
    journal: &mut FactJournal<'_>,
    first: &mut Option<ClangDiagnostic>,
) -> Result<u32, ClangError<'input>> {
    // SAFETY: the diagnostic is live for this query.
    let severity = unsafe { ffi::clang_get_diagnostic_severity(diagnostic) };
    let severity = match severity {
        ffi::CX_DIAGNOSTIC_NOTE => ClangDiagnosticSeverity::Note,
        ffi::CX_DIAGNOSTIC_WARNING => ClangDiagnosticSeverity::Warning,
        ffi::CX_DIAGNOSTIC_ERROR | ffi::CX_DIAGNOSTIC_FATAL => ClangDiagnosticSeverity::Error,
        _ => return Ok(0),
    };
    // SAFETY: the diagnostic is live for this query.
    let location = unsafe { ffi::clang_get_diagnostic_location(diagnostic) };
    // This check is based on libclang's file identity, not the diagnostic spelling.
    // Header diagnostics therefore cannot be mis-owned by the caller's source buffer.
    // SAFETY: the location belongs to the live translation unit.
    if unsafe { ffi::clang_location_is_from_main_file(location) } == 0 {
        return Ok(0);
    }
    let mut line = 0_u32;
    let mut column = 0_u32;
    // SAFETY: the location belongs to the live translation unit and the out-pointers are
    // reserved stack slots.
    unsafe {
        ffi::clang_get_expansion_location(
            location,
            ptr::null_mut(),
            &mut line,
            &mut column,
            ptr::null_mut(),
        );
    }
    let native = diagnostic_span(source, severity, line, column)?;
    first.get_or_insert(native);
    journal
        .push(ClangFact::Diagnostic(native))
        .map_err(|issue| match issue {
            VisitIssue::FactScratchCapacity { provided, required } => {
                ClangError::LibclangFactScratchTooSmall {
                    source_name,
                    provided,
                    required,
                }
            }
            _ => ClangError::FactCountOverflow,
        })?;
    Ok(1)
}

/// Reported native resource metrics of one translation unit.
struct ResourceMetrics {
    max_category_bytes: usize,
    aggregate_bytes: u128,
    categories: usize,
}

/// Aggregates the translation unit's reported resource usage.
fn resource_metrics(
    translation_unit: ffi::CxTranslationUnit,
) -> Result<ResourceMetrics, VisitIssue> {
    // SAFETY: the translation unit is live for the entire resource usage query and the
    // returned entries are borrowed until `clang_dispose_tu_resource_usage`.
    // `max_category_bytes` is deliberately not called TU peak: libclang only exposes the
    // maximum amount for each category here. The aggregate is reported separately for the
    // same reason.
    let usage = unsafe { ffi::clang_get_tu_resource_usage(translation_unit) };
    let mut peak = 0_usize;
    let mut aggregate = 0_u128;
    let mut categories = 0_usize;
    if !usage.entries.is_null() {
        for index in 0..usage.num_entries {
            // SAFETY: the index is below the live entry count returned by libclang.
            let amount = unsafe { (*usage.entries.add(index as usize)).amount } as usize;
            peak = peak.max(amount);
            aggregate = aggregate
                .checked_add(amount as u128)
                .ok_or(VisitIssue::FactCountOverflow)?;
            categories = categories
                .checked_add(usize::from(amount != 0))
                .ok_or(VisitIssue::FactCountOverflow)?;
        }
    }
    // SAFETY: the report was produced live and is disposed exactly once here.
    unsafe { ffi::clang_dispose_tu_resource_usage(usage) };
    Ok(ResourceMetrics {
        max_category_bytes: peak,
        aggregate_bytes: aggregate,
        categories,
    })
}

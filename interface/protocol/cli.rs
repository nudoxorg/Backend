//! Defines cli behavior for `interface-protocol`, whose purpose is to decode and project the shared application vocabulary for external transports.
//! This module owns the cli invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Command-line token decoding with no business behavior.

use std::{error::Error, fmt, num::ParseIntError};

use heart_adaptive::CapabilityDomain;
use heart_identity::{ContentIdDecodeError, Domain, HASH_BYTES, IndexSnapshotDomain, RootDomain};
use interface_core::{
    ApplicationInput, BatteryState, ByteCount, ContentId, CorrelationId, GenerateRequest,
    GenerateTarget, InconsistentRecovery, InputText, InputTextError, OperationBudget, OperationKey,
    Pin, Pressure, RejectedSourceText, ResourceBudget, RetryBudget, SourceText,
};

use crate::command::{
    RawApplicationCommand, RawGenerate, RawHealth, RawInconsistentPolicy, RawNumber, RawOperation,
    RawPolicy, RawRetrieval, RawSearch, RawSnapshot, RawStage,
};
use crate::field::AdapterField;
use crate::source::{CliCommand, SourceEncodingError, SourceIngressRole, SourceIoFact};

/// Largest positional CLI field count for the closed application command vocabulary.
pub const MAX_CLI_ARGUMENTS: usize = 14;
/// Exact text width of one formatted content identity (`content:` plus 32 lower-hex bytes).
pub const CANONICAL_CONTENT_ID_TEXT_BYTES: usize = 8 + HASH_BYTES * 2;
/// Largest number of commands sharing one bounded CLI service session.
const MAX_CLI_COMMANDS: usize = 4;
/// Token that separates commands while retaining one in-process service owner.
pub const CLI_COMMAND_SEPARATOR: &str = "--";
const MAX_CLI_SESSION_ARGUMENTS: usize =
    MAX_CLI_ARGUMENTS * MAX_CLI_COMMANDS + (MAX_CLI_COMMANDS - 1);

/// Closed adapter-only diagnostic code for malformed transport input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdapterErrorCode {
    /// Command token did not name a supported adapter operation.
    UnknownAction,
    /// Required positional field was absent.
    MissingField,
    /// A positional number was malformed or out of its transport type.
    InvalidNumber,
    /// JSON-RPC fields had the wrong structural shape.
    InvalidShape,
    /// A field exceeded the bounded frame text limit before business dispatch.
    FieldTooLong,
    /// The process adapter rejected excess positional fields before storing them.
    TooManyFields,
}

/// Exact upstream source retained by an adapter rejection where one exists.
#[derive(Debug)]
pub enum AdapterErrorCause {
    /// The framed body was not valid JSON according to the pinned parser.
    Json(serde_json::Error),
    /// A numeric transport field failed the primitive integer parser.
    Number(ParseIntError),
    /// A native JSON number exceeded the target typed integer width.
    NumberRange {
        /// Native numeric value observed on the wire.
        actual: u64,
        /// Largest value representable by the target type.
        maximum: u64,
    },
    /// A transport field exceeded fixed retained input capacity.
    TextLength(InputTextError),
    /// A compiler source exceeded its named local admission budget; the exact source owner is
    /// retained for the CLI or MCP process to report without a second copy.
    SourceLength(RejectedSourceText),
    /// The CLI reader could not reserve bounded ingress storage without losing the allocator
    /// cause.
    SourceAllocation {
        /// Process edge whose bounded reader requested capacity.
        role: SourceIngressRole,
        /// Exact additional capacity reservation requested.
        requested: usize,
        /// Standard-library allocation source.
        source: std::collections::TryReserveError,
    },
    /// A bounded language token was not one of the canonical compiler language values.
    UnknownLanguage(InputText),
    /// A bounded stage token was not one of the canonical compiler stage values.
    UnknownStage(InputText),
    /// Exact native I/O facts from the CLI-owned source ingress boundary.
    SourceIo(SourceIoFact),
    /// A bounded CLI source was not valid UTF-8 and its original bytes were retained.
    SourceEncoding(SourceEncodingError),
    /// The fixed source budget could not express its one-byte overflow sentinel.
    SourceLimitOverflow {
        /// Product source limit whose sentinel arithmetic overflowed.
        limit: interface_core::SourceByteLimit,
    },
    /// A reader's checked buffered length could not represent one more chunk.
    SourceCapacityOverflow {
        /// Bytes already retained by the reader.
        buffered: usize,
        /// Bytes supplied by the current reader call.
        appended: usize,
    },
    /// Standard input was already consumed by an earlier command in this process session.
    StandardInputConsumed,
    /// A formatted content identity failed exact syntax or typed domain validation.
    CanonicalContentId(CanonicalContentIdDecodeError),
}

impl fmt::Display for AdapterErrorCause {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(source) => source.fmt(formatter),
            Self::Number(source) => source.fmt(formatter),
            Self::NumberRange { actual, maximum } => {
                write!(formatter, "number {actual} exceeds maximum {maximum}")
            }
            Self::TextLength(source) => write!(
                formatter,
                "field length {} exceeds {}",
                source.actual, source.maximum
            ),
            Self::SourceLength(rejected) => write!(
                formatter,
                "source length {} exceeds {}",
                rejected.error.observed, rejected.error.limit.bytes
            ),
            Self::SourceAllocation {
                role, requested, ..
            } => write!(
                formatter,
                "{role:?} source reader could not reserve {requested} bytes"
            ),
            Self::UnknownLanguage(observed) => {
                write!(formatter, "unknown compiler language {}", &**observed)
            }
            Self::UnknownStage(observed) => {
                write!(formatter, "unknown compiler stage {}", &**observed)
            }
            Self::SourceIo(fact) => write!(
                formatter,
                "{role:?} source {phase:?} failed with {kind:?} ({raw_os_code:?})",
                role = fact.role,
                phase = fact.phase,
                kind = fact.kind,
                raw_os_code = fact.raw_os_code,
            ),
            Self::SourceEncoding(error) => write!(
                formatter,
                "source is not UTF-8 after byte {} ({:?})",
                error.valid_up_to, error.error_length
            ),
            Self::SourceLimitOverflow { limit } => write!(
                formatter,
                "source limit {} cannot reserve an overflow sentinel",
                limit.bytes
            ),
            Self::SourceCapacityOverflow { buffered, appended } => write!(
                formatter,
                "source reader cannot append {appended} bytes after {buffered} buffered bytes"
            ),
            Self::StandardInputConsumed => {
                formatter.write_str("standard input was already consumed")
            }
            Self::CanonicalContentId(source) => source.fmt(formatter),
        }
    }
}

impl Error for AdapterErrorCause {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Json(source) => Some(source),
            Self::Number(source) => Some(source),
            Self::SourceAllocation { source, .. } => Some(source),
            Self::CanonicalContentId(source) => Some(source),
            Self::NumberRange { .. }
            | Self::TextLength(_)
            | Self::SourceLength(_)
            | Self::UnknownLanguage(_)
            | Self::UnknownStage(_)
            | Self::SourceIo(_)
            | Self::SourceEncoding(_)
            | Self::SourceLimitOverflow { .. }
            | Self::SourceCapacityOverflow { .. }
            | Self::StandardInputConsumed => None,
        }
    }
}

/// Structured transport rejection that never enters application dispatch.
#[derive(Debug)]
pub struct AdapterError {
    /// Closed adapter error code.
    pub code: AdapterErrorCode,
    /// Named offending transport field.
    pub field: AdapterField,
    /// Exact observed transport width when it exceeds the retained field bound.
    pub actual: Option<usize>,
    /// Preserved parser or width cause, never an erased mapper closure.
    pub cause: Option<AdapterErrorCause>,
}

/// Exact rejection while parsing the string-only canonical content-identity encoding.
#[derive(Debug, Eq, PartialEq)]
pub enum CanonicalContentIdDecodeError {
    /// The complete formatted identity width was not retained exactly.
    Width {
        /// Observed UTF-8 byte width.
        actual: usize,
        /// Required formatted width.
        expected: usize,
    },
    /// The fixed textual authority prefix was not present.
    Prefix {
        /// Observed prefix bytes.
        observed: [u8; 8],
    },
    /// One formatted digest nibble was not lower hexadecimal.
    Hex {
        /// Byte offset in the complete formatted value.
        offset: usize,
        /// Observed non-hex byte.
        observed: u8,
    },
    /// The decoded fixed-width identity failed typed authority validation.
    ContentId(ContentIdDecodeError),
}

impl fmt::Display for CanonicalContentIdDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Width { actual, expected } => write!(
                formatter,
                "canonical content identity has {actual} text bytes, expected {expected}"
            ),
            Self::Prefix { observed } => {
                write!(
                    formatter,
                    "canonical content identity prefix is not `content:`: "
                )?;
                for byte in observed {
                    write!(formatter, "{byte:02x}")?;
                }
                Ok(())
            }
            Self::Hex { offset, observed } => write!(
                formatter,
                "canonical content identity byte {offset} is not lower hexadecimal: {observed:02x}"
            ),
            Self::ContentId(source) => source.fmt(formatter),
        }
    }
}

impl Error for CanonicalContentIdDecodeError {}

/// One typed content identity carried through a fixed string-only adapter boundary.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CanonicalContentId<DomainTag>(ContentId<DomainTag>);

impl<DomainTag> From<ContentId<DomainTag>> for CanonicalContentId<DomainTag> {
    fn from(value: ContentId<DomainTag>) -> Self {
        Self(value)
    }
}

impl<DomainTag> From<CanonicalContentId<DomainTag>> for ContentId<DomainTag> {
    fn from(value: CanonicalContentId<DomainTag>) -> Self {
        value.0
    }
}

impl<DomainTag> fmt::Display for CanonicalContentId<DomainTag> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl<DomainTag: Domain> TryFrom<&str> for CanonicalContentId<DomainTag> {
    type Error = CanonicalContentIdDecodeError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let bytes = value.as_bytes();
        if bytes.len() != CANONICAL_CONTENT_ID_TEXT_BYTES {
            return Err(CanonicalContentIdDecodeError::Width {
                actual: bytes.len(),
                expected: CANONICAL_CONTENT_ID_TEXT_BYTES,
            });
        }
        if &bytes[..8] != b"content:" {
            let mut observed = [0; 8];
            observed.copy_from_slice(&bytes[..8]);
            return Err(CanonicalContentIdDecodeError::Prefix { observed });
        }

        let mut raw = [0; HASH_BYTES];
        for (index, byte) in raw.iter_mut().enumerate() {
            let high_offset = 8 + index * 2;
            let high = hex_nibble(bytes[high_offset], high_offset)?;
            let low_offset = high_offset + 1;
            let low = hex_nibble(bytes[low_offset], low_offset)?;
            *byte = (high << 4) | low;
        }
        ContentId::<DomainTag>::try_from(raw)
            .map(Self)
            .map_err(CanonicalContentIdDecodeError::ContentId)
    }
}

fn hex_nibble(byte: u8, offset: usize) -> Result<u8, CanonicalContentIdDecodeError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(CanonicalContentIdDecodeError::Hex {
            offset,
            observed: byte,
        }),
    }
}

impl AdapterError {
    pub(crate) const fn simple(code: AdapterErrorCode, field: AdapterField) -> Self {
        Self {
            code,
            field,
            actual: None,
            cause: None,
        }
    }

    pub(crate) fn field_too_long(field: AdapterField, source: InputTextError) -> Self {
        Self {
            code: AdapterErrorCode::FieldTooLong,
            field,
            actual: Some(source.actual),
            cause: Some(AdapterErrorCause::TextLength(source)),
        }
    }

    #[must_use]
    /// Retains an over-limit source together with its exact observed byte length.
    pub fn source_too_long(source: RejectedSourceText) -> Self {
        Self {
            code: AdapterErrorCode::FieldTooLong,
            field: AdapterField::Source,
            actual: Some(source.error.observed),
            cause: Some(AdapterErrorCause::SourceLength(source)),
        }
    }

    pub(crate) fn unknown_language(observed: InputText) -> Self {
        Self {
            code: AdapterErrorCode::InvalidShape,
            field: AdapterField::Language,
            actual: None,
            cause: Some(AdapterErrorCause::UnknownLanguage(observed)),
        }
    }

    pub(crate) fn unknown_stage(observed: InputText) -> Self {
        Self {
            code: AdapterErrorCode::InvalidShape,
            field: AdapterField::Stage,
            actual: None,
            cause: Some(AdapterErrorCause::UnknownStage(observed)),
        }
    }

    /// Preserves exact native source-ingress facts without retaining platform-specific display
    /// text.
    #[must_use]
    pub fn source_io(fact: SourceIoFact) -> Self {
        Self {
            code: AdapterErrorCode::InvalidShape,
            field: AdapterField::Source,
            actual: None,
            cause: Some(AdapterErrorCause::SourceIo(fact)),
        }
    }

    /// Preserves a bounded reader allocation failure and the exact requested capacity.
    #[must_use]
    pub fn source_allocation(
        role: SourceIngressRole,
        requested: usize,
        source: std::collections::TryReserveError,
    ) -> Self {
        Self {
            code: AdapterErrorCode::InvalidShape,
            field: AdapterField::Source,
            actual: Some(requested),
            cause: Some(AdapterErrorCause::SourceAllocation {
                role,
                requested,
                source,
            }),
        }
    }

    /// Retains malformed bounded source bytes and the exact UTF-8 decoder boundary.
    pub fn source_encoding(bytes: Vec<u8>, source: std::str::Utf8Error) -> Self {
        let error_length = source.error_len().and_then(std::num::NonZeroUsize::new);
        Self {
            code: AdapterErrorCode::InvalidShape,
            field: AdapterField::Source,
            actual: Some(bytes.len()),
            cause: Some(AdapterErrorCause::SourceEncoding(SourceEncodingError {
                bytes: bytes.into_boxed_slice(),
                valid_up_to: source.valid_up_to(),
                error_length,
            })),
        }
    }

    /// Reports impossible overflow while forming the bounded reader's one-byte sentinel.
    #[must_use]
    pub fn source_limit_overflow(limit: interface_core::SourceByteLimit) -> Self {
        Self {
            code: AdapterErrorCode::InvalidShape,
            field: AdapterField::Source,
            actual: Some(limit.bytes),
            cause: Some(AdapterErrorCause::SourceLimitOverflow { limit }),
        }
    }

    /// Retains exact reader capacity arithmetic when an append cannot be represented.
    #[must_use]
    pub fn source_capacity_overflow(buffered: usize, appended: usize) -> Self {
        Self {
            code: AdapterErrorCode::InvalidShape,
            field: AdapterField::Source,
            actual: Some(buffered),
            cause: Some(AdapterErrorCause::SourceCapacityOverflow { buffered, appended }),
        }
    }

    #[must_use]
    /// Reports a second attempt to consume the process's single standard-input stream.
    pub fn standard_input_consumed() -> Self {
        Self {
            code: AdapterErrorCode::InvalidShape,
            field: AdapterField::Source,
            actual: None,
            cause: Some(AdapterErrorCause::StandardInputConsumed),
        }
    }

    pub(crate) fn invalid_number(field: AdapterField, source: ParseIntError) -> Self {
        Self {
            code: AdapterErrorCode::InvalidNumber,
            field,
            actual: None,
            cause: Some(AdapterErrorCause::Number(source)),
        }
    }

    pub(crate) fn invalid_number_range(field: AdapterField, actual: u64, maximum: u64) -> Self {
        Self {
            code: AdapterErrorCode::InvalidNumber,
            field,
            actual: None,
            cause: Some(AdapterErrorCause::NumberRange { actual, maximum }),
        }
    }

    pub(crate) fn invalid_content_id(
        field: AdapterField,
        source: CanonicalContentIdDecodeError,
    ) -> Self {
        Self {
            code: AdapterErrorCode::InvalidShape,
            field,
            actual: None,
            cause: Some(AdapterErrorCause::CanonicalContentId(source)),
        }
    }

    pub(crate) fn invalid_json(field: AdapterField, source: serde_json::Error) -> Self {
        Self {
            code: AdapterErrorCode::InvalidShape,
            field,
            actual: None,
            cause: Some(AdapterErrorCause::Json(source)),
        }
    }
}

impl fmt::Display for AdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "adapter {} rejected", self.field)
    }
}

impl Error for AdapterError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.cause.as_ref().map(|cause| cause as &dyn Error)
    }
}

/// Decodes one complete CLI argument vector into a closed typed application input.
///
/// # Errors
///
/// Returns [`AdapterError`] for malformed CLI shape before a request enters semantic dispatch.
pub fn decode_cli(arguments: &[String]) -> Result<ApplicationInput, AdapterError> {
    raw_command(arguments)?.try_into()
}

/// Decodes one CLI command, retaining file and standard-input source acquisition at the process
/// edge while sharing the same typed compiler target with direct CLI and MCP source text.
///
/// # Errors
///
/// Returns [`AdapterError`] when the command shape or compiler target is malformed before source
/// acquisition starts.
pub fn decode_cli_command(arguments: &[String]) -> Result<CliCommand, AdapterError> {
    match field(arguments, 0, AdapterField::Action)? {
        "generate-file" => generate_file_command(arguments),
        "generate-stdin" => generate_standard_input_command(arguments),
        _ => decode_cli(arguments).map(CliCommand::Application),
    }
}

fn raw_command(arguments: &[String]) -> Result<RawApplicationCommand, AdapterError> {
    let action = field(arguments, 0, AdapterField::Action)?;
    let correlation = cli_number(arguments, 1, AdapterField::Correlation)?;
    if let Some(expected) = expected_fields(action) {
        exact_fields(arguments, expected)?;
    }
    match action {
        "generate" => Ok(RawApplicationCommand::Generate(RawGenerate {
            correlation,
            profile: raw_profile(arguments, 2)?,
            stage: raw_stage(arguments, 3)?,
            source: owned(arguments, 4, AdapterField::Source)?,
        })),
        "status" => Ok(RawApplicationCommand::SnapshotStatus(RawSnapshot {
            correlation,
            snapshot: owned(arguments, 2, AdapterField::Snapshot)?,
        })),
        "search" => Ok(RawApplicationCommand::Search(RawSearch {
            correlation,
            snapshot: owned(arguments, 2, AdapterField::Snapshot)?,
            query: owned(arguments, 3, AdapterField::Query)?,
            limit: raw_number(arguments, 4, AdapterField::Limit)?,
        })),
        "graph" => Ok(RawApplicationCommand::Graph(RawRetrieval {
            correlation,
            snapshot: owned(arguments, 2, AdapterField::Snapshot)?,
            limit: raw_number(arguments, 3, AdapterField::Limit)?,
        })),
        "vector" => Ok(RawApplicationCommand::Vector(RawRetrieval {
            correlation,
            snapshot: owned(arguments, 2, AdapterField::Snapshot)?,
            limit: raw_number(arguments, 3, AdapterField::Limit)?,
        })),
        "locality" => Ok(RawApplicationCommand::Locality(RawSnapshot {
            correlation,
            snapshot: owned(arguments, 2, AdapterField::Snapshot)?,
        })),
        "health" => Ok(RawApplicationCommand::Health(RawHealth { correlation })),
        "recover-local" => raw_recover_command(arguments, correlation),
        "recover-inconsistent" => raw_inconsistent_policy_command(arguments, correlation),
        "release-local" => raw_release_command(arguments, correlation),
        "poll-execution" => Ok(RawApplicationCommand::PollExecution(RawOperation {
            correlation,
            operation: raw_number(arguments, 2, AdapterField::Operation)?,
        })),
        "cancel" => Ok(RawApplicationCommand::Cancel(RawOperation {
            correlation,
            operation: raw_number(arguments, 2, AdapterField::Operation)?,
        })),
        _ => Err(AdapterError::simple(
            AdapterErrorCode::UnknownAction,
            AdapterField::Action,
        )),
    }
}

fn expected_fields(action: &str) -> Option<usize> {
    match action {
        "generate" | "generate-file" | "search" => Some(5),
        "generate-stdin" | "graph" | "vector" => Some(4),
        "status" | "locality" | "poll-execution" | "cancel" => Some(3),
        "health" => Some(2),
        "recover-local" | "release-local" => Some(12),
        "recover-inconsistent" => Some(14),
        _ => None,
    }
}

fn exact_fields(arguments: &[String], expected: usize) -> Result<(), AdapterError> {
    if arguments.len() > expected {
        Err(AdapterError {
            code: AdapterErrorCode::TooManyFields,
            field: AdapterField::Arguments,
            actual: Some(arguments.len()),
            cause: None,
        })
    } else {
        Ok(())
    }
}

/// Retains only the fixed count and width of positional CLI tokens before semantic decoding.
///
/// # Errors
///
/// Returns [`AdapterError`] before allocation grows beyond the closed adapter input bound.
pub fn collect_cli_arguments(
    arguments: impl IntoIterator<Item = String>,
) -> Result<Vec<String>, AdapterError> {
    let mut bounded = Vec::with_capacity(MAX_CLI_SESSION_ARGUMENTS);
    let mut command_count = 1_usize;
    let mut field_count = 0_usize;
    for argument in arguments {
        if argument == CLI_COMMAND_SEPARATOR {
            if field_count == 0 {
                return Err(AdapterError::simple(
                    AdapterErrorCode::MissingField,
                    AdapterField::Action,
                ));
            }
            if command_count == MAX_CLI_COMMANDS {
                return Err(AdapterError {
                    code: AdapterErrorCode::TooManyFields,
                    field: AdapterField::Commands,
                    actual: Some(command_count + 1),
                    cause: None,
                });
            }
            command_count += 1;
            field_count = 0;
            bounded.push(argument);
            continue;
        }
        if field_count == MAX_CLI_ARGUMENTS {
            return Err(AdapterError {
                code: AdapterErrorCode::TooManyFields,
                field: AdapterField::Arguments,
                actual: Some(MAX_CLI_ARGUMENTS + 1),
                cause: None,
            });
        }
        bounded.push(argument);
        field_count += 1;
    }
    if bounded
        .last()
        .is_some_and(|last| last == CLI_COMMAND_SEPARATOR)
    {
        return Err(AdapterError::simple(
            AdapterErrorCode::MissingField,
            AdapterField::Action,
        ));
    }
    Ok(bounded)
}

impl TryFrom<RawApplicationCommand> for ApplicationInput {
    type Error = AdapterError;

    fn try_from(command: RawApplicationCommand) -> Result<Self, Self::Error> {
        match command {
            RawApplicationCommand::Generate(raw) => generate_input(raw),
            RawApplicationCommand::SnapshotStatus(raw) => status_input(raw),
            RawApplicationCommand::Search(raw) => search_input(raw),
            RawApplicationCommand::Graph(raw) => graph_input(raw),
            RawApplicationCommand::Vector(raw) => vector_input(raw),
            RawApplicationCommand::Locality(raw) => locality_input(raw),
            RawApplicationCommand::Health(raw) => Ok(ApplicationInput::Health {
                correlation: CorrelationId(raw.correlation),
            }),
            RawApplicationCommand::RecoverLocal(raw) => recover_input(raw),
            RawApplicationCommand::RecoverInconsistent(raw) => inconsistent_policy_input(raw),
            RawApplicationCommand::ReleaseLocal(raw) => release_input(raw),
            RawApplicationCommand::PollExecution(raw) => poll_input(raw),
            RawApplicationCommand::Cancel(raw) => cancel_input(raw),
        }
    }
}

fn generate_input(raw: RawGenerate) -> Result<ApplicationInput, AdapterError> {
    source_input(
        generate_target(raw.correlation, raw.profile, raw.stage),
        raw.source,
    )
}

/// Admits one source owner after a CLI, MCP, file, or standard-input boundary has already selected
/// a canonical compiler target.
///
/// # Errors
///
/// Returns an [`AdapterError`] retaining the original source owner when it exceeds the named
/// portable local source budget.
pub fn source_input(
    target: GenerateTarget,
    source: String,
) -> Result<ApplicationInput, AdapterError> {
    let source = SourceText::try_from(source).map_err(AdapterError::source_too_long)?;
    Ok(ApplicationInput::Generate(GenerateRequest {
        target,
        source,
    }))
}

fn generate_file_command(arguments: &[String]) -> Result<CliCommand, AdapterError> {
    let target = cli_generate_target(arguments)?;
    exact_fields(arguments, 5)?;
    Ok(CliCommand::GenerateFile {
        target,
        path: owned(arguments, 4, AdapterField::Path)?,
    })
}

fn generate_standard_input_command(arguments: &[String]) -> Result<CliCommand, AdapterError> {
    let target = cli_generate_target(arguments)?;
    exact_fields(arguments, 4)?;
    Ok(CliCommand::GenerateStandardInput { target })
}

fn cli_generate_target(arguments: &[String]) -> Result<GenerateTarget, AdapterError> {
    Ok(generate_target(
        cli_number(arguments, 1, AdapterField::Correlation)?,
        raw_profile(arguments, 2)?,
        raw_stage(arguments, 3)?,
    ))
}

fn generate_target(
    correlation: u64,
    profile: compiler_vocabulary::LanguageProfile,
    stage: RawStage,
) -> GenerateTarget {
    GenerateTarget {
        correlation: CorrelationId(correlation),
        profile,
        stage: stage.into(),
    }
}

fn raw_profile(
    arguments: &[String],
    index: usize,
) -> Result<compiler_vocabulary::LanguageProfile, AdapterError> {
    let value = InputText::try_from_str(field(arguments, index, AdapterField::Language)?)
        .map_err(|source| AdapterError::field_too_long(AdapterField::Language, source))?;
    compiler_vocabulary::LanguageProfile::try_from(&*value)
        .map_err(|_| AdapterError::unknown_language(value))
}

fn raw_stage(arguments: &[String], index: usize) -> Result<RawStage, AdapterError> {
    let value = InputText::try_from_str(field(arguments, index, AdapterField::Stage)?)
        .map_err(|source| AdapterError::field_too_long(AdapterField::Stage, source))?;
    RawStage::try_from(value).map_err(AdapterError::unknown_stage)
}

fn status_input(raw: RawSnapshot) -> Result<ApplicationInput, AdapterError> {
    Ok(ApplicationInput::SnapshotStatus {
        correlation: CorrelationId(raw.correlation),
        snapshot: input_text(raw.snapshot, AdapterField::Snapshot)?,
    })
}

fn search_input(raw: RawSearch) -> Result<ApplicationInput, AdapterError> {
    Ok(ApplicationInput::Search {
        correlation: CorrelationId(raw.correlation),
        snapshot: input_text(raw.snapshot, AdapterField::Snapshot)?,
        query: input_text(raw.query, AdapterField::Query)?,
        limit: number(raw.limit, AdapterField::Limit)?,
    })
}

fn retrieval_parts(raw: RawRetrieval) -> Result<(CorrelationId, InputText, u8), AdapterError> {
    Ok((
        CorrelationId(raw.correlation),
        input_text(raw.snapshot, AdapterField::Snapshot)?,
        number(raw.limit, AdapterField::Limit)?,
    ))
}

fn graph_input(raw: RawRetrieval) -> Result<ApplicationInput, AdapterError> {
    let (correlation, snapshot, limit) = retrieval_parts(raw)?;
    Ok(ApplicationInput::Graph {
        correlation,
        snapshot,
        limit,
    })
}

fn vector_input(raw: RawRetrieval) -> Result<ApplicationInput, AdapterError> {
    let (correlation, snapshot, limit) = retrieval_parts(raw)?;
    Ok(ApplicationInput::Vector {
        correlation,
        snapshot,
        limit,
    })
}

fn locality_input(raw: RawSnapshot) -> Result<ApplicationInput, AdapterError> {
    Ok(ApplicationInput::Locality {
        correlation: CorrelationId(raw.correlation),
        snapshot: input_text(raw.snapshot, AdapterField::Snapshot)?,
    })
}

type PolicyParts = (
    CorrelationId,
    Pin,
    ContentId<CapabilityDomain>,
    ResourceBudget,
);

fn policy_parts(raw: RawPolicy) -> Result<PolicyParts, AdapterError> {
    Ok((
        CorrelationId(raw.correlation),
        pin_from_text(&raw.generation, &raw.snapshot)?,
        canonical_content_id(&raw.bundle, AdapterField::Bundle)?,
        budget(
            raw.ram_free,
            raw.nvme_free,
            raw.operations,
            raw.retries,
            &raw.memory_pressure,
            &raw.storage_pressure,
            &raw.battery,
        )?,
    ))
}

fn recover_input(raw: RawPolicy) -> Result<ApplicationInput, AdapterError> {
    let (correlation, pin, bundle, budget) = policy_parts(raw)?;
    Ok(ApplicationInput::RecoverLocal {
        correlation,
        pin,
        bundle,
        budget,
    })
}

fn inconsistent_policy_input(raw: RawInconsistentPolicy) -> Result<ApplicationInput, AdapterError> {
    Ok(ApplicationInput::RecoverInconsistent(
        InconsistentRecovery {
            correlation: CorrelationId(raw.correlation),
            expected: pin_from_text(&raw.expected_generation, &raw.expected_snapshot)?,
            observed: pin_from_text(&raw.observed_generation, &raw.observed_snapshot)?,
            bundle: canonical_content_id(&raw.bundle, AdapterField::Bundle)?,
            budget: budget(
                raw.ram_free,
                raw.nvme_free,
                raw.operations,
                raw.retries,
                &raw.memory_pressure,
                &raw.storage_pressure,
                &raw.battery,
            )?,
        },
    ))
}

fn release_input(raw: RawPolicy) -> Result<ApplicationInput, AdapterError> {
    let (correlation, pin, bundle, budget) = policy_parts(raw)?;
    Ok(ApplicationInput::ReleaseLocal {
        correlation,
        pin,
        bundle,
        budget,
    })
}

fn operation_parts(raw: RawOperation) -> Result<(CorrelationId, OperationKey), AdapterError> {
    Ok((
        CorrelationId(raw.correlation),
        OperationKey(number(raw.operation, AdapterField::Operation)?),
    ))
}

fn poll_input(raw: RawOperation) -> Result<ApplicationInput, AdapterError> {
    let (correlation, operation) = operation_parts(raw)?;
    Ok(ApplicationInput::PollExecution {
        correlation,
        operation,
    })
}

fn cancel_input(raw: RawOperation) -> Result<ApplicationInput, AdapterError> {
    let (correlation, operation) = operation_parts(raw)?;
    Ok(ApplicationInput::Cancel {
        correlation,
        operation,
    })
}

fn field(arguments: &[String], index: usize, name: AdapterField) -> Result<&str, AdapterError> {
    arguments
        .get(index)
        .map(String::as_str)
        .ok_or(AdapterError::simple(AdapterErrorCode::MissingField, name))
}

fn owned(arguments: &[String], index: usize, name: AdapterField) -> Result<String, AdapterError> {
    Ok(field(arguments, index, name)?.to_owned())
}

fn raw_number(
    arguments: &[String],
    index: usize,
    name: AdapterField,
) -> Result<RawNumber, AdapterError> {
    Ok(RawNumber::Text(owned(arguments, index, name)?))
}

fn cli_number<T>(arguments: &[String], index: usize, name: AdapterField) -> Result<T, AdapterError>
where
    T: core::str::FromStr<Err = ParseIntError>,
{
    field(arguments, index, name)?
        .parse::<T>()
        .map_err(|source| AdapterError::invalid_number(name, source))
}

fn input_text(value: String, name: AdapterField) -> Result<InputText, AdapterError> {
    let result = InputText::try_from_str(&value)
        .map_err(|source| AdapterError::field_too_long(name, source));
    drop(value);
    result
}

trait RawInteger: core::str::FromStr<Err = ParseIntError> + TryFrom<u64> {
    const MAX: u64;
}

impl RawInteger for u8 {
    const MAX: u64 = u8::MAX as u64;
}

impl RawInteger for u32 {
    const MAX: u64 = u32::MAX as u64;
}

impl RawInteger for u64 {
    const MAX: u64 = u64::MAX;
}

fn number<T>(value: RawNumber, name: AdapterField) -> Result<T, AdapterError>
where
    T: RawInteger,
{
    match value {
        RawNumber::Number(value) => {
            T::try_from(value).map_err(|_| AdapterError::invalid_number_range(name, value, T::MAX))
        }
        RawNumber::Text(value) => value
            .parse::<T>()
            .map_err(|source| AdapterError::invalid_number(name, source)),
    }
}

fn raw_policy(arguments: &[String], correlation: u64) -> Result<RawPolicy, AdapterError> {
    Ok(RawPolicy {
        correlation,
        generation: owned(arguments, 2, AdapterField::Generation)?,
        snapshot: owned(arguments, 3, AdapterField::Snapshot)?,
        bundle: owned(arguments, 4, AdapterField::Bundle)?,
        ram_free: raw_number(arguments, 5, AdapterField::RamFree)?,
        nvme_free: raw_number(arguments, 6, AdapterField::NvmeFree)?,
        operations: raw_number(arguments, 7, AdapterField::Operations)?,
        retries: raw_number(arguments, 8, AdapterField::Retries)?,
        memory_pressure: owned(arguments, 9, AdapterField::MemoryPressure)?,
        storage_pressure: owned(arguments, 10, AdapterField::StoragePressure)?,
        battery: owned(arguments, 11, AdapterField::Battery)?,
    })
}

fn raw_recover_command(
    arguments: &[String],
    correlation: u64,
) -> Result<RawApplicationCommand, AdapterError> {
    raw_policy(arguments, correlation).map(RawApplicationCommand::RecoverLocal)
}

fn raw_release_command(
    arguments: &[String],
    correlation: u64,
) -> Result<RawApplicationCommand, AdapterError> {
    raw_policy(arguments, correlation).map(RawApplicationCommand::ReleaseLocal)
}

fn raw_inconsistent_policy_command(
    arguments: &[String],
    correlation: u64,
) -> Result<RawApplicationCommand, AdapterError> {
    Ok(RawApplicationCommand::RecoverInconsistent(
        RawInconsistentPolicy {
            correlation,
            expected_generation: owned(arguments, 2, AdapterField::ExpectedGeneration)?,
            expected_snapshot: owned(arguments, 3, AdapterField::ExpectedSnapshot)?,
            observed_generation: owned(arguments, 4, AdapterField::ObservedGeneration)?,
            observed_snapshot: owned(arguments, 5, AdapterField::ObservedSnapshot)?,
            bundle: owned(arguments, 6, AdapterField::Bundle)?,
            ram_free: raw_number(arguments, 7, AdapterField::RamFree)?,
            nvme_free: raw_number(arguments, 8, AdapterField::NvmeFree)?,
            operations: raw_number(arguments, 9, AdapterField::Operations)?,
            retries: raw_number(arguments, 10, AdapterField::Retries)?,
            memory_pressure: owned(arguments, 11, AdapterField::MemoryPressure)?,
            storage_pressure: owned(arguments, 12, AdapterField::StoragePressure)?,
            battery: owned(arguments, 13, AdapterField::Battery)?,
        },
    ))
}

fn pin_from_text(generation: &str, snapshot: &str) -> Result<Pin, AdapterError> {
    Ok(Pin {
        generation: canonical_content_id::<RootDomain>(generation, AdapterField::Generation)?,
        snapshot: canonical_content_id::<IndexSnapshotDomain>(snapshot, AdapterField::Snapshot)?,
    })
}

fn canonical_content_id<DomainTag: Domain>(
    value: &str,
    field: AdapterField,
) -> Result<ContentId<DomainTag>, AdapterError> {
    CanonicalContentId::<DomainTag>::try_from(value)
        .map(Into::into)
        .map_err(|source| AdapterError::invalid_content_id(field, source))
}

fn budget(
    ram_free: RawNumber,
    nvme_free: RawNumber,
    operations: RawNumber,
    retries: RawNumber,
    memory_pressure: &str,
    storage_pressure: &str,
    battery_state: &str,
) -> Result<ResourceBudget, AdapterError> {
    Ok(ResourceBudget {
        ram_free: ByteCount::from(number::<u32>(ram_free, AdapterField::RamFree)?),
        nvme_free: ByteCount::from(number::<u32>(nvme_free, AdapterField::NvmeFree)?),
        operations: OperationBudget::from(number::<u8>(operations, AdapterField::Operations)?),
        retries: RetryBudget::from(number::<u8>(retries, AdapterField::Retries)?),
        memory_pressure: pressure(memory_pressure, AdapterField::MemoryPressure)?,
        storage_pressure: pressure(storage_pressure, AdapterField::StoragePressure)?,
        battery: battery(battery_state)?,
    })
}

fn pressure(value: &str, field: AdapterField) -> Result<Pressure, AdapterError> {
    match value {
        "relaxed" => Ok(Pressure::Relaxed),
        "elevated" => Ok(Pressure::Elevated),
        "critical" => Ok(Pressure::Critical),
        _ => Err(AdapterError::simple(AdapterErrorCode::InvalidShape, field)),
    }
}

fn battery(value: &str) -> Result<BatteryState, AdapterError> {
    match value {
        "charging" => Ok(BatteryState::Charging),
        "normal" => Ok(BatteryState::Normal),
        "low" => Ok(BatteryState::Low),
        "critical" => Ok(BatteryState::Critical),
        _ => Err(AdapterError::simple(
            AdapterErrorCode::InvalidShape,
            AdapterField::Battery,
        )),
    }
}

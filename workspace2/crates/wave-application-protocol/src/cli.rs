//! Command-line token decoding with no business behavior.

use std::{error::Error, fmt, num::ParseIntError};

use nudox_adaptive::CapabilityDomain;
use nudox_id::{ContentIdDecodeError, Domain, HASH_BYTES, IndexSnapshotDomain, RootDomain};
use wave_application_core::{
    ApplicationInput, BatteryState, ByteCount, ContentId, CorrelationId, InconsistentRecovery,
    InputText, InputTextError, OperationBudget, OperationKey, Pin, Pressure, ResourceBudget,
    RetryBudget,
};

use crate::command::{
    RawApplicationCommand, RawGenerate, RawHealth, RawInconsistentPolicy, RawNumber, RawOperation,
    RawPolicy, RawRetrieval, RawSearch, RawSnapshot,
};

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
            Self::CanonicalContentId(source) => source.fmt(formatter),
        }
    }
}

impl Error for AdapterErrorCause {}

/// Structured transport rejection that never enters application dispatch.
#[derive(Debug)]
pub struct AdapterError {
    /// Closed adapter error code.
    pub code: AdapterErrorCode,
    /// Named offending transport field.
    pub field: &'static str,
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
    pub(crate) const fn simple(code: AdapterErrorCode, field: &'static str) -> Self {
        Self {
            code,
            field,
            actual: None,
            cause: None,
        }
    }

    pub(crate) fn field_too_long(field: &'static str, source: InputTextError) -> Self {
        Self {
            code: AdapterErrorCode::FieldTooLong,
            field,
            actual: Some(source.actual),
            cause: Some(AdapterErrorCause::TextLength(source)),
        }
    }

    pub(crate) fn invalid_number(field: &'static str, source: ParseIntError) -> Self {
        Self {
            code: AdapterErrorCode::InvalidNumber,
            field,
            actual: None,
            cause: Some(AdapterErrorCause::Number(source)),
        }
    }

    pub(crate) fn invalid_number_range(field: &'static str, actual: u64, maximum: u64) -> Self {
        Self {
            code: AdapterErrorCode::InvalidNumber,
            field,
            actual: None,
            cause: Some(AdapterErrorCause::NumberRange { actual, maximum }),
        }
    }

    pub(crate) fn invalid_content_id(
        field: &'static str,
        source: CanonicalContentIdDecodeError,
    ) -> Self {
        Self {
            code: AdapterErrorCode::InvalidShape,
            field,
            actual: None,
            cause: Some(AdapterErrorCause::CanonicalContentId(source)),
        }
    }

    pub(crate) fn invalid_json(field: &'static str, source: serde_json::Error) -> Self {
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

impl Error for AdapterError {}

/// Decodes one complete CLI argument vector into a closed typed application input.
///
/// # Errors
///
/// Returns [`AdapterError`] for malformed CLI shape before a request enters semantic dispatch.
pub fn decode_cli(arguments: &[String]) -> Result<ApplicationInput, AdapterError> {
    raw_command(arguments)?.try_into()
}

fn raw_command(arguments: &[String]) -> Result<RawApplicationCommand, AdapterError> {
    let action = field(arguments, 0, "action")?;
    let correlation = cli_number(arguments, 1, "correlation")?;
    if let Some(expected) = expected_fields(action) {
        exact_fields(arguments, expected)?;
    }
    match action {
        "generate" => Ok(RawApplicationCommand::Generate(RawGenerate {
            correlation,
            language: owned(arguments, 2, "language")?,
            stage: owned(arguments, 3, "stage")?,
            source: owned(arguments, 4, "source")?,
        })),
        "status" => Ok(RawApplicationCommand::SnapshotStatus(RawSnapshot {
            correlation,
            snapshot: owned(arguments, 2, "snapshot")?,
        })),
        "search" => Ok(RawApplicationCommand::Search(RawSearch {
            correlation,
            snapshot: owned(arguments, 2, "snapshot")?,
            query: owned(arguments, 3, "query")?,
            limit: raw_number(arguments, 4, "limit")?,
        })),
        "graph" => Ok(RawApplicationCommand::Graph(RawRetrieval {
            correlation,
            snapshot: owned(arguments, 2, "snapshot")?,
            limit: raw_number(arguments, 3, "limit")?,
        })),
        "vector" => Ok(RawApplicationCommand::Vector(RawRetrieval {
            correlation,
            snapshot: owned(arguments, 2, "snapshot")?,
            limit: raw_number(arguments, 3, "limit")?,
        })),
        "locality" => Ok(RawApplicationCommand::Locality(RawSnapshot {
            correlation,
            snapshot: owned(arguments, 2, "snapshot")?,
        })),
        "health" => Ok(RawApplicationCommand::Health(RawHealth { correlation })),
        "recover-local" => raw_recover_command(arguments, correlation),
        "recover-inconsistent" => raw_inconsistent_policy_command(arguments, correlation),
        "release-local" => raw_release_command(arguments, correlation),
        "poll-execution" => Ok(RawApplicationCommand::PollExecution(RawOperation {
            correlation,
            operation: raw_number(arguments, 2, "operation")?,
        })),
        "cancel" => Ok(RawApplicationCommand::Cancel(RawOperation {
            correlation,
            operation: raw_number(arguments, 2, "operation")?,
        })),
        _ => Err(AdapterError::simple(
            AdapterErrorCode::UnknownAction,
            "action",
        )),
    }
}

fn expected_fields(action: &str) -> Option<usize> {
    match action {
        "generate" | "search" => Some(5),
        "status" | "locality" | "poll-execution" | "cancel" => Some(3),
        "graph" | "vector" => Some(4),
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
            field: "arguments",
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
                    "action",
                ));
            }
            if command_count == MAX_CLI_COMMANDS {
                return Err(AdapterError {
                    code: AdapterErrorCode::TooManyFields,
                    field: "commands",
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
                field: "arguments",
                actual: Some(MAX_CLI_ARGUMENTS + 1),
                cause: None,
            });
        }
        if argument.len() > CANONICAL_CONTENT_ID_TEXT_BYTES {
            return Err(AdapterError::field_too_long(
                "argument",
                InputTextError {
                    actual: argument.len(),
                    maximum: CANONICAL_CONTENT_ID_TEXT_BYTES,
                },
            ));
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
            "action",
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
    Ok(ApplicationInput::Generate {
        correlation: CorrelationId(raw.correlation),
        language: input_text(raw.language, "language")?,
        stage: input_text(raw.stage, "stage")?,
        source: input_text(raw.source, "source")?,
    })
}

fn status_input(raw: RawSnapshot) -> Result<ApplicationInput, AdapterError> {
    Ok(ApplicationInput::SnapshotStatus {
        correlation: CorrelationId(raw.correlation),
        snapshot: input_text(raw.snapshot, "snapshot")?,
    })
}

fn search_input(raw: RawSearch) -> Result<ApplicationInput, AdapterError> {
    Ok(ApplicationInput::Search {
        correlation: CorrelationId(raw.correlation),
        snapshot: input_text(raw.snapshot, "snapshot")?,
        query: input_text(raw.query, "query")?,
        limit: number(raw.limit, "limit")?,
    })
}

fn retrieval_parts(raw: RawRetrieval) -> Result<(CorrelationId, InputText, u8), AdapterError> {
    Ok((
        CorrelationId(raw.correlation),
        input_text(raw.snapshot, "snapshot")?,
        number(raw.limit, "limit")?,
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
        snapshot: input_text(raw.snapshot, "snapshot")?,
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
        canonical_content_id(&raw.bundle, "bundle")?,
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
            bundle: canonical_content_id(&raw.bundle, "bundle")?,
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
        OperationKey(number(raw.operation, "operation")?),
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

fn field<'input>(
    arguments: &'input [String],
    index: usize,
    name: &'static str,
) -> Result<&'input str, AdapterError> {
    arguments
        .get(index)
        .map(String::as_str)
        .ok_or(AdapterError::simple(AdapterErrorCode::MissingField, name))
}

fn owned(arguments: &[String], index: usize, name: &'static str) -> Result<String, AdapterError> {
    Ok(field(arguments, index, name)?.to_owned())
}

fn raw_number(
    arguments: &[String],
    index: usize,
    name: &'static str,
) -> Result<RawNumber, AdapterError> {
    Ok(RawNumber::Text(owned(arguments, index, name)?))
}

fn cli_number<T>(arguments: &[String], index: usize, name: &'static str) -> Result<T, AdapterError>
where
    T: core::str::FromStr<Err = ParseIntError>,
{
    field(arguments, index, name)?
        .parse::<T>()
        .map_err(|source| AdapterError::invalid_number(name, source))
}

fn input_text(value: String, name: &'static str) -> Result<InputText, AdapterError> {
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

fn number<T>(value: RawNumber, name: &'static str) -> Result<T, AdapterError>
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
        generation: owned(arguments, 2, "generation")?,
        snapshot: owned(arguments, 3, "snapshot")?,
        bundle: owned(arguments, 4, "bundle")?,
        ram_free: raw_number(arguments, 5, "ram_free")?,
        nvme_free: raw_number(arguments, 6, "nvme_free")?,
        operations: raw_number(arguments, 7, "operations")?,
        retries: raw_number(arguments, 8, "retries")?,
        memory_pressure: owned(arguments, 9, "memory_pressure")?,
        storage_pressure: owned(arguments, 10, "storage_pressure")?,
        battery: owned(arguments, 11, "battery")?,
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
            expected_generation: owned(arguments, 2, "expected_generation")?,
            expected_snapshot: owned(arguments, 3, "expected_snapshot")?,
            observed_generation: owned(arguments, 4, "observed_generation")?,
            observed_snapshot: owned(arguments, 5, "observed_snapshot")?,
            bundle: owned(arguments, 6, "bundle")?,
            ram_free: raw_number(arguments, 7, "ram_free")?,
            nvme_free: raw_number(arguments, 8, "nvme_free")?,
            operations: raw_number(arguments, 9, "operations")?,
            retries: raw_number(arguments, 10, "retries")?,
            memory_pressure: owned(arguments, 11, "memory_pressure")?,
            storage_pressure: owned(arguments, 12, "storage_pressure")?,
            battery: owned(arguments, 13, "battery")?,
        },
    ))
}

fn pin_from_text(generation: &str, snapshot: &str) -> Result<Pin, AdapterError> {
    Ok(Pin {
        generation: canonical_content_id::<RootDomain>(generation, "generation")?,
        snapshot: canonical_content_id::<IndexSnapshotDomain>(snapshot, "snapshot")?,
    })
}

fn canonical_content_id<DomainTag: Domain>(
    value: &str,
    field: &'static str,
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
        ram_free: ByteCount::from(number::<u32>(ram_free, "ram_free")?),
        nvme_free: ByteCount::from(number::<u32>(nvme_free, "nvme_free")?),
        operations: OperationBudget::from(number::<u8>(operations, "operations")?),
        retries: RetryBudget::from(number::<u8>(retries, "retries")?),
        memory_pressure: pressure(memory_pressure, "memory_pressure")?,
        storage_pressure: pressure(storage_pressure, "storage_pressure")?,
        battery: battery(battery_state)?,
    })
}

fn pressure(value: &str, field: &'static str) -> Result<Pressure, AdapterError> {
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
            "battery",
        )),
    }
}

//! Command-line token decoding with no business behavior.

use std::{error::Error, fmt, num::ParseIntError};

use nudox_id::{ContentIdDecodeError, Domain, HASH_BYTES, IndexSnapshotDomain, RootDomain};
use wave_application_core::{
    ApplicationInput, BatteryState, ByteCount, ContentId, CorrelationId, InconsistentRecovery,
    InputText, InputTextError, OperationBudget, OperationKey, Pin, Pressure, ResourceBudget,
    RetryBudget,
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
    let action = field(arguments, 0, "action")?;
    let correlation = CorrelationId(number(arguments, 1, "correlation")?);
    if let Some(expected) = expected_fields(action) {
        exact_fields(arguments, expected)?;
    }
    match action {
        "generate" => Ok(ApplicationInput::Generate {
            correlation,
            language: text(arguments, 2, "language")?,
            stage: text(arguments, 3, "stage")?,
            package: text(arguments, 4, "package")?,
            source: text(arguments, 5, "source")?,
        }),
        "status" => Ok(ApplicationInput::SnapshotStatus {
            correlation,
            snapshot: text(arguments, 2, "snapshot")?,
        }),
        "search" => Ok(ApplicationInput::Search {
            correlation,
            snapshot: text(arguments, 2, "snapshot")?,
            query: text(arguments, 3, "query")?,
            limit: number(arguments, 4, "limit")?,
        }),
        "graph" => Ok(ApplicationInput::Graph {
            correlation,
            snapshot: text(arguments, 2, "snapshot")?,
            limit: number(arguments, 3, "limit")?,
        }),
        "vector" => Ok(ApplicationInput::Vector {
            correlation,
            snapshot: text(arguments, 2, "snapshot")?,
            limit: number(arguments, 3, "limit")?,
        }),
        "locality" => Ok(ApplicationInput::Locality {
            correlation,
            snapshot: text(arguments, 2, "snapshot")?,
        }),
        "health" => Ok(ApplicationInput::Health { correlation }),
        "recover-local" => policy_command(arguments, correlation, true),
        "recover-inconsistent" => inconsistent_policy_command(arguments, correlation),
        "release-local" => policy_command(arguments, correlation, false),
        "poll-execution" => Ok(ApplicationInput::PollExecution {
            correlation,
            operation: OperationKey(number(arguments, 2, "operation")?),
        }),
        "cancel" => Ok(ApplicationInput::Cancel {
            correlation,
            operation: OperationKey(number(arguments, 2, "operation")?),
        }),
        _ => Err(AdapterError::simple(
            AdapterErrorCode::UnknownAction,
            "action",
        )),
    }
}

fn expected_fields(action: &str) -> Option<usize> {
    match action {
        "generate" => Some(6),
        "status" | "locality" | "poll-execution" | "cancel" => Some(3),
        "search" => Some(5),
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

pub(crate) fn input_from_json(
    action: &str,
    correlation: u64,
    argument: impl Fn(&'static str) -> Result<String, AdapterError>,
) -> Result<ApplicationInput, AdapterError> {
    let correlation = CorrelationId(correlation);
    match action {
        "generate" => generate_input(correlation, &argument),
        "status" => status_input(correlation, &argument),
        "search" => search_input(correlation, &argument),
        "graph" => graph_input(correlation, &argument),
        "vector" => vector_input(correlation, &argument),
        "locality" => locality_input(correlation, &argument),
        "health" => Ok(ApplicationInput::Health { correlation }),
        "recover-local" => policy_input(correlation, &argument, true),
        "recover-inconsistent" => inconsistent_input(correlation, &argument),
        "release-local" => policy_input(correlation, &argument, false),
        "poll-execution" => operation_input(correlation, &argument, false),
        "cancel" => operation_input(correlation, &argument, true),
        _ => Err(AdapterError::simple(
            AdapterErrorCode::UnknownAction,
            "action",
        )),
    }
}

fn generate_input(
    correlation: CorrelationId,
    argument: &impl Fn(&'static str) -> Result<String, AdapterError>,
) -> Result<ApplicationInput, AdapterError> {
    Ok(ApplicationInput::Generate {
        correlation,
        language: input_text(&argument("language")?, "language")?,
        stage: input_text(&argument("stage")?, "stage")?,
        package: input_text(&argument("package")?, "package")?,
        source: input_text(&argument("source")?, "source")?,
    })
}

fn status_input(
    correlation: CorrelationId,
    argument: &impl Fn(&'static str) -> Result<String, AdapterError>,
) -> Result<ApplicationInput, AdapterError> {
    Ok(ApplicationInput::SnapshotStatus {
        correlation,
        snapshot: input_text(&argument("snapshot")?, "snapshot")?,
    })
}

fn search_input(
    correlation: CorrelationId,
    argument: &impl Fn(&'static str) -> Result<String, AdapterError>,
) -> Result<ApplicationInput, AdapterError> {
    Ok(ApplicationInput::Search {
        correlation,
        snapshot: input_text(&argument("snapshot")?, "snapshot")?,
        query: input_text(&argument("query")?, "query")?,
        limit: number_text(&argument("limit")?, "limit")?,
    })
}

fn graph_input(
    correlation: CorrelationId,
    argument: &impl Fn(&'static str) -> Result<String, AdapterError>,
) -> Result<ApplicationInput, AdapterError> {
    Ok(ApplicationInput::Graph {
        correlation,
        snapshot: input_text(&argument("snapshot")?, "snapshot")?,
        limit: number_text(&argument("limit")?, "limit")?,
    })
}

fn vector_input(
    correlation: CorrelationId,
    argument: &impl Fn(&'static str) -> Result<String, AdapterError>,
) -> Result<ApplicationInput, AdapterError> {
    Ok(ApplicationInput::Vector {
        correlation,
        snapshot: input_text(&argument("snapshot")?, "snapshot")?,
        limit: number_text(&argument("limit")?, "limit")?,
    })
}

fn locality_input(
    correlation: CorrelationId,
    argument: &impl Fn(&'static str) -> Result<String, AdapterError>,
) -> Result<ApplicationInput, AdapterError> {
    Ok(ApplicationInput::Locality {
        correlation,
        snapshot: input_text(&argument("snapshot")?, "snapshot")?,
    })
}

fn policy_input(
    correlation: CorrelationId,
    argument: &impl Fn(&'static str) -> Result<String, AdapterError>,
    recover: bool,
) -> Result<ApplicationInput, AdapterError> {
    let pin = pin_from_text(&argument("generation")?, &argument("snapshot")?)?;
    let bundle = canonical_content_id(&argument("bundle")?, "bundle")?;
    let budget = budget_from_text(argument)?;
    if recover {
        Ok(ApplicationInput::RecoverLocal {
            correlation,
            pin,
            bundle,
            budget,
        })
    } else {
        Ok(ApplicationInput::ReleaseLocal {
            correlation,
            pin,
            bundle,
            budget,
        })
    }
}

fn inconsistent_input(
    correlation: CorrelationId,
    argument: &impl Fn(&'static str) -> Result<String, AdapterError>,
) -> Result<ApplicationInput, AdapterError> {
    Ok(ApplicationInput::RecoverInconsistent(
        InconsistentRecovery {
            correlation,
            expected: pin_from_text(
                &argument("expected_generation")?,
                &argument("expected_snapshot")?,
            )?,
            observed: pin_from_text(
                &argument("observed_generation")?,
                &argument("observed_snapshot")?,
            )?,
            bundle: canonical_content_id(&argument("bundle")?, "bundle")?,
            budget: budget_from_text(argument)?,
        },
    ))
}

fn operation_input(
    correlation: CorrelationId,
    argument: &impl Fn(&'static str) -> Result<String, AdapterError>,
    cancel: bool,
) -> Result<ApplicationInput, AdapterError> {
    let operation = OperationKey(number_text(&argument("operation")?, "operation")?);
    if cancel {
        Ok(ApplicationInput::Cancel {
            correlation,
            operation,
        })
    } else {
        Ok(ApplicationInput::PollExecution {
            correlation,
            operation,
        })
    }
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

fn text(arguments: &[String], index: usize, name: &'static str) -> Result<InputText, AdapterError> {
    input_text(field(arguments, index, name)?, name)
}

fn input_text(value: &str, name: &'static str) -> Result<InputText, AdapterError> {
    InputText::try_from_str(value).map_err(|source| AdapterError::field_too_long(name, source))
}

fn number<T>(arguments: &[String], index: usize, name: &'static str) -> Result<T, AdapterError>
where
    T: core::str::FromStr<Err = ParseIntError>,
{
    number_text(field(arguments, index, name)?, name)
}

fn number_text<T>(value: &str, name: &'static str) -> Result<T, AdapterError>
where
    T: core::str::FromStr<Err = ParseIntError>,
{
    value
        .parse::<T>()
        .map_err(|source| AdapterError::invalid_number(name, source))
}

fn policy_command(
    arguments: &[String],
    correlation: CorrelationId,
    recover: bool,
) -> Result<ApplicationInput, AdapterError> {
    let pin = pin_from_text(
        field(arguments, 2, "generation")?,
        field(arguments, 3, "snapshot")?,
    )?;
    let bundle = canonical_content_id(field(arguments, 4, "bundle")?, "bundle")?;
    let budget = ResourceBudget {
        ram_free: bytes(arguments, 5, "ram_free")?,
        nvme_free: bytes(arguments, 6, "nvme_free")?,
        operations: operations(arguments, 7)?,
        retries: retries(arguments, 8)?,
        memory_pressure: pressure(field(arguments, 9, "memory_pressure")?, "memory_pressure")?,
        storage_pressure: pressure(
            field(arguments, 10, "storage_pressure")?,
            "storage_pressure",
        )?,
        battery: battery(field(arguments, 11, "battery")?)?,
    };
    if recover {
        Ok(ApplicationInput::RecoverLocal {
            correlation,
            pin,
            bundle,
            budget,
        })
    } else {
        Ok(ApplicationInput::ReleaseLocal {
            correlation,
            pin,
            bundle,
            budget,
        })
    }
}

fn inconsistent_policy_command(
    arguments: &[String],
    correlation: CorrelationId,
) -> Result<ApplicationInput, AdapterError> {
    let expected = pin_from_text(
        field(arguments, 2, "expected_generation")?,
        field(arguments, 3, "expected_snapshot")?,
    )?;
    let observed = pin_from_text(
        field(arguments, 4, "observed_generation")?,
        field(arguments, 5, "observed_snapshot")?,
    )?;
    let bundle = canonical_content_id(field(arguments, 6, "bundle")?, "bundle")?;
    let budget = ResourceBudget {
        ram_free: bytes(arguments, 7, "ram_free")?,
        nvme_free: bytes(arguments, 8, "nvme_free")?,
        operations: operations(arguments, 9)?,
        retries: retries(arguments, 10)?,
        memory_pressure: pressure(field(arguments, 11, "memory_pressure")?, "memory_pressure")?,
        storage_pressure: pressure(
            field(arguments, 12, "storage_pressure")?,
            "storage_pressure",
        )?,
        battery: battery(field(arguments, 13, "battery")?)?,
    };
    Ok(ApplicationInput::RecoverInconsistent(
        InconsistentRecovery {
            correlation,
            expected,
            observed,
            bundle,
            budget,
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

fn budget_from_text(
    argument: &impl Fn(&'static str) -> Result<String, AdapterError>,
) -> Result<ResourceBudget, AdapterError> {
    Ok(ResourceBudget {
        ram_free: bytes_text(&argument("ram_free")?, "ram_free")?,
        nvme_free: bytes_text(&argument("nvme_free")?, "nvme_free")?,
        operations: operations_text(&argument("operations")?)?,
        retries: retries_text(&argument("retries")?)?,
        memory_pressure: pressure(&argument("memory_pressure")?, "memory_pressure")?,
        storage_pressure: pressure(&argument("storage_pressure")?, "storage_pressure")?,
        battery: battery(&argument("battery")?)?,
    })
}

fn bytes(
    arguments: &[String],
    index: usize,
    name: &'static str,
) -> Result<ByteCount, AdapterError> {
    Ok(ByteCount::from(number::<u32>(arguments, index, name)?))
}

fn bytes_text(value: &str, name: &'static str) -> Result<ByteCount, AdapterError> {
    Ok(ByteCount::from(number_text::<u32>(value, name)?))
}

fn operations(arguments: &[String], index: usize) -> Result<OperationBudget, AdapterError> {
    Ok(OperationBudget::from(number::<u8>(
        arguments,
        index,
        "operations",
    )?))
}

fn operations_text(value: &str) -> Result<OperationBudget, AdapterError> {
    Ok(OperationBudget::from(number_text::<u8>(
        value,
        "operations",
    )?))
}

fn retries(arguments: &[String], index: usize) -> Result<RetryBudget, AdapterError> {
    Ok(RetryBudget::from(number::<u8>(
        arguments, index, "retries",
    )?))
}

fn retries_text(value: &str) -> Result<RetryBudget, AdapterError> {
    Ok(RetryBudget::from(number_text::<u8>(value, "retries")?))
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

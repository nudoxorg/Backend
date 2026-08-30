//! Command-line token decoding with no business behavior.

use std::{error::Error, fmt, num::ParseIntError};

use wave_application_core::{
    ApplicationInput, BatteryState, ByteCount, CapabilityDomain, ContentId, CorrelationId,
    GenerationId, INPUT_TEXT_BYTES, IndexSnapshotId, InputText, InputTextError, OperationBudget,
    OperationKey, Pin, Pressure, ResourceBudget, RetryBudget,
};

/// Largest positional CLI field count for the closed application command vocabulary.
pub const MAX_CLI_ARGUMENTS: usize = 12;
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
        }
    }
}

impl Error for AdapterErrorCause {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Json(source) => Some(source),
            Self::Number(source) => Some(source),
            Self::TextLength(_) => None,
        }
    }
}

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

impl Error for AdapterError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.cause
            .as_ref()
            .map(|source| source as &(dyn Error + 'static))
    }
}

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
        if argument.len() > INPUT_TEXT_BYTES {
            return Err(AdapterError::field_too_long(
                "argument",
                InputTextError {
                    actual: argument.len(),
                    maximum: INPUT_TEXT_BYTES,
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
        "generate" => Ok(ApplicationInput::Generate {
            correlation,
            language: input_text(&argument("language")?, "language")?,
            stage: input_text(&argument("stage")?, "stage")?,
            package: input_text(&argument("package")?, "package")?,
            source: input_text(&argument("source")?, "source")?,
        }),
        "status" => Ok(ApplicationInput::SnapshotStatus {
            correlation,
            snapshot: input_text(&argument("snapshot")?, "snapshot")?,
        }),
        "search" => Ok(ApplicationInput::Search {
            correlation,
            snapshot: input_text(&argument("snapshot")?, "snapshot")?,
            query: input_text(&argument("query")?, "query")?,
            limit: number_text(&argument("limit")?, "limit")?,
        }),
        "graph" => Ok(ApplicationInput::Graph {
            correlation,
            snapshot: input_text(&argument("snapshot")?, "snapshot")?,
            limit: number_text(&argument("limit")?, "limit")?,
        }),
        "vector" => Ok(ApplicationInput::Vector {
            correlation,
            snapshot: input_text(&argument("snapshot")?, "snapshot")?,
            limit: number_text(&argument("limit")?, "limit")?,
        }),
        "locality" => Ok(ApplicationInput::Locality {
            correlation,
            snapshot: input_text(&argument("snapshot")?, "snapshot")?,
        }),
        "health" => Ok(ApplicationInput::Health { correlation }),
        "recover-local" => Ok(ApplicationInput::RecoverLocal {
            correlation,
            pin: pin_from_text(&argument("generation")?, &argument("snapshot")?),
            bundle: ContentId::from_canonical_bytes(argument("bundle")?.as_bytes()),
            budget: budget_from_text(&argument)?,
        }),
        "release-local" => Ok(ApplicationInput::ReleaseLocal {
            correlation,
            pin: pin_from_text(&argument("generation")?, &argument("snapshot")?),
            bundle: ContentId::from_canonical_bytes(argument("bundle")?.as_bytes()),
            budget: budget_from_text(&argument)?,
        }),
        "poll-execution" => Ok(ApplicationInput::PollExecution {
            correlation,
            operation: OperationKey(number_text(&argument("operation")?, "operation")?),
        }),
        "cancel" => Ok(ApplicationInput::Cancel {
            correlation,
            operation: OperationKey(number_text(&argument("operation")?, "operation")?),
        }),
        _ => Err(AdapterError::simple(
            AdapterErrorCode::UnknownAction,
            "action",
        )),
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
    );
    let bundle = ContentId::<CapabilityDomain>::from_canonical_bytes(
        field(arguments, 4, "bundle")?.as_bytes(),
    );
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

fn pin_from_text(generation: &str, snapshot: &str) -> Pin {
    Pin {
        generation: GenerationId::from_canonical_bytes(generation.as_bytes()),
        snapshot: IndexSnapshotId::from_canonical_bytes(snapshot.as_bytes()),
    }
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

//! Closed serde DTOs for the MCP request grammar.
//!
//! JSON-RPC request identities are intentionally kept as the one dynamic edge: the protocol
//! allows string, number, and null identities, and the process must echo that value exactly. The
//! application tool arguments are an internally tagged closed action enum. Every action carries
//! its required transport fields in its own variant, so a field from one action cannot silently
//! satisfy another action.

use serde::{Deserialize, Deserializer};
use serde_json::Value;
use wave_application_core::{ApplicationInput, CorrelationId};

use crate::{AdapterError, AdapterErrorCode, cli::input_from_json};

/// Presence-aware JSON-RPC identity field used for both requests and cancellation parameters.
///
/// A missing identity and an explicit `null` identity have different JSON-RPC meaning: the former
/// is a notification, while the latter is a request whose response identity is `null`.
#[derive(Debug, Default)]
pub(super) enum RequestIdField {
    /// The field was not present in its containing object.
    #[default]
    Missing,
    /// The field was present, including when its value is JSON `null`.
    Present(Value),
}

impl<'de> Deserialize<'de> for RequestIdField {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Value::deserialize(deserializer).map(Self::Present)
    }
}

/// Typed top-level MCP request shape. Business validation is deliberately deferred to the shared
/// CLI/MCP input decoder after this shape has been accepted.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RequestDto {
    /// JSON-RPC protocol marker, checked by the adapter after decoding.
    #[serde(default)]
    pub(super) jsonrpc: Option<String>,
    /// Optional request identity; `RequestIdField::Missing` denotes a notification.
    #[serde(default)]
    pub(super) id: RequestIdField,
    /// Method token, mapped to the closed method vocabulary by the adapter.
    #[serde(default)]
    pub(super) method: Option<String>,
    /// Closed method parameter record.
    #[serde(default)]
    pub(super) params: Option<ParamsDto>,
}

/// Identity-only fallback used when a later typed field rejects. It lets the process retain the
/// original response id without making the whole request grammar dynamic.
#[derive(Debug, Deserialize)]
pub(super) struct RequestIdentityDto {
    /// Optional request identity.
    #[serde(default)]
    pub(super) id: RequestIdField,
}

/// MCP method parameters. The method determines which subset is meaningful; unknown keys are
/// rejected by serde before any application operation is constructed.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ParamsDto {
    /// Fixed application tool name for `tools/call`.
    #[serde(default)]
    pub(super) name: Option<String>,
    /// Closed application action record for `tools/call`.
    #[serde(default)]
    pub(super) arguments: Option<ApplicationArgumentsDto>,
    /// Original request identity for `$/cancelRequest`.
    #[serde(default, rename = "requestId")]
    pub(super) request_id: RequestIdField,
}

/// Closed application action DTO. Required fields are represented directly in each variant;
/// serde rejects a missing field or a field belonging to a different action before dispatch.
#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub(super) enum ApplicationArgumentsDto {
    /// Compiler vocabulary request.
    Generate {
        /// Request correlation.
        correlation: u64,
        /// Compiler language token.
        language: String,
        /// Compiler stage token.
        stage: String,
        /// Package name.
        package: String,
        /// Source text.
        source: String,
    },
    /// Snapshot status request.
    #[serde(rename = "status")]
    SnapshotStatus {
        /// Request correlation.
        correlation: u64,
        /// Snapshot selector.
        snapshot: String,
    },
    /// Lexical retrieval request.
    Search {
        /// Request correlation.
        correlation: u64,
        /// Snapshot selector.
        snapshot: String,
        /// Lexical query.
        query: String,
        /// Result limit as a native number or decimal text.
        limit: JsonArgument,
    },
    /// Graph retrieval request.
    Graph {
        /// Request correlation.
        correlation: u64,
        /// Snapshot selector.
        snapshot: String,
        /// Result limit as a native number or decimal text.
        limit: JsonArgument,
    },
    /// Vector retrieval request.
    Vector {
        /// Request correlation.
        correlation: u64,
        /// Snapshot selector.
        snapshot: String,
        /// Result limit as a native number or decimal text.
        limit: JsonArgument,
    },
    /// Locality request.
    Locality {
        /// Request correlation.
        correlation: u64,
        /// Snapshot selector.
        snapshot: String,
    },
    /// Capability health request.
    Health {
        /// Request correlation.
        correlation: u64,
    },
    /// Local recovery policy request.
    RecoverLocal {
        /// Request correlation.
        correlation: u64,
        /// Generation authority.
        generation: String,
        /// Snapshot authority.
        snapshot: String,
        /// Analyzer bundle identity.
        bundle: String,
        /// Free RAM budget.
        ram_free: JsonArgument,
        /// Free `NVMe` budget.
        nvme_free: JsonArgument,
        /// Operation budget.
        operations: JsonArgument,
        /// Retry budget.
        retries: JsonArgument,
        /// Memory pressure token.
        memory_pressure: String,
        /// Storage pressure token.
        storage_pressure: String,
        /// Battery state token.
        battery: String,
    },
    /// Inconsistent remote recovery policy request.
    RecoverInconsistent {
        /// Request correlation.
        correlation: u64,
        /// Expected generation authority.
        expected_generation: String,
        /// Expected snapshot authority.
        expected_snapshot: String,
        /// Observed generation authority.
        observed_generation: String,
        /// Observed snapshot authority.
        observed_snapshot: String,
        /// Analyzer bundle identity.
        bundle: String,
        /// Free RAM budget.
        ram_free: JsonArgument,
        /// Free `NVMe` budget.
        nvme_free: JsonArgument,
        /// Operation budget.
        operations: JsonArgument,
        /// Retry budget.
        retries: JsonArgument,
        /// Memory pressure token.
        memory_pressure: String,
        /// Storage pressure token.
        storage_pressure: String,
        /// Battery state token.
        battery: String,
    },
    /// Local release policy request.
    ReleaseLocal {
        /// Request correlation.
        correlation: u64,
        /// Generation authority.
        generation: String,
        /// Snapshot authority.
        snapshot: String,
        /// Analyzer bundle identity.
        bundle: String,
        /// Free RAM budget.
        ram_free: JsonArgument,
        /// Free `NVMe` budget.
        nvme_free: JsonArgument,
        /// Operation budget.
        operations: JsonArgument,
        /// Retry budget.
        retries: JsonArgument,
        /// Memory pressure token.
        memory_pressure: String,
        /// Storage pressure token.
        storage_pressure: String,
        /// Battery state token.
        battery: String,
    },
    /// Execution polling request.
    PollExecution {
        /// Request correlation.
        correlation: u64,
        /// Service execution operation key.
        operation: JsonArgument,
    },
    /// Execution cancellation request.
    Cancel {
        /// Request correlation.
        correlation: u64,
        /// Service execution operation key.
        operation: JsonArgument,
    },
}

impl ApplicationArgumentsDto {
    /// Converts one shape-checked action through the shared CLI/MCP semantic decoder.
    pub(super) fn application_input(&self) -> Result<ApplicationInput, AdapterError> {
        match self {
            Self::Generate { .. } => generate_input(self),
            Self::SnapshotStatus { .. } => snapshot_input(self, "status"),
            Self::Search { .. } => search_input(self),
            Self::Graph { .. } => retrieval_input(self, "graph"),
            Self::Vector { .. } => retrieval_input(self, "vector"),
            Self::Locality { .. } => snapshot_input(self, "locality"),
            Self::Health { correlation } => Ok(ApplicationInput::Health {
                correlation: CorrelationId(*correlation),
            }),
            Self::RecoverLocal { .. } => policy_input(self, "recover-local"),
            Self::RecoverInconsistent { .. } => inconsistent_input(self),
            Self::ReleaseLocal { .. } => policy_input(self, "release-local"),
            Self::PollExecution { .. } => operation_input(self, "poll-execution"),
            Self::Cancel { .. } => operation_input(self, "cancel"),
        }
    }
}

fn generate_input(arguments: &ApplicationArgumentsDto) -> Result<ApplicationInput, AdapterError> {
    let ApplicationArgumentsDto::Generate {
        correlation,
        language,
        stage,
        package,
        source,
    } = arguments
    else {
        return invalid_argument("action");
    };
    input_from_json("generate", *correlation, |name| match name {
        "language" => Ok(language.clone()),
        "stage" => Ok(stage.clone()),
        "package" => Ok(package.clone()),
        "source" => Ok(source.clone()),
        _ => invalid_argument(name),
    })
}

fn snapshot_input(
    arguments: &ApplicationArgumentsDto,
    action: &'static str,
) -> Result<ApplicationInput, AdapterError> {
    let (correlation, snapshot) = match arguments {
        ApplicationArgumentsDto::SnapshotStatus {
            correlation,
            snapshot,
        }
        | ApplicationArgumentsDto::Locality {
            correlation,
            snapshot,
        } => (*correlation, snapshot),
        _ => return invalid_argument("action"),
    };
    input_from_json(action, correlation, |name| match name {
        "snapshot" => Ok(snapshot.clone()),
        _ => invalid_argument(name),
    })
}

fn search_input(arguments: &ApplicationArgumentsDto) -> Result<ApplicationInput, AdapterError> {
    let ApplicationArgumentsDto::Search {
        correlation,
        snapshot,
        query,
        limit,
    } = arguments
    else {
        return invalid_argument("action");
    };
    input_from_json("search", *correlation, |name| match name {
        "snapshot" => Ok(snapshot.clone()),
        "query" => Ok(query.clone()),
        "limit" => Ok(limit.as_text()),
        _ => invalid_argument(name),
    })
}

fn retrieval_input(
    arguments: &ApplicationArgumentsDto,
    action: &'static str,
) -> Result<ApplicationInput, AdapterError> {
    let (correlation, snapshot, limit) = match arguments {
        ApplicationArgumentsDto::Graph {
            correlation,
            snapshot,
            limit,
        }
        | ApplicationArgumentsDto::Vector {
            correlation,
            snapshot,
            limit,
        } => (*correlation, snapshot, limit),
        _ => return invalid_argument("action"),
    };
    input_from_json(action, correlation, |name| match name {
        "snapshot" => Ok(snapshot.clone()),
        "limit" => Ok(limit.as_text()),
        _ => invalid_argument(name),
    })
}

fn operation_input(
    arguments: &ApplicationArgumentsDto,
    action: &'static str,
) -> Result<ApplicationInput, AdapterError> {
    let (correlation, operation) = match arguments {
        ApplicationArgumentsDto::PollExecution {
            correlation,
            operation,
        }
        | ApplicationArgumentsDto::Cancel {
            correlation,
            operation,
        } => (*correlation, operation),
        _ => return invalid_argument("action"),
    };
    input_from_json(action, correlation, |name| match name {
        "operation" => Ok(operation.as_text()),
        _ => invalid_argument(name),
    })
}

fn policy_input(
    arguments: &ApplicationArgumentsDto,
    action: &'static str,
) -> Result<ApplicationInput, AdapterError> {
    let fields = policy_fields(arguments)?;
    input_from_json(action, fields.correlation, |name| match name {
        "generation" => Ok(fields.generation.to_owned()),
        "snapshot" => Ok(fields.snapshot.to_owned()),
        "bundle" => Ok(fields.bundle.to_owned()),
        "ram_free" => Ok(fields.ram_free.as_text()),
        "nvme_free" => Ok(fields.nvme_free.as_text()),
        "operations" => Ok(fields.operations.as_text()),
        "retries" => Ok(fields.retries.as_text()),
        "memory_pressure" => Ok(fields.memory_pressure.to_owned()),
        "storage_pressure" => Ok(fields.storage_pressure.to_owned()),
        "battery" => Ok(fields.battery.to_owned()),
        _ => invalid_argument(name),
    })
}

struct PolicyFields<'a> {
    correlation: u64,
    generation: &'a str,
    snapshot: &'a str,
    bundle: &'a str,
    ram_free: &'a JsonArgument,
    nvme_free: &'a JsonArgument,
    operations: &'a JsonArgument,
    retries: &'a JsonArgument,
    memory_pressure: &'a str,
    storage_pressure: &'a str,
    battery: &'a str,
}

fn policy_fields(arguments: &ApplicationArgumentsDto) -> Result<PolicyFields<'_>, AdapterError> {
    match arguments {
        ApplicationArgumentsDto::RecoverLocal {
            correlation,
            generation,
            snapshot,
            bundle,
            ram_free,
            nvme_free,
            operations,
            retries,
            memory_pressure,
            storage_pressure,
            battery,
        }
        | ApplicationArgumentsDto::ReleaseLocal {
            correlation,
            generation,
            snapshot,
            bundle,
            ram_free,
            nvme_free,
            operations,
            retries,
            memory_pressure,
            storage_pressure,
            battery,
        } => Ok(PolicyFields {
            correlation: *correlation,
            generation,
            snapshot,
            bundle,
            ram_free,
            nvme_free,
            operations,
            retries,
            memory_pressure,
            storage_pressure,
            battery,
        }),
        _ => invalid_argument("action"),
    }
}

fn inconsistent_input(
    arguments: &ApplicationArgumentsDto,
) -> Result<ApplicationInput, AdapterError> {
    let fields = inconsistent_fields(arguments)?;
    input_from_json(
        "recover-inconsistent",
        fields.correlation,
        |name| match name {
            "expected_generation" => Ok(fields.expected_generation.to_owned()),
            "expected_snapshot" => Ok(fields.expected_snapshot.to_owned()),
            "observed_generation" => Ok(fields.observed_generation.to_owned()),
            "observed_snapshot" => Ok(fields.observed_snapshot.to_owned()),
            "bundle" => Ok(fields.bundle.to_owned()),
            "ram_free" => Ok(fields.ram_free.as_text()),
            "nvme_free" => Ok(fields.nvme_free.as_text()),
            "operations" => Ok(fields.operations.as_text()),
            "retries" => Ok(fields.retries.as_text()),
            "memory_pressure" => Ok(fields.memory_pressure.to_owned()),
            "storage_pressure" => Ok(fields.storage_pressure.to_owned()),
            "battery" => Ok(fields.battery.to_owned()),
            _ => invalid_argument(name),
        },
    )
}

struct InconsistentFields<'a> {
    correlation: u64,
    expected_generation: &'a str,
    expected_snapshot: &'a str,
    observed_generation: &'a str,
    observed_snapshot: &'a str,
    bundle: &'a str,
    ram_free: &'a JsonArgument,
    nvme_free: &'a JsonArgument,
    operations: &'a JsonArgument,
    retries: &'a JsonArgument,
    memory_pressure: &'a str,
    storage_pressure: &'a str,
    battery: &'a str,
}

fn inconsistent_fields(
    arguments: &ApplicationArgumentsDto,
) -> Result<InconsistentFields<'_>, AdapterError> {
    match arguments {
        ApplicationArgumentsDto::RecoverInconsistent {
            correlation,
            expected_generation,
            expected_snapshot,
            observed_generation,
            observed_snapshot,
            bundle,
            ram_free,
            nvme_free,
            operations,
            retries,
            memory_pressure,
            storage_pressure,
            battery,
        } => Ok(InconsistentFields {
            correlation: *correlation,
            expected_generation,
            expected_snapshot,
            observed_generation,
            observed_snapshot,
            bundle,
            ram_free,
            nvme_free,
            operations,
            retries,
            memory_pressure,
            storage_pressure,
            battery,
        }),
        _ => invalid_argument("action"),
    }
}

/// A numeric transport field that accepts the two representations already supported by MCP: a
/// JSON unsigned integer or a decimal string. Semantic width and range remain in `input_from_json`.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(super) enum JsonArgument {
    /// Native JSON unsigned integer.
    Number(u64),
    /// Decimal text retained for the common CLI/MCP vocabulary.
    Text(String),
}

impl JsonArgument {
    fn as_text(&self) -> String {
        match self {
            Self::Number(value) => value.to_string(),
            Self::Text(value) => value.clone(),
        }
    }
}

fn invalid_argument<T>(name: &'static str) -> Result<T, AdapterError> {
    Err(AdapterError::simple(AdapterErrorCode::InvalidShape, name))
}

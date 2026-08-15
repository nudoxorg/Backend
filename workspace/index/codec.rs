//! Codec glue between typed row fields and the engine facade's [`Value`]/[`Row`].
//!
//! Table modules bind columns through these helpers so every conversion is
//! total and centralized: a `TEXT`-enum column binds via [`bind_text_enum`] and
//! reads back via [`read_text_enum`]; a `BLOB16`/`BLOB32` id binds via
//! `to_blob().to_vec()` and reads via `from_blob`. Nothing here panics; every
//! decode error is a typed [`CodecError`].

use crate::enums::{TextEnum, TextEnumError};
use crate::ids::IdDecodeError;
use crate::engine::{EngineError, Value};

/// Why a stored row could not be decoded into typed fields.
#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    /// A `TEXT`-enum column held an unknown token.
    #[error(transparent)]
    TextEnum(#[from] TextEnumError),
    /// A `BLOB16`/`BLOB32` id column held a wrong-length blob.
    #[error(transparent)]
    Id(#[from] IdDecodeError),
    /// The engine failed to yield a column.
    #[error(transparent)]
    Engine(#[from] EngineError),
    /// A JSON-typed `TEXT` column failed to parse.
    #[error("malformed JSON column: {0}")]
    Json(String),
}

/// Bind a `TEXT`-enum value as its stored token.
pub fn bind_text_enum<E: TextEnum>(value: E) -> Value {
    Value::Text(value.as_token().to_owned())
}

/// Read a `TEXT`-enum value from its stored token.
pub fn read_text_enum<E: TextEnum>(token: &str) -> Result<E, CodecError> {
    E::from_token(token).map_err(CodecError::from)
}

/// Bind an optional `TEXT`-enum column.
pub fn bind_optional_text_enum<E: TextEnum>(value: Option<E>) -> Value {
    value.map_or(Value::Null, bind_text_enum)
}

/// Bind an optional owned string as `TEXT`.
pub fn bind_optional_text(value: Option<String>) -> Value {
    value.map_or(Value::Null, Value::Text)
}

/// Bind an optional integer.
pub fn bind_optional_integer(value: Option<i64>) -> Value {
    value.map_or(Value::Null, Value::Integer)
}

/// Bind a boolean as `INTEGER` 0/1 (schema v4 stores flags as INTEGER).
pub fn bind_bool(value: bool) -> Value {
    Value::Integer(i64::from(value))
}

/// Read a boolean from an `INTEGER` 0/1 column.
pub fn read_bool(value: i64) -> bool {
    value != 0
}

/// Serialize a value to a JSON `TEXT` column.
pub fn bind_json<T: serde::Serialize>(value: &T) -> Result<Value, CodecError> {
    serde_json::to_string(value)
        .map(Value::Text)
        .map_err(|error| CodecError::Json(error.to_string()))
}

/// Deserialize a JSON `TEXT` column.
pub fn read_json<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, CodecError> {
    serde_json::from_str(text).map_err(|error| CodecError::Json(error.to_string()))
}

impl From<CodecError> for EngineError {
    /// Collapse a codec failure into the facade's error so a row-mapping closure
    /// (which must return [`EngineError`]) can propagate a decode failure. The
    /// store layer re-wraps [`EngineError`] into
    /// [`MetaError`](crate::store::MetaError) at the boundary, so no information
    /// is lost to the caller.
    fn from(error: CodecError) -> Self {
        match error {
            CodecError::Engine(inner) => inner,
            other => EngineError::Statement(other.to_string()),
        }
    }
}

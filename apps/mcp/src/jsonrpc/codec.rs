//! Bounded JSON-RPC value, argument, URI, and newline framing helpers.

use super::RpcError;
use backend_library::{GraphValue, MAX_GRAPH_VALUE_DEPTH};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::io::{self, BufRead, Write};

/// Maximum JSON nesting accepted at the JSON-RPC boundary.
///
/// The request frame is bounded, but a tiny deeply nested value could still
/// exhaust the stack in a recursive argument or context projection. The
/// iterative check keeps that hostile shape outside all recursive helpers.
pub(super) const MAX_JSON_DEPTH: usize = 32;

pub(super) fn success(id: Value, result: Value) -> Value {
    let mut reply = Map::with_capacity(3);
    reply.insert("jsonrpc".to_owned(), Value::String("2.0".to_owned()));
    reply.insert("id".to_owned(), id);
    reply.insert("result".to_owned(), result);
    Value::Object(reply)
}

pub(super) fn error_reply(id: Value, code: i64, message: &str, data: Option<Value>) -> Value {
    let mut error = json!({ "code": code, "message": message });
    if let Some(data) = data {
        error["data"] = data;
    }
    let mut reply = Map::with_capacity(3);
    reply.insert("jsonrpc".to_owned(), Value::String("2.0".to_owned()));
    reply.insert("id".to_owned(), id);
    reply.insert("error".to_owned(), error);
    Value::Object(reply)
}

pub(super) fn valid_id(id: &Value) -> bool {
    matches!(id, Value::String(_) | Value::Number(_))
}

pub(super) fn object(value: &Value) -> Result<&Map<String, Value>, RpcError> {
    value
        .as_object()
        .ok_or_else(|| RpcError::invalid("params must be an object"))
}

pub(super) fn string<'a>(object: &'a Map<String, Value>, key: &str) -> Result<&'a str, RpcError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| RpcError::invalid(format!("{key} must be a non-empty string")))
}

pub(super) fn limit(arguments: &Map<String, Value>) -> Result<u16, RpcError> {
    match arguments.get("limit") {
        None => Ok(backend_present::DEFAULT_LIMIT),
        Some(value) => value
            .as_u64()
            .and_then(|value| u16::try_from(value).ok())
            .filter(|value| (1..=backend_library::QueryLimit::MAX).contains(value))
            .ok_or_else(|| {
                RpcError::invalid(format!(
                    "limit must be an integer from 1 through {}",
                    backend_library::QueryLimit::MAX
                ))
            }),
    }
}

pub(super) fn query_variables(
    arguments: &Map<String, Value>,
) -> Result<BTreeMap<String, GraphValue>, RpcError> {
    match arguments.get("variables") {
        None | Some(Value::Null) => Ok(BTreeMap::new()),
        Some(Value::Object(variables)) => variables
            .iter()
            .map(|(name, value)| query_value(value, 0).map(|value| (name.clone(), value)))
            .collect(),
        Some(_) => Err(RpcError::invalid("variables must be an object")),
    }
}

fn query_value(value: &Value, depth: usize) -> Result<GraphValue, RpcError> {
    if depth > MAX_GRAPH_VALUE_DEPTH {
        return Err(RpcError::invalid(
            "query variable exceeds the graph value nesting bound",
        ));
    }
    match value {
        Value::Null => Ok(GraphValue::Null),
        Value::Bool(value) => Ok(GraphValue::Boolean(*value)),
        Value::Number(value) => value
            .as_i64()
            .map(GraphValue::Signed)
            .or_else(|| value.as_u64().map(GraphValue::Unsigned))
            .or_else(|| {
                value
                    .as_f64()
                    .and_then(|value| GraphValue::finite_float(value).ok())
            })
            .ok_or_else(|| RpcError::invalid("query variable number is not finite")),
        Value::String(value) => Ok(GraphValue::String(value.clone())),
        Value::Array(values) => values
            .iter()
            .map(|value| query_value(value, depth.saturating_add(1)))
            .collect::<Result<Vec<_>, _>>()
            .map(|values| GraphValue::List(values.into_boxed_slice())),
        Value::Object(_) => Err(RpcError::invalid(
            "query variables may be scalars or lists, not objects",
        )),
    }
}

pub(super) fn no_extra(arguments: &Map<String, Value>, allowed: &[&str]) -> Result<(), RpcError> {
    if let Some(key) = arguments
        .keys()
        .find(|key| !allowed.contains(&key.as_str()))
    {
        Err(RpcError::invalid(format!("unknown argument: {key}")))
    } else {
        Ok(())
    }
}

pub(super) fn empty_cursor(params: &Value) -> Result<(), RpcError> {
    let params = object(params)?;
    if params.get("cursor").is_some_and(|value| !value.is_null()) {
        Err(RpcError::invalid(
            "this bounded list has no continuation cursor",
        ))
    } else {
        Ok(())
    }
}

pub(super) fn percent_encode(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            output.push(char::from(byte));
        } else {
            output.push('%');
            output.push(hex_digit(byte >> 4));
            output.push(hex_digit(byte & 0x0f));
        }
    }
    output
}

pub(super) fn percent_decode(value: &str) -> Result<String, RpcError> {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let high = bytes.get(at + 1).and_then(|byte| hex_value(*byte));
            let low = bytes.get(at + 2).and_then(|byte| hex_value(*byte));
            let (Some(high), Some(low)) = (high, low) else {
                return Err(RpcError::invalid(
                    "resource URI has invalid percent encoding",
                ));
            };
            output.push(high * 16 + low);
            at += 3;
        } else {
            output.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8(output).map_err(|_| RpcError::invalid("resource URI is not UTF-8"))
}

const fn hex_digit(value: u8) -> char {
    match value {
        0..=9 => (b'0' + value) as char,
        _ => (b'A' + value - 10) as char,
    }
}

const fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

pub(super) fn read_line(reader: &mut impl BufRead) -> io::Result<Option<Vec<u8>>> {
    let mut output = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            if output.len() > crate::MAX_MCP_REQUEST_FRAME {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "MCP message exceeds the bounded frame",
                ));
            }
            return if output.is_empty() {
                Ok(None)
            } else {
                Ok(Some(output))
            };
        }
        let end = available.iter().position(|byte| *byte == b'\n');
        let take = end.map_or(available.len(), |position| position + 1);
        if output.len().saturating_add(take) > crate::MAX_MCP_REQUEST_FRAME.saturating_add(1) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "MCP message exceeds the bounded frame",
            ));
        }
        output.extend_from_slice(&available[..take]);
        reader.consume(take);
        if end.is_some() {
            output.pop();
            if output.last() == Some(&b'\r') {
                output.pop();
            }
            return Ok(Some(output));
        }
    }
}

/// Checks JSON nesting without recursing into the untrusted value.
pub(super) fn json_depth_within(value: &Value) -> bool {
    let mut pending = vec![(value, 0_usize)];
    while let Some((value, depth)) = pending.pop() {
        if depth > MAX_JSON_DEPTH {
            return false;
        }
        match value {
            Value::Array(values) => {
                pending.extend(values.iter().map(|value| (value, depth.saturating_add(1))))
            }
            Value::Object(fields) => pending.extend(
                fields
                    .values()
                    .map(|value| (value, depth.saturating_add(1))),
            ),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }
    true
}

pub(super) fn write_message(writer: &mut impl Write, value: &Value) -> io::Result<()> {
    let mut body = BoundedBuffer::new(crate::MAX_MCP_RESPONSE_FRAME);
    serde_json::to_writer(&mut body, value).map_err(io::Error::other)?;
    writer.write_all(&body.bytes)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

struct BoundedBuffer {
    bytes: Vec<u8>,
    limit: usize,
}

impl BoundedBuffer {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(limit.min(64 * 1024)),
            limit,
        }
    }
}

impl Write for BoundedBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.bytes.len().saturating_add(bytes.len()) > self.limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "MCP response exceeds the bounded frame",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

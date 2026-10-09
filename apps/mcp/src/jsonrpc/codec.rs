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
        .ok_or_else(|| RpcError::invalid_argument(key, format!("{key} must be a non-empty string")))
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

/// Framing is selected at the encoding boundary, after semantic dispatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResponseTransport {
    HttpBody,
    StdioLine,
}

impl ResponseTransport {
    pub(crate) const fn limit_bytes(self) -> usize {
        match self {
            Self::HttpBody | Self::StdioLine => {
                if super::MCP_RESULT_BUDGET_BYTES < crate::MAX_MCP_RESPONSE_FRAME {
                    super::MCP_RESULT_BUDGET_BYTES
                } else {
                    crate::MAX_MCP_RESPONSE_FRAME
                }
            }
        }
    }

    pub(super) const fn scope(self) -> &'static str {
        match self {
            Self::HttpBody => "complete_jsonrpc_body",
            Self::StdioLine => "complete_jsonrpc_line",
        }
    }

    pub(super) const fn encoded_len(self, json_bytes: usize) -> Option<usize> {
        match self {
            Self::HttpBody => Some(json_bytes),
            Self::StdioLine => json_bytes.checked_add(1),
        }
    }
}

/// A finalized reply owns both the admitted disposition and its exact wire
/// bytes. Session admission must depend on this disposition, since a semantic
/// success can become a budget fault while encoding its caller's envelope.
pub(crate) struct EncodedResponse {
    bytes: Vec<u8>,
    is_result: bool,
}

impl EncodedResponse {
    pub(crate) const fn is_result(&self) -> bool {
        self.is_result
    }

    pub(crate) fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

/// Admit and serialize exactly the bytes this transport will emit. The same
/// path finalizes normal replies, tool refusals, protocol errors and fallback
/// errors; neither transport serializes or adds framing a second time.
pub(crate) fn finalize_response(
    value: Value,
    transport: ResponseTransport,
) -> io::Result<EncodedResponse> {
    let value = super::bound_rpc_reply(value, transport);
    let is_result = value.get("result").is_some();
    let mut body = BoundedBuffer::new(transport.limit_bytes());
    serde_json::to_writer(&mut body, &value).map_err(io::Error::other)?;
    if transport == ResponseTransport::StdioLine {
        body.write_all(b"\n")?;
    }
    Ok(EncodedResponse {
        bytes: body.bytes,
        is_result,
    })
}

pub(crate) fn encode_response(value: Value, transport: ResponseTransport) -> io::Result<Vec<u8>> {
    finalize_response(value, transport).map(EncodedResponse::into_bytes)
}

pub(super) fn write_message(writer: &mut impl Write, value: &Value) -> io::Result<()> {
    let bytes = encode_response(value.clone(), ResponseTransport::StdioLine)?;
    writer.write_all(&bytes)?;
    writer.flush()
}

#[derive(Default)]
pub(super) struct ResponseByteCounter {
    bytes: usize,
}

impl ResponseByteCounter {
    pub(super) const fn bytes(&self) -> usize {
        self.bytes
    }
}

impl Write for ResponseByteCounter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes = self.bytes.checked_add(bytes.len()).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "MCP response byte count overflow",
            )
        })?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
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
        if self
            .bytes
            .len()
            .checked_add(bytes.len())
            .is_none_or(|length| length > self.limit)
        {
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

#[cfg(test)]
mod tests {
    use super::super::{MCP_RESULT_BUDGET_BYTES, attach_wire_budget};
    use super::*;
    use backend_present::{ESTIMATED_BYTES_PER_TOKEN, estimate_tokens};

    fn budget(reply: &Value) -> &Value {
        if reply.get("result").is_some() {
            &reply["result"]["_meta"]["backend/wireBudget"]
        } else {
            &reply["error"]["data"]["_meta"]["backend/wireBudget"]
        }
    }

    fn decoded(bytes: &[u8], transport: ResponseTransport) -> Value {
        assert!(bytes.len() <= MCP_RESULT_BUDGET_BYTES);
        assert_eq!(
            bytes.last() == Some(&b'\n'),
            transport == ResponseTransport::StdioLine
        );
        let reply: Value = serde_json::from_slice(bytes).expect("whole encoded JSON-RPC reply");
        let budget = budget(&reply);
        assert_eq!(budget["bytes"], bytes.len());
        assert_eq!(budget["estimatedTokens"], estimate_tokens(bytes.len()));
        assert_eq!(budget["bytesPerToken"], ESTIMATED_BYTES_PER_TOKEN);
        assert_eq!(budget["limitBytes"], MCP_RESULT_BUDGET_BYTES);
        assert_eq!(budget["scope"], transport.scope());
        reply
    }

    fn reply_at_size(target: usize, transport: ResponseTransport) -> Value {
        let mut reply = success(json!("request-42"), json!({"payload":""}));
        let base = super::super::transport_serialized_bytes(
            &attach_wire_budget(reply.clone(), transport),
            transport,
        );
        let mut padding = target.saturating_sub(base);
        for _ in 0..8 {
            reply["result"]["payload"] = json!("p".repeat(padding));
            let bytes = super::super::transport_serialized_bytes(
                &attach_wire_budget(reply.clone(), transport),
                transport,
            );
            if bytes == target {
                return reply;
            }
            padding = if bytes < target {
                padding + target - bytes
            } else {
                padding - (bytes - target)
            };
        }
        assert_eq!(
            super::super::transport_serialized_bytes(
                &attach_wire_budget(reply.clone(), transport),
                transport
            ),
            target,
            "exact boundary fixture"
        );
        reply
    }

    #[test]
    fn transport_lengths_and_counting_refuse_integer_overflow() {
        assert_eq!(
            ResponseTransport::HttpBody.encoded_len(usize::MAX),
            Some(usize::MAX)
        );
        assert_eq!(ResponseTransport::StdioLine.encoded_len(usize::MAX), None);
        let mut count = ResponseByteCounter { bytes: usize::MAX };
        assert_eq!(
            count.write(b"x").expect_err("overflow refuses").kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(count.bytes(), usize::MAX);
        let mut buffer = BoundedBuffer::new(0);
        assert!(buffer.write_all(b"\n").is_err());
        assert!(buffer.bytes.is_empty());
    }

    #[test]
    fn emitted_tool_success_and_refusal_metadata_preserve_escaped_payloads() {
        for transport in [ResponseTransport::HttpBody, ResponseTransport::StdioLine] {
            for is_error in [false, true] {
                let structured = json!({"unicode":"λ אב🙂", "escaped":"\\\"\n\r\t", "nextCursor":"exact-owner-cursor"});
                let reply = success(
                    json!("request-λ-אב🙂"),
                    json!({
                        "content":[{"type":"text","text":"λ אב🙂\n\\\""}],
                        "structuredContent":structured,
                        "isError":is_error,
                        "_meta":{"owner":"retained", "backend/wireBudget":{"bytes":999999, "scope":"stale"}}
                    }),
                );
                let bytes = match transport {
                    ResponseTransport::HttpBody => {
                        encode_response(reply, transport).expect("encoded body")
                    }
                    ResponseTransport::StdioLine => {
                        let mut line = Vec::new();
                        write_message(&mut line, &reply).expect("production stdio writer");
                        line
                    }
                };
                let emitted = decoded(&bytes, transport);
                assert_eq!(emitted["id"], "request-λ-אב🙂");
                assert_eq!(emitted["result"]["structuredContent"], structured);
                assert_eq!(emitted["result"]["isError"], is_error);
                assert_eq!(emitted["result"]["_meta"]["owner"], "retained");
                assert_eq!(
                    encode_response(emitted, transport).expect("idempotent encoding"),
                    bytes
                );
            }
        }
    }

    #[test]
    fn transport_preview_fitting_preserves_full_structured_page_and_owner_cursor() {
        let structured = json!({
            "rows":"r".repeat(31_000), "terminal":"limit_reached",
            "nextCursor":"exact-owner-cursor", "more":true
        });
        let text = "λ אב🙂\n\\\"".repeat(10_000);
        let reply = success(
            json!(format!("request-{}", "אב🙂".repeat(1000))),
            json!({
                "content":[{"type":"text", "text":text}],
                "structuredContent":structured, "isError":false
            }),
        );
        for transport in [ResponseTransport::HttpBody, ResponseTransport::StdioLine] {
            let bytes = encode_response(reply.clone(), transport).expect("bounded preview");
            let emitted = decoded(&bytes, transport);
            assert_eq!(emitted["result"]["structuredContent"], structured);
            assert_eq!(emitted["id"], reply["id"]);
            assert_eq!(emitted["result"]["isError"], false);
            let preview = emitted["result"]["content"][0]["text"]
                .as_str()
                .expect("readable preview");
            assert!(preview.starts_with("λ אב🙂\n"));
            assert!(preview.ends_with(super::super::PREVIEW_MARKER));
            assert!(preview.len() < text.len());
        }
    }

    #[test]
    fn metadata_fixed_point_crosses_decimal_widths_and_the_exact_context_ceiling() {
        for transport in [ResponseTransport::HttpBody, ResponseTransport::StdioLine] {
            for target in [
                999,
                1001,
                9999,
                10001,
                MCP_RESULT_BUDGET_BYTES - 1,
                MCP_RESULT_BUDGET_BYTES,
                MCP_RESULT_BUDGET_BYTES + 1,
            ] {
                let reply = reply_at_size(target, transport);
                let bytes = encode_response(reply.clone(), transport).expect("bounded encoding");
                let emitted = decoded(&bytes, transport);
                assert_eq!(emitted["id"], reply["id"]);
                if target <= MCP_RESULT_BUDGET_BYTES {
                    assert_eq!(bytes.len(), target);
                    assert_eq!(emitted["result"]["payload"], reply["result"]["payload"]);
                    assert!(emitted.get("error").is_none());
                } else {
                    assert_eq!(emitted["error"]["code"], -32000);
                    assert!(emitted.get("result").is_none());
                }
            }
            // The count's own added digit creates a two-byte jump. Exact
            // 1000/10000-byte minimal fixed points are unreachable for this
            // one-byte padding family; observe the real encoded transition.
            for lower in [999, 9999] {
                let mut reply = reply_at_size(lower, transport);
                let payload = reply["result"]["payload"]
                    .as_str()
                    .expect("ASCII padding")
                    .to_owned();
                reply["result"]["payload"] = json!(payload + "p");
                let bytes = encode_response(reply, transport).expect("decimal-width transition");
                decoded(&bytes, transport);
                assert_eq!(bytes.len(), lower + 2);
            }
        }
    }

    #[test]
    fn transport_admission_counts_only_the_framing_it_emits() {
        for http_size in [MCP_RESULT_BUDGET_BYTES - 1, MCP_RESULT_BUDGET_BYTES] {
            let reply = reply_at_size(http_size, ResponseTransport::HttpBody);
            let http = decoded(
                &encode_response(reply.clone(), ResponseTransport::HttpBody).expect("HTTP body"),
                ResponseTransport::HttpBody,
            );
            let stdio_bytes =
                encode_response(reply, ResponseTransport::StdioLine).expect("stdio line");
            let stdio = decoded(&stdio_bytes, ResponseTransport::StdioLine);
            assert!(http.get("result").is_some());
            if http_size == MCP_RESULT_BUDGET_BYTES - 1 {
                assert_eq!(stdio_bytes.len(), MCP_RESULT_BUDGET_BYTES);
                assert!(stdio.get("result").is_some());
            } else {
                assert_eq!(stdio["error"]["code"], -32000);
            }
        }
    }

    #[test]
    fn ordinary_results_protocol_errors_and_fallbacks_share_exact_encoding() {
        for transport in [ResponseTransport::HttpBody, ResponseTransport::StdioLine] {
            for reply in [
                success(json!(7), json!({})),
                error_reply(json!(7), -32700, "Parse error", None),
                error_reply(
                    json!(7),
                    -32002,
                    "Authentication pending",
                    Some(json!({"kind":"authentication_pending", "_meta":{"owner":"retained"}})),
                ),
                success(
                    json!("x".repeat(MCP_RESULT_BUDGET_BYTES)),
                    json!({"payload":"x".repeat(MCP_RESULT_BUDGET_BYTES)}),
                ),
            ] {
                let bytes = encode_response(reply, transport).expect("encoded response");
                decoded(&bytes, transport);
            }
            let reply = error_reply(
                json!("x".repeat(MCP_RESULT_BUDGET_BYTES)),
                -32002,
                "Authentication pending",
                Some(json!({"kind":"authentication_pending"})),
            );
            let emitted = decoded(
                &encode_response(reply, transport).expect("uncorrelated bounded diagnosis"),
                transport,
            );
            assert!(emitted["id"].is_null());
            assert_eq!(emitted["error"]["data"]["kind"], "authentication_pending");
            let scalar_data =
                error_reply(json!(7), -32000, "scalar application data", Some(json!(42)));
            let emitted: Value = serde_json::from_slice(
                &encode_response(scalar_data, transport).expect("scalar data encoding"),
            )
            .expect("scalar data reply");
            assert_eq!(emitted["error"]["data"], 42);
            for meta in [Value::Null, json!("verbatim"), json!([42])] {
                let data = json!({"kind":"application_diagnosis", "_meta":meta});
                let reply =
                    error_reply(json!(7), -32000, "application metadata", Some(data.clone()));
                let bytes = encode_response(reply, transport).expect("raw error metadata");
                assert!(bytes.len() <= MCP_RESULT_BUDGET_BYTES);
                let emitted: Value = serde_json::from_slice(&bytes).expect("preserved error reply");
                assert_eq!(emitted["error"]["data"], data);
            }
        }
    }
}

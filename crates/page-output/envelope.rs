//! Exact transport-envelope accounting shared by page producers and egress.
//!
//! The request ID, signed continuation and metadata are supplied for this
//! response. They are measured values, never part of a stable query recipe.

use crate::{
    BudgetExceeded, DEFAULT_RESPONSE_BUDGET_BYTES, ESTIMATED_BYTES_PER_TOKEN, estimate_tokens,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::io::{self, Write};

/// Framing actually written by the selected JSON-RPC transport.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JsonRpcFraming {
    /// JSON followed by the one newline written by the stdio codec.
    StdioLine,
    /// The JSON HTTP entity, without a stdio newline.
    HttpBody,
}

impl JsonRpcFraming {
    /// Bytes outside the serialized JSON value.
    #[must_use]
    pub const fn suffix_bytes(self) -> usize {
        match self {
            Self::StdioLine => 1,
            Self::HttpBody => 0,
        }
    }

    const fn scope(self) -> &'static str {
        match self {
            Self::StdioLine => "complete_jsonrpc_line",
            Self::HttpBody => "complete_jsonrpc_body",
        }
    }
}

/// Actual per-call values used to measure one complete successful response.
/// The result must already contain its real signed token and current metadata.
#[derive(Clone, Copy, Debug)]
pub struct ResponseEnvelopeContext<'a> {
    /// Current admitted JSON-RPC request ID.
    pub id: &'a Value,
    /// Framing used when the response is emitted.
    pub framing: JsonRpcFraming,
}

impl ResponseEnvelopeContext<'_> {
    /// Builds the same complete JSON-RPC success object used at egress.
    #[must_use]
    pub fn success(self, result: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": self.id, "result": result })
    }
}

/// Counts the canonical serde JSON encoding without allocating its bytes.
/// Serialization failure or arithmetic overflow never counts as fitting.
#[must_use]
pub fn serialized_json_bytes<T: Serialize + ?Sized>(value: &T) -> usize {
    let mut counter = JsonCounter(0);
    serde_json::to_writer(&mut counter, value).map_or(usize::MAX, |()| counter.0)
}

/// Counts the complete actual transport frame, including stdio's newline.
#[must_use]
pub fn jsonrpc_response_bytes(value: &Value, framing: JsonRpcFraming) -> usize {
    serialized_json_bytes(value).saturating_add(framing.suffix_bytes())
}

struct JsonCounter(usize);

impl Write for JsonCounter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("serialized JSON size overflow"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Exact fixed-point accounting for a tool result's complete response.
/// Existing route metadata and the typed payload's own budget are preserved.
#[must_use]
pub fn attach_jsonrpc_wire_budget(mut value: Value, framing: JsonRpcFraming) -> Value {
    let is_tool_result = value
        .get("result")
        .and_then(Value::as_object)
        .is_some_and(|result| {
            result.contains_key("content") && result.contains_key("structuredContent")
        });
    if !is_tool_result {
        return value;
    }
    let result = value["result"].as_object_mut().expect("tool result object");
    if !result.get("_meta").is_some_and(Value::is_object) {
        result.insert("_meta".to_owned(), json!({}));
    }
    result["_meta"]["backend/wireBudget"] = json!({
        "bytes": 0,
        "estimatedTokens": 0,
        "bytesPerToken": ESTIMATED_BYTES_PER_TOKEN,
        "limitBytes": DEFAULT_RESPONSE_BUDGET_BYTES,
        "scope": framing.scope()
    });
    // A decimal usize has at most twenty digits. Only two integer widths can
    // change here, so eight fixed-point steps leave ample room on every host.
    for _ in 0..8 {
        let bytes = jsonrpc_response_bytes(&value, framing);
        value["result"]["_meta"]["backend/wireBudget"]["bytes"] = json!(bytes);
        value["result"]["_meta"]["backend/wireBudget"]["estimatedTokens"] =
            json!(estimate_tokens(bytes));
        if jsonrpc_response_bytes(&value, framing) == bytes {
            break;
        }
    }
    value
}

/// Disclosure used only when shortening duplicate readable text.
pub const JSONRPC_PREVIEW_MARKER: &str = "\n\n… readable preview shortened to fit this response; structuredContent contains the complete page and any nextCursor.";

/// Admits the actual complete response under the existing application cap.
/// Only duplicate readable text may be shortened. Typed rows, tokens, coverage
/// and terminals are never changed to make a response fit.
pub fn admit_jsonrpc_response(
    value: Value,
    framing: JsonRpcFraming,
) -> Result<Value, BudgetExceeded> {
    let value = attach_jsonrpc_wire_budget(value, framing);
    let value = if jsonrpc_response_bytes(&value, framing) > DEFAULT_RESPONSE_BUDGET_BYTES {
        fit_preview(value, framing)
    } else {
        value
    };
    let bytes = jsonrpc_response_bytes(&value, framing);
    if bytes <= DEFAULT_RESPONSE_BUDGET_BYTES {
        Ok(value)
    } else {
        Err(BudgetExceeded {
            bytes,
            budget: DEFAULT_RESPONSE_BUDGET_BYTES,
        })
    }
}

fn fit_preview(mut value: Value, framing: JsonRpcFraming) -> Value {
    let Some(text) = value
        .get("result")
        .and_then(|result| result.get("content"))
        .and_then(Value::as_array)
        .and_then(|content| content.first())
        .and_then(|entry| entry.get("text"))
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return value;
    };
    let prefix = text.strip_suffix(JSONRPC_PREVIEW_MARKER).unwrap_or(&text);
    value["result"]["content"][0]["text"] = json!(JSONRPC_PREVIEW_MARKER);
    let mut best = attach_jsonrpc_wire_budget(value.clone(), framing);
    if jsonrpc_response_bytes(&best, framing) > DEFAULT_RESPONSE_BUDGET_BYTES {
        return best;
    }
    let mut lower = 0;
    let mut upper = prefix.len();
    while lower < upper {
        let midpoint = lower + (upper - lower).div_ceil(2);
        let mut end = midpoint;
        while !prefix.is_char_boundary(end) {
            end -= 1;
        }
        let mut candidate = value.clone();
        candidate["result"]["content"][0]["text"] =
            json!(format!("{}{JSONRPC_PREVIEW_MARKER}", &prefix[..end]));
        candidate = attach_jsonrpc_wire_budget(candidate, framing);
        if jsonrpc_response_bytes(&candidate, framing) <= DEFAULT_RESPONSE_BUDGET_BYTES {
            lower = midpoint;
            best = candidate;
        } else {
            upper = midpoint - 1;
        }
    }
    best
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    fn response(id: Value, text: &str, rows: Value) -> Value {
        ResponseEnvelopeContext { id: &id, framing: JsonRpcFraming::StdioLine }
            .success(json!({ "content": [{"type":"text", "text": text}], "structuredContent": rows, "isError": false, "_meta": {"other": "kept"} }))
    }

    #[test]
    fn exact_counter_counts_escaping_utf8_and_only_actual_framing() {
        let value = response(
            json!("id\n\"é"),
            "文字\\\n",
            json!({"nextCursor":"mcp1-real-signed-token", "rows":["\"é\n"]}),
        );
        let body = serde_json::to_vec(&value).expect("JSON");
        assert_eq!(serialized_json_bytes(&value), body.len());
        assert_eq!(
            jsonrpc_response_bytes(&value, JsonRpcFraming::HttpBody),
            body.len()
        );
        assert_eq!(
            jsonrpc_response_bytes(&value, JsonRpcFraming::StdioLine),
            body.len() + 1
        );
    }

    #[test]
    fn final_metadata_measures_its_own_bytes_for_both_transports() {
        for framing in [JsonRpcFraming::StdioLine, JsonRpcFraming::HttpBody] {
            let value = admit_jsonrpc_response(
                response(json!(391), "preview", json!({"rows": [1,2,3]})),
                framing,
            )
            .expect("fit");
            let measured = jsonrpc_response_bytes(&value, framing);
            assert_eq!(
                value["result"]["_meta"]["backend/wireBudget"]["bytes"],
                json!(measured)
            );
            assert_eq!(value["result"]["_meta"]["other"], "kept");
            assert_eq!(
                value["result"]["_meta"]["backend/wireBudget"]["scope"],
                framing.scope()
            );
        }
    }

    #[test]
    fn huge_id_cannot_change_typed_rows_or_escape_the_cap() {
        let rows = json!({"rows":["é".repeat(6000)], "nextCursor":"real-token", "terminal":"more"});
        let value = admit_jsonrpc_response(
            response(json!("x".repeat(32000)), &"文".repeat(5000), rows.clone()),
            JsonRpcFraming::StdioLine,
        )
        .expect("preview fits");
        assert_eq!(value["result"]["structuredContent"], rows);
        assert!(
            value["result"]["content"][0]["text"]
                .as_str()
                .expect("text")
                .ends_with(JSONRPC_PREVIEW_MARKER)
        );
        assert!(
            jsonrpc_response_bytes(&value, JsonRpcFraming::StdioLine)
                <= DEFAULT_RESPONSE_BUDGET_BYTES
        );
        assert!(
            admit_jsonrpc_response(
                response(json!("x".repeat(50000)), "preview", json!({"rows":[1]})),
                JsonRpcFraming::StdioLine
            )
            .is_err()
        );
    }
}

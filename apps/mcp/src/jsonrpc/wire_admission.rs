//! Private, owned proof for one immutable escaped JSON envelope. No cache.
use super::{MCP_CL100K_TOKEN_LIMIT, ResponseTransport};
use backend_present::{Cl100kError, cl100k_tokens};
use serde_json::Value;
use std::io::{self, Write};

pub(super) struct MeasuredEnvelope {
    value: Value,
    transport: ResponseTransport,
    json: Option<Vec<u8>>,
    frame_bytes: usize,
    tokens: Result<usize, Cl100kError>,
}
impl MeasuredEnvelope {
    pub(super) fn new(value: Value, transport: ResponseTransport) -> Self {
        Self::measure(value, transport, cl100k_tokens)
    }
    fn measure(
        value: Value,
        transport: ResponseTransport,
        count: impl FnOnce(&str) -> Result<usize, Cl100kError>,
    ) -> Self {
        let mut capture = BoundedCapture {
            json: Some(Vec::with_capacity(transport.limit_bytes())),
            bytes: 0,
            limit: transport.limit_bytes(),
        };
        let encoded = serde_json::to_writer(&mut capture, &value).is_ok();
        let frame_bytes = if encoded {
            transport.encoded_len(capture.bytes).unwrap_or(usize::MAX)
        } else {
            usize::MAX
        };
        let tokens = if !encoded {
            Err(Cl100kError::InvalidEncoding)
        } else if frame_bytes > transport.limit_bytes() {
            Err(Cl100kError::InputTooLarge)
        } else {
            capture
                .json
                .as_deref()
                .map_or(Err(Cl100kError::InputTooLarge), |json| {
                    std::str::from_utf8(json)
                        .map_err(|_| Cl100kError::InvalidEncoding)
                        .and_then(count)
                })
        };
        Self {
            value,
            transport,
            json: capture.json,
            frame_bytes,
            tokens,
        }
    }
    pub(super) fn fits(&self) -> bool {
        self.frame_bytes <= self.transport.limit_bytes()
            && self
                .tokens
                .is_ok_and(|tokens| tokens <= MCP_CL100K_TOKEN_LIMIT)
    }
    pub(super) fn value(&self) -> &Value {
        &self.value
    }
    pub(super) fn bytes(&self) -> usize {
        self.frame_bytes
    }
    pub(super) fn tokens(&self) -> Result<usize, Cl100kError> {
        self.tokens
    }
    pub(super) fn into_value(self) -> Value {
        self.value
    }
    /// Emission consumes the proof and its exact already-counted JSON bytes.
    /// It cannot serialize a mutated value or add a different framing mode.
    pub(super) fn into_frame(self) -> io::Result<(Vec<u8>, bool)> {
        if !self.fits() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "MCP envelope failed final byte/token admission",
            ));
        }
        let mut json = self.json.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "MCP envelope lacks bounded JSON bytes",
            )
        })?;
        if self.transport == ResponseTransport::StdioLine {
            json.push(b'\n');
        }
        debug_assert_eq!(json.len(), self.frame_bytes);
        Ok((json, self.value.get("result").is_some()))
    }
}
// Count the complete serialization even after overflow, but retain no prefix
// as a possible successful frame and allocate at most the frame byte bound.
struct BoundedCapture {
    json: Option<Vec<u8>>,
    bytes: usize,
    limit: usize,
}
impl Write for BoundedCapture {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes = self
            .bytes
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "MCP byte count overflow"))?;
        if self.bytes > self.limit {
            self.json = None;
        } else if let Some(json) = &mut self.json {
            json.extend_from_slice(bytes);
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn immutable_measurement_is_reused_until_consumed_and_emits_identical_escaping() {
        let value = json!({"jsonrpc":"2.0","id":"אב🙂","result":{"text":"\\\"\nλ"}});
        for transport in [ResponseTransport::StdioLine, ResponseTransport::HttpBody] {
            let calls = std::cell::Cell::new(0);
            let proof = MeasuredEnvelope::measure(value.clone(), transport, |text| {
                calls.set(calls.get() + 1);
                cl100k_tokens(text)
            });
            assert!(proof.fits());
            assert!(proof.fits());
            assert!(proof.tokens().is_ok());
            assert_eq!(calls.get(), 1, "queries of the proof never rerun BPE");
            let (bytes, result) = proof.into_frame().expect("admitted emission");
            let mut expected = serde_json::to_vec(&value).expect("reference serialization");
            if transport == ResponseTransport::StdioLine {
                expected.push(b'\n');
            }
            assert!(result);
            assert_eq!(bytes, expected);
        }
    }
    #[test]
    fn oversized_capture_drops_prefix_and_never_tokenizes_or_emits_it() {
        let calls = std::cell::Cell::new(0);
        let proof = MeasuredEnvelope::measure(
            json!({"id":"x".repeat(100_000)}),
            ResponseTransport::StdioLine,
            |_| {
                calls.set(1);
                Ok(0)
            },
        );
        assert!(!proof.fits());
        assert!(proof.bytes() > 100_000);
        assert_eq!(calls.get(), 0);
        assert!(proof.json.is_none());
        assert!(proof.into_frame().is_err());
    }

    #[test]
    fn framing_overflow_refuses_before_tokenizing_the_retained_json() {
        let transport = ResponseTransport::StdioLine;
        let value = Value::String(" ".repeat(transport.limit_bytes() - 2));
        let calls = std::cell::Cell::new(0);
        let proof = MeasuredEnvelope::measure(value, transport, |_| {
            calls.set(calls.get() + 1);
            Ok(0)
        });
        assert_eq!(proof.bytes(), transport.limit_bytes() + 1);
        assert!(
            proof.json.is_some(),
            "JSON itself fits; its newline does not"
        );
        assert_eq!(calls.get(), 0, "a refused frame performs no BPE work");
        assert_eq!(proof.tokens(), Err(Cl100kError::InputTooLarge));
        assert!(!proof.fits());
        assert!(proof.into_frame().is_err());
    }
}

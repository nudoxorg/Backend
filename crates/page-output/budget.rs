//! One typed envelope, fixed-point metadata and hard-cap writer.
use serde::Serialize;
use std::io::{self, Write};

/// Bytes-per-token ratio used for a stable, provider-independent estimate.
///
/// This is deliberately an estimate, not a tokenizer claim.  Measuring bytes
/// first keeps budgets reproducible across model providers and versions.
pub const ESTIMATED_BYTES_PER_TOKEN: usize = 4;

/// Default structured response budget for a context-sized answer.
pub const DEFAULT_RESPONSE_BUDGET_BYTES: usize = 48 * 1024;

/// Hard response budget for the largest complete typed payload admitted.
pub const MAX_RESPONSE_BUDGET_BYTES: usize = 256 * 1024;

/// Byte ceiling for human-readable duplicate text carried beside a typed
/// projection. The typed value remains the complete machine-readable answer;
/// this preview is deliberately kept small so the two together stay within a
/// context-sized MCP result.
pub const MAX_PREVIEW_TEXT_BYTES: usize = 16 * 1024;

/// How much detail a caller requests from a presentation answer.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Detail {
    /// Keep identity, status, coverage, and short row summaries.
    #[default]
    Summary,
    /// Keep the normal typed DTO, subject to the byte budget.
    Standard,
    /// Keep every available field, subject to the byte budget.
    Full,
}

impl Detail {
    /// Parses the stable wire spelling.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "summary" | "minimal" => Some(Self::Summary),
            "standard" | "default" => Some(Self::Standard),
            "full" | "detail" => Some(Self::Full),
            _ => None,
        }
    }

    /// Returns the stable wire spelling.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Summary => "summary",
            Self::Standard => "standard",
            Self::Full => "full",
        }
    }
}

/// Deterministic size measurements emitted by adapters and fixtures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PayloadBudget {
    /// UTF-8 JSON bytes, excluding framing newline bytes.
    pub bytes: usize,
    /// Provider-independent estimated tokens.
    pub estimated_tokens: usize,
}

/// A complete, bounded JSON payload produced by the typed encoder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EncodedPayload {
    /// UTF-8 JSON bytes, excluding framing newline bytes.
    pub bytes: Box<[u8]>,
    /// Measured payload budget, including the budget object itself.
    pub budget: PayloadBudget,
}

/// A typed admission failure for a response that cannot fit the requested
/// context budget without changing its DTO meaning.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BudgetExceeded {
    /// Encoded JSON bytes observed.
    pub bytes: usize,
    /// Maximum bytes admitted for this response.
    pub budget: usize,
}

impl std::fmt::Display for BudgetExceeded {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "response is {} bytes, above the {} byte budget; lower limit or request detail=summary",
            self.bytes, self.budget
        )
    }
}

/// Estimates tokens from bytes using the fixed repository ratio.
#[must_use]
pub const fn estimate_tokens(bytes: usize) -> usize {
    bytes.saturating_add(ESTIMATED_BYTES_PER_TOKEN - 1) / ESTIMATED_BYTES_PER_TOKEN
}

pub fn encode_typed<'a, T: Serialize>(
    answer: &'static str,
    detail: Detail,
    next_cursor: Option<&'a str>,
    body: T,
    budget: usize,
) -> Result<EncodedPayload, BudgetExceeded> {
    // Keep every adapter behind the same protocol ceiling, even if a caller
    // accidentally supplies an unbounded or oversized local budget.
    let budget = budget.min(MAX_RESPONSE_BUDGET_BYTES);
    let mut envelope = BudgetEnvelope {
        body,
        answer,
        detail: detail.name(),
        next_cursor,
        budget: BudgetMetadata::default(),
    };
    let measured = settle_budget(&mut envelope);
    if measured > budget {
        return Err(BudgetExceeded {
            bytes: measured,
            budget,
        });
    }
    let mut writer = HardCapWriter::new(budget);
    serde_json::to_writer(&mut writer, &envelope).map_err(|_| BudgetExceeded {
        bytes: measured,
        budget,
    })?;
    if writer.bytes.len() != measured {
        return Err(BudgetExceeded {
            bytes: writer.bytes.len(),
            budget,
        });
    }
    Ok(EncodedPayload {
        bytes: writer.bytes.into_boxed_slice(),
        budget: PayloadBudget {
            bytes: measured,
            estimated_tokens: estimate_tokens(measured),
        },
    })
}

/// Encodes another typed, map-shaped presentation body with the same bounded
/// envelope used by CLI/MCP answers.
pub fn encode_serializable<'a, T: Serialize>(
    answer: &'static str,
    detail: Detail,
    next_cursor: Option<&'a str>,
    body: T,
    budget: usize,
) -> Result<EncodedPayload, BudgetExceeded> {
    encode_typed(answer, detail, next_cursor, body, budget)
}

/// Serializes a typed JSON value under the same hard cap as answer envelopes.
///
/// Faults, protocol metadata, and other route-level values do not have an
/// presentation answer variant, but still cross the same context boundary. This
/// helper lets those callers measure before allocating and refuse atomically.
pub fn encode_value<T: Serialize>(
    value: &T,
    budget: usize,
) -> Result<EncodedPayload, BudgetExceeded> {
    let budget = budget.min(MAX_RESPONSE_BUDGET_BYTES);
    let measured = count_json(value);
    if measured > budget {
        return Err(BudgetExceeded {
            bytes: measured,
            budget,
        });
    }
    let mut writer = HardCapWriter::new(budget);
    serde_json::to_writer(&mut writer, value).map_err(|_| BudgetExceeded {
        bytes: measured,
        budget,
    })?;
    if writer.bytes.len() != measured {
        return Err(BudgetExceeded {
            bytes: writer.bytes.len(),
            budget,
        });
    }
    Ok(EncodedPayload {
        bytes: writer.bytes.into_boxed_slice(),
        budget: PayloadBudget {
            bytes: measured,
            estimated_tokens: estimate_tokens(measured),
        },
    })
}

#[derive(Serialize)]
struct BudgetEnvelope<'a, T> {
    #[serde(flatten)]
    body: T,
    answer: &'static str,
    detail: &'static str,
    #[serde(rename = "nextCursor", skip_serializing_if = "Option::is_none")]
    next_cursor: Option<&'a str>,
    budget: BudgetMetadata,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
struct BudgetMetadata {
    bytes: usize,
    #[serde(rename = "estimatedTokens")]
    estimated_tokens: usize,
    #[serde(rename = "bytesPerToken")]
    bytes_per_token: usize,
}

impl BudgetMetadata {
    const fn from_bytes(bytes: usize) -> Self {
        Self {
            bytes,
            estimated_tokens: estimate_tokens(bytes),
            bytes_per_token: ESTIMATED_BYTES_PER_TOKEN,
        }
    }
}

fn settle_budget<T: Serialize>(envelope: &mut BudgetEnvelope<'_, T>) -> usize {
    let mut measured = count_json(envelope);
    for _ in 0..8 {
        envelope.budget = BudgetMetadata::from_bytes(measured);
        let next = count_json(envelope);
        if next == measured {
            break;
        }
        measured = next;
    }
    envelope.budget = BudgetMetadata::from_bytes(measured);
    count_json(envelope)
}

fn count_json<T: Serialize>(value: &T) -> usize {
    let mut writer = CountingWriter::default();
    serde_json::to_writer(&mut writer, value)
        .map(|()| writer.bytes)
        .unwrap_or(usize::MAX)
}

struct HardCapWriter {
    bytes: Vec<u8>,
    cap: usize,
}

impl HardCapWriter {
    fn new(cap: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(cap.min(64 * 1024)),
            cap,
        }
    }
}

impl Write for HardCapWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.bytes.len().saturating_add(bytes.len()) > self.cap {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "serialized response exceeds the hard cap",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct CountingWriter {
    bytes: usize,
}

impl Write for CountingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes = self.bytes.saturating_add(bytes.len());
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Borrowed generic canonical typed envelope, streamed for exact outer costs.
pub struct PayloadProjection<'a, T> {
    pub answer: &'static str,
    pub detail: Detail,
    pub next_cursor: Option<&'a str>,
    pub body: T,
}
impl<T: Serialize> Serialize for PayloadProjection<'_, T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut envelope = BudgetEnvelope {
            body: &self.body,
            answer: self.answer,
            detail: self.detail.name(),
            next_cursor: self.next_cursor,
            budget: BudgetMetadata::default(),
        };
        settle_budget(&mut envelope);
        envelope.serialize(serializer)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hard_cap_writer_never_returns_a_truncated_success() {
        let mut writer = HardCapWriter::new(4);
        assert!(writer.write_all(b"abc").is_ok());
        assert!(writer.write_all(b"de").is_err());
        assert_eq!(writer.bytes, b"abc");
    }
}

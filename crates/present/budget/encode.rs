//! Bounded JSON encoding for presentation answers.
//!
//! Detail levels, token estimates, and preview text stay with the budget
//! module. This module counts a typed envelope before it allocates the bytes
//! that cross the CLI and MCP boundary.

use super::{
    BudgetExceeded, Detail, ESTIMATED_BYTES_PER_TOKEN, EncodedPayload, MAX_RESPONSE_BUDGET_BYTES,
    PayloadBudget, estimate_tokens,
};
use crate::Answer;
use crate::dto::{
    CapabilitiesDto, CoverageDto, FaultDto, IdentityDto, OutlineDto, OutlineNodeDto, PageDto,
    ProductDto, ProductRecordDto, RecordDto, RecordListDto, ShelfDto, ShelfEntryDto, StatusDto,
};
use serde::Serialize;
use std::io::{self, Write};

/// Serializes one answer from typed DTOs through a hard-cap writer.
///
/// The normal CLI/MCP answer path uses this function. It never lowers an
/// answer to `serde_json::Value`, and it counts before allocating the final
/// bytes. A payload that does not fit returns a typed admission error without
/// materializing an oversized response.
pub fn encode_answer(
    answer: &Answer,
    detail: Detail,
    next_cursor: Option<&str>,
    budget: usize,
) -> Result<EncodedPayload, BudgetExceeded> {
    if matches!(detail, Detail::Summary) {
        return match answer {
            Answer::Page(page) => encode_typed(
                answer.kind(),
                detail,
                next_cursor,
                SummaryPageDto {
                    identity: IdentityDto::new(page.identity()),
                    kind: page.kind().map(|kind| kind.name().to_owned()),
                    language: page.language().name().to_owned(),
                },
                budget,
            ),
            Answer::Records(list) => {
                let records = list
                    .records()
                    .iter()
                    .map(|record| {
                        let record = RecordDto::new(record);
                        SummaryRecordDto {
                            identity: record.identity,
                            kind: record.kind,
                            language: record.language,
                            state: record.state,
                            summary: record.summary,
                            score: record.score,
                        }
                    })
                    .collect();
                encode_typed(
                    answer.kind(),
                    detail,
                    next_cursor,
                    SummaryRecordsDto {
                        query: list.query(),
                        coverage: CoverageDto::all(list.coverage()),
                        readiness: list.coverage().readiness(),
                        records,
                        more: list.has_more(),
                    },
                    budget,
                )
            }
            Answer::Shelf(shelf) => {
                let projects = shelf
                    .entries()
                    .iter()
                    .map(|project| {
                        let project = ShelfEntryDto::new(project);
                        SummaryShelfEntryDto {
                            identity: project.identity,
                            readiness: project.readiness,
                            rows: project.rows,
                            declarations: project.declarations,
                            fault: project.fault,
                        }
                    })
                    .collect();
                encode_typed(
                    answer.kind(),
                    detail,
                    next_cursor,
                    SummaryShelfDto {
                        revision: shelf.revision().to_string(),
                        projects,
                    },
                    budget,
                )
            }
            Answer::Outline(outline) => {
                let outline = OutlineDto::new(outline);
                encode_typed(
                    answer.kind(),
                    detail,
                    next_cursor,
                    SummaryOutlineDto {
                        package: outline.package,
                        roots: outline.roots,
                        extent: outline.extent,
                        declarations: outline.declarations,
                        unnamed: outline.unnamed,
                    },
                    budget,
                )
            }
            Answer::Status(status) => {
                let status = StatusDto::new(status);
                encode_typed(
                    answer.kind(),
                    detail,
                    next_cursor,
                    SummaryStatusDto {
                        readiness: status.readiness,
                        revision: status.revision,
                        revision_tag: status.revision_tag,
                        rows: status.rows,
                        project: status.project,
                        coverage: status.coverage,
                        capabilities: status.capabilities,
                    },
                    budget,
                )
            }
            Answer::Product(product) => {
                let product = ProductDto::new(product);
                encode_typed(
                    answer.kind(),
                    detail,
                    next_cursor,
                    SummaryProductDto {
                        heading: product.heading,
                        records: product.records,
                        note: product.note,
                        fault: product.fault,
                    },
                    budget,
                )
            }
        };
    }
    match answer {
        Answer::Page(page) => encode_typed(
            answer.kind(),
            detail,
            next_cursor,
            PageDto::new(page),
            budget,
        ),
        Answer::Records(list) => encode_typed(
            answer.kind(),
            detail,
            next_cursor,
            RecordListDto::new(list),
            budget,
        ),
        Answer::Shelf(shelf) => encode_typed(
            answer.kind(),
            detail,
            next_cursor,
            ShelfDto::new(shelf),
            budget,
        ),
        Answer::Outline(outline) => encode_typed(
            answer.kind(),
            detail,
            next_cursor,
            OutlineDto::new(outline),
            budget,
        ),
        Answer::Status(status) => encode_typed(
            answer.kind(),
            detail,
            next_cursor,
            StatusDto::new(status),
            budget,
        ),
        Answer::Product(product) => encode_typed(
            answer.kind(),
            detail,
            next_cursor,
            ProductDto::new(product),
            budget,
        ),
    }
}

fn encode_typed<'a, T: Serialize>(
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
    let mut measured = count_json(&envelope);
    for _ in 0..4 {
        envelope.budget = BudgetMetadata::from_bytes(measured);
        let next = count_json(&envelope);
        if next == measured {
            break;
        }
        measured = next;
    }
    envelope.budget = BudgetMetadata::from_bytes(measured);
    measured = count_json(&envelope);
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
/// [`Answer`] variant, but they still cross the same context boundary. This
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

#[derive(Serialize)]
struct SummaryPageDto {
    identity: IdentityDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    language: String,
}

#[derive(Serialize)]
struct SummaryRecordDto {
    identity: IdentityDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    score: Option<u32>,
}

#[derive(Serialize)]
struct SummaryRecordsDto<'a> {
    query: &'a str,
    coverage: Vec<CoverageDto>,
    readiness: &'a str,
    records: Vec<SummaryRecordDto>,
    more: bool,
}

#[derive(Serialize)]
struct SummaryShelfEntryDto {
    identity: IdentityDto,
    readiness: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    rows: Option<u64>,
    declarations: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    fault: Option<FaultDto>,
}

#[derive(Serialize)]
struct SummaryShelfDto {
    revision: String,
    projects: Vec<SummaryShelfEntryDto>,
}

#[derive(Serialize)]
struct SummaryOutlineDto {
    package: IdentityDto,
    roots: Vec<OutlineNodeDto>,
    extent: String,
    declarations: usize,
    unnamed: usize,
}

#[derive(Serialize)]
struct SummaryStatusDto {
    readiness: String,
    revision: String,
    revision_tag: String,
    rows: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<String>,
    coverage: Vec<CoverageDto>,
    capabilities: CapabilitiesDto,
}

#[derive(Serialize)]
struct SummaryProductDto {
    heading: String,
    records: Vec<ProductRecordDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fault: Option<FaultDto>,
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

#[cfg(test)]
mod tests {
    use super::HardCapWriter;
    use std::io::Write;

    #[test]
    fn hard_cap_writer_never_returns_a_truncated_success() {
        let mut writer = HardCapWriter::new(4);
        assert!(writer.write_all(b"abc").is_ok());
        assert!(writer.write_all(b"de").is_err());
        assert_eq!(writer.bytes, b"abc");
    }
}

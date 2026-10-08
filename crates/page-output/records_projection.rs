//! One canonical records envelope shared by metering and all surface output.
use crate::budget::{PayloadProjection, encode_value};
use crate::{
    BudgetExceeded, CoverageDto, Detail, EncodedPayload, IdentityDto, RecordDto, RecordList,
    RecordListDto,
};
use serde::Serialize;
/// Borrowed canonical record-page projection. Dynamic token text is supplied
/// per response and never enters the immutable query recipe.
pub struct RecordsProjection<'a> {
    pub list: &'a RecordList,
    pub detail: Detail,
    pub next_cursor: Option<&'a str>,
}
impl Serialize for RecordsProjection<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if self.detail == Detail::Summary {
            let records = self
                .list
                .records()
                .iter()
                .map(|record| ProjectedRecord::new(record, self.detail))
                .collect();
            PayloadProjection {
                answer: "records",
                detail: self.detail,
                next_cursor: self.next_cursor,
                body: SummaryRecordsDto {
                    query: self.list.query(),
                    coverage: CoverageDto::all(self.list.coverage()),
                    readiness: self.list.coverage().readiness(),
                    records,
                    more: self.list.has_more(),
                },
            }
            .serialize(serializer)
        } else {
            PayloadProjection {
                answer: "records",
                detail: self.detail,
                next_cursor: self.next_cursor,
                body: RecordListDto::new(self.list),
            }
            .serialize(serializer)
        }
    }
}
/// Encodes the same borrowed projection under the existing typed payload cap.
pub fn encode_records(
    list: &RecordList,
    detail: Detail,
    cursor: Option<&str>,
    budget: usize,
) -> Result<EncodedPayload, BudgetExceeded> {
    encode_value(
        &RecordsProjection {
            list,
            detail,
            next_cursor: cursor,
        },
        budget,
    )
}
#[derive(Serialize)]
struct SummaryRecordsDto<'a> {
    query: &'a str,
    coverage: Vec<CoverageDto>,
    readiness: &'a str,
    records: Vec<ProjectedRecord>,
    more: bool,
}

/// One canonical detail projection used by output and row commitments.
pub(crate) struct ProjectedRecord {
    record: RecordDto,
    detail: Detail,
}
impl ProjectedRecord {
    pub(crate) fn new(record: &crate::Record, detail: Detail) -> Self {
        Self {
            record: RecordDto::new(record),
            detail,
        }
    }
}
impl Serialize for ProjectedRecord {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if self.detail == Detail::Summary {
            #[derive(Serialize)]
            struct Summary<'a> {
                identity: &'a IdentityDto,
                #[serde(skip_serializing_if = "Option::is_none")]
                kind: &'a Option<String>,
                #[serde(skip_serializing_if = "Option::is_none")]
                language: &'a Option<String>,
                #[serde(skip_serializing_if = "Option::is_none")]
                state: &'a Option<String>,
                #[serde(skip_serializing_if = "Option::is_none")]
                summary: &'a Option<String>,
                #[serde(skip_serializing_if = "Option::is_none")]
                score: Option<u32>,
            }
            Summary {
                identity: &self.record.identity,
                kind: &self.record.kind,
                language: &self.record.language,
                state: &self.record.state,
                summary: &self.record.summary,
                score: self.record.score,
            }
            .serialize(serializer)
        } else {
            self.record.serialize(serializer)
        }
    }
}

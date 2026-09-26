//! The stable JSON projection of the presentation model.
//!
//! `--format json` and MCP `structuredContent` emit these types and nothing
//! else. They are deliberately *not* the wire DTOs: a wire reply is shaped for
//! proof and framing, and dumping it at a caller leaks identity digests,
//! certificate claims, and cursor internals that no consumer should bind to.
//! What a consumer binds to is here — identity parts, resolved names, typed
//! coverage, typed faults — and it changes only when the presentation model
//! changes.

use backend_library::RegistryNativeMetadata;
use serde::{Deserialize, Serialize};

use crate::coverage::{CoverageLine, LaneState};
use crate::drive::Answer;
use crate::fault::{Affordance, Fault};
use crate::identity::Identity;
use crate::outline::{OutlineEntry, OutlineTree};
use crate::page::{Page, Prose, Source};
use crate::product::ProductView;
use crate::record::{Record, RecordList};
use crate::shelf::{Readiness, Shelf, ShelfEntry};
use crate::signature::{Signature, Token};
use crate::status::Status;

/// Projects whichever answer a surface produced, tagged by its `answer` field.
///
/// The CLI's `--format json` and the MCP's `structuredContent` both emit this
/// value, so a consumer that learns one has learned the other.
///
/// The discriminator is `answer` rather than the obvious `kind` because a page
/// already has a `kind` — its declaration kind — and a tag that overwrites a
/// payload field does not announce itself: the consumer simply reads
/// `"kind": "page"` where `"kind": "function"` belonged and never learns that a
/// fact went missing. No presentation DTO has a field called `answer`, and the
/// crate's tests hold that true.
#[must_use]
pub fn answer_value(answer: &Answer) -> serde_json::Value {
    match answer {
        Answer::Page(value) => tagged(answer.kind(), &PageDto::new(value)),
        Answer::Records(value) => tagged(answer.kind(), &RecordListDto::new(value)),
        Answer::Shelf(value) => tagged(answer.kind(), &ShelfDto::new(value)),
        Answer::Outline(value) => tagged(answer.kind(), &OutlineDto::new(value)),
        Answer::Status(value) => tagged(answer.kind(), &StatusDto::new(value)),
        Answer::Product(value) => tagged(answer.kind(), &ProductDto::new(value)),
    }
}

/// Projects one fault as the same tagged shape an answer uses.
#[must_use]
pub fn fault_value(fault: &Fault) -> serde_json::Value {
    tagged("fault", &FaultDto::new(fault))
}

fn tagged<T: Serialize>(kind: &str, value: &T) -> serde_json::Value {
    let body = serde_json::to_value(value).unwrap_or(serde_json::Value::Null);
    match body {
        serde_json::Value::Object(mut fields) => {
            fields.insert(
                "answer".to_owned(),
                serde_json::Value::String(kind.to_owned()),
            );
            serde_json::Value::Object(fields)
        }
        other => serde_json::json!({ "answer": kind, "value": other }),
    }
}

/// One identity, in parts.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct IdentityDto {
    /// The exact coordinate the engine accepts.
    pub coordinate: String,
    /// The readable trail.
    pub trail: String,
    /// The declaration's own name.
    pub name: String,
    /// Which closed spelling this coordinate uses.
    pub shape: String,
    /// The owning project's absolute root.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// The package-relative source path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The one-based declaration line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// The `::`-separated symbol path segments.
    pub segments: Vec<String>,
    /// The display abbreviation of the row's stable key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

impl IdentityDto {
    /// Projects one identity.
    #[must_use]
    pub fn new(identity: &Identity) -> Self {
        Self {
            coordinate: identity.coordinate().as_str().to_owned(),
            trail: identity.trail_within(None),
            name: identity.name().to_owned(),
            shape: shape_name(identity),
            project: identity
                .project()
                .map(|project| project.root().to_owned()),
            path: identity.path().map(|path| path.as_str().to_owned()),
            line: identity.line().map(crate::identity::LineNumber::get),
            segments: identity
                .trail()
                .segments()
                .iter()
                .map(|segment| segment.as_str().to_owned())
                .collect(),
            key: identity.key().tag().map(|tag| tag.to_string()),
        }
    }
}

fn shape_name(identity: &Identity) -> String {
    match identity.shape() {
        crate::identity::IdentityShape::Package => "package",
        crate::identity::IdentityShape::Module => "module",
        crate::identity::IdentityShape::Declaration => "declaration",
        crate::identity::IdentityShape::Semantic => "semantic",
        crate::identity::IdentityShape::External => "external",
        crate::identity::IdentityShape::Opaque => "opaque",
    }
    .to_owned()
}

/// One classified signature token.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SignatureTokenDto {
    /// The lexeme text, exactly as it appeared.
    pub text: String,
    /// What the lexeme reads as.
    pub kind: String,
    /// The coordinate of an unproven, name-matched target.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// How the target was matched, when there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved: Option<String>,
}

impl SignatureTokenDto {
    /// Projects one token.
    #[must_use]
    pub fn new(token: &Token) -> Self {
        Self {
            text: token.text().to_owned(),
            kind: token.kind().name().to_owned(),
            target: token
                .target()
                .map(|target| target.coordinate().as_str().to_owned()),
            resolved: token.target().map(|_| "by-name".to_owned()),
        }
    }

    /// Projects a whole signature.
    #[must_use]
    pub fn all(signature: &Signature) -> Vec<Self> {
        signature.tokens().iter().map(Self::new).collect()
    }
}

/// One lane's coverage.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CoverageDto {
    /// Lane name.
    pub lane: String,
    /// Lane state: `complete`, `partial`, `unavailable`, or `unobserved`.
    pub state: String,
    /// Completed shards, when the lane is partial.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed: Option<u16>,
    /// Declared shards, when the lane is partial.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<u16>,
    /// Why the lane answered nothing, when it is unavailable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl CoverageDto {
    /// Projects one folded coverage line.
    #[must_use]
    pub fn all(line: CoverageLine) -> Vec<Self> {
        line.lanes()
            .iter()
            .map(|lane| match lane.state() {
                LaneState::Complete => Self::simple(lane.name(), "complete"),
                LaneState::Unobserved => Self::simple(lane.name(), "unobserved"),
                LaneState::Partial { completed, total } => Self {
                    lane: lane.name().to_owned(),
                    state: "partial".to_owned(),
                    completed: Some(completed.get()),
                    total: Some(total.get()),
                    reason: None,
                },
                LaneState::Unavailable { reason } => Self {
                    lane: lane.name().to_owned(),
                    state: "unavailable".to_owned(),
                    completed: None,
                    total: None,
                    reason: Some(crate::coverage::reason_name(reason).to_owned()),
                },
            })
            .collect()
    }

    fn simple(lane: &str, state: &str) -> Self {
        Self {
            lane: lane.to_owned(),
            state: state.to_owned(),
            completed: None,
            total: None,
            reason: None,
        }
    }
}

/// One typed failure.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FaultDto {
    /// The closed failure class.
    pub slug: String,
    /// The exact operand, rendered.
    pub operand: String,
    /// The closed cause slug.
    pub cause: String,
    /// The readable cause sentence.
    pub detail: String,
    /// The next step as a shell command.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    /// The next step as an MCP tool call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call: Option<serde_json::Value>,
}

impl FaultDto {
    /// Projects one fault.
    #[must_use]
    pub fn new(fault: &Fault) -> Self {
        Self {
            slug: fault.slug().as_str().to_owned(),
            operand: fault.operand().render(),
            cause: fault.cause().slug().as_str().to_owned(),
            detail: fault.cause().sentence().to_owned(),
            shell: fault.affordance().shell(),
            call: fault.affordance().tool_call(),
        }
    }

    /// Returns whether this fault carries an actionable next step.
    #[must_use]
    pub fn is_actionable(fault: &Fault) -> bool {
        !matches!(fault.affordance(), Affordance::None)
    }
}

/// One declaration page.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PageDto {
    /// The page's identity.
    pub identity: IdentityDto,
    /// The declaration kind.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The source language.
    pub language: String,
    /// The signature text, reassembled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// The classified signature tokens.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tokens: Vec<SignatureTokenDto>,
    /// The producer's captured documentation, as paragraphs.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub prose: Vec<String>,
    /// Members grouped by kind.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<MemberGroupDto>,
    /// Labelled relation groups.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub relations: Vec<RelationGroupDto>,
    /// The captured source, when there is any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceDto>,
    /// Why there is no source, when there is none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_fault: Option<FaultDto>,
    /// Faults explaining any section this page could not fill.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<FaultDto>,
}

/// Members that share one kind.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MemberGroupDto {
    /// The shared declaration kind.
    pub kind: String,
    /// The members.
    pub members: Vec<RecordDto>,
}

/// Relations that share one label.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RelationGroupDto {
    /// The group label.
    pub label: String,
    /// The related declarations.
    pub relations: Vec<IdentityDto>,
}

/// Captured source text.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceDto {
    /// The package-relative capture path.
    pub path: String,
    /// The one-based capture line.
    pub line: u32,
    /// The captured lines, in order.
    pub lines: Vec<String>,
    /// Whether the retained text is complete.
    pub extent: String,
}

impl PageDto {
    /// Projects one page.
    #[must_use]
    pub fn new(page: &Page) -> Self {
        Self {
            identity: IdentityDto::new(page.identity()),
            kind: page.kind().map(|kind| kind.name().to_owned()),
            language: page.language().name().to_owned(),
            signature: page.signature().map(Signature::text),
            tokens: page.signature().map(SignatureTokenDto::all).unwrap_or_default(),
            prose: page
                .prose()
                .iter()
                .map(|block| match block {
                    Prose::Text(text) | Prose::Code(text) | Prose::Link { label: text, .. } => {
                        text.clone()
                    }
                })
                .collect(),
            members: page
                .members()
                .iter()
                .map(|group| MemberGroupDto {
                    kind: group.kind().name().to_owned(),
                    members: group
                        .members()
                        .iter()
                        .map(|member| RecordDto {
                            identity: IdentityDto::new(member.identity()),
                            kind: member.kind().map(|kind| kind.name().to_owned()),
                            language: None,
                            state: None,
                            signature: member.signature().map(Signature::text),
                            // A member's own first documentation line is worth
                            // a field but not a second text line: the page's
                            // Markdown keeps one line per member, and a program
                            // that wants the sentence reads it here.
                            summary: member.summary().map(ToOwned::to_owned),
                            score: None,
                        })
                        .collect(),
                })
                .collect(),
            relations: page
                .relations()
                .iter()
                .map(|group| RelationGroupDto {
                    label: group.label().as_str().to_owned(),
                    relations: group
                        .relations()
                        .iter()
                        .map(|relation| IdentityDto::new(relation.identity()))
                        .collect(),
                })
                .collect(),
            source: source_dto(page.source()),
            source_fault: page.source().fault().map(FaultDto::new),
            notes: page.notes().iter().map(FaultDto::new).collect(),
        }
    }
}

fn source_dto(source: &Source) -> Option<SourceDto> {
    let Source::Captured {
        site,
        lines,
        truncation,
    } = source
    else {
        return None;
    };
    Some(SourceDto {
        path: site.path().as_str().to_owned(),
        line: site.line().get(),
        lines: lines.iter().map(|line| line.text().to_owned()).collect(),
        extent: truncation.name().to_owned(),
    })
}

/// One result record.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RecordDto {
    /// The record identity.
    pub identity: IdentityDto,
    /// The declaration kind.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The source language.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// The row lifecycle.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// The signature text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// A one-line summary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// The reported score.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<u32>,
}

impl RecordDto {
    /// Projects one record.
    #[must_use]
    pub fn new(record: &Record) -> Self {
        Self {
            identity: IdentityDto::new(record.identity()),
            kind: record.kind().map(|kind| kind.name().to_owned()),
            language: Some(record.language().name().to_owned()),
            state: Some(record.state().name().to_owned()),
            signature: record.signature().map(Signature::text),
            summary: record.summary().map(ToOwned::to_owned),
            score: record.score().map(crate::record::Score::get),
        }
    }
}

/// One bounded result page.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RecordListDto {
    /// The query text this page answers.
    pub query: String,
    /// The honest lane coverage.
    pub coverage: Vec<CoverageDto>,
    /// The single-word readiness summary.
    pub readiness: String,
    /// The records in rank order.
    pub records: Vec<RecordDto>,
    /// Whether another page follows.
    pub more: bool,
    /// Why an empty page is empty, when the caller proved a reason.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub empty_reason: Option<String>,
}

impl RecordListDto {
    /// Projects one result page.
    #[must_use]
    pub fn new(list: &RecordList) -> Self {
        Self {
            query: list.query().to_owned(),
            coverage: CoverageDto::all(list.coverage()),
            readiness: list.coverage().readiness().to_owned(),
            records: list.records().iter().map(RecordDto::new).collect(),
            more: list.has_more(),
            empty_reason: list.empty_reason().map(str::to_owned),
        }
    }
}

/// One project on the shelf.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ShelfEntryDto {
    /// The project identity.
    pub identity: IdentityDto,
    /// How ready the project is.
    pub readiness: String,
    /// Rows published so far, when the project is still indexing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows: Option<u64>,
    /// The total declaration count.
    pub declarations: u64,
    /// Per-language declaration counts.
    pub languages: Vec<LanguageCountDto>,
    /// Why the project is not readable, when it failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fault: Option<FaultDto>,
}

/// One language's contribution to a project.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LanguageCountDto {
    /// The language.
    pub language: String,
    /// How many declarations it contributes.
    pub declarations: u64,
}

impl ShelfEntryDto {
    /// Projects one shelf entry.
    #[must_use]
    pub fn new(entry: &ShelfEntry) -> Self {
        Self {
            identity: IdentityDto::new(entry.identity()),
            readiness: entry.readiness().name().to_owned(),
            rows: match entry.readiness() {
                Readiness::Indexing { rows } => Some(rows.get()),
                _ => None,
            },
            declarations: entry.declarations().get(),
            languages: entry
                .languages()
                .iter()
                .map(|count| LanguageCountDto {
                    language: count.language().name().to_owned(),
                    declarations: count.declarations().get(),
                })
                .collect(),
            fault: entry.readiness().fault().map(FaultDto::new),
        }
    }
}

/// The whole shelf.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ShelfDto {
    /// The revision the shelf was read at.
    pub revision: String,
    /// Every project in display order.
    pub projects: Vec<ShelfEntryDto>,
}

impl ShelfDto {
    /// Projects one shelf.
    #[must_use]
    pub fn new(shelf: &Shelf) -> Self {
        Self {
            revision: shelf.revision().to_string(),
            projects: shelf.entries().iter().map(ShelfEntryDto::new).collect(),
        }
    }
}

/// One outline node.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OutlineNodeDto {
    /// The node's display name.
    pub name: String,
    /// The node's declaration kind.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The exact coordinate, when the node resolved to a row.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coordinate: Option<String>,
    /// The display abbreviation of the node's key.
    pub key: String,
    /// The child nodes.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<OutlineNodeDto>,
}

impl OutlineNodeDto {
    /// Projects one outline node and its subtree.
    #[must_use]
    pub fn new(entry: &OutlineEntry) -> Self {
        Self {
            name: entry.name(),
            kind: entry.kind().map(|kind| kind.name().to_owned()),
            coordinate: entry
                .identity()
                .map(|identity| identity.coordinate().as_str().to_owned()),
            key: entry.tag().to_string(),
            children: entry.children().iter().map(Self::new).collect(),
        }
    }
}

/// One whole outline.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OutlineDto {
    /// The package this outline describes.
    pub package: IdentityDto,
    /// Every top-level node.
    pub roots: Vec<OutlineNodeDto>,
    /// Whether the bounded response carried the complete outline.
    pub extent: String,
    /// The total node count.
    pub declarations: usize,
    /// How many nodes did not resolve to a name.
    pub unnamed: usize,
}

impl OutlineDto {
    /// Projects one outline.
    #[must_use]
    pub fn new(tree: &OutlineTree) -> Self {
        Self {
            package: IdentityDto::new(tree.package()),
            roots: tree.roots().iter().map(OutlineNodeDto::new).collect(),
            extent: tree.truncation().name().to_owned(),
            declarations: tree.count(),
            unnamed: tree.unresolved(),
        }
    }
}

/// The engine's whole state.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StatusDto {
    /// The single-word readiness summary.
    pub readiness: String,
    /// The exact current view revision, as canonical hexadecimal.
    ///
    /// This is the whole value, not the eight-digit tag a reader sees, because
    /// a consumer that wants to pin its next request to this revision needs all
    /// of it. The abbreviation is in `revision_tag` for anything that prints.
    pub revision: String,
    /// The readable eight-digit abbreviation of `revision`.
    pub revision_tag: String,
    /// The exact source object the view is based on.
    pub source: String,
    /// The readable eight-digit abbreviation of `source`.
    pub source_tag: String,
    /// The owner's subscription sequence position.
    pub sequence: u64,
    /// How many rows the visible root committed.
    pub rows: u64,
    /// The active project's absolute root.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// The honest lane coverage.
    pub coverage: Vec<CoverageDto>,
    /// The rolled-up capability inventory.
    pub capabilities: CapabilitiesDto,
}

/// The rolled-up capability inventory.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CapabilitiesDto {
    /// How many structural frontend slots reported ready.
    pub frontends_ready: u32,
    /// How many structural frontend slots exist.
    pub frontends_total: u32,
    /// How many language oracle slots reported ready.
    pub oracles_ready: u32,
    /// How many language oracle slots exist.
    pub oracles_total: u32,
    /// The embedding lane state.
    pub embedding: String,
    /// Unavailability reasons, most common first.
    pub unavailable: Vec<ReasonDto>,
    /// The one-line summary every surface prints.
    pub summary: String,
}

/// One rolled-up unavailability reason.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReasonDto {
    /// The reason name.
    pub reason: String,
    /// How many slots reported it.
    pub count: u32,
}

impl StatusDto {
    /// Projects the engine's whole state.
    #[must_use]
    pub fn new(status: &Status) -> Self {
        let capabilities = status.capabilities();
        Self {
            readiness: status.readiness().to_owned(),
            revision: status.revision_id(),
            revision_tag: status.revision().to_string(),
            source: status.source_id(),
            source_tag: status.source().to_string(),
            sequence: status.sequence().get(),
            rows: status.rows().get(),
            project: status.project().map(|project| project.root().to_owned()),
            coverage: CoverageDto::all(status.coverage()),
            capabilities: CapabilitiesDto {
                frontends_ready: capabilities.frontends().ready().get(),
                frontends_total: capabilities.frontends().total().get(),
                oracles_ready: capabilities.oracles().ready().get(),
                oracles_total: capabilities.oracles().total().get(),
                embedding: capabilities.embedding().name().to_owned(),
                unavailable: capabilities
                    .reasons()
                    .iter()
                    .map(|rollup| ReasonDto {
                        reason: rollup.name().to_owned(),
                        count: rollup.count().get(),
                    })
                    .collect(),
                summary: capabilities.render(),
            },
        }
    }
}

/// One durable product answer.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProductDto {
    /// The heading naming what was asked.
    pub heading: String,
    /// The records in reply order.
    pub records: Vec<ProductRecordDto>,
    /// The one-line note, when the reply carried a scalar answer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Why the configured feed does not publish this fact.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fault: Option<FaultDto>,
}

/// One product record.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProductRecordDto {
    /// The readable title.
    pub title: String,
    /// The exact operand a caller passes back.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operand: Option<String>,
    /// The tags shown after the title.
    pub tags: Vec<String>,
    /// Typed native registry facts when this row came from a package release.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_metadata: Option<RegistryNativeMetadata>,
}

impl ProductDto {
    /// Projects one product answer.
    #[must_use]
    pub fn new(view: &ProductView) -> Self {
        Self {
            heading: view.heading().to_owned(),
            records: view
                .records()
                .iter()
                .map(|record| ProductRecordDto {
                    title: record.title().to_owned(),
                    operand: record.operand().map(ToOwned::to_owned),
                    tags: record.tags().to_vec(),
                    native_metadata: record.native_metadata().cloned(),
                })
                .collect(),
            note: view.note().map(ToOwned::to_owned),
            fault: view.fault().map(FaultDto::new),
        }
    }
}

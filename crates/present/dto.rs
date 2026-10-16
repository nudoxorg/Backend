//! The stable JSON projection of the presentation model.
//!
//! Standard/full `--format json` and MCP `structuredContent` use these DTOs.
//! Summary retains the public identity and result-list grammar; a Summary page
//! uses [`SummaryPageDto`] and omits the unrequested page sections. Projection
//! envelope fields such as `answer`, `detail`, and `nextCursor` are additional.
//! These are deliberately *not* the wire DTOs: a wire reply is shaped for
//! proof and framing, and dumping it at a caller leaks identity digests,
//! certificate claims, and cursor internals that no consumer should bind to.
//! What a consumer binds to is here — identity parts, resolved names, typed
//! coverage, typed faults — and it changes only when the presentation model
//! changes.

use backend_library::{CompilerNativeToolFact, PackageCompilerFailure, RegistryNativeMetadata};
use serde::{Deserialize, Serialize};

use crate::coverage::{CoverageLine, LaneState};
use crate::drive::Answer;
use crate::fault::{Affordance, Fault};
use crate::identity::Identity;
use crate::outline::{OutlineEntry, OutlineTree};
use crate::page::{Page, Prose, Source};
use crate::product::{IndexJobProjection, ProductView};
use crate::record::{Record, RecordList};
use crate::shelf::{Readiness, Shelf, ShelfEntry};
use crate::signature::{Signature, Token};
use crate::status::Status;
use crate::{Detail, PageProjection};

/// Projects whichever answer a surface produced, tagged by its `answer` field.
///
/// This is the complete Standard/Full projection. CLI JSON and MCP structured
/// output apply the requested detail and budget through the shared projection
/// boundary; Summary pages use [`SummaryPageDto`].
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

/// Full selected-row operand bytes, never a compiler key or proof.
pub use backend_library::SelectedSymbolId as SelectedSymbolIdDto;

/// Closed identity operand facet; display abbreviations and other key families
/// cannot be treated as selected symbol identifiers.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
pub enum IdentitySemanticData {
    /// Full bytes retained from the actual selected symbol row.
    SelectedSymbolId(SelectedSymbolIdDto),
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
    /// Exact selected symbol operand, independently of its short display label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_data: Option<IdentitySemanticData>,
}

impl IdentityDto {
    /// Projects one identity.
    #[must_use]
    pub fn new(identity: &Identity) -> Self {
        Self {
            coordinate: identity.coordinate().as_str().to_owned(),
            trail: identity.trail_within(None),
            name: identity.name().to_owned(),
            shape: shape_name(identity).to_owned(),
            project: identity.project().map(|project| project.root().to_owned()),
            path: identity.path().map(|path| path.as_str().to_owned()),
            line: identity.line().map(crate::identity::LineNumber::get),
            segments: identity
                .trail()
                .segments()
                .iter()
                .map(|segment| segment.as_str().to_owned())
                .collect(),
            key: identity.key().tag().map(|tag| tag.to_string()),
            semantic_data: identity_semantic_data(identity),
        }
    }
}

pub(crate) fn identity_semantic_data(identity: &Identity) -> Option<IdentitySemanticData> {
    match identity.key() {
        crate::IdentityKey::Symbol(key) => SelectedSymbolIdDto::new(*key.as_bytes()).map(IdentitySemanticData::SelectedSymbolId),
        crate::IdentityKey::Package(_) | crate::IdentityKey::Absent => None,
    }
}

pub(crate) const fn shape_name(identity: &Identity) -> &'static str {
    match identity.shape() {
        crate::identity::IdentityShape::Package => "package",
        crate::identity::IdentityShape::Module => "module",
        crate::identity::IdentityShape::Declaration => "declaration",
        crate::identity::IdentityShape::Semantic => "semantic",
        crate::identity::IdentityShape::External => "external",
        crate::identity::IdentityShape::Opaque => "opaque",
    }
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
    /// Lane state, in the readiness vocabulary every surface shares:
    /// `complete`; `indexing` while work runs and the counts still move;
    /// `partial` once work finished with settled source gaps and readable
    /// rows; `unavailable`; or `unobserved`.
    pub state: String,
    /// Completed shards, when the lane is indexing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed: Option<u16>,
    /// Declared shards, when the lane is indexing.
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
                    state: "indexing".to_owned(),
                    completed: Some(completed.get()),
                    total: Some(total.get()),
                    reason: None,
                },
                LaneState::Gapped => Self::simple(lane.name(), "partial"),
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
    /// Exact producer-supplied bounded compiler refusal, including closed phase and kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compiler_failure: Option<PackageCompilerFailure>,
    /// Native setup requirement projected from the same closed compiler cause.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compiler_tool_requirement: Option<CompilerToolRequirementDto>,
    /// Exact partial publication basis and the full typed terminal profile partition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partial_publication: Option<backend_library::IndexJobPartialPublication>,
    /// Exact corpus readiness; distinct from index and vector-search status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_preparation: Option<QueryPreparationDto>,
    /// Saved key with unconfirmed delivery; no terminal mutation outcome is claimed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unconfirmed_index_delivery: Option<backend_library::IndexOperationKey>,
}

/// A query waiting on the single asynchronous corpus preparation worker.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QueryPreparationDto {
    /// Exact view requested by the caller, encoded as its complete identity.
    pub basis: String,
    /// Preparing or retiring; neither is a completed query result.
    pub state: backend_library::QueryPreparationState,
}

/// Machine-actionable setup facts derived from a producer's native tool requirement.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CompilerToolRequirementDto {
    /// Exact selected native tool family.
    pub tool: CompilerNativeToolFact,
    /// Executable expected by the compiler registry.
    pub executable: String,
    /// Public host configuration variable for this executable family.
    pub configuration_variable: String,
    /// Currently configured family, when a setup mismatch supplied one.
    pub configured_tool: Option<CompilerNativeToolFact>,
    /// Whether configuring this native tool can satisfy the setup requirement.
    pub configuration_required: bool,
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
            unconfirmed_index_delivery: fault.unconfirmed_index_delivery(),
            compiler_failure: fault.compiler_failure().cloned(),
            partial_publication: fault.partial_publication_receipt().cloned(),
            query_preparation: fault.query_preparation().map(|(basis, state)| {
                QueryPreparationDto {
                    basis: backend_library::encode_id(basis.as_bytes()),
                    state,
                }
            }),
            compiler_tool_requirement: fault.compiler_failure().and_then(|failure| {
                failure
                    .required_native_tool()
                    .map(|tool| CompilerToolRequirementDto {
                        tool,
                        executable: tool.executable().to_owned(),
                        configuration_variable: tool.configuration_variable().to_owned(),
                        configured_tool: failure.configured_native_tool(),
                        configuration_required: failure.requires_tool_configuration(),
                    })
            }),
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tokens: Vec<SignatureTokenDto>,
    /// The producer's captured documentation, as paragraphs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prose: Vec<String>,
    /// Members grouped by kind.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<MemberGroupDto>,
    /// Labelled relation groups.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relations: Vec<RelationGroupDto>,
    /// The captured source, when there is any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceDto>,
    /// Why there is no source, when there is none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_fault: Option<FaultDto>,
    /// Faults explaining any section this page could not fill.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<FaultDto>,
}

/// The owned decoder for a Summary declaration page.
///
/// The identity, kind, language and signature are retained facts. Sections
/// omitted by the Summary projection do not become empty page sections.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SummaryPageDto {
    /// The retained public identity, including its readable trail and segments.
    pub identity: IdentityDto,
    /// The declaration kind, when observed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The observed source language.
    pub language: String,
    /// The retained signature text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// Why captured source is unavailable, when the producer supplied a fault.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_fault: Option<FaultDto>,
    /// Retained failures explaining unavailable page sections.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
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
        Self::from_projection(&PageProjection::new(page, Detail::Standard))
    }

    /// Projects the fields chosen by the shared typed page policy.
    #[must_use]
    pub fn from_projection(page: &PageProjection<'_>) -> Self {
        Self {
            identity: IdentityDto::new(page.identity()),
            kind: page.kind().map(|kind| kind.name().to_owned()),
            language: page.language().name().to_owned(),
            signature: page.signature().map(Signature::text),
            tokens: page
                .signature_tokens()
                .included()
                .unwrap_or_default()
                .iter()
                .map(SignatureTokenDto::new)
                .collect(),
            prose: page
                .prose()
                .included()
                .unwrap_or_default()
                .iter()
                .map(|block| match block {
                    Prose::Text(text) | Prose::Code(text) | Prose::Link { label: text, .. } => {
                        text.clone()
                    }
                })
                .collect(),
            members: page
                .members()
                .included()
                .unwrap_or_default()
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
                .included()
                .unwrap_or_default()
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
            source: page.source().included().and_then(source_dto),
            source_fault: page.source_fault().map(FaultDto::new),
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
    /// Per-query outcome of the optional semantic ranking lane.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_search: Option<backend_library::SemanticSearchStatus>,
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
            semantic_search: list.coverage().semantic_search_status(),
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
    /// Rows published so far, when the project is still indexing and the reply
    /// carried the published count. Absent means "not read", never zero.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows: Option<u64>,
    /// The declaration count, or null when this reply supplied no count evidence.
    pub declarations: Option<u64>,
    /// Lanes which finished with settled source gaps. The project is ready and
    /// usable; these lanes say where it is partial.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub partial_lanes: Vec<String>,
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
                Readiness::Indexing { rows } => rows.map(crate::shelf::RowCount::get),
                _ => None,
            },
            declarations: entry.declarations().map(crate::shelf::RowCount::get),
            partial_lanes: entry
                .partial_lanes()
                .iter()
                .map(|lane| crate::coverage::lane_name(*lane).to_owned())
                .collect(),
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
    /// The workspace readiness word `status` prints for the same revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readiness: Option<String>,
    /// Rows the owner published at this revision, when the reply carried them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<u64>,
    /// Lane coverage of the revision, identical to `status`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub coverage: Vec<CoverageDto>,
}

impl ShelfDto {
    /// Projects one shelf.
    #[must_use]
    pub fn new(shelf: &Shelf) -> Self {
        Self {
            revision: shelf.revision().to_string(),
            projects: shelf.entries().iter().map(ShelfEntryDto::new).collect(),
            readiness: shelf.facts().lanes().map(|line| line.readiness().to_owned()),
            rows: shelf.facts().rows().map(crate::shelf::RowCount::get),
            coverage: shelf
                .facts()
                .lanes()
                .map_or_else(Vec::new, |line| CoverageDto::all(*line)),
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
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
    /// Native paging scope, absent for the legacy plain outline query.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<OutlinePageDto>,
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
            page: tree.page().map(|page| OutlinePageDto {
                limit: page.limit,
                more: page.continuation.is_some(),
                coverage: CoverageDto::all(page.coverage),
                source_root: backend_library::encode_id(page.basis.root.as_bytes()),
                source_object: backend_library::encode_id(page.basis.object.as_bytes()),
                branch: backend_library::encode_id(page.basis.branch.as_bytes()),
                log: backend_library::encode_id(page.basis.log.as_bytes()),
                schema: page.basis.schema,
                sequence: page.sequence,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PageDto;

    #[test]
    fn omitted_empty_page_sections_decode_and_remain_omitted() {
        let value = serde_json::json!({
            "identity": {
                "coordinate": "/workspace::Widget",
                "trail": "Widget",
                "name": "Widget",
                "shape": "declaration",
                "segments": ["Widget"]
            },
            "language": "typescript"
        });

        let page: PageDto = serde_json::from_value(value.clone()).expect("sparse page DTO");
        assert!(page.tokens.is_empty());
        assert!(page.prose.is_empty());
        assert!(page.members.is_empty());
        assert!(page.relations.is_empty());
        assert!(page.notes.is_empty());
        assert_eq!(
            serde_json::to_value(page).expect("serialized page DTO"),
            value
        );
    }
}

/// Exact admitted scope of an outline page. The continuation itself is
/// transported in the common `nextCursor` envelope, never inside this DTO.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OutlinePageDto {
    /// Effective native query limit; reuse unchanged with the next cursor.
    pub limit: u16,
    /// The owner has another page.
    pub more: bool,
    /// Complete lane coverage facts for this projection.
    pub coverage: Vec<CoverageDto>,
    /// Exact producer source relation root.
    pub source_root: String,
    /// Exact producer source object.
    pub source_object: String,
    /// Producer branch.
    pub branch: String,
    /// Producer log.
    pub log: String,
    /// Producer schema.
    pub schema: u16,
    /// Producer sequence.
    pub sequence: u64,
}

/// The engine's whole state.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StatusDto {
    /// Owner availability proven by the admitted health reply.
    #[serde(default = "unknown_status_facet")]
    pub availability: String,
    /// Exact visible view cardinality facet; not a claim about pending work.
    #[serde(default = "unknown_status_facet")]
    pub publication: String,
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

fn unknown_status_facet() -> String {
    "unknown".to_owned()
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
            availability: status.availability().to_owned(),
            publication: status.publication_state().name().to_owned(),
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
    /// Stable index-search snapshot and continuation metadata, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_search_page: Option<ProductIndexSearchPageDto>,
    /// Exact owner-issued indexing ticket, observation, or terminal receipt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_job: Option<IndexJobProjection>,
    /// Exact durable index-operation status, including a Published receipt or
    /// typed failure/unresolved reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_operation: Option<backend_library::IndexOperationObservation>,
    /// Exact executable observation of accepted indexing work, when present.
    /// This is guidance derived from the retained typed result, not authority
    /// to start a mutation or a claim that publication has completed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_poll: Option<IndexPollDto>,
    /// Exact checked Project membership captured with the selected semantic generations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_source_frontier: Option<backend_library::SelectedProjectSourceFrontier>,
    /// Exact selected Project source-membership page and its continuation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_source_membership_page:
        Option<backend_library::PackageSourceMembershipPageResultV1>,
    /// Current selected compiler captures and complete diagnostic read selectors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compiler_outcomes: Option<backend_library::PackageCompilerOutcomes>,
    /// Exact guarded native graph page, retaining every edge and authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_graph_page: Option<backend_library::PackageGraphPage>,
    /// Exact existing shape wire tree exported after direct certificate admission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_data: Option<crate::product::ProductSemanticData>,
    /// Exact source-only package metadata lookup evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_discovery: Option<crate::product::PackageDiscoveryProjection>,
}

/// Copyable CLI and MCP actions for the same admitted index observation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IndexPollDto {
    /// Optional command in an observed installed CLI/default-authority context.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    /// Exact next MCP tool call; no new index mutation is inferred.
    pub call: serde_json::Value,
}

impl IndexPollDto {
    /// Derives one observation from the shared typed operation/job policy.
    #[must_use]
    pub fn new(view: &ProductView) -> Option<Self> {
        Self::from_affordance(view.index_poll_affordance()?, None, view.cli_context())
    }

    /// Keeps the requested receipt detail when the next MCP action is copied.
    #[must_use]
    pub fn for_detail(view: &ProductView, detail: Detail) -> Option<Self> {
        Self::from_affordance(
            view.index_poll_affordance()?,
            Some(detail),
            view.cli_context(),
        )
    }

    /// Projects an admitted unchanged reference with the same detail policy.
    #[must_use]
    pub fn for_reference(
        reference: &backend_client::IndexProgressReference,
        detail: Detail,
    ) -> Option<Self> {
        Self::from_affordance(
            Affordance::ObserveIndexChanges {
                reference: reference.clone(),
            },
            Some(detail),
            None,
        )
    }

    /// Supplies the same optional observed CLI context for a conditional poll.
    #[must_use]
    pub fn for_reference_in_context(
        reference: &backend_client::IndexProgressReference,
        detail: Detail,
        context: Option<&crate::StartupRecoveryContext>,
    ) -> Option<Self> {
        Self::from_affordance(
            Affordance::ObserveIndexChanges {
                reference: reference.clone(),
            },
            Some(detail),
            context,
        )
    }

    fn from_affordance(
        affordance: Affordance,
        detail: Option<Detail>,
        context: Option<&crate::StartupRecoveryContext>,
    ) -> Option<Self> {
        let mut call = affordance.tool_call()?;
        if let Some(detail) = detail {
            call.get_mut("arguments")?.as_object_mut()?.insert(
                "detail".to_owned(),
                serde_json::Value::String(detail.name().to_owned()),
            );
        }
        Some(Self {
            shell: context.and_then(|context| affordance.poll_shell_in_context(context, detail)),
            call,
        })
    }
}

/// Shared page envelope projected for CLI, MCP, and desktop product replies.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProductIndexSearchPageDto {
    /// Structural index snapshot shared by pages in the cursor chain.
    pub snapshot: [u8; 32],
    /// Time at which mutable overlays were evaluated.
    pub evaluated_at_millis: u64,
    /// Exact count, lower bound, or unknown count state.
    pub result_count: backend_library::IndexSearchResultCount,
    /// Opaque continuation for the next page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// One product record.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProductRecordDto {
    /// Completeness of retained source text in a batch declaration read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extent: Option<backend_library::DeclarationReadExtent>,
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
    /// Static Python source declarations, independent of registry metadata authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_metadata: Option<backend_library::PythonProjectMetadata>,
    /// Exact bounded forge commit, manifest, and repository metadata facts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forge: Option<backend_library::ForgePackageRecord>,
    /// Typed forge manifest details, including a source pin with no release PURL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forge_package_detail: Option<backend_library::ForgePackageDetailRecord>,
    /// Source-attributed, source-only registry facts for a discovered row.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discovery: Option<backend_library::RegistryDiscoveryCandidate>,
    /// Source-scoped, version-specific lineage group for index search.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package_group: Option<backend_library::RegistryPackageSearchGroup>,
    /// Exact local declaration source identity; absent for registry packages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_declaration: Option<backend_library::LocalDeclarationSearchRecord>,
    /// Concise typed derived-history state for the readable generation row.
    /// The complete immutable proof remains in `semantic_data`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_status: Option<crate::product::ProductSemanticHistoryStatus>,
    /// Exact canonical two-byte profile of an immutable semantic generation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compiler_profile: Option<backend_library::SemanticLanguageProfile>,
}

impl ProductDto {
    /// Projects one product answer.
    #[must_use]
    pub fn new(view: &ProductView) -> Self {
        Self::project(view, None)
    }

    /// Projects the complete receipt and preserves detail in its next action.
    #[must_use]
    pub fn for_detail(view: &ProductView, detail: Detail) -> Self {
        Self::project(view, Some(detail))
    }

    fn project(view: &ProductView, detail: Option<Detail>) -> Self {
        Self {
            heading: view.heading().to_owned(),
            records: view
                .records()
                .iter()
                .map(|record| ProductRecordDto {
                    extent: record.extent(),
                    title: record.title().to_owned(),
                    operand: record.operand().map(ToOwned::to_owned),
                    tags: record.tags().to_vec(),
                    native_metadata: record.native_metadata().cloned(),
                    source_metadata: record.source_metadata().cloned(),
                    forge: record.forge_details().cloned(),
                    forge_package_detail: record.forge_package_detail().cloned(),
                    discovery: record.discovery_details().cloned(),
                    package_group: record.package_group().cloned(),
                    local_declaration: record.local_declaration().cloned(),
                    history_status: record.history_status().cloned(),
                    compiler_profile: record.compiler_profile(),
                })
                .collect(),
            note: view.note().map(ToOwned::to_owned),
            fault: view.fault().map(FaultDto::new),
            index_search_page: view
                .index_search_page()
                .map(|page| ProductIndexSearchPageDto {
                    snapshot: *page.snapshot(),
                    evaluated_at_millis: page.evaluated_at_millis(),
                    result_count: page.result_count(),
                    next_cursor: page.next_cursor().map(ToOwned::to_owned),
                }),
            index_job: view.index_job().cloned(),
            index_operation: view.index_operation().cloned(),
            index_poll: match detail {
                Some(detail) => IndexPollDto::for_detail(view, detail),
                None => IndexPollDto::new(view),
            },
            selected_source_frontier: view.selected_source_frontier().cloned(),
            package_source_membership_page: view.package_source_membership_page().cloned(),
            compiler_outcomes: view.compiler_outcomes().cloned(),
            package_graph_page: view.package_graph_page().cloned(),
            semantic_data: view.semantic_data().cloned(),
            package_discovery: view.package_discovery().cloned(),
        }
    }
}

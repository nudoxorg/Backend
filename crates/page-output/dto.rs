//! Canonical pure identity, coverage and record projections.
use crate::coverage::{CoverageLine, LaneState};
use crate::identity::Identity;
use crate::record::{Record, RecordList};
use crate::signature::{Signature, Token};
use serde::{Deserialize, Serialize};

/// Full bytes of an observed selected symbol row, exported as display operand
/// material. This does not deserialize into a compiler `SymbolKey` or proof.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct SelectedSymbolIdDto([u8; 32]);

impl SelectedSymbolIdDto {
    /// Exact retained row bytes for a public selected-symbol operand.
    #[must_use]
    pub const fn bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl<'de> Deserialize<'de> for SelectedSymbolIdDto {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let bytes = <[u8; 32]>::deserialize(deserializer)?;
        if bytes == [0; 32] {
            return Err(serde::de::Error::custom("selected symbol row id is empty"));
        }
        Ok(Self(bytes))
    }
}

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
            shape: shape_name(identity),
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
            semantic_data: match identity.key() {
                crate::IdentityKey::Symbol(key) if key.as_bytes() != &[0; 32] => Some(
                    IdentitySemanticData::SelectedSymbolId(SelectedSymbolIdDto(*key.as_bytes())),
                ),
                crate::IdentityKey::Package(_)
                | crate::IdentityKey::Symbol(_)
                | crate::IdentityKey::Absent => None,
            },
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

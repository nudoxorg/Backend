//! Reply-only source-policy claims. Deserialization never authenticates a claim.
//!
//! Existing 578 producers do not have the selected NSE capability and therefore
//! emit only `Unknown`. A captured statement is not an admitted observation
//! until its owner seal and the selected snapshot are independently checked.
use serde::{Deserialize, Serialize};

/// The producer cannot establish global discovery-policy stability.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourcePolicyStability {
    /// No claim is made about the stability of effective source policy.
    Unknown,
}

/// Complete bounded claim. Raw decoded fields are not source or compiler authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourcePolicyStatement {
    symbol: [u8; 32],
    selection_version: [u8; 32],
    #[serde(deserialize_with = "deserialize_coordinate")]
    selection_coordinate: String,
    view_root: [u8; 32],
    source_object: [u8; 32],
    source_branch: [u8; 32],
    source_log: [u8; 32],
    source_schema: u16,
    workspace_root: [u8; 32],
    evidence_root: [u8; 32],
    project: [u8; 32],
    profile: crate::SemanticLanguageProfile,
    publication_profile: crate::SemanticLanguageProfile,
    #[serde(deserialize_with = "deserialize_coordinate")]
    publication_coordinate: String,
    #[serde(deserialize_with = "deserialize_path")]
    source_path: String,
    source_fact: [u8; 32],
    input_digest: [u8; 32],
    byte_len: u64,
    publication_binding: [u8; 32],
    image_identity: [u8; 32],
    image_byte_len: u32,
    program_membership: [u8; 32],
    entity_index: u32,
    declaration_family: [u8; 16],
    declaration_variant: [u8; 16],
    policy_stability: SourcePolicyStability,
}
impl crate::NativeOwnerStatement for SourcePolicyStatement {
    const DOMAIN: &'static [u8] = b"backend.source-policy.qualification.v1\0";
}
fn valid_path(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 4096
        && !p.starts_with('/')
        && !p.contains('\\')
        && !p.bytes().any(|b| b.is_ascii_control())
        && !p
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        && !p.as_bytes().get(1).is_some_and(|b| *b == b':')
}
fn deserialize_path<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    deserializer.deserialize_str(BoundedText { path: true })
}
fn deserialize_coordinate<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<String, D::Error> {
    deserializer.deserialize_str(BoundedText { path: false })
}
struct BoundedText {
    path: bool,
}
impl<'de> serde::de::Visitor<'de> for BoundedText {
    type Value = String;
    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("bounded source policy text")
    }
    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<String, E> {
        let valid = if self.path {
            valid_path(value)
        } else {
            !value.is_empty() && value.len() <= 4096 && !value.bytes().any(|b| b.is_ascii_control())
        };
        if !valid {
            return Err(E::custom("invalid bounded source policy text"));
        }
        Ok(value.to_owned())
    }
    fn visit_string<E: serde::de::Error>(self, value: String) -> Result<String, E> {
        let valid = if self.path {
            valid_path(&value)
        } else {
            !value.is_empty() && value.len() <= 4096 && !value.bytes().any(|b| b.is_ascii_control())
        };
        if !valid {
            return Err(E::custom("invalid bounded source policy text"));
        }
        Ok(value)
    }
}
/// Untrusted observation fields supplied by an owner that has independently
/// admitted a selected native entity and verified its actual source bytes.
/// Constructing these fields does not establish source or compiler authority.
#[derive(Clone, Debug)]
pub struct SourcePolicyObservation {
    /// Exact selected workspace root.
    pub workspace_root: [u8; 32],
    /// Exact selected native source evidence relation root.
    pub evidence_root: [u8; 32],
    /// Exact owning project key.
    pub project: [u8; 32],
    /// Per-file native language profile.
    pub profile: crate::SemanticLanguageProfile,
    /// Exact selected compiler publication profile, which can differ from the file profile.
    pub publication_profile: crate::SemanticLanguageProfile,
    /// Selected compiler publication coordinate.
    pub publication_coordinate: String,
    /// Confined, portable source path.
    pub source_path: String,
    /// Source fact digest claim, checked against the actual host by the issuer.
    pub source_fact: [u8; 32],
    /// Input digest claim, checked against the same actual host buffer.
    pub input_digest: [u8; 32],
    /// Actual verified source byte length.
    pub byte_len: u64,
    /// Selected compilation binding identity.
    pub publication_binding: [u8; 32],
    /// Reopened semantic image identity.
    pub image_identity: [u8; 32],
    /// Reopened semantic image byte length.
    pub image_byte_len: u32,
    /// Admitted native program membership identity.
    pub program_membership: [u8; 32],
    /// Exact dense entity coordinate, meaningful only with the admitted image identity.
    pub entity_index: u32,
    /// Exact selected declaration family.
    pub declaration_family: [u8; 16],
    /// Exact selected declaration variant.
    pub declaration_variant: [u8; 16],
}

impl SourcePolicyStatement {
    fn valid(&self) -> bool {
        self.profile.profile().is_ok()
            && self.publication_profile.profile().is_ok()
            && !self.selection_coordinate.is_empty()
            && self.selection_coordinate.len() <= 4096
            && !self.selection_coordinate.bytes().any(|byte| byte.is_ascii_control())
            && valid_path(&self.source_path)
            && !self.publication_coordinate.is_empty()
            && self.publication_coordinate.len() <= 4096
            && !self
                .publication_coordinate
                .bytes()
                .any(|b| b.is_ascii_control())
            && self.image_byte_len != 0
    }
    /// Checks the raw claim against the selected document, without authenticating it.
    pub fn matches_selection(&self, selection: &crate::DocumentSelection) -> bool {
        let basis = selection.source();
        self.valid()
            && self.symbol == *selection.symbol().as_bytes()
            && self.selection_version == *selection.version().as_bytes()
            && self.selection_coordinate == selection.coordinate()
            && self.view_root == *basis.root.as_bytes()
            && self.source_object == *basis.object.as_bytes()
            && self.source_branch == *basis.branch.as_bytes()
            && self.source_log == *basis.log.as_bytes()
            && self.source_schema == basis.schema
    }
}

/// Reply qualification. A captured statement is an unverified owner-sealed claim.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourcePolicyQualification {
    /// No selected observation is proven, including older replies.
    #[default]
    Unknown,
    /// Claimed captured observations, with stability explicitly unknown.
    CapturedObserved {
        /// Boxed claim keeps unknown per-row results compact.
        data: Box<SourcePolicyCaptured>,
    },
}
/// Unverified statement and its owner seal; neither is standalone authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourcePolicyCaptured {
    statement: SourcePolicyStatement,
    seal: [u8; 32],
}
/// A seal-verified claim. Snapshot and actual source checks remain mandatory.
#[derive(Clone, Copy, Debug)]
pub struct VerifiedSourcePolicyClaim<'a> {
    statement: &'a SourcePolicyStatement,
}
impl VerifiedSourcePolicyClaim<'_> {
    /// The owner-authenticated statement; it does not prove current host bytes.
    pub fn statement(&self) -> &SourcePolicyStatement {
        self.statement
    }
}
impl SourcePolicyQualification {
    /// Checks claim correlation with an independently selected result owner.
    /// This only checks shape; it does not authenticate the owner or seal.
    #[must_use]
    pub fn matches_result_owner(
        &self,
        selection: &crate::DocumentSelection,
        owner: crate::SymbolKey,
        basis: crate::Basis,
    ) -> bool {
        selection.symbol() == owner
            && selection.source() == basis
            && matches!(self, Self::CapturedObserved { data } if data.statement.matches_selection(selection))
    }

    /// Seal a captured observation for this exact selected document.
    ///
    /// This constructs a claim, not authority from the supplied fields. The
    /// issuer must first admit the selected native entity and verify actual
    /// source bytes. Receivers must still verify the seal and selected snapshot.
    pub fn issue_observed(
        authority: &[u8; 32],
        selection: &crate::DocumentSelection,
        observed: SourcePolicyObservation,
    ) -> Result<Self, crate::ProductAdmissionError> {
        let basis = selection.source();
        let statement = SourcePolicyStatement {
            symbol: *selection.symbol().as_bytes(),
            selection_version: *selection.version().as_bytes(),
            selection_coordinate: selection.coordinate().to_owned(),
            view_root: *basis.root.as_bytes(),
            source_object: *basis.object.as_bytes(),
            source_branch: *basis.branch.as_bytes(),
            source_log: *basis.log.as_bytes(),
            source_schema: basis.schema,
            workspace_root: observed.workspace_root,
            evidence_root: observed.evidence_root,
            project: observed.project,
            profile: observed.profile,
            publication_profile: observed.publication_profile,
            publication_coordinate: observed.publication_coordinate,
            source_path: observed.source_path,
            source_fact: observed.source_fact,
            input_digest: observed.input_digest,
            byte_len: observed.byte_len,
            publication_binding: observed.publication_binding,
            image_identity: observed.image_identity,
            image_byte_len: observed.image_byte_len,
            program_membership: observed.program_membership,
            entity_index: observed.entity_index,
            declaration_family: observed.declaration_family,
            declaration_variant: observed.declaration_variant,
            policy_stability: SourcePolicyStability::Unknown,
        };
        if !statement.matches_selection(selection) {
            return Err(crate::ProductAdmissionError::SemanticVersionShape);
        }
        let seal = crate::seal_native_owner_statement(authority, &statement)?;
        Ok(Self::CapturedObserved {
            data: Box::new(SourcePolicyCaptured { statement, seal }),
        })
    }
    /// Bound used before accepting the encoded claim, including both bounded strings.
    pub(crate) fn encoded_size_bound(&self) -> usize {
        match self {
            Self::Unknown => 0,
            Self::CapturedObserved { data } => 4096usize
                .saturating_add(data.statement.selection_coordinate.len().saturating_mul(6))
                .saturating_add(data.statement.source_path.len().saturating_mul(6))
                .saturating_add(
                    data.statement
                        .publication_coordinate
                        .len()
                        .saturating_mul(6),
                ),
        }
    }

    pub(crate) fn matches_document(&self, document: &crate::Document) -> bool {
        match self {
            Self::Unknown => document
                .source_range
                .as_ref()
                .is_none_or(|page| page.source_policy.is_unknown()),
            Self::CapturedObserved { data } => {
                let Some(selection) = document.selection.as_ref() else {
                    return false;
                };
                data.statement.matches_selection(selection)
                    && selection.symbol() == document.symbol
                    && document.source_basis() == Some(selection.source())
                    && document.location.captured().is_none_or(|location| {
                        location.path() == data.statement.source_path
                    })
                    && document.source_range.as_ref().is_none_or(|page| {
                        page.source_policy == *self
                            && page.source_identity == data.statement.source_fact
                    })
            }
        }
    }
    /// Correlate a raw per-record claim with its requested label and selected
    /// row digest. This recomputes a commitment only; it mints no typed ID.
    pub(crate) fn matches_record(&self, record: &crate::DeclarationRecord) -> bool {
        match self {
            Self::Unknown => true,
            Self::CapturedObserved { data } => {
                let claim = &data.statement;
                if !claim.valid() || record.stable_id != Some(claim.symbol)
                    || record.label.as_str() != claim.selection_coordinate.as_str()
                { return false; }
                let bytes = crate::view::document_selection_canonical_bytes(
                    [claim.symbol, claim.view_root, claim.source_object, claim.source_branch, claim.source_log],
                    claim.source_schema, &claim.selection_coordinate,
                );
                *crate::DocumentVersion::from_value(&bytes).as_bytes() == claim.selection_version
            }
        }
    }
    /// Checks a reference's raw caller, selection commitment and byte bounds.
    /// This does not authenticate the peer, owner, edge, or source bytes.
    #[must_use]
    pub fn matches_reference_shape(&self, record: &crate::ReferenceRecord) -> bool {
        let Self::CapturedObserved { data } = self else { return self.is_unknown() };
        let claim = &data.statement;
        let Some(span) = &record.evidence.source else { return false };
        if !claim.valid() || record.site_symbol != Some(claim.symbol)
            || record.site.as_str() != claim.selection_coordinate
            || span.file.as_str() != claim.source_path
            || span.start >= span.end || u64::from(span.end) > claim.byte_len
        { return false; }
        let bytes = crate::view::document_selection_canonical_bytes(
            [claim.symbol, claim.view_root, claim.source_object, claim.source_branch, claim.source_log],
            claim.source_schema, &claim.selection_coordinate,
        );
        *crate::DocumentVersion::from_value(&bytes).as_bytes() == claim.selection_version
    }

    /// Correlates a raw claim with an independently admitted complete view basis.
    /// This does not mint source authority.
    pub(crate) fn matches_graph_basis(&self, basis: crate::Basis) -> bool {
        match self {
            Self::Unknown => true,
            Self::CapturedObserved { data } => {
                let claim = &data.statement;
                claim.view_root == *basis.root.as_bytes()
                    && claim.source_object == *basis.object.as_bytes()
                    && claim.source_branch == *basis.branch.as_bytes()
                    && claim.source_log == *basis.log.as_bytes()
                    && claim.source_schema == basis.schema
            }
        }
    }

    /// Checks a raw graph owner against this claim's selected symbol and view.
    /// This validates correlation only; it does not authenticate an edge or peer.
    pub(crate) fn matches_graph_shape(&self, graph_id: &[u8; 32], coordinate: Option<&str>) -> bool {
        let Self::CapturedObserved { data } = self else { return self.is_unknown() };
        let claim = &data.statement;
        if !claim.valid()
            || coordinate.is_some_and(|value| value != claim.selection_coordinate) { return false; }
        let bytes = crate::view::document_selection_canonical_bytes(
            [claim.symbol, claim.view_root, claim.source_object, claim.source_branch, claim.source_log],
            claim.source_schema, &claim.selection_coordinate,
        );
        if *crate::DocumentVersion::from_value(&bytes).as_bytes() != claim.selection_version { return false; }
        let mut hash = blake3::Hasher::new();
        hash.update(b"nudox.rich-ir.graph.node.row.v1\0");
        hash.update(b"symbol:");
        hash.update(crate::canonical::encode_id(&claim.symbol).as_bytes());
        hash.finalize().as_bytes() == graph_id
    }

    /// Correlates a raw observed center claim with the original page address.
    /// This does not authenticate the claim or promote its bytes to a key.
    pub(crate) fn matches_graph_page_address(&self, address: crate::SymbolAddress) -> bool {
        let Self::CapturedObserved { data } = self else { return false };
        let claim = &data.statement;
        if !claim.valid() { return false; }
        let requested = address.claimed_bytes();
        if address.is_selected() { return requested == claim.symbol; }
        requested == claim.symbol
            || requested == crate::canonical::symbol_key(&claim.selection_coordinate).to_bytes()
    }

    /// Whether this reply has no observed policy qualification.
    pub const fn is_unknown(&self) -> bool {
        matches!(self, Self::Unknown)
    }
    /// Verify the owner seal and exact selected document; revalidate the snapshot separately.
    pub fn verify_owner<'a>(
        &'a self,
        authority: &[u8; 32],
        selection: &crate::DocumentSelection,
    ) -> Result<VerifiedSourcePolicyClaim<'a>, crate::ProductAdmissionError> {
        let Self::CapturedObserved { data } = self else {
            return Err(crate::ProductAdmissionError::SemanticVersionShape);
        };
        let statement = &data.statement;
        if !statement.matches_selection(selection) {
            return Err(crate::ProductAdmissionError::SemanticVersionShape);
        }
        crate::verify_native_owner_statement(authority, statement, &data.seal)?;
        Ok(VerifiedSourcePolicyClaim { statement })
    }
}

/// A result-specific selected source-policy claim. This carrier checks
/// correlation only; a local authenticated channel and owner seal are separate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectedResultSourcePolicy {
    selection: crate::DocumentSelection,
    qualification: SourcePolicyQualification,
}

impl SelectedResultSourcePolicy {
    /// Associate a claim with an independently selected result owner and basis.
    #[must_use]
    pub fn new(
        owner: crate::SymbolKey,
        basis: crate::Basis,
        selection: crate::DocumentSelection,
        qualification: SourcePolicyQualification,
    ) -> Option<Self> {
        qualification.matches_result_owner(&selection, owner, basis).then_some(Self {
            selection, qualification,
        })
    }
    /// Exact selected declaration associated with this claim.
    #[must_use]
    pub fn selection(&self) -> &crate::DocumentSelection { &self.selection }
    /// Untrusted observed claim; admission and seal verification remain separate.
    #[must_use]
    pub fn qualification(&self) -> &SourcePolicyQualification { &self.qualification }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::canonical::{object_version, symbol_key, view_state_root};

    fn claim() -> (SourcePolicyStatement, crate::DocumentSelection) {
        let basis = crate::Basis::new(view_state_root(&[]), object_version(b"fixture"));
        let selection = crate::DocumentSelection::from_admitted_parts(
            symbol_key("fixture::x"),
            basis,
            "fixture::x".into(),
        )
        .expect("fixture selection");
        (
            SourcePolicyStatement {
                symbol: *selection.symbol().as_bytes(),
                selection_version: *selection.version().as_bytes(),
                selection_coordinate: selection.coordinate().to_owned(),
                view_root: *basis.root.as_bytes(),
                source_object: *basis.object.as_bytes(),
                source_branch: *basis.branch.as_bytes(),
                source_log: *basis.log.as_bytes(),
                source_schema: basis.schema,
                workspace_root: [12; 32],
                evidence_root: [13; 32],
                project: [2; 32],
                profile: crate::SemanticLanguageProfile::from_name("typescript").expect("profile"),
                publication_profile: crate::SemanticLanguageProfile::from_name("tsx")
                    .expect("publication profile"),
                publication_coordinate: "pkg:npm/example@1.0.0".into(),
                source_path: "src/a.ts".into(),
                source_fact: [3; 32],
                input_digest: [4; 32],
                byte_len: 9,
                publication_binding: [5; 32],
                image_identity: [6; 32],
                image_byte_len: 128,
                program_membership: [9; 32],
                entity_index: 1,
                declaration_family: [10; 16],
                declaration_variant: [11; 16],
                policy_stability: SourcePolicyStability::Unknown,
            },
            selection,
        )
    }
    #[test]
    fn captured_document_location_must_match_the_sealed_source_path() {
        let (statement, selection) = claim();
        let seal = crate::seal_native_owner_statement(&[7; 32], &statement).expect("seal");
        let qualification = SourcePolicyQualification::CapturedObserved {
            data: Box::new(SourcePolicyCaptured { statement, seal }),
        };
        let basis = selection.source();
        let mut document = crate::Document::new(selection.symbol(), basis.root, [])
            .with_source_basis(basis).with_selection(selection).expect("selection");
        document.source_policy = qualification.clone();
        document.location = crate::SourceAvailability::Captured(crate::SourceLocation::new("src/a.ts", 3).expect("site"));
        assert!(qualification.matches_document(&document));
        document.location = crate::SourceAvailability::Captured(crate::SourceLocation::new("src/other.ts", 3).expect("site"));
        assert!(!qualification.matches_document(&document));
        document.source_policy = SourcePolicyQualification::Unknown;
        assert!(document.source_policy.matches_document(&document), "legacy unknown remains unchanged");
    }
    #[test]
    fn result_policy_correlates_owner_basis_and_selection() {
        let (first_statement, first) = claim();
        let second = crate::DocumentSelection::from_admitted_parts(
            symbol_key("fixture::other"), first.source(), "fixture::other".into(),
        ).expect("second selection");
        let mut second_statement = first_statement.clone();
        second_statement.symbol = *second.symbol().as_bytes();
        second_statement.selection_version = *second.version().as_bytes();
        second_statement.selection_coordinate = second.coordinate().to_owned();
        let seal = |statement: SourcePolicyStatement| SourcePolicyQualification::CapturedObserved {
            data: Box::new(SourcePolicyCaptured {
                seal: crate::seal_native_owner_statement(&[7; 32], &statement).expect("seal"),
                statement,
            }),
        };
        let first_claim = seal(first_statement);
        let second_claim = seal(second_statement);
        assert!(SelectedResultSourcePolicy::new(first.symbol(), first.source(), first.clone(), first_claim.clone()).is_some());
        assert!(SelectedResultSourcePolicy::new(first.symbol(), first.source(), first.clone(), second_claim.clone()).is_none());
        assert!(SelectedResultSourcePolicy::new(first.symbol(), first.source(), second.clone(), second_claim).is_none());
        let other_basis = crate::Basis::new(view_state_root(&[]), object_version(b"other"));
        assert!(SelectedResultSourcePolicy::new(first.symbol(), other_basis, first.clone(), first_claim).is_none());
        assert!(SelectedResultSourcePolicy::new(first.symbol(), first.source(), first, SourcePolicyQualification::Unknown).is_none());
    }

    #[test]
    fn graph_claim_correlates_origin_node_and_exact_view() {
        let (statement, selection) = claim();
        let seal = crate::seal_native_owner_statement(&[7; 32], &statement).expect("seal");
        let claim = SourcePolicyQualification::CapturedObserved {
            data: Box::new(SourcePolicyCaptured { statement, seal }),
        };
        let node = crate::GraphNodeId::for_symbol(selection.symbol());
        assert!(claim.matches_graph_shape(node.as_bytes(), Some(selection.coordinate())));
        assert!(!claim.matches_graph_shape(&[0; 32], Some(selection.coordinate())));
        assert!(!claim.matches_graph_shape(node.as_bytes(), Some("fixture::other")));
        assert!(claim.matches_graph_basis(selection.source()));
        let wrong_basis = crate::Basis::new(view_state_root(&[("a".into(), "b".into())]), object_version(b"fixture"));
        assert!(!claim.matches_graph_basis(wrong_basis));
    }

    #[test]
    fn observed_graph_builder_requires_gap_before_finish() {
        let (statement, selection) = claim();
        let seal = crate::seal_native_owner_statement(&[7; 32], &statement).expect("seal");
        let claim = SourcePolicyQualification::CapturedObserved {
            data: Box::new(SourcePolicyCaptured { statement, seal }),
        };
        let id = crate::GraphNodeId::for_symbol(selection.symbol());
        // Materialized graph root can differ from the full selected view basis.
        let revision = crate::RichGraphRevision::new([9; 32],
            crate::SemanticGenerationId::new([1; 32]), [2; 32]);
        let builder = || {
            let mut graph = crate::RichGraphBuilder::new(revision, id);
            let mut node = crate::RichGraphNode::new(id, "x", Some(selection.coordinate().to_owned()),
                Some("Function".to_owned()), crate::GraphAvailability::Ready, None).expect("node");
            node.source_policy = claim.clone();
            graph.add_node(node).expect("insert node");
            graph
        };
        assert!(builder().finish().is_err());
        let gap = crate::GraphAvailability::unavailable("observed policy stability unknown").expect("gap");
        assert!(builder().finish_with_availability(gap).is_ok());
    }

    #[test]
    fn unknown_defaults_and_closed_captured_shape() {
        assert!(std::mem::size_of::<SourcePolicyQualification>() <= 16);
        #[derive(Serialize, Deserialize)]
        struct Page {
            #[serde(default, skip_serializing_if = "SourcePolicyQualification::is_unknown")]
            policy: SourcePolicyQualification,
        }
        let legacy: Page = serde_json::from_str("{}").expect("legacy");
        assert!(legacy.policy.is_unknown());
        assert_eq!(serde_json::to_string(&legacy).expect("encode"), "{}");
        assert!(
            serde_json::from_str::<SourcePolicyQualification>(r#"{"state":"complete"}"#).is_err()
        );
        assert!(
            serde_json::from_str::<SourcePolicyQualification>(r#"{"state":"unknown","extra":1}"#)
                .is_err()
        );
        let (statement, _) = claim();
        let mut raw = serde_json::to_value(&statement).expect("statement");
        raw["source_path"] = serde_json::json!("../escape.ts");
        assert!(serde_json::from_value::<SourcePolicyStatement>(raw).is_err());
    }

    #[test]
    fn graph_page_center_witness_matches_exact_requested_address() {
        let (mut statement, first) = claim();
        // A semantic key may differ from the hash of its display coordinate.
        let native = crate::DocumentSelection::from_admitted_parts(
            symbol_key("native-entity-key"), first.source(), first.coordinate().to_owned(),
        ).expect("native selected row");
        statement.symbol = *native.symbol().as_bytes();
        statement.selection_version = *native.version().as_bytes();
        statement.selection_coordinate = native.coordinate().to_owned();
        let qualification = SourcePolicyQualification::CapturedObserved {
            data: Box::new(SourcePolicyCaptured {
                seal: crate::seal_native_owner_statement(&[7; 32], &statement).expect("seal"),
                statement,
            }),
        };
        assert!(qualification.matches_graph_page_address(crate::SymbolAddress::selected(native.symbol())));
        assert!(qualification.matches_graph_page_address(crate::SymbolAddress::canonical(symbol_key(native.coordinate()))));
        assert!(!qualification.matches_graph_page_address(crate::SymbolAddress::selected(first.symbol())));
        assert!(!qualification.matches_graph_page_address(crate::SymbolAddress::canonical(symbol_key("fixture::other"))));
        assert!(!SourcePolicyQualification::Unknown.matches_graph_page_address(crate::SymbolAddress::selected(native.symbol())));
    }
    #[test]
    fn observed_issuer_binds_selection_and_rejects_bad_paths() {
        let (s, selection) = claim();
        let mut observation = SourcePolicyObservation {
            workspace_root: s.workspace_root,
            evidence_root: s.evidence_root,
            project: s.project,
            profile: s.profile,
            publication_profile: s.publication_profile,
            publication_coordinate: s.publication_coordinate,
            source_path: s.source_path,
            source_fact: s.source_fact,
            input_digest: s.input_digest,
            byte_len: s.byte_len,
            publication_binding: s.publication_binding,
            image_identity: s.image_identity,
            image_byte_len: s.image_byte_len,
            program_membership: s.program_membership,
            entity_index: s.entity_index,
            declaration_family: s.declaration_family,
            declaration_variant: s.declaration_variant,
        };
        let issued =
            SourcePolicyQualification::issue_observed(&[7; 32], &selection, observation.clone())
                .expect("selected claim");
        assert!(issued.verify_owner(&[7; 32], &selection).is_ok());
        assert!(issued.verify_owner(&[8; 32], &selection).is_err());
        observation.source_path = "../escape.ts".into();
        assert!(
            SourcePolicyQualification::issue_observed(&[7; 32], &selection, observation).is_err()
        );
    }

    #[test]
    fn seal_is_bound_to_selected_row_basis_and_complete_statement() {
        let (statement, selection) = claim();
        let seal = crate::seal_native_owner_statement(&[7; 32], &statement).expect("seal");
        let claim = SourcePolicyQualification::CapturedObserved {
            data: Box::new(SourcePolicyCaptured { statement, seal }),
        };
        assert!(claim.verify_owner(&[7; 32], &selection).is_ok());
        assert!(claim.verify_owner(&[8; 32], &selection).is_err());
        let other = crate::DocumentSelection::from_admitted_parts(
            symbol_key("fixture::other"),
            selection.source(),
            "fixture::other".into(),
        )
        .expect("other selection");
        assert!(claim.verify_owner(&[7; 32], &other).is_err());
        let mut tampered = serde_json::to_value(&claim).expect("encode");
        tampered["data"]["statement"]["byte_len"] = serde_json::json!(10);
        let reopened: SourcePolicyQualification =
            serde_json::from_value(tampered).expect("parse claim");
        assert!(reopened.verify_owner(&[7; 32], &selection).is_err());
    }

    #[cfg(unix)]
    fn document_policy_for_test(selection: &crate::DocumentSelection) -> SourcePolicyQualification {
        let (statement, expected) = claim();
        assert_eq!(selection, &expected);
        let seal = crate::seal_native_owner_statement(&[7; 32], &statement).expect("seal");
        SourcePolicyQualification::CapturedObserved {
            data: Box::new(SourcePolicyCaptured { statement, seal }),
        }
    }

    #[cfg(unix)]
    #[test]
    fn observed_document_requires_real_local_peer_and_exact_selection() {
        use std::os::unix::{
            fs::PermissionsExt,
            net::{UnixListener, UnixStream},
        };
        let root = std::env::temp_dir().join(format!(
            "nudox-policy-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir(&root).expect("private fixture directory");
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
            .expect("private directory");
        let path = root.join("owner.sock");
        let listener = UnixListener::bind(&path).expect("owner socket");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("private socket");
        let client = UnixStream::connect(&path).expect("connect owner");
        let (_server, _) = listener.accept().expect("accept owner");
        let peer = backend_replication::AuthenticatedLocalPeer::authenticate(&client, &path)
            .expect("authenticate owner");
        let (statement, selection) = claim();
        let seal = crate::seal_native_owner_statement(&[7; 32], &statement).expect("seal");
        let qualification = SourcePolicyQualification::CapturedObserved {
            data: Box::new(SourcePolicyCaptured { statement, seal }),
        };
        let source = selection.source();
        let range = crate::SourceRangeRequest {
            start: 0,
            limit: 4,
            source_identity: None,
        };
        let mut document = crate::Document::new(selection.symbol(), source.root, [])
            .with_source_basis(source)
            .with_selection(selection.clone())
            .expect("selection");
        document.source_policy = qualification.clone();
        document.source_range = Some(crate::SourceRangePage {
            source_policy: qualification,
            text: "x".into(),
            start: 0,
            end: 1,
            total: 1,
            scope: crate::SourceRangeScope::NativePrimaryDeclaration,
            source_identity: [3; 32],
            owner_start: 0,
            owner_end: 1,
        });
        let request = crate::CommandDto::new(
            42,
            crate::Command::Document(
                crate::DocumentQuery::selected(selection.symbol(), source.root)
                    .with_source_basis(source)
                    .with_source_range(range),
            ),
        );
        let page = document.source_range.as_ref().expect("qualified page");
        assert!(!page.admits(range), "raw admission remains closed");
        assert!(
            page.matches_request_shape(range),
            "geometry alone remains inspectable"
        );
        assert!(!page.matches_request_shape(crate::SourceRangeRequest { start: 1, ..range }));
        let reply = crate::ReplyDto::new(42, crate::CommandReply::Document(document.clone()));
        assert!(crate::admit_reply(&request, &reply).is_err());
        assert!(crate::admit_reply_with_local_source_policy(&request, &reply, None, &peer).is_ok());
        document.source_range.as_mut().expect("page").source_policy =
            SourcePolicyQualification::Unknown;
        let mismatch = crate::ReplyDto::new(42, crate::CommandReply::Document(document));
        assert!(
            crate::admit_reply_with_local_source_policy(&request, &mismatch, None, &peer).is_err()
        );
        assert!(serde_json::to_vec(&mismatch).is_err());
        let label = crate::ProductText::from_static("fixture::x");
        let record = crate::DeclarationRecord {
            source_policy: document_policy_for_test(&selection),
            label: label.clone(),
            stable_id: Some(*selection.symbol().as_bytes()),
            signature: Some(crate::ProductText::from_static("x")),
            extent: Some(crate::DeclarationReadExtent::Truncated),
        };
        let read = crate::CommandDto::new(43, crate::Command::Surface(
            crate::SurfaceCommand::Read { locators: vec![label].into_boxed_slice() }
        ));
        let make = |record| crate::ReplyDto::new(43, crate::CommandReply::Surface(
            crate::SurfaceReply::Read(vec![record].into_boxed_slice())
        ));
        let good = make(record.clone());
        assert!(crate::admit_reply(&read, &good).is_err());
        assert!(crate::admit_reply_with_local_source_policy(&read, &good, None, &peer).is_ok());
        let mut wrong = record.clone();
        wrong.stable_id = Some([0; 32]);
        assert!(crate::admit_reply_with_local_source_policy(&read, &make(wrong), None, &peer).is_err());
        let mut wrong = record;
        wrong.label = crate::ProductText::from_static("fixture::other");
        assert!(crate::admit_reply_with_local_source_policy(&read, &make(wrong), None, &peer).is_err());
        let mut wrong = make(crate::DeclarationRecord {
            source_policy: document_policy_for_test(&selection),
            label: crate::ProductText::from_static("fixture::x"),
            stable_id: Some(*selection.symbol().as_bytes()),
            signature: None,
            extent: None,
        });
        if let crate::CommandReply::Surface(crate::SurfaceReply::Read(records)) = &mut wrong.reply {
            if let SourcePolicyQualification::CapturedObserved { data } = &mut records[0].source_policy {
                data.statement.source_object[0] ^= 1;
            }
        }
        assert!(crate::admit_reply_with_local_source_policy(&read, &wrong, None, &peer).is_err());
        let encoded = serde_json::to_vec(&good).expect("shape-checked observed reply");
        assert!(serde_json::from_slice::<crate::ReplyDto>(&encoded).is_err());
        let decoded = crate::decode_reply_body_with_local_source_policy(&encoded, &peer)
            .expect("authenticated local Read without a certificate");
        assert!(crate::admit_reply_with_local_source_policy(&read, &decoded, None, &peer).is_ok());
        assert!(crate::admit_reply(&read, &decoded).is_err());
        let crate::CommandReply::Surface(reply) = &decoded.reply else { panic!("Read reply") };
        let crate::Command::Surface(command) = &read.command else { panic!("Read request") };
        assert!(crate::admit_surface_read_shape(command, reply).is_ok());
        assert!(crate::admit_surface_reply(command, reply).is_err());
        let reference = crate::ReferenceRecord {
            site_symbol: Some(*selection.symbol().as_bytes()),
            source_policy: document_policy_for_test(&selection),
            site: crate::ProductText::from_static("fixture::x"),
            target: crate::SemanticLinkTarget::Local {
                declaration: crate::SemanticDeclarationIdentity { family: [1; 16], variant: [2; 16] },
            },
            relation: crate::SemanticLinkKind::Calls,
            evidence: crate::SemanticLinkEvidence {
                confidence: crate::SemanticConfidence::Syntactic,
                source: Some(crate::SemanticSourceSpan {
                    file: crate::ProductText::from_static("src/a.ts"), start: 1, end: 3,
                }),
            },
        };
        let target = crate::ProductText::from_static("fixture::target");
        let request = crate::CommandDto::new(44, crate::Command::Surface(
            crate::SurfaceCommand::References { target: target.clone() }
        ));
        let make_refs = |reference| crate::ReplyDto::new(44, crate::CommandReply::Surface(
            crate::SurfaceReply::References { target: target.clone(), references: vec![reference].into_boxed_slice(),
                coverage: crate::ReferenceCoverage::Partial { reason: crate::ProductText::from_static("observed policy unknown") } }
        ));
        let good = make_refs(reference.clone());
        let encoded = serde_json::to_vec(&good).expect("reference shape");
        assert!(crate::decode_reply_body(&encoded).is_err());
        let decoded = crate::decode_reply_body_with_local_source_policy(&encoded, &peer).expect("local reference");
        assert!(crate::admit_reply_with_local_source_policy(&request, &decoded, None, &peer).is_ok());
        assert!(crate::admit_reply(&request, &decoded).is_err());
        let crate::CommandReply::Surface(reply) = &decoded.reply else { panic!("References reply") };
        let crate::Command::Surface(command) = &request.command else { panic!("References request") };
        assert!(crate::admit_surface_references_shape(command, reply).is_ok());
        assert!(crate::admit_surface_reply(command, reply).is_err());
        let mut wrong = reference.clone();
        wrong.site_symbol = Some([0; 32]);
        assert!(serde_json::to_vec(&make_refs(wrong)).is_err());
        let mut wrong = reference.clone();
        wrong.evidence.source.as_mut().expect("site").file = crate::ProductText::from_static("other.ts");
        assert!(serde_json::to_vec(&make_refs(wrong)).is_err());
        let mut wrong = reference;
        wrong.evidence.source.as_mut().expect("site").end = 10;
        assert!(serde_json::to_vec(&make_refs(wrong)).is_err());
        std::fs::remove_file(path).expect("remove socket");
        std::fs::remove_dir(root).expect("remove directory");
    }

    #[test]
    fn unadmitted_captured_claim_cannot_pass_578_success_boundaries() {
        let (statement, _) = claim();
        let qualification = SourcePolicyQualification::CapturedObserved {
            data: Box::new(SourcePolicyCaptured {
                statement,
                seal: [0; 32],
            }),
        };
        let page = crate::SourceRangePage {
            source_policy: qualification.clone(),
            text: "x".into(),
            start: 0,
            end: 1,
            total: 1,
            scope: crate::SourceRangeScope::NativePrimaryDeclaration,
            source_identity: [4; 32],
            owner_start: 0,
            owner_end: 1,
        };
        assert!(!page.admits(crate::SourceRangeRequest {
            start: 0,
            limit: 4,
            source_identity: None
        }));
        let record = crate::DeclarationRecord {
            source_policy: qualification.clone(),
            label: crate::ProductText::from_static("fixture::x"),
            stable_id: Some([1; 32]),
            signature: Some(crate::ProductText::from_static("x")),
            extent: None,
        };
        assert!(
            crate::SurfaceReply::Read(vec![record].into_boxed_slice())
                .admit(crate::CommandId::Read)
                .is_err()
        );
        let make_reply = |source_policy| {
            crate::ReplyDto::new(
                7,
                crate::CommandReply::Surface(crate::SurfaceReply::Read(
                    vec![crate::DeclarationRecord {
                        source_policy,
                        label: crate::ProductText::from_static("fixture::x"),
                        stable_id: Some([1; 32]),
                        signature: None,
                        extent: None,
                    }]
                    .into_boxed_slice(),
                )),
            )
        };
        assert!(serde_json::to_vec(&make_reply(qualification.clone())).is_err());
        let mut forged = serde_json::to_value(make_reply(SourcePolicyQualification::Unknown))
            .expect("legacy reply");
        forged["reply"]["data"]["data"][0]["source_policy"] =
            serde_json::to_value(qualification).expect("captured wire claim");
        assert!(serde_json::from_value::<crate::ReplyDto>(forged).is_err());
    }

    #[test]
    fn in_process_document_and_page_reject_unadmitted_capture_without_a_range() {
        let (statement, selection) = claim();
        let mut document = crate::Document::new(selection.symbol(), selection.source().root, []);
        document.source_policy = SourcePolicyQualification::CapturedObserved {
            data: Box::new(SourcePolicyCaptured {
                statement,
                seal: [0; 32],
            }),
        };
        let requests = [
            crate::CommandDto::new(
                7,
                crate::Command::Show {
                    symbol: selection.symbol(),
                },
            ),
            crate::CommandDto::new(
                7,
                crate::Command::Document(crate::DocumentQuery::selected(
                    selection.symbol(),
                    selection.source().root,
                )),
            ),
        ];
        for request in requests {
            for reply in [
                crate::CommandReply::Document(document.clone()),
                crate::CommandReply::Page(document.clone()),
            ] {
                assert!(crate::admit_reply(&request, &crate::ReplyDto::new(7, reply)).is_err());
            }
        }
    }
}

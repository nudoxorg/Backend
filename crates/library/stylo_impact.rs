//! Possible same-name consumers for an owner-selected CSS custom-property site.
//! Provider selection and DOM/cascade applicability remain unknown.
use crate::stylo_query::{
    StyloCursorClaims, StyloQueryBasis, StyloQueryCursor, StyloQueryHit, StyloQueryRequest,
    StyloSourceReference,
};
use crate::{
    NativeOwnerStatement, ProductAdmissionError, seal_native_owner_statement,
    verify_native_owner_statement,
};
use serde::{Deserialize, Serialize};

/// Space for 128 row wrappers, a seed, and the nested owner cursor.
/// Each row adds under 64 JSON bytes; the owner tokens are bounded to 260 and
/// 2048 bytes and the fixed seal/framing is under 1024 bytes. The 16 KiB
/// reserve exceeds their combined worst case, including JSON escaping.
pub const STYLO_IMPACT_ENVELOPE_RESERVE: u32 = 16_384;

/// A selected authored declaration and a bounded native fact scan.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StyloImpactRequest {
    /// Owner-issued exact declaration site in the selected snapshot.
    pub seed: StyloSourceReference,
    /// Search limits, selected package and expected decoded custom-property name.
    /// The continuation must be absent; use `continuation` below. Impact pages
    /// require at least 147456 output bytes, 10 scan rows, and 327680 scan bytes
    /// to include seed verification and response framing.
    pub query: StyloQueryRequest,
    /// Owner-authenticated frontier, bound to the seed and native query.
    pub continuation: Option<StyloImpactCursor>,
}
impl StyloImpactRequest {
    /// Validate request shape before selecting an owner.
    pub fn admit(&self) -> Result<(), ProductAdmissionError> {
        self.seed.admit_shape()?;
        self.query.normalized_name()?;
        if self.query.role != crate::stylo_query::StyloNameRole::CustomProperty
            || self.query.continuation.is_some()
            || self.query.output_bytes < 131_072 + STYLO_IMPACT_ENVELOPE_RESERVE
            || self.query.scan_rows < 10
            || self.query.scan_bytes < 327_680
        {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        if let Some(cursor) = &self.continuation {
            cursor.admit_shape()?;
        }
        Ok(())
    }
}
/// Authenticated cursor binds the causal seed as well as the underlying root-bound scan.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StyloImpactCursor {
    seed: StyloSourceReference,
    scan: StyloQueryCursor,
    seal: [u8; 32],
}
#[derive(Serialize)]
struct CursorStatement<'a> {
    seed: &'a StyloSourceReference,
    scan: &'a StyloQueryCursor,
}
impl NativeOwnerStatement for CursorStatement<'_> {
    const DOMAIN: &'static [u8] = b"backend.stylo.impact-cursor.v1\0";
}
impl StyloImpactCursor {
    /// Validate token shapes before owner authentication.
    pub fn admit_shape(&self) -> Result<(), ProductAdmissionError> {
        self.seed.admit_shape()?;
        self.scan.admit_shape()
    }
    /// Whether this frontier names the requested source seed.
    pub fn matches_seed(&self, seed: &StyloSourceReference) -> bool {
        &self.seed == seed
    }
    /// Seal a selected frontier.
    pub fn issue(
        seed: StyloSourceReference,
        scan: StyloQueryCursor,
        secret: &[u8; 32],
    ) -> Result<Self, ProductAdmissionError> {
        let seal = seal_native_owner_statement(
            secret,
            &CursorStatement {
                seed: &seed,
                scan: &scan,
            },
        )?;
        Ok(Self { seed, scan, seal })
    }
    /// Verify seed binding and scan authentication before resuming.
    pub fn verify(
        &self,
        seed: &StyloSourceReference,
        secret: &[u8; 32],
    ) -> Result<StyloCursorClaims, ProductAdmissionError> {
        if seed != &self.seed {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        verify_native_owner_statement(
            secret,
            &CursorStatement {
                seed: &self.seed,
                scan: &self.scan,
            },
            &self.seal,
        )?;
        self.scan.verify(secret)
    }
    /// Underlying authenticated scan token.
    pub fn scan(&self) -> &StyloQueryCursor {
        &self.scan
    }
}
/// Resolution of an impact row. No provider binding has been established.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StyloImpactStrength {
    /// The matching native name is a possible dependency, not an exact provider.
    Candidate,
}
/// Candidate native dependency. Its native occurrence is a witness, not an authored byte site.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StyloImpactRow {
    /// Explicit candidate status for machine consumers.
    pub strength: StyloImpactStrength,
    /// The selected native dependency and its declaration/rule context.
    pub witness: StyloQueryHit,
}
/// Enumeration completion is separate from unknown provider and coverage.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum StyloImpactTerminal {
    /// The bounded selected scan reached its end; gaps remain.
    Complete,
    /// A root- and seed-bound scan frontier remains.
    More {
        /// Authenticated continuation.
        continuation: StyloImpactCursor,
        /// Work rather than row output limited this page.
        work_limited: bool,
    },
    /// A native result exceeded the output budget.
    OutputLimit {
        /// Required native output budget.
        required_bytes: u32,
        /// Resume if the owner can issue a frontier for the larger budget.
        continuation: Option<StyloImpactCursor>,
    },
    /// No selected native analysis is available.
    Unavailable,
}
/// Candidate-only result with one provenance header and exact seed source handle.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StyloImpactPage {
    /// Selected exact roots, producer and coverage gaps.
    pub basis: StyloQueryBasis,
    /// Exact authored seed source handle.
    pub seed: StyloSourceReference,
    /// Candidate dependency witnesses.
    pub rows: Vec<StyloImpactRow>,
    /// Scan completion and optional continuation.
    pub terminal: StyloImpactTerminal,
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::stylo_query::{StyloNameRole, StyloSourceClaims};

    #[test]
    fn continuation_authenticates_the_complete_seed_and_scan() {
        let secret = [7; 32];
        let site = |id| {
            StyloSourceReference::issue(
                StyloSourceClaims {
                    manifest_root: [1; 32],
                    native_root: [2; 32],
                    project: [3; 32],
                    host: [4; 32],
                    site: [id; 32],
                },
                &secret,
            )
            .unwrap()
        };
        let seed = site(5);
        let other = site(6);
        let claims = StyloCursorClaims {
            manifest_root: [1; 32],
            native_root: [2; 32],
            project: [3; 32],
            role: StyloNameRole::CustomProperty,
            name: "--tone".into(),
            page_rows: 1,
            output_bytes: 131_072,
            scan_rows: 7,
            scan_bytes: 229_376,
            after: [8; 32],
        };
        let scan = StyloQueryCursor::issue(claims.clone(), &secret).unwrap();
        let cursor = StyloImpactCursor::issue(seed.clone(), scan, &secret).unwrap();
        assert_eq!(cursor.verify(&seed, &secret).unwrap(), claims);
        assert!(!cursor.matches_seed(&other));
        assert!(cursor.verify(&other, &secret).is_err());
        assert!(cursor.verify(&seed, &[9; 32]).is_err());
        let mut forged = cursor;
        forged.seed = other.clone();
        assert!(forged.verify(&other, &secret).is_err());
    }
    #[test]
    fn full_page_and_long_cursor_fit_the_reserved_envelope() {
        use crate::stylo_query::{
            StyloAttemptState, StyloHostTerminalSummary, StyloQueryPage, StyloQueryTerminal,
            StyloWorkerCapability,
        };
        use backend_semantic::stylo_analysis::{
            StyloDeclaration, StyloDeclarationKind, StyloDependencyCoverage, StyloRule,
            StyloRuleKind, StyloVariableDependency,
        };
        let secret = [7; 32];
        let seed = StyloSourceReference::issue(
            StyloSourceClaims {
                manifest_root: [1; 32],
                native_root: [2; 32],
                project: [3; 32],
                host: [4; 32],
                site: [5; 32],
            },
            &secret,
        )
        .unwrap();
        let cursor = StyloQueryCursor::issue(
            StyloCursorClaims {
                manifest_root: [1; 32],
                native_root: [2; 32],
                project: [3; 32],
                role: StyloNameRole::CustomProperty,
                name: format!("--{}", "x".repeat(1022)),
                page_rows: 128,
                output_bytes: 4_000_000,
                scan_rows: 4096,
                scan_bytes: 4_000_000,
                after: [8; 32],
            },
            &secret,
        )
        .unwrap();
        let hit = StyloQueryHit::NativeDependency {
            id: [9; 32],
            path: "x".repeat(4096),
            rule: StyloRule {
                path: vec![1],
                kind: StyloRuleKind::Style,
                native_kind: "Style".into(),
                condition: None,
                location: None,
            },
            declaration: StyloDeclaration {
                rule_path: vec![1],
                ordinal: 0,
                kind: StyloDeclarationKind::Custom,
                property: "--p".into(),
                value: "x".repeat(2000),
                value_available: true,
                important: false,
                dependency_coverage: StyloDependencyCoverage::Known,
            },
            dependency: StyloVariableDependency {
                rule_path: vec![1],
                declaration_ordinal: 0,
                name: "tone".into(),
                kind: "var".into(),
                reference_path: vec![0],
            },
        };
        let basis = StyloQueryBasis {
            manifest_root: [1; 32],
            native_root: Some([2; 32]),
            attempt: StyloAttemptState::LatestObserved,
            worker_capability: StyloWorkerCapability::Unknown,
            host_terminal_summary: StyloHostTerminalSummary::Unknown,
            source_partial: true,
            gaps: vec![],
            producer: Some([3; 32]),
            selection_complete: false,
        };
        let inner = StyloQueryPage {
            basis: basis.clone(),
            hits: vec![hit.clone(); 128],
            terminal: StyloQueryTerminal::More {
                continuation: cursor.clone(),
                work_limited: false,
            },
        };
        let outer = StyloImpactPage {
            basis,
            seed: seed.clone(),
            rows: vec![
                StyloImpactRow {
                    strength: StyloImpactStrength::Candidate,
                    witness: hit
                };
                128
            ],
            terminal: StyloImpactTerminal::More {
                continuation: StyloImpactCursor::issue(seed, cursor, &secret).unwrap(),
                work_limited: false,
            },
        };
        let inner_size = serde_json::to_vec(&inner).unwrap().len();
        let outer_size = serde_json::to_vec(&outer).unwrap().len();
        assert!(inner_size > 500_000);
        assert!(outer_size - inner_size < STYLO_IMPACT_ENVELOPE_RESERVE as usize);
    }
}

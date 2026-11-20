//! Owner-authenticated, ephemeral CSS comparison operands. A lease names
//! captured selected material; its presence and TTL are checked by the owner.
use crate::stylo_query::{StyloFailure, StyloSourceReference, StyloWorkerCapability};
use crate::{
    NativeOwnerStatement, PackageReference, ProductAdmissionError, seal_native_owner_statement,
    verify_native_owner_statement,
};
use serde::{Deserialize, Serialize};

/// Capture one current selected CSS declaration and bounded project material.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImpactCaptureRequest {
    /// Selected local project.
    pub package: PackageReference,
    /// Optional current selected custom-property declaration. Omit to capture
    /// a comparison basis without asserting a seed exists on this side.
    pub seed: Option<StyloSourceReference>,
    /// Maximum native relation reads, including both scan passes.
    pub scan_rows: u32,
    /// Maximum canonical native row bytes inspected.
    pub scan_bytes: u32,
    /// Maximum bytes retained for this capture.
    pub retain_bytes: u32,
    /// Maximum source bytes attempted during capture.
    pub source_bytes: u32,
    /// Separate conservative temporary descriptor/index budget.
    pub temporary_bytes: u32,
    /// Monotonic cooperative capture deadline in milliseconds.
    pub time_ms: u32,
}
impl ImpactCaptureRequest {
    /// Validate the request before owner admission.
    pub fn admit(&self) -> Result<(), ProductAdmissionError> {
        if let Some(seed) = &self.seed {
            seed.admit_shape()?;
        }
        if !matches!(self.package, PackageReference::Local(_))
            || self.scan_rows == 0
            || self.scan_rows > 262_144
            || self.scan_bytes < 32_768
            || self.scan_bytes > 67_108_864
            || self.retain_bytes == 0
            || self.retain_bytes > 67_108_864
            || self.source_bytes > self.retain_bytes
            || self.temporary_bytes < 131_072
            || self.temporary_bytes > 67_108_864
            || self.time_ms == 0
            || self.time_ms > 30_000
        {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        Ok(())
    }
}
/// State of the latest native attempt at capture time. UnknownLegacy means
/// the exact selected relation had no recorded attempt; Unknown means capture
/// did not establish whether an attempt row existed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields, from = "AttemptWire")]
pub enum ImpactAttemptAtCapture {
    LatestObserved,
    RetainedAfterUnavailable { reason: StyloFailure },
    UnknownLegacy,
    Unknown,
}
// Serde ignores unknown keys next to a unit variant of an internally tagged enum even
// with `deny_unknown_fields`. These wire twins use empty struct variants, which do
// reject them, and convert into the public enums with the same JSON shape.
#[derive(Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum AttemptWire {
    LatestObserved {},
    RetainedAfterUnavailable { reason: StyloFailure },
    UnknownLegacy {},
    Unknown {},
}
impl From<AttemptWire> for ImpactAttemptAtCapture {
    fn from(wire: AttemptWire) -> Self {
        match wire {
            AttemptWire::LatestObserved {} => Self::LatestObserved,
            AttemptWire::RetainedAfterUnavailable { reason } => Self::RetainedAfterUnavailable { reason },
            AttemptWire::UnknownLegacy {} => Self::UnknownLegacy,
            AttemptWire::Unknown {} => Self::Unknown,
        }
    }
}
/// Exact admitted capture identity. Scope binds the full selected row and
/// producer names an exact native support fingerprint, not database truth.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImpactLeaseClaims {
    pub epoch: [u8; 16],
    pub sequence: u64,
    pub manifest: [u8; 32],
    pub native: [u8; 32],
    pub project: [u8; 32],
    pub scope: [u8; 32],
    pub producer: [u8; 32],
    pub seed: Option<[u8; 32]>,
    pub host: Option<[u8; 32]>,
    /// Latest selected attempt observed at capture time, not current freshness.
    pub attempt_at_capture: ImpactAttemptAtCapture,
    /// Worker capability witnessed by that attempt.
    pub worker_at_capture: StyloWorkerCapability,
    /// Whether all admitted native fact rows for the selected project were copied.
    pub native_rows_complete: bool,
    /// Whether verified source bytes were copied for every selected native host.
    pub native_host_sources_complete: bool,
}
impl NativeOwnerStatement for ImpactLeaseClaims {
    const DOMAIN: &'static [u8] = b"backend.impact.lease.v3\0";
}
/// Sealed lease claim; owner checks the in-memory entry and expiry separately.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImpactLease {
    claims: ImpactLeaseClaims,
    seal: [u8; 32],
}
impl ImpactLease {
    pub fn issue(
        claims: ImpactLeaseClaims,
        secret: &[u8; 32],
    ) -> Result<Self, ProductAdmissionError> {
        if claims.sequence == 0 || claims.seed.is_some() != claims.host.is_some() {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        let seal = seal_native_owner_statement(secret, &claims)?;
        Ok(Self { claims, seal })
    }
    pub fn verify(&self, secret: &[u8; 32]) -> Result<&ImpactLeaseClaims, ProductAdmissionError> {
        if self.claims.sequence == 0 || self.claims.seed.is_some() != self.claims.host.is_some() {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        verify_native_owner_statement(secret, &self.claims, &self.seal)?;
        Ok(&self.claims)
    }
    pub fn claims(&self) -> &ImpactLeaseClaims {
        &self.claims
    }
}
/// A separate namespace for bytes captured with a lease. It never authorizes
/// reading a live path or substitutes for the normal current-source reader.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImpactSourceClaims {
    pub lease: ImpactLease,
    pub host: [u8; 32],
    pub site: [u8; 32],
}
impl NativeOwnerStatement for ImpactSourceClaims {
    const DOMAIN: &'static [u8] = b"backend.impact.historical-source.v3\0";
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImpactSource {
    claims: ImpactSourceClaims,
    seal: [u8; 32],
}
impl ImpactSource {
    pub fn issue(
        claims: ImpactSourceClaims,
        secret: &[u8; 32],
    ) -> Result<Self, ProductAdmissionError> {
        claims.lease.verify(secret)?;
        let seal = seal_native_owner_statement(secret, &claims)?;
        Ok(Self { claims, seal })
    }
    pub fn claims(&self) -> &ImpactSourceClaims {
        &self.claims
    }
    pub fn verify(&self, secret: &[u8; 32]) -> Result<&ImpactSourceClaims, ProductAdmissionError> {
        self.claims.lease.verify(secret)?;
        verify_native_owner_statement(secret, &self.claims, &self.seal)?;
        Ok(&self.claims)
    }
}
/// Base or target is a selection, not proof of a removed/added operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImpactSide {
    Base,
    Target,
}
/// Explicit limits for a deterministic recomputation and output page.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImpactLimits {
    pub input_rows: u32,
    pub input_bytes: u32,
    pub edges: u32,
    pub nodes: u32,
    pub depth: u16,
    pub page_rows: u16,
    pub output_bytes: u32,
    pub witness_bytes: u32,
    /// Separate conservative temporary descriptor/index budget.
    pub temporary_bytes: u32,
    /// Monotonic computation deadline per page, in milliseconds.
    pub time_ms: u32,
}
impl ImpactLimits {
    pub fn admit(&self) -> Result<(), ProductAdmissionError> {
        if self.input_rows == 0
            || self.input_rows > 262_144
            || self.input_bytes == 0
            || self.input_bytes > 67_108_864
            || self.edges == 0
            || self.edges > 1_000_000
            || self.nodes == 0
            || self.nodes > 262_144
            || self.depth == 0
            || self.depth > 128
            || self.page_rows == 0
            || self.page_rows > 128
            || self.output_bytes < 131_072
            || self.output_bytes > 4_194_304
            || self.witness_bytes == 0
            || self.witness_bytes > 16_777_216
            || self.temporary_bytes < 131_072
            || self.temporary_bytes > 67_108_864
            || self.time_ms == 0
            || self.time_ms > 30_000
        {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        Ok(())
    }
}
/// Cursor binds both leases, selected side, complete limits and output offset.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImpactCursorClaims {
    /// Digest of both leases, side, limits, and comparison contract version.
    pub recipe: [u8; 32],
    /// Number of already emitted candidate occurrences.
    pub offset: u32,
}
impl NativeOwnerStatement for ImpactCursorClaims {
    const DOMAIN: &'static [u8] = b"backend.impact.comparison-cursor.v3\0";
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImpactCursor {
    claims: ImpactCursorClaims,
    seal: [u8; 32],
}
impl ImpactCursor {
    pub fn issue(
        claims: ImpactCursorClaims,
        secret: &[u8; 32],
    ) -> Result<Self, ProductAdmissionError> {
        let seal = seal_native_owner_statement(secret, &claims)?;
        Ok(Self { claims, seal })
    }
    pub fn claims(&self) -> &ImpactCursorClaims {
        &self.claims
    }
    pub fn verify(&self, secret: &[u8; 32]) -> Result<&ImpactCursorClaims, ProductAdmissionError> {
        verify_native_owner_statement(secret, &self.claims, &self.seal)?;
        Ok(&self.claims)
    }
}
/// Compare one explicitly selected retained side; no change is inferred.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImpactCompareRequest {
    pub base: ImpactLease,
    pub target: ImpactLease,
    pub side: ImpactSide,
    pub limits: ImpactLimits,
    pub continuation: Option<ImpactCursor>,
    pub cancel: bool,
}

impl ImpactCompareRequest {
    /// Versioned deterministic binding of both owner leases, side and budgets.
    /// Cancellation and the continuation itself are intentionally excluded.
    pub fn recipe(&self) -> Result<[u8; 32], ProductAdmissionError> {
        let encoded = serde_json::to_vec(&(&self.base, &self.target, self.side, &self.limits))
            .map_err(|_| ProductAdmissionError::SemanticVersionShape)?;
        let mut hash = blake3::Hasher::new();
        hash.update(b"backend.impact.comparison-recipe.v3\0");
        hash.update(&encoded);
        Ok(*hash.finalize().as_bytes())
    }
}

/// Capture receipt. Missing facts/source remain gapped even if this operation
/// returned successfully.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImpactCaptureReply {
    pub seed: Option<StyloSourceReference>,
    pub lease: ImpactLease,
    pub source: Option<ImpactSource>,
    pub relation_gapped: bool,
    pub source_gapped: bool,
    /// Latest indexing attempt authenticated at capture time.
    pub attempt_at_capture: ImpactAttemptAtCapture,
    pub worker_at_capture: StyloWorkerCapability,
}
/// Qualification retained across every provider and ownership step.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImpactStrength {
    Candidate,
}
/// Candidate dependency occurrence and exact native owner witness, if retained.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImpactCandidate {
    pub strength: ImpactStrength,
    pub dependency: [u8; 32],
    /// Project-relative native owner path, if captured.
    pub path: Option<String>,
    pub owner: Option<[u8; 32]>,
    pub witness: Vec<[u8; 32]>,
}
/// Completion of bounded enumeration, never a proof of complete CSS coverage.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields, from = "TerminalWire")]
pub enum ImpactTerminal {
    Complete,
    More {
        continuation: ImpactCursor,
    },
    WorkLimit {
        reason: String,
    },
    Cancelled,
    Expired,
    /// The selected side has no seed; this does not imply no impacts.
    SeedUnavailable,
}
#[derive(Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum TerminalWire {
    Complete {},
    More { continuation: ImpactCursor },
    WorkLimit { reason: String },
    Cancelled {},
    Expired {},
    SeedUnavailable {},
}
impl From<TerminalWire> for ImpactTerminal {
    fn from(wire: TerminalWire) -> Self {
        match wire {
            TerminalWire::Complete {} => Self::Complete,
            TerminalWire::More { continuation } => Self::More { continuation },
            TerminalWire::WorkLimit { reason } => Self::WorkLimit { reason },
            TerminalWire::Cancelled {} => Self::Cancelled,
            TerminalWire::Expired {} => Self::Expired,
            TerminalWire::SeedUnavailable {} => Self::SeedUnavailable,
        }
    }
}
/// Work counters for this recomputed bounded page.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImpactCounters {
    /// None means the operation stopped before a complete counter was captured.
    pub input_rows: Option<u32>,
    pub input_bytes: Option<u32>,
    /// Conservative descriptor/index allocation charge, not total RSS.
    pub temporary_bytes: Option<u32>,
    pub edges: Option<u32>,
    pub nodes: Option<u32>,
    pub witness_bytes: Option<u32>,
}
/// Both exact captured bases and the chosen side accompany each result page.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImpactCompareReply {
    pub base: ImpactLease,
    pub target: ImpactLease,
    pub side: ImpactSide,
    pub rows: Vec<ImpactCandidate>,
    pub relation_gapped: bool,
    pub source_gapped: bool,
    /// The selected attempt states observed when each lease was captured.
    pub base_attempt_at_capture: ImpactAttemptAtCapture,
    pub target_attempt_at_capture: ImpactAttemptAtCapture,
    pub base_worker_at_capture: StyloWorkerCapability,
    pub target_worker_at_capture: StyloWorkerCapability,
    pub provider_unknown: bool,
    pub missing_owner: bool,
    pub counters: ImpactCounters,
    pub terminal: ImpactTerminal,
}
/// Read the exact seed extent from captured bytes, without reopening the file.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImpactReadSourceRequest {
    pub source: ImpactSource,
}
/// Historical read outcomes do not change current-source freshness.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum ImpactSourceReply {
    Available { text: String, start: u64, end: u64 },
    Unavailable,
    Expired,
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    fn lease(seed: u8) -> ImpactLease {
        ImpactLease::issue(
            ImpactLeaseClaims {
                epoch: [1; 16],
                sequence: 1,
                manifest: [2; 32],
                native: [3; 32],
                project: [4; 32],
                scope: [5; 32],
                producer: [6; 32],
                seed: Some([seed; 32]),
                host: Some([8; 32]),
                attempt_at_capture: ImpactAttemptAtCapture::Unknown,
                worker_at_capture: StyloWorkerCapability::Unknown,
                native_rows_complete: false,
                native_host_sources_complete: false,
            },
            &[9; 32],
        )
        .unwrap()
    }
    fn limits() -> ImpactLimits {
        ImpactLimits {
            input_rows: 100,
            input_bytes: 100_000,
            edges: 100,
            nodes: 100,
            depth: 5,
            page_rows: 10,
            output_bytes: 131_072,
            witness_bytes: 100_000,
            temporary_bytes: 1_000_000,
            time_ms: 10_000,
        }
    }
    #[test]
    fn comparison_state_rejects_unknown_fields() {
        assert!(
            serde_json::from_value::<ImpactAttemptAtCapture>(
                serde_json::json!({"state":"latest_observed", "extra":true})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<ImpactTerminal>(
                serde_json::json!({"state":"complete", "extra":true})
            )
            .is_err()
        );
    }
    #[test]
    fn diff_cursor_binds_both_roots_domain_and_work_policy() {
        let limits = ImpactDiffLimits {
            scan_rows: 100,
            scan_bytes: 1_000_000,
            temporary_bytes: 1_000_000,
            compared_bytes: 1_000_000,
            comparisons: 100,
            page_rows: 5,
            output_bytes: 131_072,
            seed_walks: 4,
            walk_input_rows: 100,
            walk_input_bytes: 1_000_000,
            walk_edges: 100,
            walk_nodes: 100,
            walk_depth: 4,
            walk_rows: 100,
            witness_bytes: 100_000,
            walk_path_bytes: 100_000,
            time_ms: 1000,
        };
        let request = ImpactDiffRequest {
            base: lease(7),
            target: lease(10),
            domain: ImpactDiffDomain::NativeFacts,
            limits,
            continuation: None,
            cancel: false,
        };
        let recipe = request.recipe().unwrap();
        let mut changed = request.clone();
        changed.domain = ImpactDiffDomain::VerifiedSourceImages;
        assert_ne!(recipe, changed.recipe().unwrap());
        changed = request.clone();
        changed.limits.comparisons += 1;
        assert_ne!(recipe, changed.recipe().unwrap());
        let cursor =
            ImpactDiffCursor::issue(ImpactDiffCursorClaims { recipe, offset: 3 }, &[9; 32])
                .unwrap();
        assert!(cursor.verify(&[9; 32]).is_ok());
        let mut forged = cursor.clone();
        forged.claims.offset = 4;
        assert!(forged.verify(&[9; 32]).is_err());
        assert!(cursor.verify(&[8; 32]).is_err());
    }
    #[test]
    fn separate_domains_bind_source_and_both_comparison_sides() {
        let base = lease(7);
        let mut unseeded_claims = base.claims.clone();
        unseeded_claims.seed = None;
        unseeded_claims.host = None;
        assert!(ImpactLease::issue(unseeded_claims.clone(), &[9; 32]).is_ok());
        unseeded_claims.host = Some([8; 32]);
        assert!(ImpactLease::issue(unseeded_claims, &[9; 32]).is_err());
        let mut forged_freshness = base.clone();
        forged_freshness.claims.attempt_at_capture = ImpactAttemptAtCapture::LatestObserved;
        assert!(forged_freshness.verify(&[9; 32]).is_err());
        let source = ImpactSource::issue(
            ImpactSourceClaims {
                lease: base.clone(),
                host: [8; 32],
                site: [7; 32],
            },
            &[9; 32],
        )
        .unwrap();
        assert!(source.verify(&[9; 32]).is_ok());
        let mut forged = source.clone();
        forged.claims.site = [10; 32];
        assert!(forged.verify(&[9; 32]).is_err());
        // An owner may issue another site only after checking retained evidence;
        // the read path independently rechecks host/site membership.
        let request = ImpactCompareRequest {
            base: base.clone(),
            target: lease(10),
            side: ImpactSide::Base,
            limits: limits(),
            continuation: None,
            cancel: false,
        };
        let recipe = request.recipe().unwrap();
        let mut other_side = request.clone();
        other_side.side = ImpactSide::Target;
        assert_ne!(recipe, other_side.recipe().unwrap());
        let mut other_limit = request.clone();
        other_limit.limits.edges += 1;
        assert_ne!(recipe, other_limit.recipe().unwrap());
        let cursor =
            ImpactCursor::issue(ImpactCursorClaims { recipe, offset: 1 }, &[9; 32]).unwrap();
        assert!(cursor.verify(&[9; 32]).is_ok());
        let mut altered = cursor.clone();
        altered.claims.recipe = [12; 32];
        assert!(altered.verify(&[9; 32]).is_err());
        assert!(cursor.verify(&[10; 32]).is_err());
    }
}

impl ImpactCaptureRequest {
    /// MCP request schema; owner-issued source handles are copied verbatim.
    #[must_use]
    pub fn json_schema() -> serde_json::Value {
        serde_json::json!({"type":"object","additionalProperties":false,"required":["package","seed","scan_rows","scan_bytes","retain_bytes","source_bytes","temporary_bytes","time_ms"],"properties":{
            "package":{"type":"object","additionalProperties":false,"required":["kind","value"],"properties":{"kind":{"const":"local"},"value":{"type":"string"}},"description":"Selected local project reference."},
            "seed":{"type":["string","null"],"description":"Copy an authored custom-property source handle from stylo-query, or use null to capture an unseeded basis."},
            "scan_rows":{"type":"integer","minimum":1,"maximum":262144},
            "scan_bytes":{"type":"integer","minimum":32768,"maximum":67108864},
            "retain_bytes":{"type":"integer","minimum":1,"maximum":67108864},
            "source_bytes":{"type":"integer","minimum":0,"maximum":67108864},
            "temporary_bytes":{"type":"integer","minimum":131072,"maximum":67108864},
            "time_ms":{"type":"integer","minimum":1,"maximum":30000,"description":"Cooperative deadline; a single confined source read is not preemptible."}
        }})
    }
}
impl ImpactCompareRequest {
    /// MCP request schema. Lease and cursor objects are owner-issued operands.
    #[must_use]
    pub fn json_schema() -> serde_json::Value {
        serde_json::json!({"type":"object","additionalProperties":false,"required":["base","target","side","limits","continuation","cancel"],"properties":{
            "base":{"type":"object","description":"Exact lease returned by impact-capture before the edit."},
            "target":{"type":"object","description":"Exact lease returned by impact-capture after the edit."},
            "side":{"enum":["base","target"]},
            "continuation":{"type":["object","null"],"description":"Copy the preceding page cursor, or null."},
            "cancel":{"type":"boolean","description":"Return a cancelled terminal before starting this request; does not interrupt another in-flight call."},
            "limits":{"type":"object","additionalProperties":false,"required":["input_rows","input_bytes","edges","nodes","depth","page_rows","output_bytes","witness_bytes","temporary_bytes","time_ms"],"properties":{
                "input_rows":{"type":"integer","minimum":1,"maximum":262144},"input_bytes":{"type":"integer","minimum":1,"maximum":67108864},"edges":{"type":"integer","minimum":1,"maximum":1000000},"nodes":{"type":"integer","minimum":1,"maximum":262144},"depth":{"type":"integer","minimum":1,"maximum":128},"page_rows":{"type":"integer","minimum":1,"maximum":128},"output_bytes":{"type":"integer","minimum":131072,"maximum":4194304},"witness_bytes":{"type":"integer","minimum":1,"maximum":16777216},"temporary_bytes":{"type":"integer","minimum":131072,"maximum":67108864},"time_ms":{"type":"integer","minimum":1,"maximum":30000}
            }}
        }})
    }
}
impl ImpactReadSourceRequest {
    /// MCP request schema. Historical source handles cannot read live files.
    #[must_use]
    pub fn json_schema() -> serde_json::Value {
        serde_json::json!({"type":"object","additionalProperties":false,"required":["source"],"properties":{"source":{"type":"object","description":"Exact historical source handle returned by impact-capture."}}})
    }
}

/// Exact retained material namespace, never a cross-language object identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImpactDiffDomain {
    NativeFacts,
    VerifiedSourceImages,
}
/// Separate diff scan, output and aggregate changed-seed traversal limits.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImpactDiffLimits {
    pub scan_rows: u32,
    pub scan_bytes: u32,
    pub temporary_bytes: u32,
    pub compared_bytes: u32,
    pub comparisons: u32,
    pub page_rows: u16,
    pub output_bytes: u32,
    pub seed_walks: u16,
    pub walk_input_rows: u32,
    pub walk_input_bytes: u32,
    pub walk_edges: u32,
    pub walk_nodes: u32,
    pub walk_depth: u16,
    pub walk_rows: u32,
    pub witness_bytes: u32,
    pub walk_path_bytes: u32,
    pub time_ms: u32,
}
impl ImpactDiffLimits {
    pub fn admit(&self) -> Result<(), ProductAdmissionError> {
        if self.scan_rows == 0
            || self.scan_rows > 524_288
            || self.scan_bytes == 0
            || self.scan_bytes > 134_217_728
            || self.temporary_bytes < 131_072
            || self.temporary_bytes > 67_108_864
            || self.compared_bytes > 134_217_728
            || self.comparisons == 0
            || self.comparisons > 524_288
            || self.page_rows == 0
            || self.page_rows > 128
            || self.output_bytes < 131_072
            || self.output_bytes > 4_194_304
            || self.seed_walks > 128
            || self.walk_input_rows > 1_000_000
            || self.walk_input_bytes > 134_217_728
            || self.walk_edges > 1_000_000
            || self.walk_nodes > 262_144
            || self.walk_depth > 128
            || self.walk_rows > 262_144
            || self.witness_bytes > 16_777_216
            || self.walk_path_bytes > 4_194_304
            || self.time_ms == 0
            || self.time_ms > 30_000
        {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImpactDiffCursorClaims {
    pub recipe: [u8; 32],
    pub offset: u32,
}
impl NativeOwnerStatement for ImpactDiffCursorClaims {
    const DOMAIN: &'static [u8] = b"backend.impact.diff-cursor.v1\0";
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImpactDiffCursor {
    claims: ImpactDiffCursorClaims,
    seal: [u8; 32],
}
impl ImpactDiffCursor {
    pub fn issue(
        claims: ImpactDiffCursorClaims,
        secret: &[u8; 32],
    ) -> Result<Self, ProductAdmissionError> {
        let seal = seal_native_owner_statement(secret, &claims)?;
        Ok(Self { claims, seal })
    }
    pub fn claims(&self) -> &ImpactDiffCursorClaims {
        &self.claims
    }
    pub fn verify(
        &self,
        secret: &[u8; 32],
    ) -> Result<&ImpactDiffCursorClaims, ProductAdmissionError> {
        verify_native_owner_statement(secret, &self.claims, &self.seal)?;
        Ok(&self.claims)
    }
}
/// Native exact-key or verified-host-image diff on two retained bases.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImpactDiffRequest {
    pub base: ImpactLease,
    pub target: ImpactLease,
    pub domain: ImpactDiffDomain,
    pub limits: ImpactDiffLimits,
    pub continuation: Option<ImpactDiffCursor>,
    pub cancel: bool,
}
impl ImpactDiffRequest {
    pub fn recipe(&self) -> Result<[u8; 32], ProductAdmissionError> {
        let encoded = serde_json::to_vec(&(&self.base, &self.target, self.domain, &self.limits))
            .map_err(|_| ProductAdmissionError::SemanticVersionShape)?;
        let mut hash = blake3::Hasher::new();
        hash.update(b"backend.impact.diff-recipe.v1\0");
        hash.update(&encoded);
        Ok(*hash.finalize().as_bytes())
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImpactDiffSide {
    BaseOnly,
    TargetOnly,
    BothChanged,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImpactDiffCertainty {
    ExactCapturedRow,
    CandidateMissing,
}
/// A changed key and candidate paths discovered from that exact side's native
/// authored custom-property occurrence, if the fact has one.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImpactDiffItem {
    pub id: [u8; 32],
    pub side: ImpactDiffSide,
    pub certainty: ImpactDiffCertainty,
    pub impacts: Vec<ImpactDiffPath>,
    pub base_source: Option<ImpactSource>,
    pub target_source: Option<ImpactSource>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImpactDiffPath {
    pub side: ImpactSide,
    pub strength: ImpactStrength,
    pub dependency: [u8; 32],
    pub owner: Option<[u8; 32]>,
    pub path: Option<String>,
    pub witness: Vec<[u8; 32]>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields, from = "DiffTerminalWire")]
pub enum ImpactDiffTerminal {
    Complete,
    More { continuation: ImpactDiffCursor },
    WorkLimit { reason: String },
    Cancelled,
    Expired,
}
#[derive(Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum DiffTerminalWire {
    Complete {},
    More { continuation: ImpactDiffCursor },
    WorkLimit { reason: String },
    Cancelled {},
    Expired {},
}
impl From<DiffTerminalWire> for ImpactDiffTerminal {
    fn from(wire: DiffTerminalWire) -> Self {
        match wire {
            DiffTerminalWire::Complete {} => Self::Complete,
            DiffTerminalWire::More { continuation } => Self::More { continuation },
            DiffTerminalWire::WorkLimit { reason } => Self::WorkLimit { reason },
            DiffTerminalWire::Cancelled {} => Self::Cancelled,
            DiffTerminalWire::Expired {} => Self::Expired,
        }
    }
}
/// Bounded work measurements. Missing counters mean work stopped before an
/// authoritative measurement was captured; temporary bytes are conservative.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImpactDiffCounters {
    pub scan_rows: u32,
    pub scan_bytes: u32,
    pub temporary_bytes: u32,
    pub seed_walks: u32,
    pub walk_input_rows: u32,
    pub walk_input_bytes: u32,
    pub walk_edges: u32,
    pub walk_nodes: u32,
    pub walk_rows: u32,
    pub witness_bytes: u32,
    pub walk_path_bytes: u32,
    pub walk_temporary_peak: u32,
}
/// A complete diff is still scoped to retained native rows/images and does not
/// prove runtime effect, source-level rename or complete CSS semantics.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImpactDiffReply {
    pub base: ImpactLease,
    pub target: ImpactLease,
    pub domain: ImpactDiffDomain,
    pub base_capture_complete: bool,
    pub target_capture_complete: bool,
    pub semantic_gapped: bool,
    pub provider_unknown: bool,
    /// Some changed-seed traversal could not be completed within bounds.
    pub walk_limited: bool,
    pub context_gapped: bool,
    pub missing_owner: bool,
    pub rows: Vec<ImpactDiffItem>,
    pub comparisons: Option<u32>,
    pub compared_bytes: Option<u32>,
    pub counters: Option<ImpactDiffCounters>,
    pub terminal: ImpactDiffTerminal,
}
impl ImpactDiffRequest {
    /// MCP request schema for exact retained-row or verified-image comparison.
    #[must_use]
    pub fn json_schema() -> serde_json::Value {
        serde_json::json!({"type":"object","additionalProperties":false,"required":["base","target","domain","limits","continuation","cancel"],"properties":{
            "base":{"type":"object","description":"Copy the retained base lease."},
            "target":{"type":"object","description":"Copy the retained target lease."},
            "domain":{"enum":["native_facts","verified_source_images"]},
            "continuation":{"type":["object","null"],"description":"Copy the preceding page cursor, or null."},
            "cancel":{"type":"boolean","description":"Cancel this request before work; does not interrupt another call."},
            "limits":{"type":"object","additionalProperties":false,"required":["scan_rows","scan_bytes","temporary_bytes","compared_bytes","comparisons","page_rows","output_bytes","seed_walks","walk_input_rows","walk_input_bytes","walk_edges","walk_nodes","walk_depth","walk_rows","witness_bytes","walk_path_bytes","time_ms"],"properties":{
                "scan_rows":{"type":"integer","minimum":1,"maximum":524288},"scan_bytes":{"type":"integer","minimum":1,"maximum":134217728},"temporary_bytes":{"type":"integer","minimum":131072,"maximum":67108864},"compared_bytes":{"type":"integer","minimum":0,"maximum":134217728},"comparisons":{"type":"integer","minimum":1,"maximum":524288},"page_rows":{"type":"integer","minimum":1,"maximum":128},"output_bytes":{"type":"integer","minimum":131072,"maximum":4194304},"seed_walks":{"type":"integer","minimum":0,"maximum":128},"walk_input_rows":{"type":"integer","minimum":0,"maximum":1000000},"walk_input_bytes":{"type":"integer","minimum":0,"maximum":134217728},"walk_edges":{"type":"integer","minimum":0,"maximum":1000000},"walk_nodes":{"type":"integer","minimum":0,"maximum":262144},"walk_depth":{"type":"integer","minimum":0,"maximum":128},"walk_rows":{"type":"integer","minimum":0,"maximum":262144},"witness_bytes":{"type":"integer","minimum":0,"maximum":16777216},"walk_path_bytes":{"type":"integer","minimum":0,"maximum":4194304},"time_ms":{"type":"integer","minimum":1,"maximum":30000,"description":"Cooperative deadline."}
            }}
        }})
    }
}

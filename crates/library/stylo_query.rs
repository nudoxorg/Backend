//! Typed native Stylo search and source-site requests. Native source sites are
//! separate from normalized declarations and carry no provider/cascade claim.
use crate::{
    IndexSearchCursor, NativeOwnerStatement, PackageReference, ProductAdmissionError,
    seal_native_owner_statement, verify_native_owner_statement,
};
use backend_semantic::stylo_analysis::{
    StyloDeclaration, StyloRule, StyloSelector, StyloSourceGap, StyloSourceOccurrence,
    StyloVariableDependency,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};

/// Name family queried against native facts and supported authored sites.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StyloNameRole {
    /// Class selector identifier.
    Class,
    /// Custom-property name, including its `--` prefix.
    CustomProperty,
}
/// Snapshot-bound, budgeted search for one decoded CSS name.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StyloQueryRequest {
    /// Selected local package/project.
    pub package: PackageReference,
    /// Native/source name role.
    pub role: StyloNameRole,
    /// Exact decoded name, without selector-syntax normalization.
    pub name: String,
    /// Maximum rows returned per page, from 1 through 128.
    pub page_rows: u16,
    /// Maximum encoded native surface-reply bytes, 128 KiB through 4 MiB.
    /// Presentation and MCP envelopes have their own separate bounds.
    pub output_bytes: u32,
    /// Maximum native relation-row reads per page, from 7 through 4096.
    pub scan_rows: u32,
    /// Maximum encoded native relation-row bytes read per page, 224 KiB through 4 MiB.
    pub scan_bytes: u32,
    /// Owner-issued continuation for the same consumed basis and request.
    pub continuation: Option<StyloQueryCursor>,
}
impl StyloQueryRequest {
    /// Validate limits and return the exact lookup spelling.
    pub fn normalized_name(&self) -> Result<&str, ProductAdmissionError> {
        let name = self.name.as_str();
        if !matches!(self.package, PackageReference::Local(_))
            || name.is_empty()
            || name.len() > 1024
            || name.contains('\0')
            || self.page_rows == 0
            || self.page_rows > 128
            || !(131_072..=4_194_304).contains(&self.output_bytes)
            || !(7..=4096).contains(&self.scan_rows)
            || !(229_376..=4_194_304).contains(&self.scan_bytes)
            || (self.role == StyloNameRole::CustomProperty
                && (!name.starts_with("--") || name.len() <= 2))
        {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        if let Some(cursor) = &self.continuation {
            cursor.admit_shape()?;
        }
        Ok(name)
    }
}
/// Complete cursor statement; authentication is required before use.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StyloCursorClaims {
    /// Checked workspace manifest root.
    pub manifest_root: [u8; 32],
    /// Selected native relation root.
    pub native_root: [u8; 32],
    /// Resolved project identity.
    pub project: [u8; 32],
    /// Normalized query role.
    pub role: StyloNameRole,
    /// Normalized decoded name.
    pub name: String,
    /// Output and scan limits.
    pub page_rows: u16,
    /// Maximum encoded response bytes per page.
    pub output_bytes: u32,
    /// Maximum row reads per page.
    pub scan_rows: u32,
    /// Maximum encoded row bytes per page.
    pub scan_bytes: u32,
    /// Last consumed physical relation key.
    pub after: [u8; 32],
}
impl NativeOwnerStatement for StyloCursorClaims {
    const DOMAIN: &'static [u8] = b"backend.stylo.query-cursor.v1\0";
}
// Tokens use fixed-width big-endian fields and a version/domain byte pair. The
// existing owner-statement seal authenticates the reconstructed typed claims.
fn decode_token(token: &str, max_len: usize) -> Result<Vec<u8>, ProductAdmissionError> {
    if token.len() > max_len {
        return Err(ProductAdmissionError::SemanticVersionShape);
    }
    IndexSearchCursor::new(token.to_owned())?;
    let bytes = URL_SAFE_NO_PAD
        .decode(token)
        .map_err(|_| ProductAdmissionError::SemanticVersionShape)?;
    if URL_SAFE_NO_PAD.encode(&bytes) != token {
        return Err(ProductAdmissionError::SemanticVersionShape);
    }
    Ok(bytes)
}
struct TokenReader<'a> {
    remaining: &'a [u8],
}
impl<'a> TokenReader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], ProductAdmissionError> {
        let (head, tail) = self
            .remaining
            .split_at_checked(n)
            .ok_or(ProductAdmissionError::SemanticVersionShape)?;
        self.remaining = tail;
        Ok(head)
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N], ProductAdmissionError> {
        self.take(N)?
            .try_into()
            .map_err(|_| ProductAdmissionError::SemanticVersionShape)
    }
    fn finish(self) -> Result<(), ProductAdmissionError> {
        if self.remaining.is_empty() {
            Ok(())
        } else {
            Err(ProductAdmissionError::SemanticVersionShape)
        }
    }
}
/// Compact owner-sealed continuation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StyloQueryCursor(String);
impl StyloQueryCursor {
    /// Parse a canonical, unauthenticated continuation token.
    pub fn parse(token: String) -> Result<Self, ProductAdmissionError> {
        let value = Self(token);
        value.admit_shape()?;
        Ok(value)
    }
    /// Return the opaque continuation for a subsequent request.
    pub fn as_str(&self) -> &str {
        &self.0
    }
    /// Check canonical token shape without asserting owner authentication.
    pub fn admit_shape(&self) -> Result<(), ProductAdmissionError> {
        self.decode_claims().map(|_| ())
    }
    /// Issue a continuation for an already selected native query basis.
    pub fn issue(
        claims: StyloCursorClaims,
        secret: &[u8; 32],
    ) -> Result<Self, ProductAdmissionError> {
        let name = claims.name.as_bytes();
        if name.is_empty() || name.len() > 1024 {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        let seal = seal_native_owner_statement(secret, &claims)?;
        let mut bytes = vec![1, 1];
        bytes.extend_from_slice(&claims.manifest_root);
        bytes.extend_from_slice(&claims.native_root);
        bytes.extend_from_slice(&claims.project);
        bytes.push(match claims.role {
            StyloNameRole::Class => 1,
            StyloNameRole::CustomProperty => 2,
        });
        bytes.extend_from_slice(&(name.len() as u16).to_be_bytes());
        bytes.extend_from_slice(name);
        bytes.extend_from_slice(&claims.page_rows.to_be_bytes());
        bytes.extend_from_slice(&claims.output_bytes.to_be_bytes());
        bytes.extend_from_slice(&claims.scan_rows.to_be_bytes());
        bytes.extend_from_slice(&claims.scan_bytes.to_be_bytes());
        bytes.extend_from_slice(&claims.after);
        bytes.extend_from_slice(&seal);
        let token = URL_SAFE_NO_PAD.encode(bytes);
        IndexSearchCursor::new(token.clone())?;
        Ok(Self(token))
    }
    /// Verify canonical encoding and the complete typed owner statement.
    pub fn verify(&self, secret: &[u8; 32]) -> Result<StyloCursorClaims, ProductAdmissionError> {
        let (claims, seal) = self.decode_claims()?;
        verify_native_owner_statement(secret, &claims, &seal)?;
        Ok(claims)
    }
    fn decode_claims(&self) -> Result<(StyloCursorClaims, [u8; 32]), ProductAdmissionError> {
        let bytes = decode_token(&self.0, 2048)?;
        let mut r = TokenReader { remaining: &bytes };
        if r.array::<2>()? != [1, 1] {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        let manifest_root = r.array()?;
        let native_root = r.array()?;
        let project = r.array()?;
        let role = match r.array::<1>()?[0] {
            1 => StyloNameRole::Class,
            2 => StyloNameRole::CustomProperty,
            _ => return Err(ProductAdmissionError::SemanticVersionShape),
        };
        let n = usize::from(u16::from_be_bytes(r.array()?));
        if n == 0 || n > 1024 {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        let name = std::str::from_utf8(r.take(n)?)
            .map_err(|_| ProductAdmissionError::SemanticVersionShape)?
            .to_owned();
        let page_rows = u16::from_be_bytes(r.array()?);
        let output_bytes = u32::from_be_bytes(r.array()?);
        let scan_rows = u32::from_be_bytes(r.array()?);
        let scan_bytes = u32::from_be_bytes(r.array()?);
        let after = r.array()?;
        let seal = r.array()?;
        r.finish()?;
        let claims = StyloCursorClaims {
            manifest_root,
            native_root,
            project,
            role,
            name,
            page_rows,
            output_bytes,
            scan_rows,
            scan_bytes,
            after,
        };
        Ok((claims, seal))
    }
}
/// Authenticated exact source-site locator, valid only at its selected roots.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StyloSourceClaims {
    /// Checked workspace manifest root.
    pub manifest_root: [u8; 32],
    /// Selected native relation root.
    pub native_root: [u8; 32],
    /// Owning project.
    pub project: [u8; 32],
    /// Captured host row key.
    pub host: [u8; 32],
    /// Authenticated authored-site row key.
    pub site: [u8; 32],
}
impl NativeOwnerStatement for StyloSourceClaims {
    const DOMAIN: &'static [u8] = b"backend.stylo.authored-site.v1\0";
}
/// Copyable owner-sealed source-site handle.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StyloSourceReference(String);
impl StyloSourceReference {
    /// Parse a canonical, unauthenticated source token.
    pub fn parse(token: String) -> Result<Self, ProductAdmissionError> {
        let value = Self(token);
        value.admit_shape()?;
        Ok(value)
    }
    /// Return the opaque token for a source-site request.
    pub fn as_str(&self) -> &str {
        &self.0
    }
    /// Check canonical token shape without asserting owner authentication.
    pub fn admit_shape(&self) -> Result<(), ProductAdmissionError> {
        let bytes = decode_token(&self.0, 260)?;
        if bytes.len() != 194 || bytes.get(..2) != Some(&[1, 2]) {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        Ok(())
    }
    /// Issue only for a selected authored source-site row.
    pub fn issue(
        claims: StyloSourceClaims,
        secret: &[u8; 32],
    ) -> Result<Self, ProductAdmissionError> {
        let seal = seal_native_owner_statement(secret, &claims)?;
        let mut bytes = vec![1, 2];
        bytes.extend_from_slice(&claims.manifest_root);
        bytes.extend_from_slice(&claims.native_root);
        bytes.extend_from_slice(&claims.project);
        bytes.extend_from_slice(&claims.host);
        bytes.extend_from_slice(&claims.site);
        bytes.extend_from_slice(&seal);
        let token = URL_SAFE_NO_PAD.encode(bytes);
        IndexSearchCursor::new(token.clone())?;
        Ok(Self(token))
    }
    /// Verify canonical encoding and the complete typed owner statement.
    pub fn verify(&self, secret: &[u8; 32]) -> Result<StyloSourceClaims, ProductAdmissionError> {
        let bytes = decode_token(&self.0, 260)?;
        let mut r = TokenReader { remaining: &bytes };
        if r.array::<2>()? != [1, 2] {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        let claims = StyloSourceClaims {
            manifest_root: r.array()?,
            native_root: r.array()?,
            project: r.array()?,
            host: r.array()?,
            site: r.array()?,
        };
        let seal = r.array()?;
        r.finish()?;
        verify_native_owner_statement(secret, &claims, &seal)?;
        Ok(claims)
    }
}
/// Freshness of selected native evidence, independent of source-byte availability.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum StyloAttemptState {
    /// The requested project is not part of the selected workspace.
    ProjectNotSelected,
    /// The project is selected but has no selected native analysis or recorded attempt.
    NoSelectedAnalysis,
    /// Latest recorded analysis observed the served selection.
    LatestObserved,
    /// Latest attempt failed; these facts belong to an older served selection.
    RetainedAfterUnavailable {
        /// Recorded native failure category.
        reason: StyloFailure,
    },
    /// Latest attempt failed without a served selection.
    Unavailable {
        /// Recorded native failure category.
        reason: StyloFailure,
    },
    /// No latest attempt row was recorded by the historical producer.
    UnknownLegacy,
}
/// Closed category of a recorded native attempt failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StyloFailure {
    /// A selected host could not be read.
    ReadUnavailable,
    /// Project selection or policy evidence was unavailable.
    SelectionUnavailable,
    /// Host bytes were not standalone UTF-8.
    InvalidUtf8,
    /// A bounded operation reached its limit.
    Limit,
    /// No native parser worker was available.
    ParserUnavailable,
    /// The requested hard process limit is unsupported.
    UnsupportedHardLimit,
    /// The worker exceeded its deadline.
    Deadline,
    /// The worker or its protocol failed.
    WorkerFailure,
    /// Process-global preferences changed during parsing.
    PreferencesChanged,
}
/// Evidence of the worker capability on the latest attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "identity", rename_all = "snake_case")]
pub enum StyloWorkerCapability {
    /// Exact admitted worker artifact.
    Admitted([u8; 32]),
    /// No worker permit was supplied.
    NoPermit,
    /// No worker admission was witnessed.
    Unknown,
}
/// Aggregate status of all selected host parse terminals.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StyloHostTerminalSummary {
    /// No bounded, admitted aggregate of host terminals is available.
    Unknown,
}
/// Capabilities not established by the standalone CSS profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StyloScopeGap {
    /// Authored source enumeration is partial.
    AuthoredSourcePartial,
    /// Authored custom-property reference sites are unavailable.
    AuthoredReferenceSitesUnavailable,
    /// Ordinary property values do not expose complete native reference lists.
    OrdinaryVariableReferencesUnavailable,
    /// Escaped native declaration names lack a decoded identity in this profile.
    NativeDeclarationNameDecodeUnavailable,
    /// Effective process-global preference stability is unknown.
    PreferenceStabilityUnknown,
    /// Imported stylesheet resolution was not attempted.
    ImportsNotResolved,
    /// Rule ancestors and their conditions are not returned with a hit.
    AncestorRuleContextNotReturned,
    /// Framework and build transforms were not attempted.
    FrameworkNotInterpreted,
    /// DOM matching and cascade were not attempted.
    DomAndCascadeNotAttempted,
    /// Custom-property providers and computed substitutions are unknown.
    ProviderUnknown,
}
/// Authenticated per-query scope and producer basis.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StyloQueryBasis {
    /// Checked workspace manifest root.
    pub manifest_root: [u8; 32],
    /// Exact selected native root, or unknown historical absence.
    pub native_root: Option<[u8; 32]>,
    /// Recorded latest-attempt state.
    pub attempt: StyloAttemptState,
    /// Worker admission on the latest attempt, if one was recorded.
    pub worker_capability: StyloWorkerCapability,
    /// Parse health is unknown until an admitted host-terminal summary exists.
    pub host_terminal_summary: StyloHostTerminalSummary,
    /// Source selection and CSS semantics always retain declared gaps.
    pub source_partial: bool,
    /// Explicit unevaluated capabilities.
    pub gaps: Vec<StyloScopeGap>,
    /// Native producer support fingerprint when a selection exists.
    pub producer: Option<[u8; 32]>,
    /// Selected discovery inventory was complete.
    pub selection_complete: bool,
}
/// One bounded hit from native rules or exact authored observer sites.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StyloQueryHit {
    /// A source occurrence and its native rule/optional selector.
    SourceSite {
        /// Revision-local authored-site key.
        id: [u8; 32],
        /// Project-relative path.
        path: String,
        /// Original occurrence claim.
        site: StyloSourceOccurrence,
        /// Proven immediate native owner rule; ancestor conditions are not returned.
        rule: StyloRule,
        /// Native selector for class sites.
        selector: Option<StyloSelector>,
        /// Exact source access handle.
        source: StyloSourceReference,
        /// Uncovered source capabilities.
        source_gaps: Vec<StyloSourceGap>,
    },
    /// A native selector; it is not proof of a rendered match.
    NativeSelector {
        /// Native fact key.
        id: [u8; 32],
        /// Project-relative host path.
        path: String,
        /// Immediate native owner rule; ancestor conditions are not returned.
        rule: StyloRule,
        /// Native selector.
        selector: StyloSelector,
    },
    /// A native retained declaration; no authored-site correspondence is implied.
    NativeDeclaration {
        /// Native fact key.
        id: [u8; 32],
        /// Project-relative host path.
        path: String,
        /// Immediate native owner rule; ancestor conditions are not returned.
        rule: StyloRule,
        /// Normalized native retained declaration.
        declaration: StyloDeclaration,
    },
    /// A native `var()` reference and its retained owner; provider remains unknown.
    NativeDependency {
        /// Native dependency row key.
        id: [u8; 32],
        /// Project-relative host path.
        path: String,
        /// Immediate native owner rule; ancestor conditions are not returned.
        rule: StyloRule,
        /// Native retained owner declaration.
        declaration: StyloDeclaration,
        /// Native reference, including its fallback structural path.
        dependency: StyloVariableDependency,
    },
}
/// Enumeration outcome, separate from CSS capability coverage.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum StyloQueryTerminal {
    /// All selected relation rows were enumerated.
    Complete,
    /// Another authenticated page exists.
    More {
        /// Continuation after the last consumed key.
        continuation: StyloQueryCursor,
        /// A scan budget, rather than output rows, ended this page.
        work_limited: bool,
    },
    /// A single result cannot fit; retry with at least this output budget.
    OutputLimit {
        /// Sufficient output budget for the blocked result, including framing margin.
        required_bytes: u32,
        /// Cursor at the last consumed row, bound to `required_bytes`; absent on the first page
        /// or when the required budget exceeds the permitted maximum.
        continuation: Option<StyloQueryCursor>,
    },
    /// No selected source project or native root is available.
    Unavailable,
}
/// One exact-root native CSS page.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StyloQueryPage {
    /// One shared scope/basis header.
    pub basis: StyloQueryBasis,
    /// Matched native facts and/or authored occurrences.
    pub hits: Vec<StyloQueryHit>,
    /// Enumeration terminal; never upgrades CSS coverage.
    pub terminal: StyloQueryTerminal,
}
/// Exact captured-site source read outcome.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum StyloSourceRead {
    /// Exact UTF-8 site from checked original host bytes.
    Available {
        /// Selected served filesystem root used for this verified read.
        served_root: String,
        /// Project-relative path.
        path: String,
        /// Inclusive host byte offset.
        start: u64,
        /// Exclusive host byte offset.
        end: u64,
        /// One-based line, counted from LF bytes in the verified host.
        line_1: u32,
        /// Exact original site text.
        text: String,
    },
    /// Host still exists but no longer matches the captured revision.
    SourceChanged,
    /// Original bytes are not retained and current host is unavailable.
    BytesNotRetained,
    /// Selected roots changed; issue a fresh query.
    Expired,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(name: &str) -> StyloQueryRequest {
        StyloQueryRequest {
            package: PackageReference::parse("/tmp/project").unwrap(),
            role: StyloNameRole::Class,
            name: name.to_owned(),
            page_rows: 1,
            output_bytes: 131_072,
            scan_rows: 7,
            scan_bytes: 229_376,
            continuation: None,
        }
    }
    #[test]
    fn decoded_names_remain_exact_and_case_sensitive() {
        assert_eq!(request(".foo").normalized_name().unwrap(), ".foo");
        assert_eq!(request("foo").normalized_name().unwrap(), "foo");
        assert_ne!(
            request(".foo").normalized_name().unwrap(),
            request("foo").normalized_name().unwrap()
        );
        let mut custom = request("--Ink");
        custom.role = StyloNameRole::CustomProperty;
        assert_eq!(custom.normalized_name().unwrap(), "--Ink");
        custom.name = "Ink".into();
        assert!(custom.normalized_name().is_err());
    }
    #[test]
    fn owner_tokens_bind_exact_roots_and_site_identity() {
        let claims = StyloCursorClaims {
            manifest_root: [1; 32],
            native_root: [2; 32],
            project: [3; 32],
            role: StyloNameRole::Class,
            name: "hud-menu".into(),
            page_rows: 1,
            output_bytes: 131_072,
            scan_rows: 7,
            scan_bytes: 229_376,
            after: [4; 32],
        };
        let cursor = StyloQueryCursor::issue(claims.clone(), &[5; 32]).unwrap();
        assert_eq!(cursor.verify(&[5; 32]).unwrap(), claims);
        assert!(cursor.verify(&[6; 32]).is_err());
        assert!(cursor.0.len() < 300);
        let mut bad_cursor = URL_SAFE_NO_PAD.decode(&cursor.0).unwrap();
        bad_cursor[0] = 2;
        assert!(
            StyloQueryCursor(URL_SAFE_NO_PAD.encode(&bad_cursor))
                .verify(&[5; 32])
                .is_err()
        );
        assert!(
            StyloQueryCursor(format!("{}=", cursor.0))
                .verify(&[5; 32])
                .is_err()
        );
        let mut trailing = URL_SAFE_NO_PAD.decode(&cursor.0).unwrap();
        trailing.push(0);
        assert!(StyloQueryCursor::parse(URL_SAFE_NO_PAD.encode(trailing)).is_err());
        let site = StyloSourceClaims {
            manifest_root: [1; 32],
            native_root: [2; 32],
            project: [3; 32],
            host: [4; 32],
            site: [5; 32],
        };
        let handle = StyloSourceReference::issue(site.clone(), &[6; 32]).unwrap();
        assert_eq!(handle.0.len(), 259);
        assert_eq!(handle.verify(&[6; 32]).unwrap(), site);
        assert!(handle.verify(&[7; 32]).is_err());
        let mut changed = URL_SAFE_NO_PAD.decode(&handle.0).unwrap();
        changed[2] ^= 1;
        assert!(
            StyloSourceReference(URL_SAFE_NO_PAD.encode(&changed))
                .verify(&[6; 32])
                .is_err()
        );
        changed[2] ^= 1;
        changed.push(0);
        assert!(
            StyloSourceReference(URL_SAFE_NO_PAD.encode(&changed))
                .verify(&[6; 32])
                .is_err()
        );
        assert!(StyloQueryCursor(handle.0.clone()).verify(&[6; 32]).is_err());
    }
}

//! Native Vue query transport types. These do not claim complete Vue, module,
//! template or CSS semantics, and no handle is valid without owner admission.
use crate::{
    IndexSearchCursor, NativeOwnerStatement, PackageReference, ProductAdmissionError,
    seal_native_owner_statement, verify_native_owner_statement,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};

/// Bounded exact-authored-name query over a selected Vue project.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VueQueryRequest {
    /// Selected local project.
    pub package: PackageReference,
    /// Exact raw authored spelling, without identifier unescaping.
    pub name: String,
    /// Query declarations or locally bound direct calls.
    pub role: VueQueryRole,
    /// Maximum returned entries, 1 through 128.
    pub limit: u16,
    /// Maximum enumerated relation rows to inspect, 1 through 4096; project
    /// and selection lookups are separate bounded authority checks.
    pub scan_rows: u32,
    /// Maximum captured host bytes to inspect, 1 MiB through 64 MiB.
    pub source_bytes: u32,
    /// Maximum encoded reply bytes, 128 KiB through 4 MiB.
    pub output_bytes: u32,
    /// Owner-issued continuation, bound to this request and selected roots.
    pub continuation: Option<VueQueryCursor>,
}
impl VueQueryRequest {
    /// Validate shape and independent work bounds.
    pub fn admit(&self) -> Result<(), ProductAdmissionError> {
        if !matches!(self.package, PackageReference::Local(_))
            || self.name.is_empty()
            || self.name.len() > 1024
            || self.name.contains('\0')
            || self.limit == 0
            || self.limit > 128
            || self.scan_rows == 0
            || self.scan_rows > 4096
            || !(1_048_576..=67_108_864).contains(&self.source_bytes)
            || !(131_072..=4_194_304).contains(&self.output_bytes)
        {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        if let Some(cursor) = &self.continuation {
            cursor.admit_shape()?;
        }
        Ok(())
    }
}
/// The independently scoped native syntax result kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VueQueryRole {
    /// TSZ-observed declaration names.
    Declaration,
    /// Direct calls bound locally within one script block.
    LocalCall,
}
/// Root and request fields authenticated by the workspace owner.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VueCursorClaims {
    /// Selected workspace manifest root.
    pub manifest_root: [u8; 32],
    /// Selected workspace closure.
    pub closure: [u8; 32],
    /// Selected Vue auxiliary relation root.
    pub vue_root: [u8; 32],
    /// Selected project identity.
    pub project: [u8; 32],
    /// Exact query role.
    pub role: VueQueryRole,
    /// Exact authored query spelling.
    pub name: String,
    /// Page result bound.
    pub limit: u16,
    /// Relation scan bound.
    pub scan_rows: u32,
    /// Maximum captured host bytes per page.
    pub source_bytes: u32,
    /// Maximum encoded reply bytes per page.
    pub output_bytes: u32,
    /// Last consumed host key.
    pub after_host: [u8; 32],
    /// Next site within that host.
    pub next_site: u32,
}
impl VueCursorClaims {
    fn admit(&self) -> Result<(), ProductAdmissionError> {
        if self.manifest_root == [0; 32]
            || self.closure == [0; 32]
            || self.vue_root == [0; 32]
            || self.project == [0; 32]
            || self.name.is_empty()
            || self.name.len() > 1024
            || self.name.contains('\0')
            || self.limit == 0
            || self.limit > 128
            || self.scan_rows == 0
            || self.scan_rows > 4096
            || !(1_048_576..=67_108_864).contains(&self.source_bytes)
            || !(131_072..=4_194_304).contains(&self.output_bytes)
        {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        Ok(())
    }
}
impl NativeOwnerStatement for VueCursorClaims {
    const DOMAIN: &'static [u8] = b"backend.vue.query-cursor.v1\0";
}
/// Opaque owner-authenticated continuation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct VueQueryCursor(String);
impl VueQueryCursor {
    /// Parse a canonical token without claiming authentication.
    pub fn parse(token: String) -> Result<Self, ProductAdmissionError> {
        let value = Self(token);
        value.admit_shape()?;
        Ok(value)
    }
    /// Return opaque token text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
    /// Validate bounded canonical token shape.
    pub fn admit_shape(&self) -> Result<(), ProductAdmissionError> {
        let envelope: Envelope<VueCursorClaims> = decode_envelope(&self.0, 4096)?;
        envelope.claims.admit()
    }
    /// Seal a complete selected-root query statement.
    pub fn issue(
        claims: VueCursorClaims,
        secret: &[u8; 32],
    ) -> Result<Self, ProductAdmissionError> {
        claims.admit()?;
        Ok(Self(encode(Envelope {
            seal: seal_native_owner_statement(secret, &claims)?,
            claims,
        })?))
    }
    /// Verify the owner seal and return the authenticated statement.
    pub fn verify(&self, secret: &[u8; 32]) -> Result<VueCursorClaims, ProductAdmissionError> {
        let envelope: Envelope<VueCursorClaims> = decode_envelope(&self.0, 4096)?;
        envelope.claims.admit()?;
        verify_native_owner_statement(secret, &envelope.claims, &envelope.seal)?;
        Ok(envelope.claims)
    }
}
/// Authenticated source-site identity, meaningful only at the selected roots.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VueSourceClaims {
    /// Selected workspace manifest root.
    pub manifest_root: [u8; 32],
    /// Selected workspace closure.
    pub closure: [u8; 32],
    /// Selected Vue relation root.
    pub vue_root: [u8; 32],
    /// Selected project identity.
    pub project: [u8; 32],
    /// Verified host-row key.
    pub host: [u8; 32],
    /// Declaration or local call kind.
    pub role: VueQueryRole,
    /// Site index within the verified host row.
    pub site: u32,
}
impl VueSourceClaims {
    fn admit(&self) -> Result<(), ProductAdmissionError> {
        if self.manifest_root == [0; 32]
            || self.closure == [0; 32]
            || self.vue_root == [0; 32]
            || self.project == [0; 32]
            || self.host == [0; 32]
        {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        Ok(())
    }
}
impl NativeOwnerStatement for VueSourceClaims {
    const DOMAIN: &'static [u8] = b"backend.vue.source-site.v1\0";
}
/// Opaque owner-sealed exact source locator.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct VueSourceReference(String);
impl VueSourceReference {
    /// Parse a canonical token without claiming authentication.
    pub fn parse(token: String) -> Result<Self, ProductAdmissionError> {
        let value = Self(token);
        value.admit_shape()?;
        Ok(value)
    }
    /// Return opaque token text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
    /// Check bounded canonical token shape.
    pub fn admit_shape(&self) -> Result<(), ProductAdmissionError> {
        let (claims, _) = self.decode()?;
        claims.admit()
    }
    /// Seal a selected Vue source site.
    pub fn issue(
        claims: VueSourceClaims,
        secret: &[u8; 32],
    ) -> Result<Self, ProductAdmissionError> {
        claims.admit()?;
        let seal = seal_native_owner_statement(secret, &claims)?;
        let mut bytes = Vec::with_capacity(199);
        bytes.extend_from_slice(&[1, 1]);
        bytes.extend_from_slice(&claims.manifest_root);
        bytes.extend_from_slice(&claims.closure);
        bytes.extend_from_slice(&claims.vue_root);
        bytes.extend_from_slice(&claims.project);
        bytes.extend_from_slice(&claims.host);
        bytes.push(match claims.role {
            VueQueryRole::Declaration => 0,
            VueQueryRole::LocalCall => 1,
        });
        bytes.extend_from_slice(&claims.site.to_be_bytes());
        bytes.extend_from_slice(&seal);
        let token = URL_SAFE_NO_PAD.encode(bytes);
        IndexSearchCursor::new(token.clone())?;
        Ok(Self(token))
    }
    /// Verify the selected owner statement.
    pub fn verify(&self, secret: &[u8; 32]) -> Result<VueSourceClaims, ProductAdmissionError> {
        let (claims, seal) = self.decode()?;
        claims.admit()?;
        verify_native_owner_statement(secret, &claims, &seal)?;
        Ok(claims)
    }
    fn decode(&self) -> Result<(VueSourceClaims, [u8; 32]), ProductAdmissionError> {
        let bytes = decode(&self.0, 266)?;
        if bytes.len() != 199 || bytes.get(..2) != Some(&[1, 1][..]) {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        let array = |start: usize| -> Result<[u8; 32], ProductAdmissionError> {
            bytes
                .get(start..start + 32)
                .and_then(|s| s.try_into().ok())
                .ok_or(ProductAdmissionError::SemanticVersionShape)
        };
        let role = match bytes[162] {
            0 => VueQueryRole::Declaration,
            1 => VueQueryRole::LocalCall,
            _ => return Err(ProductAdmissionError::SemanticVersionShape),
        };
        let site = u32::from_be_bytes(
            bytes[163..167]
                .try_into()
                .map_err(|_| ProductAdmissionError::SemanticVersionShape)?,
        );
        Ok((
            VueSourceClaims {
                manifest_root: array(2)?,
                closure: array(34)?,
                vue_root: array(66)?,
                project: array(98)?,
                host: array(130)?,
                role,
                site,
            },
            array(167)?,
        ))
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope<T> {
    claims: T,
    seal: [u8; 32],
}
fn encode<T: Serialize>(value: Envelope<T>) -> Result<String, ProductAdmissionError> {
    let bytes =
        serde_json::to_vec(&value).map_err(|_| ProductAdmissionError::SemanticVersionShape)?;
    let token = URL_SAFE_NO_PAD.encode(bytes);
    if token.len() > 4096 {
        return Err(ProductAdmissionError::SemanticVersionShape);
    }
    IndexSearchCursor::new(token.clone())?;
    Ok(token)
}
fn decode(token: &str, max: usize) -> Result<Vec<u8>, ProductAdmissionError> {
    if token.len() > max {
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
fn decode_envelope<T: for<'de> Deserialize<'de> + Serialize>(
    token: &str,
    max: usize,
) -> Result<Envelope<T>, ProductAdmissionError> {
    let bytes = decode(token, max)?;
    let envelope: Envelope<T> =
        serde_json::from_slice(&bytes).map_err(|_| ProductAdmissionError::SemanticVersionShape)?;
    if serde_json::to_vec(&envelope).map_err(|_| ProductAdmissionError::SemanticVersionShape)?
        != bytes
    {
        return Err(ProductAdmissionError::SemanticVersionShape);
    }
    Ok(envelope)
}
/// Vue coverage independent of the page enumeration terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VueCapability {
    /// Native analysis has known gaps; no negative result is complete.
    Partial,
    /// No analysis available.
    Unavailable,
}
/// Separate source capability coverage.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VueCoverage {
    /// Local script syntax and direct-call observations.
    pub script: VueCapability,
    /// Template semantics.
    pub template: VueCapability,
    /// Style semantics.
    pub style: VueCapability,
}
/// Evidence for the effective source-selection policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VueSelectionPolicy {
    /// Global or ancestor ignore inputs were not fully witnessed.
    Unknown,
}
/// One verified syntax occurrence, not a globally resolved symbol.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VueQueryHit {
    /// Exact authored spelling.
    pub name: String,
    /// Effective discovery policy qualification retained with this occurrence.
    pub selection_policy: VueSelectionPolicy,
    /// Project-relative host path.
    pub path: String,
    /// Opaque exact host source locator.
    pub source: VueSourceReference,
}
/// Bounded page terminal; even exhausted pages may have partial coverage.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum VueQueryTerminal {
    /// Selected relation enumeration ended.
    Complete,
    /// Another bounded page remains.
    More {
        /// Authenticated continuation.
        continuation: VueQueryCursor,
    },
    /// Selection or source authority is absent.
    Unavailable,
}
/// One exact-root Vue result page.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VueQueryPage {
    /// Shared selected workspace root.
    pub manifest_root: [u8; 32],
    /// Exact selected Vue root.
    pub vue_root: [u8; 32],
    /// Selected workspace closure.
    pub closure: [u8; 32],
    /// Selected local project identity.
    pub project: [u8; 32],
    /// Number of observed hosts without native facts.
    pub unavailable_hosts: u32,
    /// Captured hosts whose current bytes differ.
    pub stale_hosts: u32,
    /// Captured hosts no longer present.
    pub removed_hosts: u32,
    /// Effective source-selection policy qualification.
    pub selection_policy: VueSelectionPolicy,
    /// Whether bounded source discovery was truncated.
    pub walk_truncated: bool,
    /// Separate capability coverage.
    pub coverage: VueCoverage,
    /// Matched syntax observations.
    pub hits: Vec<VueQueryHit>,
    /// Enumeration outcome.
    pub terminal: VueQueryTerminal,
}
/// Result of reading a selected source site.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum VueSourceRead {
    /// Original verified authored source bytes.
    Available {
        /// The exact owner-sealed request reference, for response correlation.
        reference: VueSourceReference,
        /// Effective discovery policy qualification retained with the source.
        selection_policy: VueSelectionPolicy,
        /// Selected source root.
        served_root: String,
        /// Project-relative path.
        path: String,
        /// Inclusive original host byte offset.
        start: u64,
        /// Exclusive original host byte offset.
        end: u64,
        /// One-based LF-counted line number.
        line_1: u32,
        /// Exact source spelling.
        text: String,
    },
    /// Original host changed.
    SourceChanged,
    /// Original host was removed.
    SourceRemoved,
    /// Original bytes are currently inaccessible.
    BytesNotRetained,
    /// The authenticated locator belongs to a different selected project.
    Foreign,
    /// Selected root or owner changed.
    Expired,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_token_authenticates_all_roots_and_site() {
        let secret = [7; 32];
        let claims = VueSourceClaims {
            manifest_root: [1; 32],
            closure: [2; 32],
            vue_root: [3; 32],
            project: [4; 32],
            host: [5; 32],
            role: VueQueryRole::LocalCall,
            site: 9,
        };
        let token = VueSourceReference::issue(claims.clone(), &secret).unwrap();
        assert_eq!(token.verify(&secret).unwrap(), claims);
        assert!(token.verify(&[8; 32]).is_err());
        let mut changed = claims;
        changed.vue_root = [8; 32];
        assert!(
            VueSourceReference::issue(changed, &secret)
                .unwrap()
                .verify(&[8; 32])
                .is_err()
        );
        let mut bad = token.as_str().to_owned();
        bad.push('=');
        assert!(VueSourceReference::parse(bad).is_err());
    }
}

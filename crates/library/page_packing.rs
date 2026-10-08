//! Separate byte-packed page claims; count-only cursor semantics stay intact.
//!
//! A packed token authenticates its exact ordered predecessor interval and
//! next-row witness. Imported claims still require verification against the
//! selected immutable source; their digest is not source authority.

use crate::{Cursor, QueryLimit, ViewRecipeId, ViewRevision};

/// Existing application response ceiling; packing cannot increase it.
pub const MAX_PACKED_WIRE_BYTES: u32 = 48 * 1024;
/// Canonical packed checkpoint schema, independent of pc3 and journal DTOs.
pub const PACKED_CHECKPOINT_SCHEMA: u16 = 1;
const POLICY_BYTES: usize = 11;
const CONTEXT_BYTES: usize = crate::CURSOR_CONTROL_BYTES + 32 + 32 + POLICY_BYTES;
const CLAIM_BYTES: usize = 2 + CONTEXT_BYTES + 8 + 2 + 8 + 32 + 1 + 32 + 4;
const AUTH_BYTES: usize = 32;
/// Exact unescaped and JSON-escaped token text length for the pp1 codec.
/// Every character is ASCII hex or a hyphen, so neither JSON nor UTF-8 changes
/// its length. This codec deliberately does not compress claims.
pub const PACKED_CHECKPOINT_TOKEN_BYTES: usize = 4 + CLAIM_BYTES * 2 + 1 + AUTH_BYTES * 2;

/// Counting hash writer for one complete canonical projected row. The shared
/// serializer writes its JSON directly here without allocating another row.
pub struct PackedProjectedRowHasher {
    hash: blake3::Hasher,
    bytes: usize,
}

impl Default for PackedProjectedRowHasher {
    fn default() -> Self {
        let mut hash = blake3::Hasher::new();
        hash.update(b"packed-canonical-projected-row-v1\0");
        Self { hash, bytes: 0 }
    }
}

impl PackedProjectedRowHasher {
    /// Completes the row commitment and reports its exact serialized bytes.
    #[must_use]
    pub fn finish(mut self) -> ([u8; 32], usize) {
        self.hash.update(&(self.bytes as u64).to_be_bytes());
        (*self.hash.finalize().as_bytes(), self.bytes)
    }
}

impl std::io::Write for PackedProjectedRowHasher {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.bytes = self
            .bytes
            .checked_add(bytes.len())
            .ok_or_else(|| std::io::Error::other("projected row size overflow"))?;
        self.hash.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Stable projection selected when a packed chain begins.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum PagePackingDetail {
    /// Summary projection.
    Summary = 0,
    /// Standard projection.
    Standard = 1,
    /// Full projection.
    Full = 2,
}

/// Canonical serialization/cost recipe selected by the caller's route.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum PagePackingProfile {
    /// Shared typed CLI JSON projection, version one.
    CliJsonV1 = 0,
    /// Shared MCP tool result, metadata and bounded preview, version one.
    McpToolV1 = 1,
}

/// Actual output scope covered by the stable packing policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum PagePackingScope {
    /// CLI typed JSON body.
    CliJsonBody = 0,
    /// Complete MCP stdio JSON-RPC response and trailing newline.
    StdioJsonRpcLine = 1,
    /// Complete MCP HTTP JSON-RPC entity without a stdio newline.
    HttpJsonRpcBody = 2,
}

/// Stable byte policy. Per-call IDs, expiry and signatures never enter it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PagePackingPolicy {
    rows: QueryLimit,
    bytes: u32,
    detail: PagePackingDetail,
    profile: PagePackingProfile,
    scope: PagePackingScope,
}

impl PagePackingPolicy {
    /// Admits a positive existing row credit and an unchanged byte ceiling.
    pub fn new(
        rows: QueryLimit,
        bytes: u32,
        detail: PagePackingDetail,
        profile: PagePackingProfile,
        scope: PagePackingScope,
    ) -> Result<Self, PagePackingError> {
        if bytes == 0 || bytes > MAX_PACKED_WIRE_BYTES {
            return Err(PagePackingError::ByteCredit);
        }
        if !matches!(
            (profile, scope),
            (PagePackingProfile::CliJsonV1, PagePackingScope::CliJsonBody)
                | (
                    PagePackingProfile::McpToolV1,
                    PagePackingScope::StdioJsonRpcLine | PagePackingScope::HttpJsonRpcBody
                )
        ) {
            return Err(PagePackingError::ProfileScope);
        }
        Ok(Self {
            rows,
            bytes,
            detail,
            profile,
            scope,
        })
    }

    /// Original requested row ceiling, unchanged throughout a chain.
    #[must_use]
    pub const fn rows(self) -> QueryLimit {
        self.rows
    }
    /// Original complete output byte ceiling.
    #[must_use]
    pub const fn bytes(self) -> u32 {
        self.bytes
    }
    /// Original projection detail.
    #[must_use]
    pub const fn detail(self) -> PagePackingDetail {
        self.detail
    }
    /// Original canonical serializer/cost profile.
    #[must_use]
    pub const fn profile(self) -> PagePackingProfile {
        self.profile
    }
    /// Original actual accounting scope.
    #[must_use]
    pub const fn scope(self) -> PagePackingScope {
        self.scope
    }

    /// Canonical stable policy bytes used by recipes and fixed-width tokens.
    #[must_use]
    pub fn canonical_bytes(self) -> [u8; POLICY_BYTES] {
        let mut bytes = [0; POLICY_BYTES];
        bytes[..2].copy_from_slice(&PACKED_CHECKPOINT_SCHEMA.to_be_bytes());
        bytes[2..4].copy_from_slice(&self.rows.get().to_be_bytes());
        bytes[4..8].copy_from_slice(&self.bytes.to_be_bytes());
        bytes[8] = self.detail as u8;
        bytes[9] = self.profile as u8;
        bytes[10] = self.scope as u8;
        bytes
    }

    /// Recipe identity of this stable policy alone.
    #[must_use]
    pub fn identity(self) -> ViewRecipeId {
        crate::view_identity_bytes(&[b"packed-page-policy-v1", &self.canonical_bytes()])
    }
}

/// Exact selected source, query and original owner bindings for one chain.
/// The query recipe must already bind text/variables and the read manifest.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PackedPageContext {
    owner: Cursor,
    revision: ViewRevision,
    query: ViewRecipeId,
    policy: PagePackingPolicy,
}

impl PackedPageContext {
    /// Binds a stable policy to an admitted source/query and zero-offset owner.
    pub fn new(
        owner: Cursor,
        revision: ViewRevision,
        query: ViewRecipeId,
        policy: PagePackingPolicy,
    ) -> Result<Self, PagePackingError> {
        if owner.query_offset() != 0 || !revision.matches(owner.root()) {
            return Err(PagePackingError::Context);
        }
        Ok(Self {
            owner,
            revision,
            query,
            policy,
        })
    }

    /// Original owner cursor, including its exact revision and frontier.
    #[must_use]
    pub const fn owner(self) -> Cursor {
        self.owner
    }
    /// Exact immutable source revision.
    #[must_use]
    pub const fn revision(self) -> ViewRevision {
        self.revision
    }
    /// Original query recipe.
    #[must_use]
    pub const fn query(self) -> ViewRecipeId {
        self.query
    }
    /// Original stable packing policy.
    #[must_use]
    pub const fn policy(self) -> PagePackingPolicy {
        self.policy
    }

    fn canonical_bytes(self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(CONTEXT_BYTES);
        bytes.extend_from_slice(&self.owner.encode_control());
        bytes.extend_from_slice(self.revision.as_bytes());
        bytes.extend_from_slice(self.query.as_bytes());
        bytes.extend_from_slice(&self.policy.canonical_bytes());
        bytes
    }
}

/// Closed packed stop reasons, distinct from count-only `PageTerminal::More`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackedPageStop {
    /// The source was observed exhausted at the checkpoint's next ordinal.
    Complete,
    /// The original row credit was reached and this next row exists.
    RowCreditReached([u8; 32]),
    /// Adding the witnessed next row exceeded the complete output credit.
    WireCreditReached([u8; 32]),
}

impl PackedPageStop {
    /// Returns the exact first un-emitted row commitment, if another page exists.
    #[must_use]
    pub const fn next_row(self) -> Option<[u8; 32]> {
        match self {
            Self::Complete => None,
            Self::RowCreditReached(row) | Self::WireCreditReached(row) => Some(row),
        }
    }
}

/// Incremental ordered prefix commitment under the exact stable context.
/// A row digest must commit to its complete canonical projected JSON bytes.
#[derive(Clone)]
pub struct PackedPrefixCommitment {
    context: PackedPageContext,
    start: u64,
    emitted: u16,
    hash: blake3::Hasher,
}

impl PackedPrefixCommitment {
    /// Begins the exact half-open predecessor interval at `start`.
    #[must_use]
    pub fn new(context: PackedPageContext, start: u64) -> Self {
        let mut hash = blake3::Hasher::new();
        hash.update(b"packed-ordered-projected-prefix-v1\0");
        hash.update(&context.canonical_bytes());
        hash.update(&start.to_be_bytes());
        Self {
            context,
            start,
            emitted: 0,
            hash,
        }
    }

    /// Appends the next absolute ordinal and canonical projected-row digest.
    /// The digest uses a distinct row domain supplied by the shared serializer.
    pub fn push(&mut self, ordinal: u64, row: [u8; 32]) -> Result<(), PagePackingError> {
        let next = self
            .start
            .checked_add(u64::from(self.emitted))
            .ok_or(PagePackingError::Offset)?;
        if ordinal != next || self.emitted >= self.context.policy.rows().get() {
            return Err(PagePackingError::Interval);
        }
        self.hash.update(&ordinal.to_be_bytes());
        self.hash.update(&row);
        self.emitted += 1;
        Ok(())
    }

    /// Number of rows in this exact ordered interval.
    #[must_use]
    pub const fn emitted(&self) -> u16 {
        self.emitted
    }

    /// Current ordered range digest; computing it does not consume the state.
    #[must_use]
    pub fn commitment(&self) -> [u8; 32] {
        *self.hash.finalize().as_bytes()
    }

    /// Finalizes claims after complete per-response measurement.
    pub fn checkpoint(
        &self,
        stop: PackedPageStop,
        measured_bytes: u32,
    ) -> Result<PackedPageCheckpoint, PagePackingError> {
        let next = self
            .start
            .checked_add(u64::from(self.emitted))
            .ok_or(PagePackingError::Offset)?;
        if measured_bytes == 0 || measured_bytes > self.context.policy.bytes() {
            return Err(PagePackingError::ByteCredit);
        }
        if stop.next_row().is_some() && self.emitted == 0 {
            return Err(PagePackingError::ZeroProgress);
        }
        if matches!(stop, PackedPageStop::RowCreditReached(_))
            && self.emitted != self.context.policy.rows().get()
        {
            return Err(PagePackingError::Interval);
        }
        Ok(PackedPageCheckpoint {
            context: self.context,
            start: self.start,
            emitted: self.emitted,
            next,
            prefix: self.commitment(),
            stop,
            measured_bytes,
        })
    }
}

/// Authenticated packed checkpoint claims. A decoded token is still a claim
/// until the owner verifies its exact range and witness against the source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackedPageCheckpoint {
    context: PackedPageContext,
    start: u64,
    emitted: u16,
    next: u64,
    prefix: [u8; 32],
    stop: PackedPageStop,
    measured_bytes: u32,
}

impl PackedPageCheckpoint {
    /// Original source/query/policy bindings.
    #[must_use]
    pub const fn context(&self) -> PackedPageContext {
        self.context
    }
    /// Previous page's first absolute ordinal.
    #[must_use]
    pub const fn start(&self) -> u64 {
        self.start
    }
    /// Positive emitted count for a continuation; never the original limit.
    #[must_use]
    pub const fn emitted(&self) -> u16 {
        self.emitted
    }
    /// Exact first ordinal of the successor page.
    #[must_use]
    pub const fn next(&self) -> u64 {
        self.next
    }
    /// The separately authenticated stop reason and next-row witness.
    #[must_use]
    pub const fn stop(&self) -> PackedPageStop {
        self.stop
    }
    /// Measured bytes of the issuing response, not the next request's envelope.
    #[must_use]
    pub const fn measured_bytes(&self) -> u32 {
        self.measured_bytes
    }

    /// Rechecks the actual ordered predecessor returned by the exact source.
    /// Call after issuer authentication and bounded immutable-source reopen.
    /// Rehashing caller-supplied rows is not a substitute for that source read.
    pub fn verify_source_range(
        &self,
        actual: &PackedPrefixCommitment,
        actual_next: Option<[u8; 32]>,
    ) -> Result<(), PagePackingError> {
        if actual.context != self.context
            || actual.start != self.start
            || actual.emitted != self.emitted
            || actual.commitment() != self.prefix
            || actual_next != self.stop.next_row()
        {
            return Err(PagePackingError::SourceRange);
        }
        Ok(())
    }

    /// Fixed-width canonical claims authenticated in their entirety.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(CLAIM_BYTES);
        bytes.extend_from_slice(&PACKED_CHECKPOINT_SCHEMA.to_be_bytes());
        bytes.extend_from_slice(&self.context.canonical_bytes());
        bytes.extend_from_slice(&self.start.to_be_bytes());
        bytes.extend_from_slice(&self.emitted.to_be_bytes());
        bytes.extend_from_slice(&self.next.to_be_bytes());
        bytes.extend_from_slice(&self.prefix);
        let (kind, next_row) = match self.stop {
            PackedPageStop::Complete => (0, [0; 32]),
            PackedPageStop::RowCreditReached(row) => (1, row),
            PackedPageStop::WireCreditReached(row) => (2, row),
        };
        bytes.push(kind);
        bytes.extend_from_slice(&next_row);
        bytes.extend_from_slice(&self.measured_bytes.to_be_bytes());
        bytes
    }

    /// Encodes actual claims with a fixed-width producer authenticator.
    /// Signing authority remains with the caller; this helper does not sign.
    #[must_use]
    pub fn encode_token(&self, authenticate: impl FnOnce(&[u8]) -> [u8; AUTH_BYTES]) -> String {
        let claims = self.canonical_bytes();
        let auth = authenticate(&claims);
        let mut text = String::with_capacity(PACKED_CHECKPOINT_TOKEN_BYTES);
        text.push_str("pp1-");
        append_hex(&mut text, &claims);
        text.push('-');
        append_hex(&mut text, &auth);
        text
    }

    /// Exact-size placeholder for measuring a prospective token. The pp1
    /// alphabet and fixed widths prove its encoded/escaped size equals every
    /// real token; no compressed proof or guessed reserve is involved.
    #[must_use]
    pub fn cost_token() -> String {
        let mut token = String::with_capacity(PACKED_CHECKPOINT_TOKEN_BYTES);
        token.push_str("pp1-");
        token.extend(std::iter::repeat_n('0', CLAIM_BYTES * 2));
        token.push('-');
        token.extend(std::iter::repeat_n('0', AUTH_BYTES * 2));
        token
    }

    /// Admits bounded syntax and authenticates all claims before interpreting
    /// them. Caller-owned typed context supplies all admitted IDs. Ordinary
    /// per-call request IDs, metadata and expiry need not match a predecessor.
    pub fn decode_token_against(
        token: &str,
        context: PackedPageContext,
        verify: impl FnOnce(&[u8], &[u8; AUTH_BYTES]) -> bool,
    ) -> Result<Self, PagePackingError> {
        let (claims, auth) = token_parts(token)?;
        if !verify(&claims, &auth) {
            return Err(PagePackingError::Authentication);
        }
        if claims[..2] != PACKED_CHECKPOINT_SCHEMA.to_be_bytes()
            || claims[2..2 + CONTEXT_BYTES] != context.canonical_bytes()
        {
            return Err(PagePackingError::Context);
        }
        let fields = checkpoint_fields(&claims, context.policy)?;
        Ok(Self {
            context,
            start: fields.start,
            emitted: fields.emitted,
            next: fields.next,
            prefix: fields.prefix,
            stop: fields.stop,
            measured_bytes: fields.measured_bytes,
        })
    }

    /// Checks only canonical syntax and returns redundant receipt metadata.
    /// No authenticator is verified and no imported identity or checkpoint is
    /// admitted. Continuations still require `decode_token_against` followed
    /// by the owner's exact immutable-source range and witness verification.
    pub(crate) fn receipt_metadata(token: &str) -> Result<(u16, bool, u32), PagePackingError> {
        let (claims, _) = token_parts(token)?;
        if claims[..2] != PACKED_CHECKPOINT_SCHEMA.to_be_bytes() {
            return Err(PagePackingError::Token);
        }
        let context = &claims[2..2 + CONTEXT_BYTES];
        let owner = &context[..crate::CURSOR_CONTROL_BYTES];
        let root = Cursor::control_root_claim(owner).map_err(|_| PagePackingError::Context)?;
        // The revision repeats the owner's root. Compare bytes without
        // constructing a digest-only typed identity from the imported claim.
        if root != context[crate::CURSOR_CONTROL_BYTES..crate::CURSOR_CONTROL_BYTES + 32] {
            return Err(PagePackingError::Context);
        }
        let policy = &context[CONTEXT_BYTES - POLICY_BYTES..];
        if policy[..2] != PACKED_CHECKPOINT_SCHEMA.to_be_bytes() {
            return Err(PagePackingError::Token);
        }
        let rows = QueryLimit::new(u16::from_be_bytes([policy[2], policy[3]]))
            .ok_or(PagePackingError::Interval)?;
        let bytes = u32::from_be_bytes([policy[4], policy[5], policy[6], policy[7]]);
        let detail = match policy[8] {
            0 => PagePackingDetail::Summary,
            1 => PagePackingDetail::Standard,
            2 => PagePackingDetail::Full,
            _ => return Err(PagePackingError::Token),
        };
        let profile = match policy[9] {
            0 => PagePackingProfile::CliJsonV1,
            1 => PagePackingProfile::McpToolV1,
            _ => return Err(PagePackingError::Token),
        };
        let scope = match policy[10] {
            0 => PagePackingScope::CliJsonBody,
            1 => PagePackingScope::StdioJsonRpcLine,
            2 => PagePackingScope::HttpJsonRpcBody,
            _ => return Err(PagePackingError::Token),
        };
        let policy = PagePackingPolicy::new(rows, bytes, detail, profile, scope)?;
        let fields = checkpoint_fields(&claims, policy)?;
        Ok((
            fields.emitted,
            fields.stop.next_row().is_some(),
            fields.measured_bytes,
        ))
    }
}

fn token_parts(token: &str) -> Result<(Vec<u8>, [u8; AUTH_BYTES]), PagePackingError> {
    if token.len() != PACKED_CHECKPOINT_TOKEN_BYTES || !token.starts_with("pp1-") {
        return Err(PagePackingError::Token);
    }
    let body = &token.as_bytes()[4..];
    if body[CLAIM_BYTES * 2] != b'-' {
        return Err(PagePackingError::Token);
    }
    let claims = decode_hex(&body[..CLAIM_BYTES * 2])?;
    let auth = decode_hex(&body[CLAIM_BYTES * 2 + 1..])?
        .try_into()
        .map_err(|_| PagePackingError::Token)?;
    Ok((claims, auth))
}

struct CheckpointFields {
    start: u64,
    emitted: u16,
    next: u64,
    prefix: [u8; 32],
    stop: PackedPageStop,
    measured_bytes: u32,
}

fn checkpoint_fields(
    claims: &[u8],
    policy: PagePackingPolicy,
) -> Result<CheckpointFields, PagePackingError> {
    let mut at = 2 + CONTEXT_BYTES;
    let start = u64::from_be_bytes(take(claims, &mut at)?);
    let emitted = u16::from_be_bytes(take(claims, &mut at)?);
    let next = u64::from_be_bytes(take(claims, &mut at)?);
    let prefix = take(claims, &mut at)?;
    let kind = *claims.get(at).ok_or(PagePackingError::Token)?;
    at += 1;
    let row = take(claims, &mut at)?;
    let stop = match kind {
        0 if row == [0; 32] => PackedPageStop::Complete,
        1 => PackedPageStop::RowCreditReached(row),
        2 => PackedPageStop::WireCreditReached(row),
        _ => return Err(PagePackingError::Token),
    };
    let measured_bytes = u32::from_be_bytes(take(claims, &mut at)?);
    if start.checked_add(u64::from(emitted)) != Some(next)
        || emitted > policy.rows().get()
        || (stop.next_row().is_some() && emitted == 0)
        || (matches!(stop, PackedPageStop::RowCreditReached(_)) && emitted != policy.rows().get())
        || measured_bytes == 0
        || measured_bytes > policy.bytes()
    {
        return Err(PagePackingError::Interval);
    }
    Ok(CheckpointFields {
        start,
        emitted,
        next,
        prefix,
        stop,
        measured_bytes,
    })
}

fn take<const N: usize>(bytes: &[u8], at: &mut usize) -> Result<[u8; N], PagePackingError> {
    let end = at.checked_add(N).ok_or(PagePackingError::Token)?;
    let value = bytes
        .get(*at..end)
        .ok_or(PagePackingError::Token)?
        .try_into()
        .map_err(|_| PagePackingError::Token)?;
    *at = end;
    Ok(value)
}

fn append_hex(text: &mut String, bytes: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for &byte in bytes {
        text.push(char::from(HEX[usize::from(byte >> 4)]));
        text.push(char::from(HEX[usize::from(byte & 15)]));
    }
}

fn decode_hex(text: &[u8]) -> Result<Vec<u8>, PagePackingError> {
    text.chunks_exact(2)
        .map(|pair| {
            let digit = |byte| match byte {
                b'0'..=b'9' => Ok(byte - b'0'),
                b'a'..=b'f' => Ok(byte - b'a' + 10),
                _ => Err(PagePackingError::Token),
            };
            Ok(digit(pair[0])? * 16 + digit(pair[1])?)
        })
        .collect()
}

/// Closed policy, checkpoint and range-verification failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PagePackingError {
    /// Zero or enlarged byte credit.
    ByteCredit,
    /// The serializer profile does not belong to the selected output scope.
    ProfileScope,
    /// Foreign owner, source, query or stable policy.
    Context,
    /// Ordinal arithmetic overflow.
    Offset,
    /// Invalid start/count/next or row-credit relationship.
    Interval,
    /// A continued page emitted no rows.
    ZeroProgress,
    /// Exact immutable-source predecessor or lookahead did not match.
    SourceRange,
    /// Noncanonical, oversized, or malformed token.
    Token,
    /// The producer authenticator refused the complete claims.
    Authentication,
}

impl std::fmt::Display for PagePackingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::ByteCredit => "packed page byte credit is outside its bound",
            Self::ProfileScope => "packed page profile does not match its transport scope",
            Self::Context => "packed page does not match its owner, source, query or policy",
            Self::Offset => "packed page ordinal overflow",
            Self::Interval => "packed page does not prove its exact emitted interval",
            Self::ZeroProgress => "packed continuation cannot make zero progress",
            Self::SourceRange => "packed predecessor or next-row witness does not match the source",
            Self::Token => "packed checkpoint token is not canonical bounded pp1",
            Self::Authentication => "packed checkpoint producer authentication failed",
        })
    }
}

impl std::error::Error for PagePackingError {}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    fn context() -> PackedPageContext {
        let owner = Cursor::new();
        let policy = PagePackingPolicy::new(
            QueryLimit::new(200).expect("credit"),
            MAX_PACKED_WIRE_BYTES,
            PagePackingDetail::Full,
            PagePackingProfile::McpToolV1,
            PagePackingScope::StdioJsonRpcLine,
        )
        .expect("policy");
        PackedPageContext::new(
            owner,
            owner.root().into(),
            crate::view_key(b"query+manifest"),
            policy,
        )
        .expect("context")
    }

    fn auth(bytes: &[u8]) -> [u8; 32] {
        *blake3::keyed_hash(&[9; 32], bytes).as_bytes()
    }

    #[test]
    fn short_packed_interval_round_trips_with_actual_fixed_width_token() {
        let mut prefix = PackedPrefixCommitment::new(context(), 17);
        prefix.push(17, [1; 32]).expect("row one");
        prefix.push(18, [2; 32]).expect("row two");
        let checkpoint = prefix
            .checkpoint(PackedPageStop::WireCreditReached([3; 32]), 49152)
            .expect("short checkpoint");
        let token = checkpoint.encode_token(auth);
        assert_eq!(token.len(), PACKED_CHECKPOINT_TOKEN_BYTES);
        assert_eq!(
            serde_json::to_string(&token).expect("JSON").len(),
            token.len() + 2
        );
        assert_eq!(token.len(), PackedPageCheckpoint::cost_token().len());
        let decoded =
            PackedPageCheckpoint::decode_token_against(&token, context(), |bytes, signature| {
                auth(bytes) == *signature
            })
            .expect("cold claims");
        assert_eq!(decoded, checkpoint);
        assert_eq!(decoded.next(), 19);
        decoded
            .verify_source_range(&prefix, Some([3; 32]))
            .expect("exact source range");
        assert!(decoded.verify_source_range(&prefix, Some([4; 32])).is_err());
    }

    #[test]
    fn every_claim_byte_is_authenticated_before_source_work() {
        let mut prefix = PackedPrefixCommitment::new(context(), 0);
        prefix.push(0, [1; 32]).expect("row");
        let checkpoint = prefix
            .checkpoint(PackedPageStop::WireCreditReached([2; 32]), 12000)
            .expect("checkpoint");
        let token = checkpoint.encode_token(auth);
        for at in 4..4 + CLAIM_BYTES * 2 {
            let mut changed = token.as_bytes().to_vec();
            changed[at] = if changed[at] == b'0' { b'1' } else { b'0' };
            let changed = String::from_utf8(changed).expect("ASCII");
            assert_eq!(
                PackedPageCheckpoint::decode_token_against(
                    &changed,
                    context(),
                    |bytes, signature| auth(bytes) == *signature
                ),
                Err(PagePackingError::Authentication)
            );
        }
    }

    #[test]
    fn rejects_zero_progress_wrong_order_overflow_and_false_row_credit() {
        let mut prefix = PackedPrefixCommitment::new(context(), u64::MAX);
        assert!(
            prefix
                .checkpoint(PackedPageStop::WireCreditReached([2; 32]), 1)
                .is_err()
        );
        assert!(prefix.push(0, [1; 32]).is_err());
        prefix
            .push(u64::MAX, [1; 32])
            .expect("last representable ordinal");
        assert_eq!(
            prefix.checkpoint(PackedPageStop::Complete, 1),
            Err(PagePackingError::Offset)
        );
        let mut prefix = PackedPrefixCommitment::new(context(), 0);
        prefix.push(0, [1; 32]).expect("row");
        assert!(
            prefix
                .checkpoint(PackedPageStop::RowCreditReached([2; 32]), 1)
                .is_err()
        );
    }

    #[test]
    fn source_verification_binds_order_and_exact_start() {
        let mut actual = PackedPrefixCommitment::new(context(), 2);
        actual.push(2, [1; 32]).expect("one");
        actual.push(3, [2; 32]).expect("two");
        let checkpoint = actual
            .checkpoint(PackedPageStop::Complete, 100)
            .expect("complete");
        let mut reordered = PackedPrefixCommitment::new(context(), 2);
        reordered.push(2, [2; 32]).expect("two first");
        reordered.push(3, [1; 32]).expect("one last");
        assert!(checkpoint.verify_source_range(&reordered, None).is_err());
        let mut shifted = PackedPrefixCommitment::new(context(), 3);
        shifted.push(3, [1; 32]).expect("shift one");
        shifted.push(4, [2; 32]).expect("shift two");
        assert!(checkpoint.verify_source_range(&shifted, None).is_err());
    }
}

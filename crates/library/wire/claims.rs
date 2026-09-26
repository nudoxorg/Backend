//! Versioned, strict transport DTOs shared by CLI, MCP, and desktop.
//!
//! Accepted backend IDs intentionally do not implement wire deserialization
//! in this crate. A wire payload contains fixed-width hexadecimal text;
//! lowering keeps it as `WireId` until a caller-owned expected value (or a
//! canonical preimage at a producer boundary) admits it before constructing
//! an in-process command/reply.

use serde::{Deserialize, Serialize};

mod admit;

/// Schema marker carried by a producer certificate.  The marker is part of
/// the certificate grammar so a preimage for one identity class cannot be
/// silently reused for another class.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireSchema {
    /// Actor object key.
    Actor,
    /// Package object key.
    Package,
    /// Symbol object key.
    Symbol,
    /// Immutable object version.
    Object,
    /// Stable view recipe key.
    ViewRecipe,
    /// Derived view version.
    ViewVersion,
    /// Branch object key.
    Branch,
    /// Append-only log object key.
    Log,
    /// Document object version.
    Document,
    /// Name query version.
    Name,
    /// Outline query version.
    Outline,
    /// Visible view relation root/delta.
    ViewRelation,
}

/// One producer-supplied canonical identity preimage.
///
/// A fixed-width digest is only a wire claim.  These records are emitted by
/// the owner that has the logical value or checked relation transition and are
/// independently rehashed by a receiving process before a typed ID is built.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", deny_unknown_fields)]
#[serde(rename_all = "snake_case")]
pub enum WireClaim {
    /// Canonical text for an object key.
    Key {
        /// Schema of the key.
        schema: WireSchema,
        /// Fixed-width digest claim.
        id: String,
        /// Canonical logical key text.
        value: String,
    },
    /// Canonical bytes for an object key whose producer preimage is not UTF-8.
    ///
    /// View recipe identities commonly bind source roots and manifests, so
    /// their complete preimage is arbitrary bytes. Keeping this separate from
    /// [`WireClaim::Key`] preserves the text contract for package/symbol keys
    /// while allowing standalone clients to verify binary recipe keys.
    KeyBytes {
        /// Schema of the key.
        schema: WireSchema,
        /// Fixed-width digest claim.
        id: String,
        /// Canonical key preimage bytes.
        value: Box<[u8]>,
    },
    /// A logical key asserted by an authenticated producer when its canonical
    /// preimage is not part of the projected row payload.
    KeyCommitment {
        /// Schema of the key commitment.
        schema: WireSchema,
        /// Fixed-width digest claim.
        id: String,
    },
    /// Canonical preimage for one visible row identity.
    ///
    /// Row identities are a separate claim class because a row's display
    /// label is allowed to be a projection.  In particular, occurrence
    /// disambiguation for overlapping language tags produces a symbol digest
    /// that cannot be recomputed from that label.  The receiver still hashes
    /// this preimage under the declared key schema before admitting it.
    RowIdentity {
        /// Schema of the row key.
        schema: WireSchema,
        /// Fixed-width row identity digest.
        id: String,
        /// Exact canonical row identity preimage.
        preimage: String,
    },
    /// Canonical bytes for an object version.
    Version {
        /// Schema of the value.
        schema: WireSchema,
        /// Fixed-width digest claim.
        id: String,
        /// Canonical value bytes.
        value: Box<[u8]>,
    },
    /// Canonical relation-node bytes for a state-root claim.
    Root {
        /// Relation schema of the root.
        schema: WireSchema,
        /// Fixed-width digest claim.
        id: String,
        /// Canonical relation-node bytes.
        canonical: Box<[u8]>,
    },
    /// A deferred relation-root commitment used by paged snapshot resets.
    ///
    /// Unlike [`WireClaim::Root`], this claim carries no complete canonical
    /// node. The receiver recomputes the root after admitting every bounded
    /// page and compares the resulting typed root with this commitment. This
    /// keeps reset metadata and each page bounded by page size rather than
    /// duplicating an entire relation in one certificate.
    RootCommitment {
        /// Relation schema of the commitment.
        schema: WireSchema,
        /// Fixed-width digest claim, checked against the final typed root.
        id: String,
    },
    /// Intent operation token and complete payload.
    Intent {
        /// Fixed-width intent identity claim.
        id: String,
        /// Stable operation token.
        token: String,
        /// Complete canonical operation payload.
        payload: Box<[u8]>,
    },
    /// Exact relation transition preimage.
    Delta {
        /// Relation schema of the transition.
        schema: WireSchema,
        /// Fixed-width transition identity claim.
        id: String,
        /// Base relation root claim.
        base: String,
        /// Target relation root claim.
        target: String,
        /// Canonical ordered change bytes.
        changes: Box<[u8]>,
    },
    /// Exact owner cursor position paired with a health or status root.
    ///
    /// The identity fields are repeated deliberately: a signed certificate
    /// must bind the monotone sequence to the same recipe, version, stream,
    /// schema, and visible root that appear in the enclosing cursor.
    Cursor {
        /// View recipe identity.
        recipe: String,
        /// Immutable view version identity.
        version: String,
        /// Branch identity.
        branch: String,
        /// Log identity.
        log: String,
        /// Cursor schema.
        schema: u16,
        /// Visible relation root.
        root: String,
        /// Monotone owner position.
        sequence: u64,
    },
    /// Producer authority evidence for one complete source object scope.
    ///
    /// Both scope claims are checked against the enclosing basis object and
    /// linked with the required producer observation through the backend
    /// coverage admission API. Carrying the complete producer, context, and
    /// evidence tuple prevents an epoch or evidence replay from crossing the
    /// authority boundary.
    Coverage {
        /// Object version whose complete scope was declared.
        scope: String,
        /// Object version observed completely by the producer.
        observed: String,
        /// Stable producer identity admitted for this observation.
        producer: String,
        /// Session/epoch/source context admitted for this observation.
        context: String,
        /// Bounded evidence checked by the producer verifier.
        evidence: Box<[u8]>,
    },
}

/// Producer-authorized identity certificate attached to a DTO envelope.
///
/// The certificate is deliberately a list instead of a digest-to-value map:
/// every claim names its identity class, and admission rejects missing,
/// duplicate, or mismatched claims.  This makes standalone CLI, MCP, and
/// desktop processes able to verify successful replies without having an
/// out-of-band typed snapshot supplied by their caller.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireCertificate {
    /// Canonical identity claims covering every identity-bearing field in the
    /// enclosing command, reply, view, or event.
    pub claims: Box<[WireClaim]>,
}

impl WireCertificate {
    /// Creates an empty certificate.  It is useful for identity-free error
    /// envelopes; identity-bearing payloads reject an incomplete certificate.
    #[must_use]
    pub fn new() -> Self {
        Self {
            claims: Box::new([]),
        }
    }

    /// Returns a certificate with one additional producer claim.
    #[must_use]
    pub fn with_claim(mut self, claim: WireClaim) -> Self {
        let mut claims = self.claims.into_vec();
        claims.push(claim);
        self.claims = claims.into_boxed_slice();
        self
    }

    /// Returns a certificate containing this exact claim once.
    ///
    /// Composition layers use this when extending a producer certificate
    /// that may already cover the requested row. A conflicting claim with the
    /// same identity remains distinct and is rejected during admission.
    #[must_use]
    pub fn with_claim_once(self, claim: WireClaim) -> Self {
        if self.claims.iter().any(|existing| existing == &claim) {
            self
        } else {
            self.with_claim(claim)
        }
    }
}

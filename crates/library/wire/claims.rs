//! Versioned, strict transport DTOs shared by CLI, MCP, and desktop.
//!
//! Accepted backend IDs intentionally do not implement wire deserialization
//! in this crate. A wire payload contains fixed-width hexadecimal text;
//! lowering keeps it as `WireId` until a caller-owned expected value (or a
//! canonical preimage at a producer boundary) admits it before constructing
//! an in-process command/reply.

use crate::CoverageCapability;
use crate::canonical::{
    ObjectSchema, admit_delta_transition, admit_intent_value, admit_key_value, admit_root_bytes,
    admit_version_against, admit_version_value, encode_id,
};
use backend_version::{
    AuthorityScopeClaim, DeltaId, ObjectKey, ObjectVersion, ProducerObservationVerifier, Relation,
    Schema, ScopeRoot, StateRoot, UntrustedProducerObservation, admit_complete_scope,
    admit_producer_observation,
};
use serde::{Deserialize, Serialize};

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

    pub(crate) fn key_value<T: Schema<Value = str>>(
        &self,
        schema: WireSchema,
        id: &str,
    ) -> Result<ObjectKey<T>, String> {
        let value = self.unique_key(schema, id)?;
        admit_key_value::<T>(id, value).map_err(|error| error.to_string())
    }

    pub(crate) fn key_bytes<T: Schema<Value = [u8]>>(
        &self,
        schema: WireSchema,
        id: &str,
    ) -> Result<ObjectKey<T>, String> {
        let value = self.unique_key_bytes(schema, id)?;
        admit_key_value::<T>(id, value).map_err(|error| error.to_string())
    }

    pub(crate) fn producer_key_value<T: Schema>(
        &self,
        schema: WireSchema,
        id: &str,
        capability: &CoverageCapability,
    ) -> Result<ObjectKey<T>, String> {
        self.key_commitment(schema, id)?;
        let claim = crate::canonical::wire_key::<T>(id).map_err(|error| error.to_string())?;
        capability
            .admit_key_claim(claim.into_untrusted())
            .map_err(|error| error.to_string())
    }

    /// Requires one exact producer key commitment without promoting its
    /// digest to an object key. Query selectors use this before retaining
    /// fixed-width bytes that an owner must resolve against a pinned view.
    pub(crate) fn key_commitment(&self, schema: WireSchema, id: &str) -> Result<(), String> {
        let mut observed = false;
        for claim in &self.claims {
            let WireClaim::KeyCommitment {
                schema: actual,
                id: actual_id,
            } = claim
            else {
                continue;
            };
            if *actual == schema && actual_id == id {
                if observed {
                    return Err("duplicate producer key commitment".to_owned());
                }
                observed = true;
            }
        }
        if !observed {
            return Err("missing producer key commitment".to_owned());
        }
        Ok(())
    }

    pub(crate) fn version_value<T: Schema<Value = [u8]>>(
        &self,
        schema: WireSchema,
        id: &str,
    ) -> Result<ObjectVersion<T>, String> {
        let value = self.unique_version(schema, id)?;
        admit_version_value::<T>(id, value).map_err(|error| error.to_string())
    }

    pub(crate) fn root_value<R: Relation>(
        &self,
        schema: WireSchema,
        id: &str,
    ) -> Result<StateRoot<R>, String> {
        let canonical = self.unique_root(schema, id)?;
        admit_root_bytes::<R>(id, canonical).map_err(|error| error.to_string())
    }

    /// Requires one exact deferred relation-root commitment claim.
    pub(crate) fn root_commitment(&self, schema: WireSchema, id: &str) -> Result<(), String> {
        let mut observed = false;
        for claim in &self.claims {
            let WireClaim::RootCommitment {
                schema: actual,
                id: actual_id,
            } = claim
            else {
                continue;
            };
            if *actual != schema || actual_id != id {
                continue;
            }
            if observed {
                return Err("duplicate deferred root commitment claim".to_owned());
            }
            crate::canonical::decode_id(actual_id).map_err(|error| error.to_string())?;
            observed = true;
        }
        if observed {
            Ok(())
        } else {
            Err("missing deferred root commitment claim".to_owned())
        }
    }

    /// Returns the fixed-width bytes of one deferred relation-root claim
    /// without promoting them to an accepted [`StateRoot`].
    ///
    /// A paged reset cannot construct its visible root until every page has
    /// been rebuilt.  The typed wire context check still happens here; the
    /// final digest admission belongs to `ViewRootDescriptorClaim::admit_rows`.
    pub(crate) fn root_commitment_bytes<R: Relation>(
        &self,
        schema: WireSchema,
        id: &str,
    ) -> Result<[u8; backend_version::ID_BYTES], String> {
        self.root_commitment(schema, id)?;
        let claim = crate::canonical::wire_root::<R>(id).map_err(|error| error.to_string())?;
        Ok(*claim.as_bytes())
    }

    pub(crate) fn producer_root_value<R: Relation>(
        &self,
        schema: WireSchema,
        id: &str,
        capability: &CoverageCapability,
    ) -> Result<StateRoot<R>, String> {
        self.root_commitment(schema, id)?;
        let claim = crate::canonical::wire_root::<R>(id).map_err(|error| error.to_string())?;
        capability
            .admit_root_claim(claim.into_untrusted())
            .map_err(|error| error.to_string())
    }

    pub(crate) fn intent_value(&self, id: &str) -> Result<crate::IntentId, String> {
        let (token, payload) = self.unique_intent(id)?;
        admit_intent_value(id, token, payload).map_err(|error| error.to_string())
    }

    pub(crate) fn delta_value<R: Relation>(
        &self,
        schema: WireSchema,
        id: &str,
        base: StateRoot<R>,
        target: StateRoot<R>,
    ) -> Result<DeltaId<R>, String> {
        let (claimed_base, claimed_target, changes) = self.unique_delta(schema, id)?;
        if claimed_base != encode_id(base.as_bytes())
            || claimed_target != encode_id(target.as_bytes())
        {
            return Err("identity certificate transition roots do not match".to_owned());
        }
        admit_delta_transition::<R>(id, base, target, changes).map_err(|error| error.to_string())
    }

    /// Checks the producer's exact owner cursor claim against a typed cursor.
    ///
    /// A health/status envelope may carry a cursor whose sequence is ahead of
    /// the visible root frontier because intent-only events do not change
    /// rows. The cursor claim binds that sequence to the producer certificate
    /// instead of allowing a receiver to accept a self-asserted number.
    pub(crate) fn cursor_claim(&self, expected: crate::Cursor) -> Result<(), String> {
        if expected.schema() != crate::CURSOR_SCHEMA {
            return Err("identity certificate cursor uses an unsupported schema".to_owned());
        }
        if expected.query_offset() != 0 {
            return Err("identity certificate cursor carries a query offset".to_owned());
        }
        let mut observed = None;
        for claim in &self.claims {
            let WireClaim::Cursor {
                recipe,
                version,
                branch,
                log,
                schema,
                root,
                sequence,
            } = claim
            else {
                continue;
            };
            if observed.is_some() {
                return Err("duplicate identity certificate cursor claim".to_owned());
            }
            if recipe != &encode_id(expected.recipe().as_bytes())
                || version != &encode_id(expected.version().as_bytes())
                || branch != &encode_id(expected.branch().as_bytes())
                || log != &encode_id(expected.log().as_bytes())
                || *schema != expected.schema()
                || root != &encode_id(expected.root().as_bytes())
                || *sequence != expected.sequence()
            {
                return Err("identity certificate cursor claim does not match".to_owned());
            }
            observed = Some(());
        }
        observed.ok_or_else(|| "missing identity certificate cursor claim".to_owned())
    }

    /// Checks producer-owned complete source coverage against an externally
    /// admitted capability. A wire certificate can prove canonical preimages,
    /// but cannot mint authority by choosing its own producer identity.
    pub(crate) fn admit_coverage_capability(
        &self,
        object_id: &str,
        capability: &CoverageCapability,
    ) -> Result<(), String> {
        let object = self.version_value::<ObjectSchema>(WireSchema::Object, object_id)?;
        let (scope, observed, producer, context, evidence) = self.unique_coverage()?;
        admit_version_against::<ObjectSchema>(scope, object).map_err(|error| error.to_string())?;
        admit_version_against::<ObjectSchema>(observed, object)
            .map_err(|error| error.to_string())?;
        let producer = crate::canonical::decode_id(producer).map_err(|error| error.to_string())?;
        let context = crate::canonical::decode_id(context).map_err(|error| error.to_string())?;
        if evidence.len() > crate::MAX_COVERAGE_EVIDENCE {
            return Err("coverage evidence exceeds the bounded certificate limit".to_owned());
        }
        let evidence_digest = *blake3::hash(evidence).as_bytes();
        if capability.scope_root() != ScopeRoot::from_bytes(object.to_bytes())
            || capability.producer_identity() != producer
            || capability.context() != context
            || capability.evidence_digest() != evidence_digest
        {
            return Err(
                "certificate producer observation does not match the admitted capability"
                    .to_owned(),
            );
        }
        Ok(())
    }

    /// Admits the complete coverage observation carried by this certificate
    /// through an owner supplied verifier. The certificate contributes the
    /// exact scope, producer, context, and evidence, but it cannot mint the
    /// opaque capability without the verifier's authority decision.
    pub(crate) fn admit_coverage_with_verifier<V: ProducerObservationVerifier>(
        &self,
        object_id: &str,
        verifier: &V,
    ) -> Result<CoverageCapability, String> {
        let object = self.version_value::<ObjectSchema>(WireSchema::Object, object_id)?;
        let (scope, observed, producer, context, evidence) = self.unique_coverage()?;
        admit_version_against::<ObjectSchema>(scope, object).map_err(|error| error.to_string())?;
        admit_version_against::<ObjectSchema>(observed, object)
            .map_err(|error| error.to_string())?;
        let producer = crate::canonical::decode_id(producer).map_err(|error| error.to_string())?;
        let context = crate::canonical::decode_id(context).map_err(|error| error.to_string())?;
        if evidence.len() > crate::MAX_COVERAGE_EVIDENCE {
            return Err("coverage evidence exceeds the bounded certificate limit".to_owned());
        }
        let observation = UntrustedProducerObservation::new(
            producer,
            ScopeRoot::from_bytes(object.to_bytes()),
            context,
            evidence.to_vec(),
        );
        let admitted = admit_producer_observation(observation, verifier)
            .map_err(|_| "producer observation rejected".to_owned())?;
        let authorized =
            admit_complete_scope(AuthorityScopeClaim::from_object_version(object), admitted)
                .map_err(|error| error.to_string())?;
        CoverageCapability::from_authorized_with_evidence(authorized, evidence.to_vec())
    }

    fn unique_key(&self, schema: WireSchema, id: &str) -> Result<&str, String> {
        let mut result = None;
        for claim in &self.claims {
            if let WireClaim::Key {
                schema: actual,
                id: actual_id,
                value,
            } = claim
                && *actual == schema
                && actual_id == id
            {
                if result.is_some() {
                    return Err("duplicate identity certificate claim".to_owned());
                }
                result = Some(value.as_str());
            }
        }
        result.ok_or_else(|| format!("missing {schema:?} key certificate claim for {id}"))
    }

    fn unique_key_bytes(&self, schema: WireSchema, id: &str) -> Result<&[u8], String> {
        let mut result = None;
        for claim in &self.claims {
            let value = match claim {
                WireClaim::Key {
                    schema: actual,
                    id: actual_id,
                    value,
                } if *actual == schema && actual_id == id => value.as_bytes(),
                WireClaim::KeyBytes {
                    schema: actual,
                    id: actual_id,
                    value,
                } if *actual == schema && actual_id == id => value.as_ref(),
                _ => continue,
            };
            if result.is_some() {
                return Err("duplicate identity certificate claim".to_owned());
            }
            result = Some(value);
        }
        result.ok_or_else(|| format!("missing {schema:?} key-bytes certificate claim for {id}"))
    }

    fn unique_version(&self, schema: WireSchema, id: &str) -> Result<&[u8], String> {
        let mut result = None;
        for claim in &self.claims {
            if let WireClaim::Version {
                schema: actual,
                id: actual_id,
                value,
            } = claim
                && *actual == schema
                && actual_id == id
            {
                if result.is_some() {
                    return Err("duplicate identity certificate claim".to_owned());
                }
                result = Some(value.as_ref());
            }
        }
        result.ok_or_else(|| format!("missing {schema:?} version certificate claim for {id}"))
    }

    fn unique_root(&self, schema: WireSchema, id: &str) -> Result<&[u8], String> {
        let mut result = None;
        for claim in &self.claims {
            if let WireClaim::Root {
                schema: actual,
                id: actual_id,
                canonical,
            } = claim
                && *actual == schema
                && actual_id == id
            {
                if result.is_some() {
                    return Err("duplicate identity certificate claim".to_owned());
                }
                result = Some(canonical.as_ref());
            }
        }
        result.ok_or_else(|| format!("missing {schema:?} root certificate claim for {id}"))
    }

    fn unique_intent(&self, id: &str) -> Result<(&str, &[u8]), String> {
        let mut result = None;
        for claim in &self.claims {
            if let WireClaim::Intent {
                id: actual_id,
                token,
                payload,
            } = claim
                && actual_id == id
            {
                if result.is_some() {
                    return Err("duplicate identity certificate claim".to_owned());
                }
                result = Some((token.as_str(), payload.as_ref()));
            }
        }
        result.ok_or_else(|| "missing identity certificate claim".to_owned())
    }

    fn unique_delta(&self, schema: WireSchema, id: &str) -> Result<(&str, &str, &[u8]), String> {
        let mut result = None;
        for claim in &self.claims {
            if let WireClaim::Delta {
                schema: actual,
                id: actual_id,
                base,
                target,
                changes,
            } = claim
                && *actual == schema
                && actual_id == id
            {
                if result.is_some() {
                    return Err("duplicate identity certificate claim".to_owned());
                }
                result = Some((base.as_str(), target.as_str(), changes.as_ref()));
            }
        }
        result.ok_or_else(|| "missing identity certificate claim".to_owned())
    }

    fn unique_coverage(&self) -> Result<(&str, &str, &str, &str, &[u8]), String> {
        let mut result = None;
        for claim in &self.claims {
            let WireClaim::Coverage {
                scope,
                observed,
                producer,
                context,
                evidence,
            } = claim
            else {
                continue;
            };
            if result.is_some() {
                return Err("duplicate identity certificate coverage claim".to_owned());
            }
            result = Some((
                scope.as_str(),
                observed.as_str(),
                producer.as_str(),
                context.as_str(),
                evidence.as_ref(),
            ));
        }
        result.ok_or_else(|| "missing complete coverage certificate claim".to_owned())
    }
}

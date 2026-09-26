//! Admission of one producer certificate into typed identity.
//!
//! Construction stays in the parent. These methods prove a single claim class
//! before a command or reply may use the identity.

use super::{WireCertificate, WireClaim, WireSchema};
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

impl WireCertificate {
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

    /// Admits one explicit row identity preimage, if the producer supplied
    /// one for this row.  Missing claims remain distinguishable from malformed
    /// claims so ordinary rows can retain the existing commitment path.
    pub(crate) fn row_identity_preimage<'a>(
        &'a self,
        schema: WireSchema,
        id: &str,
    ) -> Result<Option<&'a str>, String> {
        let mut result = None;
        let mut ordinary_claim = false;
        for claim in &self.claims {
            match claim {
                WireClaim::RowIdentity {
                    schema: actual,
                    id: actual_id,
                    preimage,
                } if *actual == schema && actual_id == id => {
                    if result.is_some() {
                        return Err("duplicate row identity certificate claim".to_owned());
                    }
                    result = Some(preimage.as_str());
                }
                WireClaim::Key {
                    schema: actual,
                    id: actual_id,
                    ..
                } if *actual == schema && actual_id == id => {
                    ordinary_claim = true;
                }
                _ => {}
            }
        }
        let Some(preimage) = result else {
            return Ok(None);
        };
        if ordinary_claim {
            return Err("row identity certificate mixes explicit and ordinary claims".to_owned());
        }
        crate::RowIdentityPreimage::validate(preimage).map_err(|error| error.to_string())?;
        match schema {
            WireSchema::Package => {
                admit_key_value::<crate::canonical::PackageSchema>(id, &preimage)
                    .map_err(|error| error.to_string())?;
            }
            WireSchema::Symbol => {
                admit_key_value::<crate::canonical::SymbolSchema>(id, &preimage)
                    .map_err(|error| error.to_string())?;
            }
            _ => return Err("row identity certificate uses an unsupported schema".to_owned()),
        }
        Ok(Some(preimage))
    }

    /// Admits an explicit row identity when present and otherwise uses the
    /// ordinary canonical key claim.  A malformed explicit claim is returned
    /// immediately; it must never be hidden by a fallback commitment.
    pub(crate) fn row_identity_or_key_value<T: Schema<Value = str>>(
        &self,
        schema: WireSchema,
        id: &str,
    ) -> Result<ObjectKey<T>, String> {
        match self.row_identity_preimage(schema, id)? {
            Some(preimage) => {
                admit_key_value::<T>(id, &preimage).map_err(|error| error.to_string())
            }
            None => self.key_value::<T>(schema, id),
        }
    }

    /// Variant used by capability-backed row decoding.  Only a missing
    /// explicit preimage may proceed to the producer commitment fallback.
    pub(crate) fn row_identity_or_key_or_producer<T: Schema<Value = str>>(
        &self,
        schema: WireSchema,
        id: &str,
        capability: &CoverageCapability,
    ) -> Result<ObjectKey<T>, String> {
        match self.row_identity_preimage(schema, id)? {
            Some(preimage) => {
                admit_key_value::<T>(id, &preimage).map_err(|error| error.to_string())
            }
            None => self
                .key_value::<T>(schema, id)
                .or_else(|_| self.producer_key_value(schema, id, capability)),
        }
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

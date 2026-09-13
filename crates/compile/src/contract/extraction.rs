//! Extraction results and compatibility fact projections.

use super::{
    AuthorityFence, AuthorityIdentity, CompleteAuthorityCoverage, FactChange, FactEvidence,
    FactKeySchema, FactRecord, FactSet, FactSetError, FactSnapshot, FactSnapshotError,
    FactValueSchema, InputManifest, InputManifestId, SessionId,
};
use backend_version::CoverageWitness;
use std::{fmt, sync::Arc};

/// An immutable extraction result with explicit coverage and provenance.
///
/// Typed records use the same persistent normalized storage as
/// [`FactSnapshot`], so passing an extraction between layers does not copy its
/// fact set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Extraction {
    coverage: CoverageWitness,
    facts: Arc<[FactChange]>,
    set: Arc<FactSet<FactKeySchema, FactValueSchema>>,
    authority: Option<SessionId>,
    manifest: Option<InputManifestId>,
    revision: Option<u64>,
    complete: Option<Arc<AuthorityFence>>,
}

impl Extraction {
    /// Creates a sorted extraction result for a non-complete observation.
    ///
    /// # Errors
    ///
    /// Returns [`ExtractionError::UnboundComplete`] for complete coverage or
    /// [`ExtractionError::DuplicateFact`] when a key occurs more than once.
    pub fn new(coverage: CoverageWitness, facts: Vec<FactChange>) -> Result<Self, ExtractionError> {
        if coverage.state().is_complete() {
            return Err(ExtractionError::UnboundComplete);
        }
        Self::assemble(coverage, facts)
    }

    /// Creates an extraction bound to an authority and exact manifest root.
    ///
    /// # Errors
    ///
    /// Returns an [`ExtractionError`] when coverage is outside the manifest
    /// scope or the facts contain duplicate keys.
    pub fn bound(
        authority: AuthorityIdentity,
        manifest: &InputManifest,
        coverage: CoverageWitness,
        facts: Vec<FactChange>,
    ) -> Result<Self, ExtractionError> {
        Self::bound_at(authority, manifest, 0, coverage, facts)
    }

    /// Creates an extraction bound to authority, manifest, and discovery
    /// revision.
    ///
    /// Complete coverage must name exactly the bound manifest scope.  A
    /// partial or unavailable witness may retain a narrower authority scope,
    /// because it cannot authorize deletion outside that scope.
    ///
    /// # Errors
    ///
    /// Returns an [`ExtractionError`] when complete coverage does not name the
    /// manifest or the facts contain duplicate keys.
    pub fn bound_at(
        authority: AuthorityIdentity,
        manifest: &InputManifest,
        revision: u64,
        coverage: CoverageWitness,
        facts: Vec<FactChange>,
    ) -> Result<Self, ExtractionError> {
        if coverage.state().is_complete() {
            return Err(ExtractionError::UnboundComplete);
        }
        let mut output = Self::assemble(coverage, facts)?;
        output.authority = Some(authority.digest());
        output.manifest = Some(manifest.digest());
        output.revision = Some(revision);
        output.complete = None;
        Ok(output)
    }

    /// Creates a non-complete extraction from typed immutable records.
    ///
    /// # Errors
    ///
    /// Returns [`ExtractionError::UnboundComplete`] for complete coverage or
    /// another [`ExtractionError`] when records are duplicated.
    pub fn from_records(
        coverage: CoverageWitness,
        records: Vec<FactRecord<FactKeySchema, FactValueSchema>>,
    ) -> Result<Self, ExtractionError> {
        if coverage.state().is_complete() {
            return Err(ExtractionError::UnboundComplete);
        }
        Self::assemble_records(coverage, records)
    }

    /// Creates a manifest-bound extraction from typed immutable records.
    ///
    /// # Errors
    ///
    /// Returns an [`ExtractionError`] when coverage is outside the manifest
    /// scope, record evidence disagrees with the fence, or records duplicate a
    /// typed key.
    pub fn bound_records(
        authority: AuthorityIdentity,
        manifest: &InputManifest,
        revision: u64,
        coverage: CoverageWitness,
        records: Vec<FactRecord<FactKeySchema, FactValueSchema>>,
    ) -> Result<Self, ExtractionError> {
        if coverage.state().is_complete() {
            return Err(ExtractionError::UnboundComplete);
        }
        let evidence = FactEvidence::new(authority, manifest, revision);
        let records = records
            .into_iter()
            .map(|record| match record.evidence() {
                None => Ok(record.with_evidence(evidence)),
                Some(existing) if existing == evidence => Ok(record),
                Some(_) => Err(ExtractionError::EvidenceMismatch),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut output = Self::assemble_records(coverage, records)?;
        output.authority = Some(authority.digest());
        output.manifest = Some(manifest.digest());
        output.revision = Some(revision);
        output.complete = None;
        Ok(output)
    }

    fn assemble(
        coverage: CoverageWitness,
        mut facts: Vec<FactChange>,
    ) -> Result<Self, ExtractionError> {
        facts.sort_by(|left, right| left.key.cmp(&right.key));
        if facts
            .windows(2)
            .any(|window| window[0].key == window[1].key)
        {
            return Err(ExtractionError::DuplicateFact);
        }
        let set = FactSet::empty().map_err(|_| ExtractionError::Canonical)?;
        Ok(Self {
            coverage,
            facts: Arc::from(facts.into_boxed_slice()),
            set: Arc::new(set),
            authority: None,
            manifest: None,
            revision: None,
            complete: None,
        })
    }

    fn assemble_records(
        coverage: CoverageWitness,
        mut records: Vec<FactRecord<FactKeySchema, FactValueSchema>>,
    ) -> Result<Self, ExtractionError> {
        records.sort_by(|left, right| {
            (left.kind, left.key_bytes.as_slice()).cmp(&(right.kind, right.key_bytes.as_slice()))
        });
        if records
            .windows(2)
            .any(|window| window[0].kind == window[1].kind && window[0].key == window[1].key)
        {
            return Err(ExtractionError::DuplicateRecord);
        }
        let records = FactSet::from_records(records).map_err(|error| match error {
            FactSetError::Duplicate => ExtractionError::DuplicateRecord,
            FactSetError::Canonical => ExtractionError::Canonical,
        })?;
        Ok(Self {
            coverage,
            facts: Arc::from(Vec::new().into_boxed_slice()),
            set: Arc::new(records),
            authority: None,
            manifest: None,
            revision: None,
            complete: None,
        })
    }

    pub(crate) fn from_complete_capability(
        capability: CompleteAuthorityCoverage,
        records: Vec<FactRecord<FactKeySchema, FactValueSchema>>,
    ) -> Result<Self, ExtractionError> {
        let snapshot = FactSnapshot::from_complete_capability(capability, records).map_err(
            |error| match error {
                FactSnapshotError::UnboundComplete | FactSnapshotError::EmptyComplete => {
                    ExtractionError::EmptyComplete
                }
                FactSnapshotError::CoverageScopeMismatch | FactSnapshotError::EvidenceMismatch => {
                    ExtractionError::EvidenceMismatch
                }
                FactSnapshotError::DuplicateRecord => ExtractionError::DuplicateRecord,
                FactSnapshotError::Canonical => ExtractionError::Canonical,
            },
        )?;
        let (coverage, set, complete) = snapshot.into_complete_parts();
        let authority = complete.as_ref().map(|fence| fence.authority.digest());
        let manifest = complete.as_ref().map(|fence| fence.manifest);
        let revision = complete.as_ref().map(|fence| fence.revision);
        Ok(Self {
            coverage,
            facts: Arc::from(Vec::new().into_boxed_slice()),
            set,
            authority,
            manifest,
            revision,
            complete,
        })
    }

    /// Returns the coverage witness.
    #[must_use]
    pub const fn coverage(&self) -> &CoverageWitness {
        &self.coverage
    }

    /// Returns canonically ordered fact changes.
    #[must_use]
    pub fn facts(&self) -> &[FactChange] {
        &self.facts
    }

    /// Returns canonical typed fact records, when the extraction used the
    /// immutable record path.
    #[must_use]
    pub fn records(&self) -> &[FactRecord<FactKeySchema, FactValueSchema>] {
        self.set.ordered()
    }

    #[cfg(test)]
    pub(crate) fn shares_record_storage_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.set, &other.set)
    }

    /// Returns the authority root used to produce these facts, if bound.
    #[must_use]
    pub const fn authority_root(&self) -> Option<SessionId> {
        self.authority
    }

    /// Returns the input manifest root used to produce these facts, if bound.
    #[must_use]
    pub const fn manifest_root(&self) -> Option<InputManifestId> {
        self.manifest
    }

    /// Returns the discovery revision used to produce these facts, if bound.
    #[must_use]
    pub const fn revision(&self) -> Option<u64> {
        self.revision
    }
}

/// Extraction contract violations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExtractionError {
    /// Complete facts must carry an authority and manifest witness.
    UnboundComplete,
    /// A complete authority result contained no typed records.
    EmptyComplete,
    /// Two fact changes used the same stable fact identity.
    DuplicateFact,
    /// The coverage scope did not equal the bound manifest scope.
    CoverageScopeMismatch,
    /// Two typed records used the same semantic family and object key.
    DuplicateRecord,
    /// A record carried evidence for a different authority fence.
    EvidenceMismatch,
    /// The normalized relation exceeded the canonical kernel limits.
    Canonical,
}

impl fmt::Display for ExtractionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::UnboundComplete => "complete extraction must be bound to authority and manifest",
            Self::EmptyComplete => "complete extraction contains no typed records",
            Self::DuplicateFact => "extraction contains a duplicate fact",
            Self::CoverageScopeMismatch => "extraction coverage scope does not match its manifest",
            Self::DuplicateRecord => "extraction contains a duplicate typed record",
            Self::EvidenceMismatch => "typed fact evidence does not match extraction fence",
            Self::Canonical => "extraction exceeds canonical relation limits",
        };
        f.write_str(message)
    }
}

impl std::error::Error for ExtractionError {}

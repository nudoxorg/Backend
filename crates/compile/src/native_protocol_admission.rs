//! Typed admission boundary for native payload identity claims.

use super::{NativeCoverage, NativeProtocolError, NativeRecord, validate_key, validate_records};
use crate::{AuthorityIdentity, InputManifestId, SessionId, SessionKey};
use std::{fmt, marker::PhantomData};

/// A decoded native payload has untrusted identity claims.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Unbound;

/// A native payload whose identity claims were admitted against caller-owned versions.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Bound;

mod sealed {
    pub trait EnvelopeState {}
    impl EnvelopeState for super::Unbound {}
    impl EnvelopeState for super::Bound {}
}

/// Typestate identity representation carried by a native envelope.
#[allow(private_bounds)]
pub trait EnvelopeState: sealed::EnvelopeState {
    /// Session identity representation.
    type Session: Clone + Copy + fmt::Debug + Eq + PartialEq;
    /// Manifest identity representation.
    type Manifest: Clone + Copy + fmt::Debug + Eq + PartialEq;
    /// Authority identity representation.
    type Authority: Clone + Copy + fmt::Debug + Eq + PartialEq;
    /// Returns the session bytes for wire encoding.
    fn session_bytes(value: &Self::Session) -> [u8; 32];
    /// Returns the manifest bytes for wire encoding.
    fn manifest_bytes(value: &Self::Manifest) -> [u8; 32];
    /// Returns the authority bytes for wire encoding.
    fn authority_bytes(value: &Self::Authority) -> [u8; 32];
}

impl EnvelopeState for Unbound {
    type Session = [u8; 32];
    type Manifest = [u8; 32];
    type Authority = [u8; 32];

    fn session_bytes(value: &Self::Session) -> [u8; 32] {
        *value
    }
    fn manifest_bytes(value: &Self::Manifest) -> [u8; 32] {
        *value
    }
    fn authority_bytes(value: &Self::Authority) -> [u8; 32] {
        *value
    }
}

impl EnvelopeState for Bound {
    type Session = SessionId;
    type Manifest = InputManifestId;
    type Authority = SessionId;

    fn session_bytes(value: &Self::Session) -> [u8; 32] {
        value.to_bytes()
    }
    fn manifest_bytes(value: &Self::Manifest) -> [u8; 32] {
        value.to_bytes()
    }
    fn authority_bytes(value: &Self::Authority) -> [u8; 32] {
        value.to_bytes()
    }
}

/// A checked, canonical native authority payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeEnvelope<S: EnvelopeState = Unbound> {
    pub(super) language: String,
    pub(super) session: S::Session,
    pub(super) manifest: S::Manifest,
    pub(super) authority: S::Authority,
    pub(super) revision: u64,
    pub(super) coverage: NativeCoverage,
    pub(super) records: Vec<NativeRecord>,
    pub(super) state: PhantomData<fn() -> S>,
}

impl NativeEnvelope<Unbound> {
    /// Builds an unbound envelope from claims decoded from a helper.
    ///
    /// # Errors
    ///
    /// Returns [`NativeProtocolError`] when records are duplicated, exceed a
    /// protocol bound, or cannot be represented in the canonical wire format.
    pub fn unbound(
        language: impl Into<String>,
        session: [u8; 32],
        manifest: [u8; 32],
        authority: [u8; 32],
        revision: u64,
        coverage: NativeCoverage,
        mut records: Vec<NativeRecord>,
    ) -> Result<Self, NativeProtocolError> {
        let language = language.into();
        validate_key(&language)?;
        if records.len() > super::MAX_NATIVE_RECORDS {
            return Err(NativeProtocolError::RecordCountLimit {
                actual: records.len(),
                maximum: super::MAX_NATIVE_RECORDS,
            });
        }
        records.sort_by(|left, right| {
            (left.kind, left.key.as_bytes()).cmp(&(right.kind, right.key.as_bytes()))
        });
        validate_records(&records)?;
        let envelope = Self {
            language,
            session,
            manifest,
            authority,
            revision,
            coverage,
            records,
            state: PhantomData,
        };
        let _ = envelope.encoded_len()?;
        Ok(envelope)
    }

    /// Admits decoded identity claims against the exact request versions.
    ///
    /// # Errors
    ///
    /// Returns [`NativeProtocolError::BindingMismatch`] when any echoed
    /// language, session, manifest, authority, or revision differs.
    pub fn admit(
        self,
        key: SessionKey,
        authority: AuthorityIdentity,
        language: &str,
        revision: u64,
    ) -> Result<NativeEnvelope<Bound>, NativeProtocolError> {
        if self.language != language
            || !super::session::claims_match(
                &self.session,
                &self.manifest,
                &self.authority,
                key,
                authority,
            )
            || self.revision != revision
        {
            return Err(NativeProtocolError::BindingMismatch);
        }
        Ok(NativeEnvelope {
            language: self.language,
            session: key.digest(),
            manifest: key.manifest(),
            authority: authority.digest(),
            revision: self.revision,
            coverage: self.coverage,
            records: self.records,
            state: PhantomData,
        })
    }

    /// Builds an envelope bound to the exact authority request.
    ///
    /// # Errors
    ///
    /// Returns [`NativeProtocolError`] when the authority does not match the
    /// session key, fields are invalid, or the payload exceeds a bound.
    pub fn bound(
        language: impl Into<String>,
        key: SessionKey,
        authority: AuthorityIdentity,
        revision: u64,
        coverage: NativeCoverage,
        mut records: Vec<NativeRecord>,
    ) -> Result<NativeEnvelope<Bound>, NativeProtocolError> {
        let language = language.into();
        validate_key(&language)?;
        if key.authority() != authority.digest() {
            return Err(NativeProtocolError::BindingMismatch);
        }
        records.sort_by(|left, right| {
            (left.kind, left.key.as_bytes()).cmp(&(right.kind, right.key.as_bytes()))
        });
        let envelope = NativeEnvelope::<Bound> {
            language,
            session: key.digest(),
            manifest: key.manifest(),
            authority: authority.digest(),
            revision,
            coverage,
            records,
            state: PhantomData,
        };
        if envelope.records.len() > super::MAX_NATIVE_RECORDS {
            return Err(NativeProtocolError::RecordCountLimit {
                actual: envelope.records.len(),
                maximum: super::MAX_NATIVE_RECORDS,
            });
        }
        validate_records(&envelope.records)?;
        let _ = envelope.encoded_len()?;
        Ok(envelope)
    }
}

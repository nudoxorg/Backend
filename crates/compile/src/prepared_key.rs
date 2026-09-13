use crate::{
    AuthorityIdentity, InputManifestId, NativeProtocolError, NativeRequestInput, SessionId,
    SessionKey, ToolchainId,
};
use backend_version::{ObjectVersion, Schema};

use super::prepared_error::PreparationError;

/// The complete identity of one prepared native request.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PreparationKey {
    authority: SessionId,
    toolchain: ToolchainId,
    manifest: InputManifestId,
    session: SessionKey,
    revision: u64,
    language: PreparationDigest,
    inputs: PreparationDigest,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct PreparationDigestSchema;

impl Schema for PreparationDigestSchema {
    const DOMAIN: u8 = 0x44;
    const TYPE: u16 = 15;
    type Value = [u8];

    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(&(value.len() as u64).to_be_bytes());
        output.extend_from_slice(value);
    }
}

type PreparationDigest = ObjectVersion<PreparationDigestSchema>;

fn digest_language(language: &str) -> Result<PreparationDigest, PreparationError> {
    if language.is_empty() {
        return Err(PreparationError::Protocol(NativeProtocolError::EmptyKey));
    }
    if language.len() > crate::MAX_NATIVE_KEY_BYTES {
        return Err(PreparationError::Protocol(NativeProtocolError::KeyLimit {
            actual: language.len(),
            maximum: crate::MAX_NATIVE_KEY_BYTES,
        }));
    }
    if language.as_bytes().contains(&0) {
        return Err(PreparationError::Protocol(NativeProtocolError::InvalidKey));
    }
    Ok(ObjectVersion::from_value(language.as_bytes()))
}

fn digest_inputs(inputs: &[NativeRequestInput]) -> Result<PreparationDigest, PreparationError> {
    if inputs.len() > crate::MAX_NATIVE_INPUTS {
        return Err(PreparationError::Protocol(
            NativeProtocolError::InputCountLimit {
                actual: inputs.len(),
                maximum: crate::MAX_NATIVE_INPUTS,
            },
        ));
    }
    let mut order = (0..inputs.len()).collect::<Vec<_>>();
    order.sort_by(|&left, &right| {
        inputs[left]
            .name()
            .as_bytes()
            .cmp(inputs[right].name().as_bytes())
    });
    for pair in order.windows(2) {
        if inputs[pair[0]].name() == inputs[pair[1]].name() {
            return Err(PreparationError::Protocol(
                NativeProtocolError::DuplicateInput {
                    name: inputs[pair[0]].name().to_owned(),
                },
            ));
        }
    }
    let mut length = 4_usize;
    for &index in &order {
        let input = &inputs[index];
        if input.name().is_empty()
            || input.name().len() > crate::MAX_NATIVE_KEY_BYTES
            || input.name().as_bytes().contains(&0)
        {
            return Err(PreparationError::Protocol(NativeProtocolError::InvalidKey));
        }
        length = length
            .checked_add(4)
            .and_then(|length| length.checked_add(input.name().len()))
            .and_then(|length| length.checked_add(8))
            .and_then(|length| length.checked_add(input.bytes().len()))
            .ok_or(PreparationError::Overflow)?;
        if length > crate::MAX_NATIVE_REQUEST_BYTES {
            return Err(PreparationError::Capacity);
        }
    }
    let mut canonical = Vec::with_capacity(length);
    canonical.extend_from_slice(
        &u32::try_from(order.len())
            .map_err(|_| PreparationError::Overflow)?
            .to_be_bytes(),
    );
    for &index in &order {
        let input = &inputs[index];
        canonical.extend_from_slice(
            &u32::try_from(input.name().len())
                .map_err(|_| PreparationError::Overflow)?
                .to_be_bytes(),
        );
        canonical.extend_from_slice(input.name().as_bytes());
        canonical.extend_from_slice(&(input.bytes().len() as u64).to_be_bytes());
        canonical.extend_from_slice(input.bytes());
    }
    Ok(ObjectVersion::from_value(canonical.as_slice()))
}

impl PreparationKey {
    /// Constructs a key after checking that the session is bound to the same
    /// authority and source manifest.
    ///
    /// # Errors
    ///
    /// Returns [`PreparationError::KeyMismatch`] when the session does not
    /// belong to the authority or manifest, or another preparation error when
    /// language or input identity cannot be admitted.
    pub fn new(
        authority: AuthorityIdentity,
        manifest: InputManifestId,
        session: SessionKey,
        revision: u64,
        language: &str,
        inputs: &[NativeRequestInput],
    ) -> Result<Self, PreparationError> {
        if session.authority() != authority.digest() || session.manifest() != manifest {
            return Err(PreparationError::KeyMismatch);
        }
        Ok(Self {
            authority: authority.digest(),
            toolchain: authority.toolchain,
            manifest,
            session,
            revision,
            language: digest_language(language)?,
            inputs: digest_inputs(inputs)?,
        })
    }

    /// Returns the authority identity digest.
    #[must_use]
    pub const fn authority(self) -> SessionId {
        self.authority
    }

    /// Returns the exact toolchain object version.
    #[must_use]
    pub const fn toolchain(self) -> ToolchainId {
        self.toolchain
    }

    /// Returns the source/input manifest version.
    #[must_use]
    pub const fn manifest(self) -> InputManifestId {
        self.manifest
    }

    /// Returns the complete session key.
    #[must_use]
    pub const fn session(self) -> SessionKey {
        self.session
    }

    /// Returns the discovery revision bound to the request.
    #[must_use]
    pub const fn revision(self) -> u64 {
        self.revision
    }

    /// Returns the canonical language component of this preparation key.
    #[must_use]
    pub const fn language_digest(self) -> [u8; 32] {
        self.language.to_bytes()
    }

    /// Returns the canonical ordered input component of this preparation key.
    #[must_use]
    pub const fn inputs_digest(self) -> [u8; 32] {
        self.inputs.to_bytes()
    }

    pub(super) fn language_matches(&self, language: &str) -> bool {
        digest_language(language).ok() == Some(self.language)
    }

    pub(super) fn matches_request(&self, language: &str, inputs: &[NativeRequestInput]) -> bool {
        digest_language(language).ok() == Some(self.language)
            && digest_inputs(inputs).ok() == Some(self.inputs)
    }
}

//! Canonical native helper request encoding.
use super::{
    MAX_NATIVE_INPUTS, MAX_NATIVE_REQUEST_BYTES, NATIVE_PAYLOAD_VERSION, NativeProtocolError,
    REQUEST_HEADER_BYTES, REQUEST_MAGIC, put_u16, put_u32, validate_key,
};
use crate::{InputManifestId, SessionId, SessionKey};

/// One raw input sent to a cold helper.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeRequestInput {
    name: String,
    bytes: Vec<u8>,
}

impl NativeRequestInput {
    /// Creates a named input field after validating its canonical name.
    ///
    /// # Errors
    ///
    /// Returns [`NativeProtocolError`] when `name` is empty, noncanonical, or
    /// exceeds the protocol bound.
    pub fn new(
        name: impl Into<String>,
        bytes: impl Into<Vec<u8>>,
    ) -> Result<Self, NativeProtocolError> {
        let name = name.into();
        validate_key(&name)?;
        Ok(Self {
            name,
            bytes: bytes.into(),
        })
    }

    /// Returns the canonical input name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the exact input bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// Canonical request body supplied to a cold authority helper.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeRequest {
    language: String,
    session: SessionId,
    manifest: InputManifestId,
    authority: SessionId,
    inputs: Vec<NativeRequestInput>,
}

impl NativeRequest {
    /// Builds a request and sorts its exact input fields by canonical name.
    ///
    /// # Errors
    ///
    /// Returns [`NativeProtocolError`] when the language or inputs are invalid,
    /// duplicated, or exceed a protocol bound.
    pub fn new(
        language: impl Into<String>,
        key: SessionKey,
        mut inputs: Vec<NativeRequestInput>,
    ) -> Result<Self, NativeProtocolError> {
        let language = language.into();
        validate_key(&language)?;
        if inputs.len() > MAX_NATIVE_INPUTS {
            return Err(NativeProtocolError::InputCountLimit {
                actual: inputs.len(),
                maximum: MAX_NATIVE_INPUTS,
            });
        }
        inputs.sort_by(|left, right| left.name.as_bytes().cmp(right.name.as_bytes()));
        for pair in inputs.windows(2) {
            if pair[0].name == pair[1].name {
                return Err(NativeProtocolError::DuplicateInput {
                    name: pair[0].name.clone(),
                });
            }
        }
        let request = Self {
            language,
            session: key.digest(),
            manifest: key.manifest(),
            authority: key.authority(),
            inputs,
        };
        let _ = request.encoded_len()?;
        Ok(request)
    }

    /// Returns the session identity carried by this request.
    #[must_use]
    pub const fn session(&self) -> SessionId {
        self.session
    }

    /// Returns the exact manifest identity carried by this request.
    #[must_use]
    pub const fn manifest(&self) -> InputManifestId {
        self.manifest
    }

    /// Returns the authority identity carried by this request.
    #[must_use]
    pub const fn authority(&self) -> SessionId {
        self.authority
    }

    /// Returns the exact input fields in canonical order.
    #[must_use]
    pub fn inputs(&self) -> &[NativeRequestInput] {
        &self.inputs
    }

    /// Encodes the request body for a cold helper.
    ///
    /// # Errors
    ///
    /// Returns [`NativeProtocolError`] when the request cannot be represented
    /// in the bounded canonical wire format.
    pub fn encode(&self) -> Result<Vec<u8>, NativeProtocolError> {
        let length = self.encoded_len()?;
        let mut output = Vec::with_capacity(length);
        output.extend_from_slice(&REQUEST_MAGIC);
        output.extend_from_slice(&NATIVE_PAYLOAD_VERSION.to_be_bytes());
        put_u16(&mut output, self.language.len())?;
        put_u16(&mut output, self.inputs.len())?;
        output.extend_from_slice(&self.session.to_bytes());
        output.extend_from_slice(&self.manifest.to_bytes());
        output.extend_from_slice(&self.authority.to_bytes());
        output.extend_from_slice(self.language.as_bytes());
        for input in &self.inputs {
            put_u16(&mut output, input.name.len())?;
            put_u32(&mut output, input.bytes.len())?;
            output.extend_from_slice(input.name.as_bytes());
            output.extend_from_slice(&input.bytes);
        }
        if output.len() != length {
            return Err(NativeProtocolError::LengthOverflow);
        }
        Ok(output)
    }

    fn encoded_len(&self) -> Result<usize, NativeProtocolError> {
        if self.inputs.len() > MAX_NATIVE_INPUTS {
            return Err(NativeProtocolError::InputCountLimit {
                actual: self.inputs.len(),
                maximum: MAX_NATIVE_INPUTS,
            });
        }
        let mut length = REQUEST_HEADER_BYTES
            .checked_add(self.language.len())
            .ok_or(NativeProtocolError::LengthOverflow)?;
        for input in &self.inputs {
            validate_key(&input.name)?;
            length = length
                .checked_add(6)
                .and_then(|length| length.checked_add(input.name.len()))
                .and_then(|length| length.checked_add(input.bytes.len()))
                .ok_or(NativeProtocolError::LengthOverflow)?;
        }
        if length > MAX_NATIVE_REQUEST_BYTES {
            return Err(NativeProtocolError::ByteLimit {
                actual: length,
                maximum: MAX_NATIVE_REQUEST_BYTES,
            });
        }
        Ok(length)
    }
}

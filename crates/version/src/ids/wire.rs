use core::{fmt, marker::PhantomData};

use super::{ID_BYTES, context::IdContext};

/// A fixed width digest that has crossed a wire boundary but is not admitted.
pub struct UntrustedId<K> {
    pub(crate) bytes: [u8; ID_BYTES],
    pub(crate) context: IdContext,
    pub(crate) _marker: PhantomData<fn() -> K>,
}

impl<K> Copy for UntrustedId<K> {}

#[allow(
    clippy::expl_impl_clone_on_copy,
    reason = "untrusted fixed-width claims are copied without changing trust"
)]
impl<K> Clone for UntrustedId<K> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K> fmt::Debug for UntrustedId<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UntrustedId")
            .field("bytes", &self.bytes)
            .field("context", &self.context)
            .finish()
    }
}

/// Failure while decoding an untrusted wire claim.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdDecodeError {
    /// The claim did not contain exactly [`ID_BYTES`] bytes.
    WrongLength,
}

impl fmt::Display for IdDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid identity wire claim: {self:?}")
    }
}

impl std::error::Error for IdDecodeError {}

/// Failure to admit a wire digest as the requested identity class.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdAdmissionError {
    /// The wire context did not match the requested schema and identity class.
    ContextMismatch {
        /// Context expected by the receiver.
        expected: IdContext,
        /// Context carried by the untrusted claim.
        actual: IdContext,
    },
    /// The claim had the right context but no canonical preimage was supplied
    /// to verify its digest.  Context metadata alone never creates a trusted
    /// typed identity.
    UnverifiedDigest,
    /// The claim's digest did not match the supplied canonical preimage.
    DigestMismatch,
}

impl fmt::Display for IdAdmissionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "identity context was not admitted: {self:?}")
    }
}

impl std::error::Error for IdAdmissionError {}

impl<K> UntrustedId<K> {
    /// Parses exactly one digest and retains its untrusted context.
    ///
    /// # Errors
    ///
    /// Returns [`IdDecodeError::WrongLength`] when `bytes` is not exactly
    /// [`ID_BYTES`] bytes long.
    pub fn from_wire(bytes: &[u8], context: IdContext) -> Result<Self, IdDecodeError> {
        let bytes: [u8; ID_BYTES] = bytes.try_into().map_err(|_| IdDecodeError::WrongLength)?;
        Ok(Self {
            bytes,
            context,
            _marker: PhantomData,
        })
    }

    /// Returns the digest bytes without treating them as an accepted identity.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; ID_BYTES] {
        &self.bytes
    }

    /// Returns the untrusted wire metadata.
    #[must_use]
    pub const fn context(&self) -> IdContext {
        self.context
    }
}

/// A fixed-width identity claim received from a wire boundary.
///
/// `WireId` retains the supplied schema context but does not itself grant a
/// typed identity.  Call [`WireId::into_untrusted`] and then the requested
/// typed ID's preimage-aware admission method after validating the surrounding
/// envelope and canonical bytes.
pub struct WireId<K> {
    pub(crate) bytes: [u8; ID_BYTES],
    pub(crate) context: IdContext,
    pub(crate) _marker: PhantomData<fn() -> K>,
}

impl<K> Copy for WireId<K> {}

#[allow(
    clippy::expl_impl_clone_on_copy,
    reason = "wire claims are fixed-width values and cloning preserves their trust level"
)]
impl<K> Clone for WireId<K> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K> fmt::Debug for WireId<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WireId")
            .field("bytes", &self.bytes)
            .field("context", &self.context)
            .finish()
    }
}

impl<K> PartialEq for WireId<K> {
    fn eq(&self, other: &Self) -> bool {
        self.bytes == other.bytes && self.context == other.context
    }
}

impl<K> Eq for WireId<K> {}

impl<K> WireId<K> {
    /// Parses exactly one fixed-width wire digest and retains its context.
    ///
    /// # Errors
    ///
    /// Returns [`IdDecodeError::WrongLength`] when `bytes` is not exactly
    /// [`ID_BYTES`] bytes long.
    pub fn decode(bytes: &[u8], context: IdContext) -> Result<Self, IdDecodeError> {
        let bytes: [u8; ID_BYTES] = bytes.try_into().map_err(|_| IdDecodeError::WrongLength)?;
        Ok(Self {
            bytes,
            context,
            _marker: PhantomData,
        })
    }

    /// Alias for [`WireId::decode`] used by transport adapters.
    ///
    /// # Errors
    ///
    /// Returns [`IdDecodeError::WrongLength`] when `bytes` is not exactly
    /// [`ID_BYTES`] bytes long.
    pub fn new(bytes: &[u8], context: IdContext) -> Result<Self, IdDecodeError> {
        Self::decode(bytes, context)
    }

    /// Returns the untrusted claim for typed admission.
    #[must_use]
    pub const fn into_untrusted(self) -> UntrustedId<K> {
        UntrustedId {
            bytes: self.bytes,
            context: self.context,
            _marker: PhantomData,
        }
    }

    /// Borrows the untrusted claim without changing its trust level.
    #[must_use]
    pub const fn as_untrusted(&self) -> UntrustedId<K> {
        UntrustedId {
            bytes: self.bytes,
            context: self.context,
            _marker: PhantomData,
        }
    }

    /// Returns the raw claim bytes for diagnostics or re-encoding.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; ID_BYTES] {
        &self.bytes
    }

    /// Returns the untrusted wire context.
    #[must_use]
    pub const fn context(&self) -> IdContext {
        self.context
    }
}

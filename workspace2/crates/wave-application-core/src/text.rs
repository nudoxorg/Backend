//! Fixed-capacity UTF-8 input retained without heap ownership.

/// Maximum retained adapter text width: the semantic bound plus its exact `limit + 1` falsifier.
pub const INPUT_TEXT_BYTES: usize = crate::model::MAX_SEMANTIC_TEXT_BYTES + 1;

/// An adapter field that has passed only transport width validation.
///
/// Semantic width and vocabulary checks intentionally remain in the application service.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InputText {
    bytes: [u8; INPUT_TEXT_BYTES],
    length: usize,
}

/// Transport-width rejection before any semantic request enters the service.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InputTextError {
    /// Observed UTF-8 byte length.
    pub actual: usize,
    /// Fixed accepted byte length.
    pub maximum: usize,
}

impl InputText {
    /// Makes one transport-bounded field from UTF-8 input.
    ///
    /// # Errors
    ///
    /// Returns [`InputTextError`] when the UTF-8 field exceeds the bounded adapter capacity.
    pub fn try_from_str(value: &str) -> Result<Self, InputTextError> {
        if value.len() > INPUT_TEXT_BYTES {
            return Err(InputTextError {
                actual: value.len(),
                maximum: INPUT_TEXT_BYTES,
            });
        }
        let mut bytes = [0; INPUT_TEXT_BYTES];
        bytes[..value.len()].copy_from_slice(value.as_bytes());
        Ok(Self {
            bytes,
            length: value.len(),
        })
    }

    /// Retains one UTF-8 compiler result only when it fits the same fixed reply capacity.
    #[must_use]
    pub(crate) fn from_compiler_bytes(value: &[u8]) -> Option<Self> {
        let value = core::str::from_utf8(value).ok()?;
        Self::try_from_str(value).ok()
    }

    /// Returns the exact validated UTF-8 bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.length]
    }

    /// Returns the observed byte length.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.length
    }

    /// Reports whether the field has no bytes.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.length == 0
    }

    /// Compares the exact UTF-8 bytes to one protocol-neutral literal.
    #[must_use]
    pub fn is(&self, expected: &str) -> bool {
        self.as_bytes() == expected.as_bytes()
    }

    /// Checks whether this static/local field contains an input query.
    #[must_use]
    pub fn contains(&self, query: &Self) -> bool {
        self.as_bytes()
            .windows(query.len())
            .any(|window| window == query.as_bytes())
    }
}

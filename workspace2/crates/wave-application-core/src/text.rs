//! Fixed-capacity UTF-8 input retained without heap ownership.

use core::{borrow::Borrow, ops::Deref};

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
}

impl Deref for InputText {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.bytes[..self.length]
    }
}

impl AsRef<[u8]> for InputText {
    fn as_ref(&self) -> &[u8] {
        self
    }
}

impl Borrow<[u8]> for InputText {
    fn borrow(&self) -> &[u8] {
        self
    }
}

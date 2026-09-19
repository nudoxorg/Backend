//! Defines text behavior for `backend-library`, whose purpose is to own the transport-independent application service and reply vocabulary.
//! This module owns the text invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Fixed-capacity UTF-8 input retained without heap ownership.

use core::{borrow::Borrow, ops::Deref};

use arrayvec::ArrayString;

/// Maximum retained adapter text width: the semantic bound plus its exact `limit + 1` falsifier.
pub const INPUT_TEXT_BYTES: usize = crate::interface::model::MAX_SEMANTIC_TEXT_BYTES + 1;

/// An adapter field that has passed only transport width validation.
///
/// Semantic width and vocabulary checks intentionally remain in the application service.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InputText {
    value: ArrayString<INPUT_TEXT_BYTES>,
}

/// Transport-width rejection before any semantic request enters the service.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InputTextError {
    /// Observed UTF-8 byte length.
    pub actual: usize,
    /// Fixed accepted byte length.
    pub maximum: usize,
}

/// Exact failure to join three UTF-8 parts into one bounded input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputTextJoinError {
    /// The mathematical joined length cannot be represented by `usize`.
    LengthOverflow {
        /// Prefix byte length.
        prefix: usize,
        /// Inserted byte length.
        inserted: usize,
        /// Suffix byte length.
        suffix: usize,
    },
    /// The exact representable joined length exceeds the transport bound.
    InputTooLong(InputTextError),
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
        ArrayString::try_from(value)
            .map(|value| Self { value })
            .map_err(|rejected| InputTextError {
                actual: rejected.element().len(),
                maximum: INPUT_TEXT_BYTES,
            })
    }

    /// Joins caller-borrowed UTF-8 segments directly into one transport-bounded field.
    ///
    /// This preserves the validated UTF-8 invariant without constructing an intermediate byte
    /// buffer and attempting to decode it again.
    ///
    /// # Errors
    ///
    /// Returns [`InputTextJoinError::InputTooLong`] with the exact joined byte length when the
    /// parts exceed the bounded adapter capacity, or [`InputTextJoinError::LengthOverflow`] with
    /// all three source lengths when their mathematical sum cannot be represented.
    pub fn try_from_parts(
        prefix: &str,
        inserted: &str,
        suffix: &str,
    ) -> Result<Self, InputTextJoinError> {
        let Some(actual) = prefix
            .len()
            .checked_add(inserted.len())
            .and_then(|length| length.checked_add(suffix.len()))
        else {
            return Err(InputTextJoinError::LengthOverflow {
                prefix: prefix.len(),
                inserted: inserted.len(),
                suffix: suffix.len(),
            });
        };
        if actual > INPUT_TEXT_BYTES {
            return Err(InputTextJoinError::InputTooLong(InputTextError {
                actual,
                maximum: INPUT_TEXT_BYTES,
            }));
        }
        let mut value = ArrayString::new();
        for part in [prefix, inserted, suffix] {
            if let Err(capacity_rejection) = value.try_push_str(part) {
                debug_assert_eq!(capacity_rejection.element(), part);
                return Err(InputTextJoinError::InputTooLong(InputTextError {
                    actual,
                    maximum: INPUT_TEXT_BYTES,
                }));
            }
        }
        Ok(Self { value })
    }
}

impl Deref for InputText {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl AsRef<[u8]> for InputText {
    fn as_ref(&self) -> &[u8] {
        self.value.as_bytes()
    }
}

impl Borrow<[u8]> for InputText {
    fn borrow(&self) -> &[u8] {
        self.value.as_bytes()
    }
}

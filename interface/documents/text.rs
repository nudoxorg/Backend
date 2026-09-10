//! Defines text behavior for `interface-documents`, whose purpose is to project semantic images into one presentation-neutral document model every surface renders.
//! This module owns the text invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Owned UTF-8 text and byte-coordinate newtypes shared by every document element.

use core::fmt;

/// Owned UTF-8 text admitted from a semantic document pool or rendered by a projector.
#[derive(Clone, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Text(Box<str>);

impl Text {
    /// Wraps already-owned text.
    #[must_use]
    pub fn new(text: impl Into<Box<str>>) -> Self {
        Self(text.into())
    }

    /// Returns the exact text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether the text is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Display for Text {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl AsRef<str> for Text {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// One declaration name. Semantic atoms may be arbitrary bytes, so construction is explicit about
/// whether a lossy projection was applied.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Name {
    text: Text,
    fidelity: NameFidelity,
}

/// Whether a name is the exact atom or a replacement-character projection of it.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum NameFidelity {
    /// The atom was valid UTF-8 and is retained exactly.
    Exact,
    /// Invalid sequences were replaced; the rendered spelling must not be used as an input key.
    Lossy,
}

impl Name {
    /// Admits an exact UTF-8 atom.
    ///
    /// # Errors
    ///
    /// Returns the byte position of the first invalid sequence.
    pub fn exact(bytes: &[u8]) -> Result<Self, NameError> {
        core::str::from_utf8(bytes)
            .map(|text| Self {
                text: Text::new(text),
                fidelity: NameFidelity::Exact,
            })
            .map_err(|error| NameError::InvalidUtf8 {
                valid_up_to: error.valid_up_to(),
            })
    }

    /// Projects any atom onto displayable text, marking the result lossy when bytes were replaced.
    #[must_use]
    pub fn displayable(bytes: &[u8]) -> Self {
        match core::str::from_utf8(bytes) {
            Ok(text) => Self {
                text: Text::new(text),
                fidelity: NameFidelity::Exact,
            },
            Err(_) => Self {
                text: Text::new(String::from_utf8_lossy(bytes).into_owned()),
                fidelity: NameFidelity::Lossy,
            },
        }
    }

    /// Returns the rendered text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.text.as_str()
    }

    /// Returns whether the spelling is exact.
    #[must_use]
    pub const fn fidelity(&self) -> NameFidelity {
        self.fidelity
    }
}

impl fmt::Display for Name {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.text.as_str())
    }
}

/// Exact name admission failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NameError {
    /// The atom was not UTF-8.
    InvalidUtf8 {
        /// Bytes before the first invalid sequence.
        valid_up_to: usize,
    },
}

/// One byte offset inside a source file.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ByteOffset(pub u32);

/// One half-open source byte span.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ByteSpan {
    /// Inclusive start.
    pub start: ByteOffset,
    /// Exclusive end.
    pub end: ByteOffset,
}

impl ByteSpan {
    /// Builds a non-inverted span.
    #[must_use]
    pub const fn new(start: u32, end: u32) -> Option<Self> {
        if start <= end {
            Some(Self {
                start: ByteOffset(start),
                end: ByteOffset(end),
            })
        } else {
            None
        }
    }

    /// Span width in bytes.
    #[must_use]
    pub const fn len(self) -> u32 {
        self.end.0 - self.start.0
    }

    /// Whether the span is empty.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.start.0 == self.end.0
    }
}

/// One byte budget: how many bytes of a variable-length plane a projector may retain.
///
/// A budget is not a count of anything observed; it is a policy the caller chose, so it is spelled
/// with its own type and never confused with a [`ByteOffset`].
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ByteBudget(pub u32);

impl ByteBudget {
    /// The budget as a `usize` on this target, saturating rather than wrapping.
    #[must_use]
    pub fn get(self) -> usize {
        usize::try_from(self.0).unwrap_or(usize::MAX)
    }
}

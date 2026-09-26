//! Cursor control and query envelopes, plus event advance and rewind.

use super::{
    CURSOR_CONTROL_BYTES, CURSOR_QUERY_BYTES, CURSOR_SCHEMA, Cursor, CursorError, CursorEvent,
};
use crate::ViewRoot;
use crate::canonical::Frontier;

impl Cursor {
    /// Encodes this subscription cursor for the authenticated local control
    /// channel.
    ///
    /// Query continuation offsets are intentionally excluded: this encoding
    /// is only for branch/log subscription positions and therefore accepts
    /// only cursors whose offset is zero.
    #[must_use]
    pub fn encode_control(self) -> Box<[u8]> {
        let mut bytes = Vec::with_capacity(CURSOR_CONTROL_BYTES);
        bytes.extend_from_slice(&CURSOR_SCHEMA.to_be_bytes());
        bytes.extend_from_slice(self.recipe.as_bytes());
        bytes.extend_from_slice(self.version.as_bytes());
        bytes.extend_from_slice(self.branch.as_bytes());
        bytes.extend_from_slice(self.log.as_bytes());
        bytes.extend_from_slice(&self.schema.to_be_bytes());
        bytes.extend_from_slice(self.root.as_bytes());
        bytes.extend_from_slice(&self.sequence.to_be_bytes());
        bytes.into_boxed_slice()
    }

    /// Admits a local control cursor by exact comparison with an already
    /// trusted typed cursor.
    ///
    /// The returned value is `expected`; no digest-only bytes become a new
    /// identity.  This is the common decoder used by desktop and locald.
    ///
    /// # Errors
    ///
    /// Returns an error when the encoded cursor has an unsupported schema or
    /// differs from `expected`.
    pub fn decode_control_against(bytes: &[u8], expected: Self) -> Result<Self, String> {
        if expected.schema != CURSOR_SCHEMA {
            return Err("unsupported subscription cursor schema".to_owned());
        }
        if expected.query_offset != 0 {
            return Err("subscription cursor carries a query offset".to_owned());
        }
        if bytes != expected.encode_control().as_ref() {
            return Err("subscription cursor does not match the admitted cursor".to_owned());
        }
        Ok(expected)
    }

    /// Admits a replacement cursor against an already checked view root.
    ///
    /// Only the sequence is read from the fixed-width envelope.  Every other
    /// identity is compared against the supplied root before a typed cursor
    /// is returned.
    ///
    /// # Errors
    ///
    /// Returns an error when the fixed-width cursor is malformed or does not
    /// name the supplied view root.
    pub fn decode_control_for_root(bytes: &[u8], root: &ViewRoot) -> Result<Self, String> {
        if bytes.len() != CURSOR_CONTROL_BYTES {
            return Err("invalid subscription cursor length".to_owned());
        }
        let sequence = u64::from_be_bytes(
            bytes[164..172]
                .try_into()
                .map_err(|_| "invalid subscription cursor sequence".to_owned())?,
        );
        let expected = Self::for_view_root_at(root, sequence);
        Self::decode_control_against(bytes, expected)
    }

    /// Encodes this cursor for a bounded query continuation.
    ///
    /// The envelope carries the same authenticated identity fields as the
    /// local control cursor plus the query offset. It is only an opaque
    /// transport representation; [`decode_query_against`](Self::decode_query_against)
    /// admits it by comparing every identity field with an owner cursor.
    #[must_use]
    pub fn encode_query(self) -> Box<[u8]> {
        let mut bytes = self.encode_control().into_vec();
        bytes.extend_from_slice(&self.query_offset.to_be_bytes());
        bytes.into_boxed_slice()
    }

    /// Admits a bounded-query cursor against an owner-provided cursor.
    ///
    /// Raw bytes never become a typed cursor on their own. The owner cursor
    /// supplies recipe, version, branch, log, schema, and root; only the
    /// checked offset is read from the envelope.
    ///
    /// # Errors
    /// Returns an error when the envelope is malformed, carries a different
    /// owner identity, or does not describe a positive query offset.
    pub fn decode_query_against(bytes: &[u8], owner: Self) -> Result<Self, String> {
        if bytes.len() != CURSOR_QUERY_BYTES {
            return Err("invalid query cursor length".to_owned());
        }
        if bytes[..CURSOR_CONTROL_BYTES] != *owner.encode_control() {
            return Err("query cursor does not match the owner context".to_owned());
        }
        let offset = u64::from_be_bytes(
            bytes[CURSOR_CONTROL_BYTES..]
                .try_into()
                .map_err(|_| "invalid query cursor offset".to_owned())?,
        );
        if offset == 0 {
            return Err("query cursor offset must be positive".to_owned());
        }
        Ok(owner.with_query_offset(offset))
    }

    /// Advances this cursor across one checked event.
    ///
    /// The stream sequence advances for both intent and view events. A view
    /// event additionally replaces the recipe, immutable version, and
    /// visible relation root with the transition's checked target. Keeping
    /// this operation here makes the subscription reducer, wire decoder, and
    /// daemon producer use one state transition instead of reimplementing
    /// identity checks independently.
    ///
    /// # Errors
    ///
    /// Returns [`CursorError`] when the event is malformed, does not chain
    /// from this cursor, or advancing the sequence would overflow.
    pub fn advance_event(self, event: &CursorEvent) -> Result<Self, CursorError> {
        let sequence = self.sequence.checked_add(1).ok_or(CursorError::Gap)?;
        match event {
            CursorEvent::Intent { .. } => Ok(Self::for_view(
                self.recipe,
                self.version,
                Frontier::new(self.branch, self.log, self.schema, self.root, sequence),
            )),
            CursorEvent::View { delta } => {
                if delta.validate().is_err() {
                    return Err(CursorError::WrongRoot);
                }
                if delta.base_recipe() != self.recipe {
                    return Err(CursorError::WrongRecipe);
                }
                if delta.base_version() != self.version {
                    return Err(CursorError::WrongVersion);
                }
                if delta.base_root() != self.root {
                    return Err(CursorError::WrongRoot);
                }
                if delta.frontier().branch != self.branch {
                    return Err(CursorError::WrongBranch);
                }
                if delta.frontier().log != self.log {
                    return Err(CursorError::WrongLog);
                }
                if delta.frontier().schema != self.schema {
                    return Err(CursorError::SchemaMismatch);
                }
                Ok(Self::for_view(
                    delta.target_recipe(),
                    delta.target_version(),
                    Frontier::new(
                        self.branch,
                        self.log,
                        self.schema,
                        delta.target_root(),
                        sequence,
                    ),
                ))
            }
        }
    }

    /// Rewinds this cursor across one checked event.
    ///
    /// This is used by an owner that retains only the final cursor and an
    /// event suffix. It validates the event's target against the final
    /// cursor, then returns the exact predecessor without manufacturing any
    /// identity from raw wire bytes.
    ///
    /// # Errors
    ///
    /// Returns [`CursorError`] when the event does not terminate at this
    /// cursor or the predecessor sequence would underflow.
    pub fn rewind_event(self, event: &CursorEvent) -> Result<Self, CursorError> {
        let sequence = self.sequence.checked_sub(1).ok_or(CursorError::Gap)?;
        match event {
            CursorEvent::Intent { .. } => Ok(Self::for_view(
                self.recipe,
                self.version,
                Frontier::new(self.branch, self.log, self.schema, self.root, sequence),
            )),
            CursorEvent::View { delta } => {
                if delta.validate().is_err() {
                    return Err(CursorError::WrongRoot);
                }
                if delta.target_recipe() != self.recipe {
                    return Err(CursorError::WrongRecipe);
                }
                if delta.target_version() != self.version {
                    return Err(CursorError::WrongVersion);
                }
                if delta.target_root() != self.root {
                    return Err(CursorError::WrongRoot);
                }
                if delta.frontier().branch != self.branch {
                    return Err(CursorError::WrongBranch);
                }
                if delta.frontier().log != self.log {
                    return Err(CursorError::WrongLog);
                }
                if delta.frontier().schema != self.schema {
                    return Err(CursorError::SchemaMismatch);
                }
                Ok(Self::for_view(
                    delta.base_recipe(),
                    delta.base_version(),
                    Frontier::new(
                        self.branch,
                        self.log,
                        self.schema,
                        delta.base_root(),
                        sequence,
                    ),
                ))
            }
        }
    }
}

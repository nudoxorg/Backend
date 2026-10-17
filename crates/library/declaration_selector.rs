//! Untrusted declaration operands resolved only in a current admitted view.

use serde::{Deserialize, Serialize};

/// Full bytes copied from a selected row. These bytes are a locator, never a
/// compiler `SymbolKey` or publication proof.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct SelectedSymbolId([u8; 32]);

impl SelectedSymbolId {
    /// Accepts exactly one nonzero, full-width selected row identifier.
    #[must_use]
    pub const fn new(bytes: [u8; 32]) -> Option<Self> {
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] != 0 {
                return Some(Self(bytes));
            }
            i += 1;
        }
        None
    }

    /// Parses the CLI spelling: exactly 64 hexadecimal digits, with no prefix.
    #[must_use]
    pub fn from_hex(text: &str) -> Option<Self> {
        if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let mut bytes = [0; 32];
        for (slot, pair) in bytes.iter_mut().zip(text.as_bytes().chunks_exact(2)) {
            let digit = |b: u8| {
                if b.is_ascii_digit() {
                    b - b'0'
                } else {
                    b.to_ascii_lowercase() - b'a' + 10
                }
            };
            *slot = digit(pair[0]) * 16 + digit(pair[1]);
        }
        Self::new(bytes)
    }

    /// Exact selected row bytes, still requiring current-view membership.
    #[must_use]
    pub const fn bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl<'de> Deserialize<'de> for SelectedSymbolId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(<[u8; 32]>::deserialize(deserializer)?)
            .ok_or_else(|| serde::de::Error::custom("selected symbol row id is empty"))
    }
}

/// The three valid public declaration operands. A coordinate alongside an ID
/// is an assertion about the resolved row label, not another identity source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeclarationSelector {
    /// Existing coordinate/name input.
    Coordinate(String),
    /// Full selected row identifier copied from a result.
    Selected(SelectedSymbolId),
    /// Selected row with an additional exact coordinate assertion.
    SelectedAt {
        /// Untrusted row locator.
        symbol: SelectedSymbolId,
        /// Expected label of the row resolved by the owner.
        coordinate: String,
    },
}

impl DeclarationSelector {
    /// Coordinate supplied by the caller, if any.
    #[must_use]
    pub fn coordinate(&self) -> Option<&str> {
        match self {
            Self::Coordinate(value)
            | Self::SelectedAt {
                coordinate: value, ..
            } => Some(value),
            Self::Selected(_) => None,
        }
    }

    /// Full selected identifier, if this is a selected read.
    #[must_use]
    pub const fn selected_id(&self) -> Option<SelectedSymbolId> {
        match self {
            Self::Selected(symbol) | Self::SelectedAt { symbol, .. } => Some(*symbol),
            Self::Coordinate(_) => None,
        }
    }

    /// Locator whose raw selected bytes can only resolve through view membership.
    #[must_use]
    pub fn address(&self) -> crate::SymbolAddress {
        match self {
            Self::Coordinate(value) => crate::SymbolAddress::canonical(crate::symbol_key(value)),
            Self::Selected(symbol) | Self::SelectedAt { symbol, .. } => {
                crate::SymbolAddress::Selected(*symbol.bytes())
            }
        }
    }
}

/// Declaration reads that support the selected-row operand protocol.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeclarationRead {
    /// Document, members, relations and captured source.
    Document,
    /// Captured declaration source.
    Source,
    /// Outgoing graph neighbourhood.
    Graph,
    /// Incoming and outgoing graph neighbourhood.
    Related,
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn selected_ids_accept_only_full_nonzero_untrusted_operands() {
        let id = SelectedSymbolId::new([0xa5; 32]).expect("full ID");
        assert_eq!(SelectedSymbolId::from_hex(&"a5".repeat(32)), Some(id));
        assert_eq!(SelectedSymbolId::from_hex(&"A5".repeat(32)), Some(id));
        for invalid in [
            "00".repeat(32),
            "a5".repeat(31),
            "a5".repeat(33),
            format!("0x{}", "a5".repeat(32)),
            "zz".repeat(32),
        ] {
            assert!(SelectedSymbolId::from_hex(&invalid).is_none(), "{invalid}");
        }
        assert!(
            serde_json::from_value::<SelectedSymbolId>(serde_json::json!(vec![0; 32])).is_err()
        );
        assert!(
            serde_json::from_value::<SelectedSymbolId>(serde_json::json!(vec![1; 31])).is_err()
        );
        assert!(
            serde_json::from_value::<SelectedSymbolId>(serde_json::json!(vec![256; 32])).is_err()
        );
        assert_eq!(
            serde_json::to_value(id).expect("JSON"),
            serde_json::json!(vec![165; 32])
        );
        assert!(
            matches!(DeclarationSelector::Selected(id).address(), crate::SymbolAddress::Selected(bytes) if bytes == [165;32])
        );
    }
}

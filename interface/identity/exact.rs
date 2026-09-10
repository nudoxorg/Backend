//! Defines exact behavior for `interface-identity`, whose purpose is to spell, parse, and abbreviate every identity a person or agent can name.
//! This module owns the exact invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! An address whose key was proven by the image that owns it, never typed by a person.

use core::{fmt, ops::Deref};

use crate::{Address, AddressKey, ContentKey, KeyExactness, PackageCoordinate, SymbolPath};

/// An [`Address`] that always carries one exact key.
///
/// Only a projector holding a reopened image can mint one, so every hit, member, crumb, and page
/// header that carries an `ExactAddress` is resolvable without a second lookup. Parsing user text
/// yields a plain [`Address`]; resolution upgrades it.
///
/// The key is stored beside the address rather than read back out of it, so [`ExactAddress::key`]
/// is total and `const`: there is no representable `ExactAddress` without a key, and no code path
/// that has to answer "what if there isn't one".
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExactAddress {
    address: Address,
    key: ContentKey,
}

impl ExactAddress {
    /// Mints an exact address from a proven key.
    #[must_use]
    pub const fn mint(package: PackageCoordinate, path: SymbolPath, key: ContentKey) -> Self {
        Self {
            address: Address::exact(package, path, key),
            key,
        }
    }

    /// The proven key.
    #[must_use]
    pub const fn key(&self) -> ContentKey {
        self.key
    }

    /// Borrows the general address.
    #[must_use]
    pub const fn as_address(&self) -> &Address {
        &self.address
    }

    /// Consumes into the general address.
    #[must_use]
    pub fn into_address(self) -> Address {
        self.address
    }

    /// Upgrades a parsed address when its key was spelled exactly.
    ///
    /// # Errors
    ///
    /// Hands the address back unchanged when it carries no key or only a family half, because a
    /// family-only spelling names a set of overload instances rather than one declaration.
    pub const fn from_parsed(address: Address) -> Result<Self, Address> {
        match address.key {
            Some(AddressKey {
                key,
                exactness: KeyExactness::Exact,
            }) => Ok(Self { address, key }),
            Some(AddressKey { .. }) | None => Err(address),
        }
    }
}

impl Deref for ExactAddress {
    type Target = Address;

    fn deref(&self) -> &Self::Target {
        &self.address
    }
}

impl fmt::Display for ExactAddress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.address, formatter)
    }
}

#[cfg(test)]
mod tests {
    use compiler_ir_vocabulary::{DeclarationFamilyId, DeclarationIdentity, VariantFingerprint};

    use super::*;
    use crate::AddressParseError;

    fn key(seed: u8) -> ContentKey {
        ContentKey::new(DeclarationIdentity {
            family: DeclarationFamilyId::from_raw([seed; 16]),
            variant: VariantFingerprint::from_raw([seed.wrapping_add(1); 16]),
        })
    }

    #[test]
    fn a_minted_address_round_trips_through_its_own_spelling()
    -> Result<(), AddressParseError> {
        let package = PackageCoordinate::parse("cargo:serde@1.0.196")
            .map_err(|cause| AddressParseError::Coordinate { cause })?;
        let path =
            SymbolPath::parse("de::Deserializer[trait]").map_err(|cause| AddressParseError::Path {
                cause,
            })?;
        let exact = ExactAddress::mint(package, path, key(0x5a));
        let parsed = Address::parse(&exact.to_string())?;
        let upgraded = ExactAddress::from_parsed(parsed)
            .map_err(|_| AddressParseError::Key {
                cause: crate::KeyParseError::Family {
                    cause: crate::HexParseError::Digit { offset: 0 },
                },
            })?;
        assert_eq!(upgraded.key(), exact.key());
        assert_eq!(upgraded, exact);
        Ok(())
    }

    #[test]
    fn a_family_only_spelling_is_handed_back_unchanged() -> Result<(), AddressParseError> {
        let text = format!("cargo:serde@1.0.196::de#{}", key(0x11).family());
        let parsed = Address::parse(&text)?;
        assert_eq!(ExactAddress::from_parsed(parsed.clone()), Err(parsed));
        Ok(())
    }
}

//! The `interface-identity` crate exists to spell, parse, and abbreviate every identity a person or agent can name.
//! Its public types are the complete boundary; implementation details remain private.
//! Callers compose capabilities through explicit authority, ownership, and failure values.
//!
//! One readable identity grammar shared by the GUI, the CLI, and the MCP server:
//!
//! ```text
//! address    ::= coordinate [ "::" path ] [ "#" key ]
//! coordinate ::= ecosystem ":" name "@" version
//! path       ::= segment { "::" segment }
//! segment    ::= name [ "[" kind "]" ]
//! key        ::= 32 lower-hex [ "." 32 lower-hex ]
//! ```
//!
//! The typed declaration identity minted by the compiler remains the only truth. Everything in this
//! crate is either a rendering of that truth or a parser that hands the exact spelling back to a
//! resolver. Nothing here fabricates a key, guesses a version, or silently normalizes a path.

mod address;
mod coordinate;
mod ecosystem;
mod exact;
mod hex;
mod key;
mod path;

pub use address::{Address, AddressKey, AddressParseError, MAX_ADDRESS_BYTES};
pub use coordinate::{
    CoordinateParseError, MAX_COORDINATE_BYTES, PackageCoordinate, PackageName, PackageVersion,
};
pub use exact::ExactAddress;
pub use ecosystem::{ALL_ECOSYSTEMS, EcosystemTag, ecosystem_tag, parse_ecosystem_tag};
pub use hex::{HexParseError, KEY_HEX_BYTES};
pub use key::{ContentKey, FamilyDisplay, KeyAbbreviation, KeyExactness, KeyParseError};
pub use path::{KindTag, MAX_PATH_SEGMENTS, PathParseError, PathSegment, SegmentName, SymbolPath};

//! Negative compile-time proof for duplicate content-domain discriminants.
//! The local registry shell mirrors production while deliberately repeating numeric code 41.
//! Successful compilation would make two distinct identity domains wire-equivalent.
struct DomainTag;
struct EncodingTag;
impl DomainTag {
    /// Constructs the minimal tag required by the registry expansion fixture.
    const fn new(_: [u8; 16]) -> Self {
        Self
    }
}
impl EncodingTag {
    /// Constructs the minimal encoding tag required by the registry expansion fixture.
    const fn new(_: [u8; 16]) -> Self {
        Self
    }
}
mod sealed {
    pub trait Domain {}
    pub trait Encoding {}
}
trait Domain: sealed::Domain {
    const TAG: DomainTag;
    const CODE: DomainCode;
}
trait Encoding: sealed::Encoding {
    const TAG: EncodingTag;
    const CODE: EncodingCode;
}
include!("../../src/identity/marker_registry.rs");

protocol_registry!(
    domains {
        (ExistingDomain, Existing, 41, b"1234567890123456"),
        (DuplicateDomain, Duplicate, 41, b"6543210987654321")
    }
    encodings {
        (OneEncoding, One, 73, b"1234567890123456")
    }
);

/// Instantiates the duplicate registry so rustc must reject its repeated discriminant.
fn main() {}

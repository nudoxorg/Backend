//! Negative compile-time proof for duplicate artifact-encoding discriminants.
//! The local registry shell mirrors production while deliberately repeating numeric code 73.
//! Successful compilation would make two physical representations wire-equivalent.
struct DomainTag;
struct EncodingTag;
impl DomainTag {
    /// Constructs the minimal domain tag required by the registry expansion fixture.
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
        (OneDomain, One, 41, b"1234567890123456")
    }
    encodings {
        (ExistingEncoding, Existing, 73, b"1234567890123456"),
        (DuplicateEncoding, Duplicate, 73, b"6543210987654321")
    }
);

/// Instantiates the duplicate registry so rustc must reject its repeated discriminant.
fn main() {}

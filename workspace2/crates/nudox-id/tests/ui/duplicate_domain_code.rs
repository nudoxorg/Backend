struct DomainTag;
struct EncodingTag;
impl DomainTag {
    const fn new(_: [u8; 16]) -> Self {
        Self
    }
}
impl EncodingTag {
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
include!("../../src/marker_registry.rs");

protocol_registry!(
    domains {
        (ExistingDomain, Existing, 41, b"1234567890123456"),
        (DuplicateDomain, Duplicate, 41, b"6543210987654321")
    }
    encodings {
        (OneEncoding, One, 73, b"1234567890123456")
    }
);

fn main() {}

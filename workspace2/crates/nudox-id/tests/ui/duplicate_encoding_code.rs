struct DomainTag;
struct EncodingTag;
impl DomainTag { const fn new(_: [u8; 16]) -> Self { Self } }
impl EncodingTag { const fn new(_: [u8; 16]) -> Self { Self } }
mod sealed { pub trait Domain {} pub trait Encoding {} }
trait Domain: sealed::Domain { const TAG: DomainTag; const CODE: DomainCode; }
trait Encoding: sealed::Encoding { const TAG: EncodingTag; const CODE: EncodingCode; }
include!("../../src/marker_registry.rs");

protocol_registry!(
    domains { (OneDomain, One, 41, b"1234567890123456") }
    encodings { (ExistingEncoding, Existing, 73, b"1234567890123456"), (DuplicateEncoding, Duplicate, 73, b"6543210987654321") }
);

fn main() {}

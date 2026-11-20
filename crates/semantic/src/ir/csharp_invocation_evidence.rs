//! Typed CAS marker for bounded C# captured invocation evidence.
//!
//! Bytes are an engine-owned claim. Selection, source freshness, invocation and
//! compiler-image authority require independent owner admission.
use backend_version::Schema;

/// Maximum complete encoded evidence aggregate retained by the compiler owner.
pub const MAX_CSHARP_INVOCATION_EVIDENCE_BYTES: usize = 64 * 1024 * 1024;

/// Typed CAS object for one staged C# unit's invocation evidence aggregate.
pub struct CSharpInvocationEvidenceSchema;
impl Schema for CSharpInvocationEvidenceSchema {
    const DOMAIN: u8 = 0x7a;
    const TYPE: u16 = 0xc00c;
    const VERSION: u8 = 1;
    type Value = [u8];
    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }
}

//! CAS identity for a complete, versioned TypeScript compiler observation ledger.
//!
//! The payload is opaque to the semantic layer. The engine owns its bounded
//! encoding and replays it through the TypeScript host and program validators.

use backend_version::Schema;

/// Maximum serialized TypeScript read-closure ledger retained for cold reuse.
/// This matches the compiler owner's aggregate in-memory snapshot budget.
pub const MAX_TYPESCRIPT_SNAPSHOT_WITNESS_BYTES: usize = 64 * 1024 * 1024;

/// Typed CAS marker for an engine-owned, versioned TypeScript observation ledger.
pub struct TypeScriptSnapshotWitnessSchema;

impl Schema for TypeScriptSnapshotWitnessSchema {
    const DOMAIN: u8 = 0x7a;
    const TYPE: u16 = 0xc00b;
    const VERSION: u8 = 1;
    type Value = [u8];

    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }
}

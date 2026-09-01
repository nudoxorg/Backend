//! Exercises the primitive-field rejection with its semantic neighbor.
//! Keeps the FFI exception adjacent to the rejected API boundary.
//! Freezes the diagnostic emitted for the rejected field only.
pub struct Count(u64);

pub struct SemanticRecord {
    pub count: Count,
}

pub struct PrimitiveRecord {
    pub count: u64,
}

#[repr(C)]
pub struct AbiRecord {
    pub status: u16,
}

fn main() {}

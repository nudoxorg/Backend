//! Exercises automatic sharing without a diagnostic.
//! Leaves auto-trait derivation to the compiler and avoids unsafe contracts.
//! Proves an ordinary Send and Sync value remains accepted.
pub struct AutomaticCarrier(u64);

pub struct ProvedCarrier(*const u8);

#[expect(
    unsafe_share_contract,
    reason = "the production boundary requires separate Loom and Miri evidence"
)]
unsafe impl Sync for ProvedCarrier {}

fn main() {}

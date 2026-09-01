//! Exercises both manual unsafe sharing contracts.
//! Uses an otherwise minimal raw-pointer carrier as the rejected boundary.
//! Freezes each independent Send and Sync diagnostic.
pub struct PointerCarrier(*const u8);

unsafe impl Send for PointerCarrier {}
unsafe impl Sync for PointerCarrier {}

fn main() {}

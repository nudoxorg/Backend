//! Object schema for owner-attested empty native TypeScript retirements.
//! An object of this schema is an untrusted claim until its seal and selected
//! transition are independently verified by the owner.
use backend_version::Schema;

pub struct NativeTsRetirementReceiptSchema;
impl Schema for NativeTsRetirementReceiptSchema {
    const DOMAIN: u8 = 0x7a;
    const TYPE: u16 = 0xc04a;
    const VERSION: u8 = 1;
    type Value = [u8];
    fn encode(value: &Self::Value, out: &mut Vec<u8>) { out.extend_from_slice(value); }
}

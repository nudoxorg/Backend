//! Exercises caller-bounded borrowing without a diagnostic.
//! Uses an explicit named lifetime to distinguish the valid contract.
//! Proves normal borrowed APIs remain available.
pub fn borrowed<'value>(value: &'value str) -> &'value str {
    value
}

fn main() {}

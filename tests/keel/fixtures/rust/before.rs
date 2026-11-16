/// Calculate the configuration value.
pub fn keel_rust_leaf() -> u32 { 7 }

pub fn keel_rust_middle() -> u32 { keel_rust_leaf() }
pub fn keel_rust_entry() -> u32 { keel_rust_middle() }

// keel_rust_leaf() is only a comment, not a call.
pub fn keel_rust_decoy() -> &'static str { "keel_rust_leaf()" }

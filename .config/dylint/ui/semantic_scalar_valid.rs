//! Exercises the all-semantic-field neighbor without a diagnostic.
//! Keeps primitive representation private inside its semantic wrapper.
//! Proves the lint leaves the public boundary untouched.
pub struct Count(u64);

pub struct SemanticRecord {
    pub count: Count,
}

fn main() {}

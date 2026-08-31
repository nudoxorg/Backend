#![allow(dead_code)]

#[non_exhaustive]
enum PublicationCause {
    Input,
    Unrecognized,
}

#[non_exhaustive]
pub enum PublishError {
    Input,
}

// Non-exhaustive structs are outside this law.
#[non_exhaustive]
struct PublicConfig {
    value: u8,
}

#[non_exhaustive]
enum AdapterRoute {
    Open,
}

fn main() {}

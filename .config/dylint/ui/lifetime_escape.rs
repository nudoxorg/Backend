//! Exercises an explicit static return beside caller-bounded borrowing.
//! Keeps the source signature narrow enough for a stable span snapshot.
//! Freezes the static-reference escape diagnostic.
pub fn borrowed<'value>(value: &'value str) -> &'value str {
    value
}

pub fn leaked() -> &'static str {
    "not capability bound"
}

fn main() {}

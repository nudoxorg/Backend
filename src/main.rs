use crate::traits::{
    builder::{self, get_registry},
    registry::Registry,
};

mod core;
mod error;
mod ir;
mod traits;

fn main() {
    println!("Hello, world!");

    let registry = get_registry(lang_types::Language::Rust);

    dbg!(registry.get_reference());
}

pub mod context;
pub mod error;
pub mod function;
pub mod generics;
pub mod item;
pub mod package;
pub mod types;

pub use self::{error::{Package, Parse, Registry}, package::{Crates, RustPackage}};

pub type Result<T> = std::result::Result<T, Parse>;

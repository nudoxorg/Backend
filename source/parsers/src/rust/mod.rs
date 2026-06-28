pub mod error;
pub mod package;
pub mod context;
pub mod types;
pub mod function;
pub mod item;
pub mod generics;

pub use self::error::{Package, Parse, Registry};
pub use self::package::{Crates, RustPackage};

pub type Result<T> = std::result::Result<T, Parse>;

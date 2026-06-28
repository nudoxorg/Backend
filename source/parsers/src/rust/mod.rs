pub mod error;
pub mod package;
pub mod parse;
pub mod types;
pub mod function;
pub mod item;
pub mod generics;

use self::error::ParseError;

pub type Result<T> = std::result::Result<T, ParseError>;

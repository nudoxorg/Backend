pub mod rust;
pub mod typescript;

use ir::kind::Entry;

pub trait DocParser: Sized {
    type Doc;
    type Error;

    fn from_doc(input: Self::Doc) -> Result<Self, Self::Error>;
    fn parse(&mut self) -> Result<Vec<Entry>, Self::Error>;
}

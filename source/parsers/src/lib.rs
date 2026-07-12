pub mod rust;
pub mod typescript;

use ir::kind::Entry;

pub trait DocParser: Sized {
    type Doc;
    type Error;

    fn from_doc(input: Self::Doc) -> Result<Self, Self::Error>;
    fn parse(&mut self) -> Result<Vec<Entry>, Self::Error>;
}

/// Maps a language-specific visibility representation to [`ir::kind::Visibility`].
pub trait VisibilityMap {
    type RawVis;
    fn visibility(&self, raw: &Self::RawVis) -> ir::kind::Visibility;
}

/// Extracts a receiver kind from the first parameter of a function.
pub trait ReceiverExtract {
    type Param;
    fn receiver(&self, first_param: Option<&Self::Param>) -> Option<ir::protocols::ReceiverKind>;
}

/// Convert an empty vec to `None`, wrapping a non-empty vec in `Some`.
pub(crate) fn empty_to_none<T>(v: Vec<T>) -> Option<Vec<T>> {
    if v.is_empty() { None } else { Some(v) }
}

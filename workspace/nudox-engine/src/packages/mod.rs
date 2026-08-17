//! Getting a package into the corpus: parse a PURL, then fetch and produce it.
//!
//! [`purl`] is the one thing a user can *type* that names a package the corpus
//! has never seen; [`acquire`] turns that typed name into a loaded package via
//! resolve → fetch → verify → extract → produce → insert. Nothing here is a
//! second identity beside `PackageLineageId` — both are inputs and renderings.

pub mod acquire;
pub mod purl;

pub use acquire::{Error as IndexError, IndexEvent, IndexStage, Integrity};
pub use purl::{Error as PurlParseError, Purl, PurlType};

//! Package browsing: the pages that find, judge and compare packages, and
//! show a project its own tree. The Library (your tree) comes first.

pub mod acquire;
pub mod action;
pub mod compare;
pub mod find;
pub mod library;
mod view;

pub use library::{
    Alert, Library, Model as LibraryModel, Role as LibraryRole, Row as LibraryRow, TWICE_AT_REST,
    Tone as AlertTone, Twice, library,
};

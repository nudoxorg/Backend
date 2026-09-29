//! The symbol page, simple form: docs.rs's reading order, drawn.
//!
//! header · the call (one rail: each input a port, the rail's end how it can
//! end) · docs · if it fails · what it is (one of / holds) · what you can do
//! with it · in your workspace, with the context rail beside it.
//!
//! [`derive`] is pure (facts in, a [`View`] out); [`page`] lays a view out.

pub mod derive;
mod body;
mod board;
mod call;
mod card;
#[cfg(feature = "gallery")]
pub(crate) mod gallery;
pub mod facts;
pub mod host;
pub mod ink;
pub mod key;
mod kit;
pub mod layout;
mod page;
mod side;
pub mod view;
mod workspace;

pub use derive::{compile, with_uses};
pub use host::{Act, Change, Fixed, Host, Spots, Ui};
pub use page::{Chrome, gem, page};
pub use facts::Facts;
pub use view::View;

//! Stateful render entities: the window and the panels it composes.
//! One entity owns the stores, the focus, and every action; panels only draw.
//! This is the only layer in the crate that needs a platform window to exist.

pub(crate) mod actions;
mod browse;
pub(crate) mod chrome;
mod context;
mod home;
pub(crate) mod keys;
pub(crate) mod library;
mod omnibar;
mod overlays;
mod package;
mod palette;
mod page;
mod project;
mod reader;
mod source;
mod status;
pub(crate) mod workspace;

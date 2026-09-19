//! Stateful render entities: the window and the panels it composes.
//! One entity owns the stores, the focus, and every action; panels only draw.
//! This is the only layer in the crate that needs a platform window to exist.

pub(crate) mod actions;
mod context;
mod first_run;
pub(crate) mod library;
mod omnibar;
mod overlays;
mod page;
mod project;
mod reader;
mod status;
pub(crate) mod workspace;

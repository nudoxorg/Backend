//! The shelf region. It lives in [`super::side`] (the sidebar: scope, lenses,
//! narrowing, state on rows); this module keeps the names the rest of the
//! shell reaches it by.

#[cfg(test)]
pub(crate) use super::side::TESTS_ROW;
pub(crate) use super::side::{Shelf, is_test_module, shelf_name};

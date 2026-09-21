//! Stateless element builders: the whole visual vocabulary, in one place.
//! Every element takes the lit theme and returns a `Div` a view can decorate.
//! No builder holds state, opens a window, or writes a colour literal.
//!
//! The split between `ui` and `views` is what keeps the design consistent
//! across nine surfaces: a shelf row and a search result differ in what they
//! say, not in how they are drawn, because both are assembled from the same
//! tiles, chips, and text rungs.

pub(crate) mod bar;
pub(crate) mod button;
pub(crate) mod chart;
pub(crate) mod chip;
pub(crate) mod components;
pub(crate) mod fault;
pub(crate) mod glyph;
pub(crate) mod icon;
pub(crate) mod prose;
pub(crate) mod source;
pub(crate) mod specimen;
pub(crate) mod surface;
pub(crate) mod text;
pub(crate) mod tip;

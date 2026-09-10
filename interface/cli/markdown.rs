//! Defines markdown behavior for `interface-cli`, whose purpose is to project the one shared local library onto a command line.
//! This module owns the markdown invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! The one call site for `--format markdown`: the shared renderer, never a second spelling of it.
//!
//! `nudox --format markdown` exists so a person can paste a reply into an issue, a review, or an
//! agent transcript and get exactly what the MCP server would have produced. That is only true if
//! this module forwards; the moment it renders anything itself, the two surfaces have drifted and
//! nobody finds out until a reader compares two transcripts.

use interface_library::{Reply, render::common::RenderContext};

/// Renders one reply as the Markdown the MCP server produces.
#[must_use]
pub(crate) fn render(reply: &Reply, context: &RenderContext) -> String {
    interface_library::render::markdown::render(reply, context)
}

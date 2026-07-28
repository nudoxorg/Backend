//! The document outline — a sidebar that exists before the document does.
//!
//! # Why this is not a table of contents
//!
//! A table of contents is derived from a parsed document, which means it cannot
//! appear until the document has been parsed. docs.rs works this way, and so
//! does every static-site generator: the sidebar and the body arrive together.
//!
//! Ours is derived from `SymbolHead::section_plan`, which §9.3 guarantees is the
//! *first* event on the stream — before a single section has been chunked. So
//! the reader gets a navigable map of the page in the same frame as the header,
//! and can jump to "Examples" while "Examples" is still being parsed. Entries
//! that have not landed yet are rendered muted, so the outline doubles as an
//! honest progress display: you can watch the document fill in.
//!
//! Labels start as the plan's `SectionKind` name and are *refined* in place when
//! the section arrives carrying a real heading ([`super::docs::DocsBody`] does
//! that refinement). The entry never moves; only its text sharpens.
//!
//! # Scroll sync
//!
//! The body's `ListState` scroll handler reports a visible item range; the page
//! forwards its first index here via [`Outline::set_active`]. Because body item
//! indices and outline entry indices are the *same* index — both are positions
//! in `section_plan` — the sync is an assignment, not a search.

use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, InteractiveElement as _, IntoElement, ParentElement, ScrollStrategy,
    SharedString, StatefulInteractiveElement as _, Styled, UniformListScrollHandle, Window, div,
    prelude::FluentBuilder as _, uniform_list,
};
use gpui_component::StyledExt as _;
use nudox_engine::wire::SectionKind;

use crate::theme::ext::ThemeExtAccessor as _;
use crate::ui::SectionHeader;

use super::docs::DocsBody;

/// One row in the outline. Mirrors one entry of `section_plan`, permanently.
#[derive(Clone, Debug, PartialEq)]
pub struct OutlineEntry {
    /// Current label: the kind name, or the section's heading once known.
    pub label: SharedString,
    /// `true` while the label is still the plan's placeholder.
    pub pending: bool,
    /// Nested one level under the preceding entry.
    pub indented: bool,
}

/// Sections that read as subordinate to the prose around them.
fn is_nested(kind: SectionKind) -> bool {
    matches!(
        kind,
        SectionKind::CodeBlock | SectionKind::Callout | SectionKind::Examples
    )
}

/// The outline sidebar.
pub struct Outline {
    entries: Arc<[OutlineEntry]>,
    active: usize,
    scroll: UniformListScrollHandle,
    /// Pre-formatted "n / m" progress caption, rebuilt only when it changes.
    progress: SharedString,
}

impl Default for Outline {
    fn default() -> Self {
        Self::new()
    }
}

impl Outline {
    /// An empty outline.
    pub fn new() -> Self {
        Self {
            entries: Arc::from(Vec::new()),
            active: 0,
            scroll: UniformListScrollHandle::new(),
            progress: SharedString::from(""),
        }
    }

    /// Whether there is anything to show.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Number of entries (== number of planned sections).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Rebuild from the body's slots. Returns `true` if anything changed.
    ///
    /// Called from the store observation, never from `render` — every
    /// `SharedString` below is built here and merely cloned per frame.
    pub fn sync(&mut self, docs: &DocsBody) -> bool {
        let slots = docs.slots();
        let next: Vec<OutlineEntry> = slots
            .iter()
            .map(|slot| OutlineEntry {
                label: slot.label.clone(),
                pending: slot.is_pending(),
                indented: is_nested(slot.kind),
            })
            .collect();

        let arrived = docs.arrived_count().min(slots.len());
        let progress = if slots.is_empty() || arrived >= slots.len() {
            SharedString::from("")
        } else {
            SharedString::from(format!("{arrived}/{}", slots.len()))
        };

        let changed = next.as_slice() != &*self.entries || progress != self.progress;
        if changed {
            self.entries = Arc::from(next);
            self.progress = progress;
            if self.active >= self.entries.len() {
                self.active = self.entries.len().saturating_sub(1);
            }
        }
        changed
    }

    /// Point the outline at the section currently at the top of the viewport.
    ///
    /// Returns `true` if the highlight moved (the caller then notifies), so a
    /// scroll that stays inside one section costs no frame.
    pub fn set_active(&mut self, ix: usize) -> bool {
        if ix == self.active || ix >= self.entries.len() {
            return false;
        }
        self.active = ix;
        // Keep the highlighted entry on screen in long outlines, without
        // yanking it to the centre.
        self.scroll.scroll_to_item(ix, ScrollStrategy::Top);
        true
    }

    /// The currently highlighted entry index.
    pub fn active(&self) -> usize {
        self.active
    }

    /// Render the sidebar. `on_pick` receives a `section_plan` index.
    pub fn render(
        &self,
        on_pick: impl Fn(usize, &mut Window, &mut App) + 'static,
        cx: &App,
    ) -> AnyElement {
        // Both tokens are `Copy`, so the theme borrow ends here and nothing
        // downstream is constrained by it.
        let (sp, colours) = {
            let ext = cx.theme_ext();
            (ext.space, ext.colours)
        };

        let entries = self.entries.clone();
        let count = entries.len();
        let active = self.active;
        let pick: Rc<dyn Fn(usize, &mut Window, &mut App)> = Rc::new(on_pick);

        let header = {
            let mut h = SectionHeader::new(SharedString::from("ON THIS PAGE"));
            if !self.progress.is_empty() {
                // "3/12" while the plan is still filling in — the outline is a
                // progress display as well as a map.
                h = h.count(self.progress.clone());
            }
            h
        };

        div()
            .id("symbol.outline")
            .v_flex()
            .w(sp.space_8 * 5.0)
            .flex_shrink_0()
            .h_full()
            .border_l_1()
            .border_color(colours.border_default)
            .bg(colours.bg_raised)
            .child(header)
            .child(
                // LD-6: a 200-section document has a 200-row outline.
                uniform_list(
                    "symbol.outline.list",
                    count,
                    move |range, _window, cx| {
                        let ext = cx.theme_ext();
                        let sp = ext.space;
                        let ts = ext.type_scale;
                        let colours = ext.colours;
                        range
                            .map(|ix| {
                                let entry = &entries[ix];
                                let is_active = ix == active;
                                let pick = pick.clone();
                                let colour = if is_active {
                                    colours.fg_default
                                } else if entry.pending {
                                    // Not here yet — visibly so.
                                    colours.fg_faint
                                } else {
                                    colours.fg_muted
                                };

                                div()
                                    .id(("symbol.outline.row", ix))
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .h(ts.dense.line_height + sp.space_2)
                                    .pr(sp.space_2)
                                    .cursor_pointer()
                                    .hover(|s| s.bg(colours.bg_hover))
                                    .on_click(move |_, window, cx| pick(ix, window, cx))
                                    // A 2 px accent rail marks the section you
                                    // are reading; every row reserves the rail
                                    // so nothing shifts as it moves.
                                    .child(
                                        div()
                                            .w(sp.focus_ring_width)
                                            .h_full()
                                            .flex_shrink_0()
                                            .when(is_active, |s| s.bg(colours.accent)),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .overflow_hidden()
                                            .truncate()
                                            .pl(if entry.indented {
                                                sp.space_4
                                            } else {
                                                sp.space_2
                                            })
                                            .text_size(ts.dense.size)
                                            .line_height(ts.dense.line_height)
                                            .text_color(colour)
                                            .child(entry.label.clone()),
                                    )
                            })
                            .collect::<Vec<_>>()
                    },
                )
                .track_scroll(&self.scroll)
                .flex_1(),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(label: &'static str, pending: bool) -> OutlineEntry {
        OutlineEntry {
            label: SharedString::from(label),
            pending,
            indented: false,
        }
    }

    /// Code, callout and example sections read as subordinate; prose, members
    /// and fields are top-level landmarks.
    #[test]
    fn nesting_rule_is_stable() {
        assert!(is_nested(SectionKind::CodeBlock));
        assert!(is_nested(SectionKind::Callout));
        assert!(is_nested(SectionKind::Examples));
        assert!(!is_nested(SectionKind::Prose));
        assert!(!is_nested(SectionKind::Members));
        assert!(!is_nested(SectionKind::Fields));
    }

    /// `set_active` is a no-op when the target is already active — a scroll
    /// that stays inside one section must not cost a frame.
    #[test]
    fn set_active_is_idempotent() {
        let mut o = Outline::new();
        o.entries = Arc::from(vec![entry("a", false), entry("b", false)]);
        assert!(o.set_active(1));
        assert!(!o.set_active(1));
        assert_eq!(o.active(), 1);
    }

    /// Out-of-range indices are ignored rather than panicking: a scroll event
    /// can race a generation change.
    #[test]
    fn set_active_ignores_out_of_range() {
        let mut o = Outline::new();
        o.entries = Arc::from(vec![entry("a", false)]);
        assert!(!o.set_active(99));
        assert_eq!(o.active(), 0);
    }

    /// A brand-new outline is empty and renders nothing rather than a stub.
    #[test]
    fn new_outline_is_empty() {
        let o = Outline::new();
        assert!(o.is_empty());
        assert_eq!(o.len(), 0);
    }
}

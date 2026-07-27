//! `Toolbar` — the horizontal filter/action strip above a list (GUI-PLAN §11).
//!
//! Used by search, logs, refs and the package browser. It exists so those four
//! surfaces cannot drift apart: a segmented filter in the log panel and one in
//! the refs table are the same component with different labels, not two
//! lookalikes that slowly diverge in padding and hover behaviour.
//!
//! # Selection is the caller's state
//!
//! The toolbar renders chips and reports clicks; it does not own which chip is
//! active. That belongs to a store, because the filter is part of the query the
//! store issues (§7.4). A toolbar that owned its own selection would be a second
//! source of truth for what the user asked for.

use gpui::{
    AnyElement, App, ElementId, InteractiveElement as _, IntoElement, ParentElement, RenderOnce,
    SharedString, StatefulInteractiveElement as _, Styled, Window, div,
};

use crate::theme::ext::ThemeExtAccessor as _;
use gpui::prelude::*;

/// One segmented-filter chip.
pub struct ToolbarChip {
    id: ElementId,
    label: SharedString,
    active: bool,
    on_click: Option<Box<dyn Fn(&mut Window, &mut App) + 'static>>,
}

impl ToolbarChip {
    /// A chip with a stable id (LD-19) and a pre-computed label (§1.1.4).
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            active: false,
            on_click: None,
        }
    }

    /// Mark this chip as the active filter.
    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    /// Report clicks upward.
    pub fn on_click(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

/// A toolbar: a row of chips, an optional trailing slot, on a raised surface.
#[derive(IntoElement)]
pub struct Toolbar {
    id: ElementId,
    chips: Vec<ToolbarChip>,
    /// Trailing content — typically a search input or a count label.
    trailing: Option<AnyElement>,
}

impl Toolbar {
    /// An empty toolbar.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            chips: Vec::new(),
            trailing: None,
        }
    }

    /// Append a filter chip.
    pub fn chip(mut self, chip: ToolbarChip) -> Self {
        self.chips.push(chip);
        self
    }

    /// Append several chips.
    pub fn chips(mut self, chips: impl IntoIterator<Item = ToolbarChip>) -> Self {
        self.chips.extend(chips);
        self
    }

    /// Set the trailing slot (search input, count, overflow menu).
    pub fn trailing(mut self, element: impl IntoElement) -> Self {
        self.trailing = Some(element.into_any_element());
        self
    }
}

impl RenderOnce for Toolbar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let ext = cx.theme_ext();
        let space = ext.space;
        let colours = ext.colours;
        let caption = ext.type_scale.caption;

        let chips = self.chips.into_iter().map(move |chip| {
            let (bg, fg) = if chip.active {
                (colours.accent, colours.accent_fg_on)
            } else {
                (colours.bg_hover, colours.fg_muted)
            };

            let mut el = div()
                .id(chip.id)
                .px(space.space_2)
                .py(space.space_1)
                .rounded(space.r_sm)
                .bg(bg)
                .text_size(caption.size)
                .line_height(caption.line_height)
                .text_color(fg)
                .child(chip.label);

            if let Some(on_click) = chip.on_click {
                el = el
                    .cursor_pointer()
                    // `hover:` is a GPUI style state, not an animation: it costs
                    // no notify and no frame (§5.1 `hover.tint`).
                    .hover(|s| s.bg(colours.bg_active))
                    .on_click(move |_, window, cx| on_click(window, cx));
            }
            el
        });

        let mut bar = div()
            .id(self.id)
            .flex()
            .items_center()
            .gap(space.space_2)
            .px(space.space_3)
            .py(space.space_2)
            .bg(colours.bg_raised)
            .border_b_1()
            .border_color(colours.border_default)
            .children(chips);

        if let Some(trailing) = self.trailing {
            bar = bar.child(div().flex_1()).child(trailing);
        }

        bar
    }
}

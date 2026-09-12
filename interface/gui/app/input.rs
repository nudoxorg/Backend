//! Defines the native input bridge for `interface-gui`.
//! This module owns the one text composition state the platform writes into and the two fields it
//! can address. Its narrow surface keeps UTF-16 platform coordinates out of the stores.

use core::ops::Range;
use std::rc::Rc;

use gpui::{
    Bounds, Context, ElementInputHandler, EntityInputHandler, FocusHandle, IntoElement, Pixels,
    Point, UTF16Selection, Window, canvas, point, prelude::*, px, size,
};

use crate::app::Workspace;

/// Which field the platform is currently composing into.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FieldTarget {
    /// The omnibar at the top of the shell.
    Omnibar,
    /// The coordinate field inside the add flow.
    AddCoordinate,
}

/// One field's geometry, remembered from the last layout so the platform can place its IME.
#[derive(Clone, Copy)]
pub struct FieldGeometry {
    /// Which field occupies the bounds.
    pub target: FieldTarget,
    /// The bounds it was laid out at.
    pub bounds: Bounds<Pixels>,
}

/// The remembered bounds, shared with the canvas that records them during layout.
pub type SharedFieldGeometry = Rc<std::cell::Cell<Option<FieldGeometry>>>;

/// Platform composition state for the active field.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NativeInputState {
    target: Option<FieldTarget>,
    selection: Range<usize>,
    marked: Option<Range<usize>>,
}

impl NativeInputState {
    /// Which field is active, when one is.
    #[must_use]
    pub const fn target(&self) -> Option<FieldTarget> {
        self.target
    }

    /// The selection in UTF-16 coordinates, for tests that drive the platform path.
    #[must_use]
    pub fn selection(&self) -> Range<usize> {
        self.selection.clone()
    }
}

impl Workspace {
    /// Mounts the invisible canvas that registers the native input handler for one field. Exactly
    /// the two fields in the shell mount it, so the platform can never compose into chrome.
    pub(crate) fn field_bridge(
        &self,
        target: FieldTarget,
        focus: &FocusHandle,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let entity = cx.entity();
        let focus = focus.clone();
        let bounds = Rc::clone(&self.field_bounds);
        canvas(
            move |_, _, _| {},
            move |layout, (), window, cx| {
                bounds.set(Some(FieldGeometry {
                    target,
                    bounds: layout,
                }));
                window.handle_input(&focus, ElementInputHandler::new(layout, entity), cx);
            },
        )
        .absolute()
        .size_full()
    }

    /// Points the composition state at whichever field holds focus, resetting the caret when the
    /// target moves. Called once per frame before the fields are drawn.
    pub(crate) fn synchronize_native_target(&mut self, window: &Window) {
        let target = self.active_field(window);
        if self.native_input.target != target {
            let end = target.map_or(0, |target| self.field_text(target).encode_utf16().count());
            self.native_input = NativeInputState {
                target,
                selection: end..end,
                marked: None,
            };
        }
    }

    fn field_text(&self, target: FieldTarget) -> &str {
        match target {
            FieldTarget::Omnibar => self.search.text(),
            FieldTarget::AddCoordinate => &self.store.add.text,
        }
    }

    fn active_field(&self, window: &Window) -> Option<FieldTarget> {
        if self.add_focus.is_focused(window) {
            return Some(FieldTarget::AddCoordinate);
        }
        if self.omnibar_focus.is_focused(window) {
            return Some(FieldTarget::Omnibar);
        }
        None
    }

    fn replace_native_text(
        &mut self,
        range: Option<Range<usize>>,
        replacement: &str,
        cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        let target = self.native_input.target?;
        let current = self.field_text(target);
        let range_utf16 = range.unwrap_or_else(|| self.native_input.selection.clone());
        let range = byte_range_from_utf16(current, range_utf16);
        let insertion_start = current[..range.start].encode_utf16().count();
        let mut value = String::with_capacity(current.len() + replacement.len());
        value.push_str(&current[..range.start]);
        value.push_str(replacement);
        value.push_str(&current[range.end..]);
        match target {
            FieldTarget::Omnibar => {
                self.search.retype(value);
                self.arm_search_debounce(cx);
            }
            FieldTarget::AddCoordinate => self.store.add.retype(value),
        }
        let inserted = insertion_start..insertion_start + replacement.encode_utf16().count();
        self.native_input.selection = inserted.end..inserted.end;
        self.native_input.marked = None;
        Some(inserted)
    }
}

impl EntityInputHandler for Workspace {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let target = self.native_input.target?;
        let text = self.field_text(target);
        let bytes = byte_range_from_utf16(text, range);
        adjusted_range.replace(utf16_range_for_bytes(text, bytes.clone()));
        Some(text[bytes].to_owned())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.native_input.selection.clone(),
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.native_input.marked.clone()
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.native_input.marked = None;
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.replace_native_text(range, text, cx).is_some() {
            cx.notify();
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let inserted = match self.replace_native_text(range, new_text, cx) {
            Some(inserted) => inserted,
            None => return,
        };
        self.native_input.marked = Some(inserted.clone());
        if let Some(selection) = new_selected_range {
            let selection =
                utf16_range_for_bytes(new_text, byte_range_from_utf16(new_text, selection));
            let start = inserted
                .start
                .saturating_add(selection.start)
                .min(inserted.end);
            let end = inserted
                .start
                .saturating_add(selection.end)
                .min(inserted.end);
            self.native_input.selection = start..end;
        }
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let target = self.native_input.target?;
        let text = self.field_text(target);
        let normalized = utf16_range_for_bytes(text, byte_range_from_utf16(text, range));
        let length = text.encode_utf16().count().max(1);
        let start_ratio = normalized.start as f32 / length as f32;
        let width_ratio = normalized.end.saturating_sub(normalized.start) as f32 / length as f32;
        Some(Bounds::new(
            point(
                bounds.left() + bounds.size.width * start_ratio,
                bounds.top(),
            ),
            size(
                if width_ratio == 0.0 {
                    px(1.0)
                } else {
                    bounds.size.width * width_ratio
                },
                bounds.size.height,
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let target = self.active_field(window)?;
        let geometry = self.field_bounds.get()?;
        if geometry.target != target {
            return None;
        }
        let bounds = geometry.bounds;
        let text = self.field_text(target);
        let length = text.encode_utf16().count();
        if length == 0 || bounds.size.width == Pixels::ZERO {
            return Some(0);
        }
        let ratio = ((position.x - bounds.left()) / bounds.size.width).clamp(0.0, 1.0);
        let mut wanted = 0;
        for candidate in 1..=length {
            if ratio < (candidate as f32 - 0.5) / length as f32 {
                break;
            }
            wanted = candidate;
        }
        Some(wanted.min(length))
    }

    fn set_selected_text_range(
        &mut self,
        range: Range<usize>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        if let Some(target) = self.native_input.target {
            let text = self.field_text(target);
            self.native_input.selection =
                utf16_range_for_bytes(text, byte_range_from_utf16(text, range));
        }
    }

    fn text_length_utf16(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let target = self.native_input.target?;
        Some(self.field_text(target).encode_utf16().count())
    }

    fn accepts_text_input(&self, window: &mut Window, _cx: &mut Context<Self>) -> bool {
        self.active_field(window).is_some()
    }
}

fn byte_range_from_utf16(text: &str, range: Range<usize>) -> Range<usize> {
    let start = byte_offset_from_utf16(text, range.start);
    let end = byte_offset_from_utf16(text, range.end);
    start.min(end)..start.max(end)
}

fn byte_offset_from_utf16(text: &str, wanted: usize) -> usize {
    let mut utf16 = 0;
    for (offset, character) in text.char_indices() {
        let next = utf16 + character.len_utf16();
        if wanted <= utf16 || wanted < next {
            return offset;
        }
        utf16 = next;
    }
    text.len()
}

fn utf16_range_for_bytes(text: &str, range: Range<usize>) -> Range<usize> {
    text[..range.start].encode_utf16().count()..text[..range.end].encode_utf16().count()
}

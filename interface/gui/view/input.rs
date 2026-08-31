//! Defines view input behavior for `interface-gui`, whose purpose is to render and control the unified application service through GPUI.
//! This module owns the view input invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Native GPUI text composition projected into fixed application input storage.

use core::ops::Range;
use std::rc::Rc;

use gpui::{
    Bounds, Context, ElementInputHandler, EntityInputHandler, FocusHandle, IntoElement, Pixels,
    Point, UTF16Selection, Window, canvas, point, prelude::*, px, size,
};
use interface_core::{InputText, InputTextJoinError};

use crate::{FormError, FormField, FormState, NativeTextInputError, TextInputTarget};

use super::GpuiShellView;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct NativeInputState {
    target: Option<TextInputTarget>,
    selection: Range<usize>,
    marked: Option<Range<usize>>,
}

#[derive(Clone, Copy)]
pub(super) struct NativeInputGeometry {
    target: TextInputTarget,
    bounds: Bounds<Pixels>,
}

pub(super) type SharedNativeInputGeometry = Rc<std::cell::Cell<Option<NativeInputGeometry>>>;

impl GpuiShellView {
    pub(super) fn native_input_bridge(
        &self,
        target: TextInputTarget,
        focus: &FocusHandle,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let entity = cx.entity();
        let focus = focus.clone();
        let input_bounds = Rc::clone(&self.native_input_bounds);
        canvas(
            |_, _, _| (),
            move |bounds, (), window, cx| {
                input_bounds.set(Some(NativeInputGeometry { target, bounds }));
                window.handle_input(&focus, ElementInputHandler::new(bounds, entity), cx);
            },
        )
        .absolute()
        .size_full()
    }

    fn active_text_target(&self) -> Option<TextInputTarget> {
        if self.state.navigation.palette.visible {
            return Some(TextInputTarget::Palette);
        }
        match self.state.form.as_ref()? {
            FormState::Generate { focused, .. }
            | FormState::Snapshot { focused, .. }
            | FormState::Search { focused, .. } => Some(TextInputTarget::Form(*focused)),
            FormState::Health | FormState::Recovery { .. } | FormState::Operation { .. } => None,
        }
    }

    fn text_for_target(&self, target: TextInputTarget) -> &str {
        match target {
            TextInputTarget::Palette => self
                .state
                .navigation
                .palette
                .query_text()
                .map_or("", |query| query),
            TextInputTarget::Form(field) => match form_text(self.state.form.as_ref(), field) {
                Some(text) => text,
                None => "",
            },
        }
    }

    fn synchronize_native_target(&mut self) -> Option<TextInputTarget> {
        let target = self.active_text_target()?;
        if self.native_input.target != Some(target) {
            let end = self.text_for_target(target).encode_utf16().count();
            self.native_input = NativeInputState {
                target: Some(target),
                selection: end..end,
                marked: None,
            };
            self.state.retain_input_error(None);
            if matches!(self.state.form_error, Some(FormError::InputTooLong { .. })) {
                self.state.retain_form_error(None);
            }
        }
        Some(target)
    }

    fn replace_native_text(
        &mut self,
        requested_range: Option<Range<usize>>,
        replacement: &str,
    ) -> Result<Option<Range<usize>>, NativeTextInputError> {
        let Some(target) = self.synchronize_native_target() else {
            return Ok(None);
        };
        let current = self.text_for_target(target);
        let range_utf16 = requested_range.unwrap_or_else(|| self.native_input.selection.clone());
        let range = byte_range_from_utf16(current, range_utf16);
        let value = match InputText::try_from_parts(
            &current[..range.start],
            replacement,
            &current[range.end..],
        ) {
            Ok(value) => value,
            Err(source) => {
                let error = match source {
                    InputTextJoinError::InputTooLong(source) => {
                        NativeTextInputError::InputTooLong {
                            target,
                            actual: source.actual,
                            maximum: source.maximum,
                        }
                    }
                    InputTextJoinError::LengthOverflow {
                        prefix,
                        inserted,
                        suffix,
                    } => NativeTextInputError::InputLengthOverflow {
                        target,
                        prefix,
                        inserted,
                        suffix,
                    },
                };
                self.retain_native_input_error(error);
                return Err(error);
            }
        };
        let insertion_start = current[..range.start].encode_utf16().count();
        self.replace_target_text(target, value);
        self.state.retain_input_error(None);

        let inserted = insertion_start..insertion_start + replacement.encode_utf16().count();
        self.native_input.selection = inserted.end..inserted.end;
        self.native_input.marked = None;
        Ok(Some(inserted))
    }

    fn replace_target_text(&mut self, target: TextInputTarget, value: InputText) {
        match target {
            TextInputTarget::Palette => self.state.replace_palette_query(value),
            TextInputTarget::Form(field) => match self.state.replace_form_text(field, value) {
                Ok(()) => {}
                Err(error) => debug_assert_eq!(self.state.form_error, Some(error)),
            },
        }
    }

    fn retain_native_input_error(&mut self, error: NativeTextInputError) {
        self.state.retain_input_error(Some(error));
        match error {
            NativeTextInputError::InputTooLong {
                target: TextInputTarget::Form(field),
                actual,
                maximum,
            } => self.state.retain_form_error(Some(FormError::InputTooLong {
                field,
                actual,
                maximum,
            })),
            NativeTextInputError::InputLengthOverflow {
                target: TextInputTarget::Form(field),
                prefix,
                inserted,
                suffix,
            } => self
                .state
                .retain_form_error(Some(FormError::InputLengthOverflow {
                    field,
                    prefix,
                    inserted,
                    suffix,
                })),
            NativeTextInputError::InputTooLong {
                target: TextInputTarget::Palette,
                ..
            }
            | NativeTextInputError::InputLengthOverflow {
                target: TextInputTarget::Palette,
                ..
            } => {}
        }
    }
}

impl EntityInputHandler for GpuiShellView {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let target = self.synchronize_native_target()?;
        let text = self.text_for_target(target);
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
        self.synchronize_native_target()?;
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
        (self.active_text_target() == self.native_input.target)
            .then(|| self.native_input.marked.clone())
            .flatten()
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.native_input.marked = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(self.replace_native_text(range, text), Ok(None)) {
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
        let inserted = match self.replace_native_text(range, new_text) {
            Ok(Some(inserted)) => inserted,
            Err(error) => {
                debug_assert_eq!(self.state.input_error, Some(error));
                cx.notify();
                return;
            }
            Ok(None) => return,
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
        let target = self.synchronize_native_target()?;
        let text = self.text_for_target(target);
        let normalized = utf16_range_for_bytes(text, byte_range_from_utf16(text, range));
        let length = text.encode_utf16().count().max(1);
        let length_coordinate = bounded_utf16_coordinate(length);
        let start_ratio = bounded_utf16_coordinate(normalized.start) / length_coordinate;
        let width_ratio = bounded_utf16_coordinate(normalized.end.saturating_sub(normalized.start))
            / length_coordinate;
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
        point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let target = self.synchronize_native_target()?;
        let geometry = self.native_input_bounds.get()?;
        if geometry.target != target {
            return None;
        }
        let bounds = geometry.bounds;
        let length = self.text_for_target(target).encode_utf16().count();
        if length == 0 || bounds.size.width == Pixels::ZERO {
            return Some(0);
        }
        let ratio = ((point.x - bounds.left()) / bounds.size.width).clamp(0.0, 1.0);
        let length_coordinate = bounded_utf16_coordinate(length);
        let mut wanted = 0;
        for candidate in 1..=length {
            let midpoint = (bounded_utf16_coordinate(candidate) - 0.5) / length_coordinate;
            if ratio < midpoint {
                break;
            }
            wanted = candidate;
        }
        Some(normalized_utf16_offset(
            self.text_for_target(target),
            wanted,
        ))
    }

    fn set_selected_text_range(
        &mut self,
        range: Range<usize>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = self.synchronize_native_target() else {
            return;
        };
        let text = self.text_for_target(target);
        self.native_input.selection =
            utf16_range_for_bytes(text, byte_range_from_utf16(text, range));
        cx.notify();
    }

    fn text_length_utf16(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let target = self.synchronize_native_target()?;
        Some(self.text_for_target(target).encode_utf16().count())
    }

    fn accepts_text_input(&self, _window: &mut Window, _cx: &mut Context<Self>) -> bool {
        self.active_text_target().is_some()
    }
}

fn form_text(form: Option<&FormState>, field: FormField) -> Option<&InputText> {
    match form? {
        FormState::Generate {
            language,
            stage,
            source,
            ..
        } => match field {
            FormField::Language => language.as_ref(),
            FormField::Stage => stage.as_ref(),
            FormField::Source => source.as_ref(),
            FormField::Snapshot | FormField::Query => None,
        },
        FormState::Snapshot { snapshot, .. } if field == FormField::Snapshot => snapshot.as_ref(),
        FormState::Search {
            snapshot, query, ..
        } => match field {
            FormField::Snapshot => snapshot.as_ref(),
            FormField::Query => query.as_ref(),
            FormField::Language | FormField::Stage | FormField::Source => None,
        },
        FormState::Snapshot { .. }
        | FormState::Health
        | FormState::Recovery { .. }
        | FormState::Operation { .. } => None,
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

fn normalized_utf16_offset(text: &str, wanted: usize) -> usize {
    let byte = byte_offset_from_utf16(text, wanted);
    text[..byte].encode_utf16().count()
}

fn bounded_utf16_coordinate(value: usize) -> f32 {
    let mut coordinate = 0.0;
    for _ in 0..value {
        coordinate += 1.0;
    }
    coordinate
}

fn utf16_range_for_bytes(text: &str, range: Range<usize>) -> Range<usize> {
    text[..range.start].encode_utf16().count()..text[..range.end].encode_utf16().count()
}

//! Native GPUI text composition projected into fixed application input storage.

use core::ops::Range;

use gpui::{
    Bounds, Context, ElementInputHandler, EntityInputHandler, IntoElement, Pixels, Point,
    UTF16Selection, Window, canvas, prelude::*,
};
use wave_application_core::{INPUT_TEXT_BYTES, InputText};

use crate::{FormError, FormField, FormState};

use super::GpuiShellView;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TextTarget {
    Palette,
    Form(FormField),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct NativeInputState {
    target: Option<TextTarget>,
    selection: Range<usize>,
    marked: Option<Range<usize>>,
    error: Option<NativeInputError>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NativeInputError {
    TooLong {
        target: TextTarget,
        actual: usize,
        maximum: usize,
    },
}

impl GpuiShellView {
    pub(super) fn native_input_bridge(&self, cx: &Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let focus = if self.state.navigation.palette.visible {
            self.palette_focus.clone()
        } else {
            self.focus.clone()
        };
        canvas(
            |_, _, _| (),
            move |bounds, (), window, cx| {
                window.handle_input(&focus, ElementInputHandler::new(bounds, entity), cx);
            },
        )
        .absolute()
        .size_full()
    }

    pub(super) const fn native_input_error_message(&self) -> Option<&'static str> {
        match self.native_input.error {
            Some(NativeInputError::TooLong { .. }) => {
                Some("Input exceeds the fixed local field limit.")
            }
            None => None,
        }
    }

    fn active_text_target(&self) -> Option<TextTarget> {
        if self.state.navigation.palette.visible {
            return Some(TextTarget::Palette);
        }
        match self.state.form.as_ref()? {
            FormState::Generate { focused, .. }
            | FormState::Snapshot { focused, .. }
            | FormState::Search { focused, .. } => Some(TextTarget::Form(*focused)),
            FormState::Health | FormState::Recovery { .. } | FormState::Operation { .. } => None,
        }
    }

    fn text_for_target(&self, target: TextTarget) -> &str {
        let value = match target {
            TextTarget::Palette => {
                return self
                    .state
                    .navigation
                    .palette
                    .query_text()
                    .unwrap_or_default();
            }
            TextTarget::Form(field) => form_text(self.state.form.as_ref(), field),
        };
        value
            .and_then(|text| core::str::from_utf8(text.as_ref()).ok())
            .unwrap_or_default()
    }

    fn synchronize_native_target(&mut self) -> Option<TextTarget> {
        let target = self.active_text_target()?;
        if self.native_input.target != Some(target) {
            let end = self.text_for_target(target).encode_utf16().count();
            self.native_input = NativeInputState {
                target: Some(target),
                selection: end..end,
                marked: None,
                error: None,
            };
        }
        Some(target)
    }

    fn replace_native_text(
        &mut self,
        requested_range: Option<Range<usize>>,
        replacement: &str,
    ) -> Option<Range<usize>> {
        let target = self.synchronize_native_target()?;
        let current = self.text_for_target(target);
        let range_utf16 = requested_range.unwrap_or_else(|| self.native_input.selection.clone());
        let range = byte_range_from_utf16(current, range_utf16);
        let next_length = current
            .len()
            .checked_sub(range.end.saturating_sub(range.start))?
            .checked_add(replacement.len())?;
        if next_length > INPUT_TEXT_BYTES {
            self.retain_native_input_error(target, next_length);
            return None;
        }

        let mut bytes = [0_u8; INPUT_TEXT_BYTES];
        bytes[..range.start].copy_from_slice(&current.as_bytes()[..range.start]);
        let inserted_end = range.start + replacement.len();
        bytes[range.start..inserted_end].copy_from_slice(replacement.as_bytes());
        bytes[inserted_end..next_length].copy_from_slice(&current.as_bytes()[range.end..]);
        let next = core::str::from_utf8(&bytes[..next_length]).ok()?;
        let value = InputText::try_from_str(next).ok()?;
        let insertion_start = current[..range.start].encode_utf16().count();
        self.replace_target_text(target, value);
        self.native_input.error = None;

        let inserted = insertion_start..insertion_start + replacement.encode_utf16().count();
        self.native_input.selection = inserted.end..inserted.end;
        self.native_input.marked = None;
        Some(inserted)
    }

    fn replace_target_text(&mut self, target: TextTarget, value: InputText) {
        match target {
            TextTarget::Palette => self.state.replace_palette_query(value),
            TextTarget::Form(field) => {
                let error = self.state.replace_form_text(field, value).err();
                self.state.retain_form_error(error);
            }
        }
    }

    fn retain_native_input_error(&mut self, target: TextTarget, actual: usize) {
        self.native_input.error = Some(NativeInputError::TooLong {
            target,
            actual,
            maximum: INPUT_TEXT_BYTES,
        });
        if let TextTarget::Form(field) = target {
            self.state.retain_form_error(Some(FormError::InputTooLong {
                field,
                actual,
                maximum: INPUT_TEXT_BYTES,
            }));
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
        if self.replace_native_text(range, text).is_some() {
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
        let Some(inserted) = self.replace_native_text(range, new_text) else {
            return;
        };
        self.native_input.marked = Some(inserted.clone());
        if let Some(selection) = new_selected_range {
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
        _range: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        Some(bounds)
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        self.synchronize_native_target()?;
        Some(self.native_input.selection.end)
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
        let length = self.text_for_target(target).encode_utf16().count();
        self.native_input.selection = range.start.min(length)..range.end.min(length);
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
            package,
            source,
            ..
        } => match field {
            FormField::Language => language.as_ref(),
            FormField::Stage => stage.as_ref(),
            FormField::Package => package.as_ref(),
            FormField::Source => source.as_ref(),
            FormField::Snapshot | FormField::Query => None,
        },
        FormState::Snapshot { snapshot, .. } if field == FormField::Snapshot => snapshot.as_ref(),
        FormState::Search {
            snapshot, query, ..
        } => match field {
            FormField::Snapshot => snapshot.as_ref(),
            FormField::Query => query.as_ref(),
            FormField::Language | FormField::Stage | FormField::Package | FormField::Source => None,
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
        if utf16 >= wanted {
            return offset;
        }
        utf16 += character.len_utf16();
    }
    text.len()
}

fn utf16_range_for_bytes(text: &str, range: Range<usize>) -> Range<usize> {
    text[..range.start].encode_utf16().count()..text[..range.end].encode_utf16().count()
}

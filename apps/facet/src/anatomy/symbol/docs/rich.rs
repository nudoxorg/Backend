//! Reading and activation use the exact StyledText layout that paints prose.
//! Native endpoints paint no second text. Pointer admission remains a UTF-8
//! glyph-range check, including both ends of a mouse click.

use super::super::host::Act;
use gpui::{
    AnyElement, App, AvailableSpace, Bounds, ClickEvent, Element, ElementId, FocusHandle,
    GlobalElementId, HighlightStyle, InspectorElementId, InteractiveElement as _, InteractiveText,
    IntoElement, LayoutId, Pixels, SharedString, StatefulInteractiveElement as _, Styled as _,
    StyledText, TextLayout, Window, div, point, px, size,
};
use std::{ops::Range, rc::Rc, sync::Arc};

/// One authored span resolved by the current page host.
pub struct Link {
    pub range: Range<usize>,
    pub destination: SharedString,
    pub activate: Act,
    /// The same current-page guard used for pointer focus and every action.
    pub admission: Option<crate::controls::button::ActivationAdmission>,
}

/// A paragraph with one shaping/layout authority and scoped native endpoints.
pub struct RichText {
    id: ElementId,
    words: SharedString,
    layout: TextLayout,
    glyphs: AnyElement,
    spans: Vec<Span>,
}

struct Span {
    range: Range<usize>,
    link: Option<Link>,
}

/// Shape the owned body once; resolved links share that body's UTF-8 ranges.
pub fn rich_text(
    id: impl Into<ElementId>,
    words: SharedString,
    highlights: Vec<(Range<usize>, HighlightStyle)>,
    mut links: Vec<Link>,
) -> RichText {
    links.retain(|link| {
        link.range.start < link.range.end
            && link.range.end <= words.len()
            && words.is_char_boundary(link.range.start)
            && words.is_char_boundary(link.range.end)
    });
    links.sort_by_key(|link| link.range.start);
    let mut spans = Vec::new();
    let mut after = 0;
    for link in links {
        // A malformed overlapping reference cannot produce duplicate actions.
        if link.range.start < after {
            continue;
        }
        if after < link.range.start {
            spans.push(Span {
                range: after..link.range.start,
                link: None,
            });
        }
        after = link.range.end;
        spans.push(Span {
            range: link.range.clone(),
            link: Some(link),
        });
    }
    if after < words.len() {
        spans.push(Span {
            range: after..words.len(),
            link: None,
        });
    }
    let glyphs = StyledText::new(words.clone()).with_highlights(highlights);
    let layout = glyphs.layout().clone();
    let (ranges, acts): (Vec<_>, Vec<_>) = spans
        .iter()
        .filter_map(|span| {
            span.link.as_ref().map(|link| {
                (
                    link.range.clone(),
                    (Rc::clone(&link.activate), link.admission.clone()),
                )
            })
        })
        .unzip();
    let glyphs =
        InteractiveText::new("doc-glyphs", glyphs).on_click(ranges, move |which, window, cx| {
            if let Some((act, admission)) = acts.get(which) {
                if admission.as_ref().is_none_or(|admit| admit(cx)) {
                    act(window, cx);
                }
            }
        });
    RichText {
        id: id.into(),
        words,
        layout,
        glyphs: glyphs.into_any_element(),
        spans,
    }
}

struct Row {
    range: Range<usize>,
    source_start: usize,
    y: Pixels,
    x_origin: Pixels,
    x_start: Pixels,
    layout: Arc<gpui::WrappedLineLayout>,
}

impl Row {
    fn fragment(&self, range: &Range<usize>, height: Pixels) -> Option<Bounds<Pixels>> {
        let start = range.start.max(self.range.start);
        let end = range.end.min(self.range.end);
        if start >= end {
            return None;
        }
        let x0 = self
            .layout
            .unwrapped_layout
            .x_for_index(start - self.source_start);
        let x1 = self
            .layout
            .unwrapped_layout
            .x_for_index(end - self.source_start);
        (x1 > x0).then(|| {
            Bounds::from_corners(
                point(self.x_origin + x0 - self.x_start, self.y),
                point(self.x_origin + x1 - self.x_start, self.y + height),
            )
        })
    }
}

fn rows(layout: &TextLayout, mask: Bounds<Pixels>) -> Vec<Row> {
    let bounds = layout.bounds();
    let height = layout.line_height();
    if height <= px(0.) || bounds.intersect(&mask).size.height <= px(0.) {
        return Vec::new();
    }
    let mut rows = Vec::new();
    let mut y = bounds.top();
    let mut source_start = 0;
    for line in layout.line_layouts() {
        let count = line.wrap_boundaries.len() + 1;
        let bottom = y + height * count;
        if bottom > mask.top() && y < mask.bottom() {
            // Retain partially visible native rows. Exact glyph fragments
            // are intersected with this mask before publishing an endpoint.
            let first = ((mask.top() - y) / height).floor().max(0.) as usize;
            let after = ((mask.bottom() - y) / height).ceil().max(0.) as usize;
            let boundary = |row: usize| {
                line.wrap_boundaries
                    .get(row)
                    .map_or(line.len(), |boundary| {
                        line.unwrapped_layout.runs[boundary.run_ix].glyphs[boundary.glyph_ix].index
                    })
            };
            for row in first.min(count)..after.min(count) {
                let start = if row == 0 { 0 } else { boundary(row - 1) };
                rows.push(Row {
                    range: source_start + start..source_start + boundary(row),
                    source_start,
                    y: y + height * row,
                    x_origin: bounds.left(),
                    x_start: line.unwrapped_layout.x_for_index(start),
                    layout: Arc::clone(&line),
                });
            }
        }
        y = bottom;
        source_start += line.len() + 1;
    }
    rows
}

impl IntoElement for RichText {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for RichText {
    type RequestLayoutState = ();
    type PrepaintState = Vec<AnyElement>;
    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }
    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.glyphs.request_layout(window, cx), ())
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Vec<AnyElement> {
        self.glyphs.prepaint(window, cx);
        if window.is_inert_subtree() {
            return Vec::new();
        }
        let mask = window.content_mask().bounds;
        let rows = rows(&self.layout, mask);
        let mut elements = Vec::new();
        for span in &self.spans {
            let Some(words) = self
                .words
                .get(span.range.clone())
                .filter(|words| !words.trim().is_empty())
            else {
                continue;
            };
            let Some(bounds) = rows
                .iter()
                .filter_map(|row| row.fragment(&span.range, self.layout.line_height()))
                .map(|bounds| bounds.intersect(&mask))
                .find(|bounds| bounds.size.width > px(0.) && bounds.size.height > px(0.))
            else {
                continue;
            };
            // The destination is part of the actual endpoint's identity. A
            // changed reference cannot inherit its predecessor's focus.
            let endpoint: SharedString = format!(
                "doc-span-{}-{}-{}",
                span.range.start,
                span.range.end,
                span.link
                    .as_ref()
                    .map_or("", |link| link.destination.as_ref())
            )
            .into();
            let mut native = div()
                .id(endpoint.clone())
                .role(gpui::Role::Label)
                .aria_label(words.to_owned())
                .w(bounds.size.width)
                .h(bounds.size.height);
            if let Some(link) = &span.link {
                let focus = window.use_keyed_state((endpoint, 0), cx, |_, cx| cx.focus_handle());
                let focus: FocusHandle = focus.read(cx).clone();
                let activate = Rc::clone(&link.activate);
                let admission = link.admission.clone();
                if let Some(admit) = &admission {
                    native = crate::controls::button::capture_activation_admission(
                        native,
                        admit.clone(),
                    );
                }
                let destination = link.destination.clone();
                native = crate::controls::button::native_button_with_event(
                    native
                        .role(gpui::Role::Link)
                        .a11y_synthetic_children(move |builder| {
                            builder.parent_node().set_url(destination.to_string())
                        }),
                    &focus,
                    move |event, window, cx| {
                        // InteractiveText owns exact pointer glyph admission
                        // across every wrapped line. This endpoint owns the
                        // native keyboard and normalized accessibility Click.
                        if matches!(event, ClickEvent::Keyboard(_))
                            && admission.as_ref().is_none_or(|admit| admit(cx))
                        {
                            activate(window, cx);
                        }
                    },
                );
            } else if !window.is_a11y_active() {
                continue;
            }
            let mut native = native.into_any_element();
            native.prepaint_as_root(
                bounds.origin,
                size(
                    AvailableSpace::Definite(bounds.size.width),
                    AvailableSpace::Definite(bounds.size.height),
                ),
                window,
                cx,
            );
            elements.push(native);
        }
        elements
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        native: &mut Vec<AnyElement>,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.glyphs.paint(window, cx);
        for element in native {
            element.paint(window, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_and_overlapping_links_cannot_duplicate_or_split_owned_unicode_body() {
        let words: SharedString = "café tick".into();
        let links = [4..5, 8..20, 7..6, 0..5, 3..7, 6..10]
            .into_iter()
            .map(|range| Link {
                range,
                destination: "crate::Tick".into(),
                activate: Rc::new(|_, _| {}),
                admission: None,
            })
            .collect();
        let body = rich_text("unicode", words.clone(), vec![], links);
        let reconstructed: String = body
            .spans
            .iter()
            .map(|span| &words[span.range.clone()])
            .collect();
        assert_eq!(
            reconstructed,
            words.as_ref(),
            "plain and linked spans partition the actual body"
        );
        let labelled: Vec<_> = body
            .spans
            .iter()
            .filter(|span| span.link.is_some())
            .map(|span| &words[span.range.clone()])
            .collect();
        assert_eq!(labelled, ["café", "tick"]);
    }
}

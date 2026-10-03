//! Native document semantics share the shaped text and its link callback.
//!
//! These elements do not paint a second copy of the words or introduce broad
//! pointer click regions. Inline's existing glyph hit test owns pointer input.
//! A visible link gets GPUI's real focus handle and accessibility action; the
//! application callback still decides whether its address is current.

use std::{
    collections::BTreeSet,
    hash::{DefaultHasher, Hash, Hasher},
    ops::Range,
    sync::Arc,
};

use gpui::{
    AccessibleAction, AnyElement, App, AvailableSpace, Bounds, ClickEvent, Element, ElementId,
    Entity, FocusHandle, GlobalElementId, InspectorElementId, InteractiveElement as _,
    IntoElement as _, LayoutId, Pixels, SharedString, StatefulInteractiveElement as _, Styled as _,
    TextLayout, Window, div, point, px, size,
};

use crate::{ActiveTheme as _, global_state::UiGlobalState};

const CONTEXT: &str = "TextViewLink";

pub(super) fn init(cx: &mut App) {
    // Keymap dispatch precedes raw key events. Suppress an enclosing Shell's
    // activation/zone binding while this actual link owns native focus, then
    // let its raw handler preserve modifier and held-key semantics.
    cx.bind_keys(
        ["enter", "space", "tab", "shift-tab"]
            .map(|key| gpui::KeyBinding::new(key, gpui::NoAction {}, Some(CONTEXT))),
    );
}

use super::{
    node::LinkMark,
    state::TextViewState,
    text_view::{LinkAvailabilityFn, LinkClickHandlerFn, handle_link_click, link_available},
};

#[derive(Debug, PartialEq)]
struct Span {
    range: Range<usize>,
    link: Option<usize>,
}

// First mark wins, just as Inline::link_for_position does. Separate emphasis
// runs for the same destination merge into one reading/focus endpoint.
fn spans(text: &str, links: &[(Range<usize>, LinkMark)]) -> Vec<Span> {
    let mut edges = vec![(0, false, usize::MAX), (text.len(), false, usize::MAX)];
    for (ix, (range, _)) in links.iter().enumerate() {
        if range.start < range.end
            && range.end <= text.len()
            && text.is_char_boundary(range.start)
            && text.is_char_boundary(range.end)
        {
            edges.push((range.start, true, ix));
            edges.push((range.end, false, ix));
        }
    }
    edges.sort_unstable();
    let mut active = BTreeSet::<usize>::new();
    let mut result: Vec<Span> = Vec::new();
    let mut previous = 0;
    let mut at = 0;
    while at < edges.len() {
        let offset = edges[at].0;
        if previous < offset {
            let link = active.first().copied();
            let same = result.last().is_some_and(|last| match (last.link, link) {
                (None, None) => true,
                (Some(a), Some(b)) => links[a].1.url == links[b].1.url,
                _ => false,
            });
            if same {
                if let Some(last) = result.last_mut() {
                    last.range.end = offset;
                }
            } else {
                result.push(Span {
                    range: previous..offset,
                    link,
                });
            }
        }
        while at < edges.len() && edges[at].0 == offset {
            let (_, start, ix) = edges[at];
            if ix != usize::MAX {
                if start {
                    active.insert(ix);
                } else {
                    active.remove(&ix);
                }
            }
            at += 1;
        }
        previous = offset;
    }
    result
}

struct VisibleRow {
    range: Range<usize>,
    y: Pixels,
    x_origin: Pixels,
    x_start: Pixels,
    layout: Arc<gpui::WrappedLineLayout>,
    source_start: usize,
}

impl VisibleRow {
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

// Walk laid-out lines, skipping whole offscreen blocks and offscreen wrap
// rows. Work on glyph boundaries, never a second per-character shape walk.
fn visible_rows(
    whole_rows: bool,
    layout: &TextLayout,
    bounds: Bounds<Pixels>,
    mask: Bounds<Pixels>,
) -> Vec<VisibleRow> {
    let height = layout.line_height();
    if height <= px(0.) || bounds.intersect(&mask).size.height <= px(0.) {
        return Vec::new();
    }
    let mut rows = Vec::new();
    let mut y = bounds.top();
    let mut source_start = 0;
    for line in layout.line_layouts() {
        let count = line.wrap_boundaries.len() + 1;
        let block_bottom = y + height * count;
        if block_bottom > mask.top() && y < mask.bottom() {
            let first = (if whole_rows {
                ((mask.top() - y) / height).ceil()
            } else {
                ((mask.top() - y) / height).floor()
            })
            .max(0.) as usize;
            let after = (if whole_rows {
                ((mask.bottom() - y) / height).floor()
            } else {
                ((mask.bottom() - y) / height).ceil()
            })
            .max(0.) as usize;
            let boundary = |row: usize| {
                line.wrap_boundaries
                    .get(row)
                    .map_or(line.len(), |boundary| {
                        line.unwrapped_layout.runs[boundary.run_ix].glyphs[boundary.glyph_ix].index
                    })
            };
            for row in first..after.min(count) {
                let start = if row == 0 { 0 } else { boundary(row - 1) };
                let end = boundary(row);
                rows.push(VisibleRow {
                    range: (source_start + start)..(source_start + end),
                    y: y + height * row,
                    x_origin: bounds.left(),
                    x_start: line.unwrapped_layout.x_for_index(start),
                    layout: Arc::clone(&line),
                    source_start,
                });
            }
        }
        y = block_bottom;
        source_start += line.len() + 1;
    }
    rows
}

struct LinkFocus {
    handle: FocusHandle,
}

fn endpoint_id(range: &Range<usize>, words: &str, url: &SharedString) -> SharedString {
    let mut digest = DefaultHasher::new();
    words.hash(&mut digest);
    url.hash(&mut digest);
    format!(
        "native-link-{}-{}-{:016x}",
        range.start,
        range.end,
        digest.finish()
    )
    .into()
}

fn activate(
    url: SharedString,
    availability: &Option<Arc<LinkAvailabilityFn>>,
    handler: &Option<Arc<LinkClickHandlerFn>>,
    state: &Option<Entity<TextViewState>>,
    admission: &Option<super::text_view::LinkAdmission>,
    event: ClickEvent,
    window: &mut Window,
    cx: &mut App,
) {
    if !admitted(admission, cx)
        || !link_available(availability, &url)
        || state
            .as_ref()
            .is_some_and(|state| state.read(cx).has_selection(cx))
    {
        return;
    }
    gpui_base::TextSelection::end(window, cx);
    cx.stop_propagation();
    handle_link_click(handler, url, event, window, cx);
}

pub(super) fn current_admission(cx: &App) -> Option<super::text_view::LinkAdmission> {
    UiGlobalState::global(cx).text_view_admission().cloned()
}

pub(super) fn admitted(predicate: &Option<super::text_view::LinkAdmission>, cx: &mut App) -> bool {
    predicate.as_ref().is_none_or(|predicate| predicate(cx))
}

fn clamped(cx: &App) -> bool {
    UiGlobalState::global(cx).text_view_clamped()
}

/// One focus/action surface for a currently visible authored link. No pointer
/// handler is installed here; the caller retains its exact glyph/image hit test.
pub(super) fn link_element(
    id: SharedString,
    label: SharedString,
    url: SharedString,
    dimensions: gpui::Size<Pixels>,
    availability: Option<Arc<LinkAvailabilityFn>>,
    handler: Option<Arc<LinkClickHandlerFn>>,
    window: &mut Window,
    cx: &mut App,
) -> gpui::Stateful<gpui::Div> {
    let focus = window.use_keyed_state((id.clone(), 0), cx, |_, cx| LinkFocus {
        handle: cx.focus_handle().tab_stop(true),
    });
    let focus = focus.read(cx).handle.clone();
    let state = UiGlobalState::global(cx).text_view_state().cloned();
    let ring = cx.theme().ring;
    let native_url = url.clone();
    let key_url = url.clone();
    let key_availability = availability.clone();
    let key_handler = handler.clone();
    let key_state = state.clone();
    let admission = current_admission(cx);
    let traversal_admission = admission.clone();
    let focus_admission = admission.clone();
    let key_admission = admission.clone();
    let ax_admission = admission.clone();
    let accessible_focus = focus.clone();
    div()
        .id(id)
        .role(gpui::Role::Link)
        .aria_label(label)
        .w(dimensions.width)
        .h(dimensions.height)
        .track_focus(&focus)
        .tab_index(0)
        .key_context(CONTEXT)
        .focus_visible(move |style| style.border_1().border_color(ring))
        .a11y_synthetic_children(move |builder| {
            builder.parent_node().set_url(native_url.to_string())
        })
        .on_a11y_action(AccessibleAction::Click, move |_, window, cx| {
            activate(
                url.clone(),
                &availability,
                &handler,
                &state,
                &ax_admission,
                ClickEvent::default(),
                window,
                cx,
            );
        })
        .on_a11y_action(AccessibleAction::Focus, move |_, window, cx| {
            if admitted(&focus_admission, cx) {
                window.focus(&accessible_focus, cx);
            }
        })
        .capture_any_mouse_down(move |_, window, cx| {
            if admission.as_ref().is_some_and(|admit| !admit(cx)) {
                window.prevent_default();
                cx.stop_propagation();
            }
        })
        .on_click(move |event, window, cx| {
            // Exact glyph/image pointer handlers below own mouse/touch input.
            // GPUI produces this keyboard click only after a clean release.
            if matches!(event, ClickEvent::Keyboard(_)) {
                activate(
                    key_url.clone(),
                    &key_availability,
                    &key_handler,
                    &key_state,
                    &key_admission,
                    event.clone(),
                    window,
                    cx,
                );
            }
        })
        .on_key_down(move |event, window, cx| {
            if !admitted(&traversal_admission, cx) {
                cx.stop_propagation();
                return;
            }
            let mut modifiers = event.keystroke.modifiers;
            modifiers.shift = false;
            if event.keystroke.key == "tab" && !modifiers.modified() && !event.is_held {
                if event.keystroke.modifiers.shift {
                    window.focus_prev(cx);
                } else {
                    window.focus_next(cx);
                }
                cx.stop_propagation();
            } else if !event.keystroke.modifiers.modified()
                && matches!(event.keystroke.key.as_str(), "enter" | "space")
            {
                cx.stop_propagation();
            }
        })
}

pub(super) fn inline_elements(
    text: &str,
    links: &[(Range<usize>, LinkMark)],
    layout: &TextLayout,
    bounds: Bounds<Pixels>,
    availability: &Option<Arc<LinkAvailabilityFn>>,
    handler: &Option<Arc<LinkClickHandlerFn>>,
    window: &mut Window,
    cx: &mut App,
) -> Vec<AnyElement> {
    let accessible = window.is_a11y_active();
    let mask = window.content_mask().bounds;
    if bounds.intersect(&mask).size.height <= px(0.) && !accessible {
        return Vec::new();
    }
    let clamped = clamped(cx);
    let rows = visible_rows(clamped, layout, bounds, mask);
    let visible_range = rows
        .first()
        .zip(rows.last())
        .map(|(first, last)| first.range.start..last.range.end);
    let mut elements = Vec::new();
    for span in spans(text, links) {
        let range = if clamped {
            let Some(visible) = &visible_range else {
                continue;
            };
            span.range.start.max(visible.start)..span.range.end.min(visible.end)
        } else {
            span.range.clone()
        };
        let Some(words) = text.get(range.clone()).filter(|words| !words.is_empty()) else {
            continue;
        };
        let link = span
            .link
            .map(|ix| &links[ix].1)
            .filter(|link| link_available(availability, &link.url));
        let visible = rows
            .iter()
            .find_map(|row| row.fragment(&span.range, layout.line_height()))
            .map(|bounds| bounds.intersect(&mask))
            .filter(|bounds| bounds.size.width > px(0.));
        let mut element = if let (Some(link), Some(visible)) = (link, visible) {
            link_element(
                endpoint_id(&span.range, words, &link.url),
                words.to_string().into(),
                link.url.clone(),
                visible.size,
                availability.clone(),
                handler.clone(),
                window,
                cx,
            )
            .into_any_element()
        } else if accessible {
            let mut node = div()
                .id(("native-words", span.range.start))
                .role(gpui::Role::Label)
                .aria_label(words.to_string())
                .w(bounds.size.width)
                .h(bounds.size.height);
            if let Some(link) = link {
                let url = link.url.clone();
                node = node
                    .role(gpui::Role::Link)
                    .a11y_synthetic_children(move |builder| {
                        builder.parent_node().set_url(url.to_string())
                    });
            }
            node.into_any_element()
        } else {
            continue;
        };
        let native_bounds = visible.unwrap_or(bounds);
        element.prepaint_as_root(
            native_bounds.origin,
            size(
                AvailableSpace::Definite(native_bounds.size.width),
                AvailableSpace::Definite(native_bounds.size.height),
            ),
            window,
            cx,
        );
        elements.push(element);
    }
    elements
}

pub(super) struct Image {
    id: ElementId,
    image: AnyElement,
    label: SharedString,
    link: Option<LinkMark>,
    availability: Option<Arc<LinkAvailabilityFn>>,
    handler: Option<Arc<LinkClickHandlerFn>>,
}

pub(super) fn image(
    id: impl Into<ElementId>,
    image: AnyElement,
    label: SharedString,
    link: Option<LinkMark>,
    availability: Option<Arc<LinkAvailabilityFn>>,
    handler: Option<Arc<LinkClickHandlerFn>>,
) -> Image {
    Image {
        id: id.into(),
        image,
        label,
        link,
        availability,
        handler,
    }
}

impl gpui::IntoElement for Image {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for Image {
    type RequestLayoutState = ();
    type PrepaintState = Option<AnyElement>;
    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }
    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.image.request_layout(window, cx), ())
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        self.image.prepaint(window, cx);
        let visible = bounds.intersect(&window.content_mask().bounds);
        let complete = visible.size.width > px(0.)
            && visible.size.height > px(0.)
            && (!clamped(cx) || visible == bounds);
        let link = self
            .link
            .as_ref()
            .filter(|link| link_available(&self.availability, &link.url));
        let mut native = if complete && let Some(link) = link {
            let label = if self.label.is_empty() {
                link.title
                    .clone()
                    .filter(|title| !title.is_empty())
                    .unwrap_or_else(|| link.url.clone())
            } else {
                self.label.clone()
            };
            link_element(
                endpoint_id(&(0..self.label.len()), &self.label, &link.url),
                label,
                link.url.clone(),
                visible.size,
                self.availability.clone(),
                self.handler.clone(),
                window,
                cx,
            )
            .into_any_element()
        } else if window.is_a11y_active()
            && !self.label.is_empty()
            && visible.size.width > px(0.)
            && visible.size.height > px(0.)
        {
            div()
                .id("native-image-description")
                .role(gpui::Role::Image)
                .aria_label(self.label.clone())
                .w(visible.size.width)
                .h(visible.size.height)
                .into_any_element()
        } else {
            return None;
        };
        native.prepaint_as_root(
            visible.origin,
            size(
                AvailableSpace::Definite(visible.size.width),
                AvailableSpace::Definite(visible.size.height),
            ),
            window,
            cx,
        );
        Some(native)
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        native: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.image.paint(window, cx);
        if let Some(native) = native {
            native.paint(window, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ElementExt as _;
    use gpui::{AppContext as _, Context, Render, TestAppContext, VisualTestContext};
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    };
    use std::{cell::Cell, rc::Rc};

    gpui::actions!(text_view_native_tests, [HostActivate, HostTraverse]);

    fn native_press(cx: &mut VisualTestContext, key: &str) {
        let keystroke = gpui::Keystroke {
            key: key.into(),
            modifiers: Default::default(),
            key_char: None,
        };
        cx.simulate_event(gpui::KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        cx.simulate_event(gpui::KeyUpEvent { keystroke });
    }

    fn link(url: &str) -> LinkMark {
        LinkMark {
            url: url.to_owned().into(),
            ..Default::default()
        }
    }

    #[test]
    fn native_runs_keep_source_order_utf8_and_pointer_precedence() {
        let links = vec![
            (4..10, link("first")),
            (0..8, link("second")),
            (1..2, link("broken utf8")),
        ];
        assert_eq!(
            spans("éabcdefghijkl", &links),
            vec![
                Span {
                    range: 0..4,
                    link: Some(1)
                },
                Span {
                    range: 4..10,
                    link: Some(0)
                },
                Span {
                    range: 10..14,
                    link: None
                },
            ]
        );
        let styled = vec![(0..2, link("same")), (2..6, link("same"))];
        assert_eq!(
            spans("bolded", &styled),
            vec![Span {
                range: 0..6,
                link: Some(0)
            }]
        );
    }

    #[test]
    fn focus_identity_changes_with_actual_words_and_destination() {
        let id = endpoint_id(&(0..4), "docs", &"https://one".into());
        assert_ne!(id, endpoint_id(&(0..4), "docs", &"https://two".into()));
        assert_ne!(id, endpoint_id(&(0..4), "code", &"https://one".into()));
    }

    struct RowsDocument(Rc<Cell<bool>>);
    impl Render for RowsDocument {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl gpui::IntoElement {
            use gpui::ParentElement as _;
            let text: SharedString = "état 🦀 repeated words wrap into narrow rows".into();
            let shaped = gpui::StyledText::new(text.clone());
            let layout = shaped.layout().clone();
            let observed = self.0.clone();
            div()
                .w(px(90.))
                .ml(px(35.))
                .mt(px(45.))
                .child(shaped)
                .on_prepaint(move |bounds, _, _| {
                    let height = layout.line_height();
                    let mask = Bounds::new(
                        point(bounds.left(), bounds.top() + height / 2.),
                        size(bounds.size.width, height),
                    );
                    let partial = visible_rows(false, &layout, bounds, mask);
                    assert_eq!(partial.len(), 2, "partially painted rows stay focusable");
                    assert!(
                        visible_rows(true, &layout, bounds, mask).is_empty(),
                        "whole-line clamp omits both cut rows"
                    );
                    for row in partial {
                        assert!(text.is_char_boundary(row.range.start));
                        assert!(text.is_char_boundary(row.range.end));
                        let fragment = row
                            .fragment(&row.range, height)
                            .expect("real glyph extent")
                            .intersect(&mask);
                        assert!(fragment.left() >= bounds.left());
                        assert!(fragment.top() >= mask.top() && fragment.bottom() <= mask.bottom());
                    }
                    observed.set(true);
                })
        }
    }

    #[gpui::test]
    fn wrapped_unicode_rows_use_original_byte_indices_and_partial_viewport(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::init);
        let observed = Rc::new(Cell::new(false));
        let result = observed.clone();
        let (_, cx) = cx.add_window_view(|window, cx| {
            crate::Root::new(cx.new(|_| RowsDocument(result)), window, cx)
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        assert!(observed.get());
    }

    struct Document {
        text: Entity<TextViewState>,
        opened: Arc<Mutex<Vec<String>>>,
        live: Arc<AtomicBool>,
        clamp: Option<usize>,
        extensions: Option<super::super::MarkdownExtensions>,
    }

    impl Render for Document {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl gpui::IntoElement {
            use gpui::{ParentElement as _, prelude::FluentBuilder as _};
            let opened = Arc::clone(&self.opened);
            let live = Arc::clone(&self.live);
            let host = Arc::clone(&self.opened);
            let traverse = Arc::clone(&self.opened);
            div()
                .size_full()
                .key_context("HostDocument")
                .on_action::<HostActivate>(move |_, _, _| host.lock().unwrap().push("HOST".into()))
                .on_action::<HostTraverse>(move |_, _, _| {
                    traverse.lock().unwrap().push("HOST TAB".into())
                })
                .child(
                    super::super::TextView::new(&self.text)
                        .selectable(true)
                        .link_availability(|url| url.starts_with("https://"))
                        .link_admission(Rc::new(move |_| live.load(Ordering::SeqCst)))
                        .on_link_click(move |url, _, _, _| {
                            // Admission, rather than a second callback guard, must
                            // reject stale native and pointer delivery.
                            opened.lock().unwrap().push(url.to_string());
                        })
                        .when_some(self.clamp, |this, clamp| this.max_lines(clamp))
                        .when_some(self.extensions.clone(), |this, extensions| {
                            this.markdown_extensions(extensions)
                        }),
                )
        }
    }

    fn mounted<'a>(
        source: &str,
        clamp: Option<usize>,
        extensions: Option<super::super::MarkdownExtensions>,
        cx: &'a mut TestAppContext,
    ) -> (
        Arc<Mutex<Vec<String>>>,
        Arc<AtomicBool>,
        &'a mut VisualTestContext,
    ) {
        cx.update(|cx| {
            crate::init(cx);
            cx.bind_keys([
                gpui::KeyBinding::new("enter", HostActivate, Some("HostDocument")),
                gpui::KeyBinding::new("space", HostActivate, Some("HostDocument")),
                gpui::KeyBinding::new("tab", HostTraverse, Some("HostDocument")),
            ]);
        });
        let opened = Arc::new(Mutex::new(Vec::new()));
        let live = Arc::new(AtomicBool::new(true));
        let source = source.to_owned();
        let urls = Arc::clone(&opened);
        let current = Arc::clone(&live);
        let (_, cx) = cx.add_window_view(move |window, cx| {
            window.set_a11y_forced(true);
            let document = cx.new(|cx| Document {
                text: cx.new(|cx| TextViewState::markdown(&source, cx)),
                opened: urls,
                live: current,
                clamp,
                extensions,
            });
            crate::Root::new(document, window, cx)
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        (opened, live, cx)
    }

    #[gpui::test]
    fn document_native_keyboard_uses_the_existing_callback_and_late_guard(cx: &mut TestAppContext) {
        let (opened, live, cx) =
            mounted("[Documentation](https://docs.rs/example)", None, None, cx);
        let node = cx.update(|window, _| {
            window
                .a11y_tree()
                .unwrap()
                .nodes
                .iter()
                .find(|(_, node)| {
                    node.role() == gpui::Role::Link && node.label() == Some("Documentation")
                })
                .map(|(id, _)| *id)
                .expect("actual native Documentation link")
        });
        action(cx, node, AccessibleAction::Focus);
        let keystroke = gpui::Keystroke {
            key: "enter".into(),
            modifiers: Default::default(),
            key_char: None,
        };
        cx.simulate_event(gpui::KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        assert!(
            opened.lock().unwrap().is_empty(),
            "key down alone cannot navigate"
        );
        cx.simulate_event(gpui::KeyUpEvent { keystroke });
        native_press(cx, "space");
        cx.simulate_keystrokes("tab");
        action(cx, node, AccessibleAction::Focus);
        native_press(cx, "enter");
        assert_eq!(*opened.lock().unwrap(), vec!["https://docs.rs/example"; 3]);
        live.store(false, Ordering::SeqCst);
        native_press(cx, "enter");
        assert_eq!(opened.lock().unwrap().len(), 3);
    }

    fn action(cx: &mut VisualTestContext, node: gpui::accesskit::NodeId, action: AccessibleAction) {
        cx.update(|window, cx| {
            window.simulate_a11y_action(
                gpui::accesskit::ActionRequest {
                    action,
                    target_tree: gpui::accesskit::TreeId::ROOT,
                    target_node: node,
                    data: None,
                },
                cx,
            )
        });
    }

    #[gpui::test]
    fn real_native_tree_words_bounds_and_late_admission_share_one_link(cx: &mut TestAppContext) {
        let (opened, live, cx) = mounted(
            "Use [état 🦀](https://docs.rs/current) now.",
            None,
            None,
            cx,
        );
        let (node, link_point, plain_point) = cx.update(|window, _| {
            let tree = window.a11y_tree().expect("committed native document");
            let (id, link) = tree
                .nodes
                .iter()
                .find(|(_, node)| {
                    node.role() == gpui::Role::Link && node.label() == Some("état 🦀")
                })
                .expect("actual shaped authored link");
            assert_eq!(link.url(), Some("https://docs.rs/current"));
            let bounds = link.bounds().expect("actual glyph fragment");
            assert!(bounds.width() > 0. && bounds.height() > 0.);
            assert!(
                tree.nodes
                    .iter()
                    .any(|(_, node)| node.label() == Some("Use "))
            );
            assert!(
                tree.nodes
                    .iter()
                    .any(|(_, node)| node.label() == Some(" now."))
            );
            let plain = tree
                .nodes
                .iter()
                .find(|(_, node)| node.label() == Some("Use "))
                .unwrap()
                .1
                .bounds()
                .unwrap();
            // AccessKit bounds are physical pixels; simulate_click takes
            // logical window pixels, including on this test window's 2x scale.
            let scale = window.scale_factor() as f64;
            let center = |rect: gpui::accesskit::Rect| {
                point(
                    px(((rect.x0 + rect.x1) / (2. * scale)) as f32),
                    px(((rect.y0 + rect.y1) / (2. * scale)) as f32),
                )
            };
            (*id, center(bounds), center(plain))
        });
        cx.simulate_click(plain_point, gpui::Modifiers::default());
        assert!(
            opened.lock().unwrap().is_empty(),
            "native words do not create a broad click region"
        );
        cx.simulate_click(link_point, gpui::Modifiers::default());
        assert_eq!(
            opened.lock().unwrap().len(),
            1,
            "native endpoint and glyph handler deliver a pointer click once"
        );
        action(cx, node, AccessibleAction::Focus);
        native_press(cx, "enter");
        action(cx, node, AccessibleAction::Click);
        assert_eq!(opened.lock().unwrap().len(), 3);
        // This synthetic admission change is deliberately not repainted: old
        // AX receipts and a clean key release must consult current admission.
        live.store(false, Ordering::SeqCst);
        cx.update(|window, _| window.blur());
        action(cx, node, AccessibleAction::Focus);
        cx.update(|window, cx| assert!(window.focused(cx).is_none()));
        action(cx, node, AccessibleAction::Click);
        cx.simulate_click(link_point, gpui::Modifiers::default());
        native_press(cx, "space");
        assert_eq!(opened.lock().unwrap().len(), 3);
    }

    #[gpui::test]
    fn native_tab_traversal_reaches_authored_links_in_reading_order(cx: &mut TestAppContext) {
        let (opened, _, cx) = mounted(
            "[First](https://first)\n\n[Second](https://second)",
            None,
            None,
            cx,
        );
        cx.update(|window, cx| {
            window.focus_next(cx);
            assert!(
                window.focused(cx).is_some(),
                "an actual link handle participates in Tab order"
            );
            window.draw(cx).clear(cx);
        });
        native_press(cx, "enter");
        assert_eq!(*opened.lock().unwrap(), vec!["https://first"]);
        // This goes through the focused native link's raw Tab event, not a
        // direct endpoint focus/action callback.
        cx.simulate_keystrokes("tab");
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        native_press(cx, "space");
        assert_eq!(
            *opened.lock().unwrap(),
            vec!["https://first", "https://second"]
        );
    }

    #[gpui::test]
    fn clamped_reader_does_not_focus_or_activate_its_hidden_tail(cx: &mut TestAppContext) {
        let (opened, _, cx) = mounted(
            "[Visible](https://visible)\n\n[Hidden](https://hidden)",
            Some(1),
            None,
            cx,
        );
        cx.update(|window, cx| {
            window.focus_next(cx);
            let _ = window.draw(cx);
        });
        native_press(cx, "enter");
        cx.update(|window, cx| {
            window.focus_next(cx);
            let _ = window.draw(cx);
        });
        native_press(cx, "enter");
        assert!(!opened.lock().unwrap().is_empty());
        assert!(
            opened
                .lock()
                .unwrap()
                .iter()
                .all(|url| url == "https://visible")
        );
    }

    #[gpui::test]
    fn custom_heading_uses_later_real_definition_and_code_stays_plain(cx: &mut TestAppContext) {
        use super::super::{MarkdownExtensions, MarkdownNode};
        use gpui::{ParentElement as _, StatefulInteractiveElement as _};
        let extensions = MarkdownExtensions::default()
            .block_parser(|node, context| {
                let markdown::mdast::Node::Heading(heading) = node else {
                    return None;
                };
                Some(MarkdownNode::new(
                    "original-heading",
                    context.prepare_inline(&heading.children, "[Docs][id]"),
                ))
            })
            .block_renderer_with_context("original-heading", |node, context, window, cx| {
                div()
                    .id("original-heading")
                    .role(gpui::Role::Heading)
                    .aria_level(2)
                    .child(
                        context.inherit_links(super::super::TextView::prepared_markdown(
                            "heading-inline",
                            node.data::<super::super::PreparedMarkdown>()
                                .unwrap()
                                .clone(),
                        )),
                    )
            });
        let (opened, _, cx) = mounted(
            "## [Docs][id]\n\n`[Literal](https://literal)`\n\n[id]: https://docs.rs/reference",
            None,
            Some(extensions),
            cx,
        );
        cx.update(|window, cx| {
            window.focus_next(cx);
            let _ = window.draw(cx);
        });
        native_press(cx, "enter");
        cx.update(|window, cx| {
            window.focus_next(cx);
            let _ = window.draw(cx);
        });
        native_press(cx, "enter");
        assert!(!opened.lock().unwrap().is_empty());
        assert!(
            opened
                .lock()
                .unwrap()
                .iter()
                .all(|url| url == "https://docs.rs/reference")
        );
    }

    struct GuardHeading {
        state: Entity<TextViewState>,
        extensions: super::super::MarkdownExtensions,
        original: Rc<Cell<bool>>,
        checks: Rc<Cell<usize>>,
        opened: Arc<std::sync::atomic::AtomicUsize>,
    }
    impl Render for GuardHeading {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl gpui::IntoElement {
            let original = self.original.clone();
            let checks = self.checks.clone();
            let opened = self.opened.clone();
            super::super::TextView::new(&self.state)
                .markdown_extensions(self.extensions.clone())
                .link_admission(Rc::new(move |_| {
                    checks.set(checks.get() + 1);
                    original.get()
                }))
                .on_link_click(move |_, _, _, _| {
                    opened.fetch_add(1, Ordering::SeqCst);
                })
        }
    }

    #[gpui::test]
    fn prepared_heading_render_clones_admission_without_reading_or_evaluating_its_owner(
        cx: &mut TestAppContext,
    ) {
        use super::super::{MarkdownExtensions, MarkdownNode, PreparedMarkdown, TextView};
        cx.update(crate::init);
        let original = Rc::new(Cell::new(true));
        let checks = Rc::new(Cell::new(0));
        let opened = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let retained_state = Rc::new(std::cell::RefCell::new(None));
        let slot = retained_state.clone();
        let current = original.clone();
        let evaluations = checks.clone();
        let activations = opened.clone();
        let extensions = MarkdownExtensions::default()
            .block_parser(|node, context| {
                let markdown::mdast::Node::Heading(heading) = node else {
                    return None;
                };
                Some(MarkdownNode::new(
                    "guarded-heading",
                    context.prepare_inline(&heading.children, "[Docs][id]"),
                ))
            })
            .block_renderer_with_context("guarded-heading", |node, context, _, _| {
                context.inherit_links(TextView::prepared_markdown(
                    "guarded-inline",
                    node.data::<PreparedMarkdown>().unwrap().clone(),
                ))
            });
        let (_, cx) = cx.add_window_view(move |window, cx| {
            window.set_a11y_forced(true);
            let state = cx.new(|cx| {
                TextViewState::markdown("## [Docs][id]\n\n[id]: https://docs.rs/original", cx)
            });
            *slot.borrow_mut() = Some(state.clone());
            let document = cx.new(|_| GuardHeading {
                state,
                extensions,
                original: current,
                checks: evaluations,
                opened: activations,
            });
            crate::Root::new(document, window, cx)
        });
        for _ in 0..2 {
            cx.run_until_parked();
            cx.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            });
        }
        assert_eq!(
            checks.get(),
            0,
            "render/layout/prepaint must not evaluate a Reader guard"
        );
        let node = cx.update(|window, _| {
            window
                .a11y_tree()
                .unwrap()
                .nodes
                .iter()
                .find(|(_, node)| node.role() == gpui::Role::Link && node.label() == Some("Docs"))
                .map(|(id, _)| *id)
                .expect("prepared parent reference link")
        });
        action(cx, node, AccessibleAction::Click);
        assert_eq!(opened.load(Ordering::SeqCst), 1);
        original.set(false);
        // Install a different ambient frame to prove retained callbacks use
        // their original frozen predicate, not a current global stack guard.
        // This is a synthetic scope test, not a live owner admission claim.
        cx.update(|_, cx| {
            UiGlobalState::global_mut(cx).push_text_view(
                retained_state.borrow().as_ref().unwrap().clone(),
                Some(Rc::new(|_| true)),
                false,
            )
        });
        action(cx, node, AccessibleAction::Click);
        action(cx, node, AccessibleAction::Focus);
        cx.update(|_, cx| UiGlobalState::global_mut(cx).pop_text_view());
        assert_eq!(
            opened.load(Ordering::SeqCst),
            1,
            "late original admission rejects the replacement frame's guard"
        );
        assert!(
            checks.get() >= 3,
            "events evaluate the captured original predicate"
        );
    }

    #[gpui::test]
    fn real_native_link_element_exposes_focus_and_click_without_pointer_regions(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::init);
        let observed = Rc::new(Cell::new(false));
        let result = Rc::clone(&observed);
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let content = cx.new(|_| Probe { observed: result });
            crate::Root::new(content, window, cx)
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(observed.get());
    }

    struct Probe {
        observed: Rc<Cell<bool>>,
    }
    impl Render for Probe {
        fn render(
            &mut self,
            window: &mut Window,
            cx: &mut Context<Self>,
        ) -> impl gpui::IntoElement {
            use gpui::ParentElement as _;
            let element = link_element(
                "docs".into(),
                "Documentation".into(),
                "https://docs.rs/example".into(),
                size(px(120.), px(20.)),
                None,
                None,
                window,
                cx,
            );
            assert_eq!(element.a11y_role(), Some(gpui::Role::Link));
            let mut node = gpui::accesskit::Node::new(gpui::Role::Link);
            element.write_a11y_info(&mut node);
            assert_eq!(node.label(), Some("Documentation"));
            assert!(node.supports_action(AccessibleAction::Focus));
            assert!(node.supports_action(AccessibleAction::Click));
            self.observed.set(true);
            div().child(element)
        }
    }
}

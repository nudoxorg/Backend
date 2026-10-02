use futures::{Stream as _, StreamExt as _};
use std::{
    ops::RangeInclusive,
    pin::Pin,
    sync::{Arc, Mutex},
    task::Poll,
};

use gpui::{
    App, AppContext as _, Bounds, Context, FocusHandle, IntoElement, KeyBinding, ListState,
    ParentElement as _, Pixels, Point, Render, SharedString, Styled as _, Task, Window,
    prelude::FluentBuilder as _, px,
};

use crate::{
    ElementExt,
    input::{self, SelectAll},
    scroll::AutoScroll,
    text::{
        CodeBlockActionsFn, LinkClickHandlerFn, MarkdownExtensions, TableActionsFn, TextViewStyle,
        document::ParsedDocument,
        format,
        node::{self, NodeContext},
        pending_update::{self, Publications, Publisher},
        selection_adapter::TextViewSelectionAdapter,
    },
    v_flex,
};

const CONTEXT: &'static str = "TextView";
// Preserve exact first-layout height for small documents while bounding the
// amount of source parsed synchronously on the UI thread.
const MAX_SYNC_FULL_REPLACE_BYTES: usize = 4 * 1024;

pub(crate) fn init(cx: &mut App) {
    cx.bind_keys(vec![
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-c", input::Copy, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-c", input::Copy, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-a", input::SelectAll, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-a", input::SelectAll, Some(CONTEXT)),
    ]);
}

/// The content format of the text view.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum TextViewFormat {
    /// Markdown view
    Markdown,
    /// HTML view
    Html,
}

/// The format of the text returned by
/// [`TextViewState::selected_text`], which is also what copy writes to the
/// clipboard.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SelectionFormat {
    /// The rendered text, without any markup.
    #[default]
    Plain,
    /// The source of the selection.
    ///
    /// Select-all returns the original source verbatim, a partial selection is
    /// reconstructed as Markdown from the parsed nodes (e.g. selecting inside
    /// a `**bold**` run yields `**bold**`).
    Source,
}

/// One text element's laid-out vertical extent, reported by `Inline` during
/// prepaint so `TextView` can snap its `max_lines` clip to a whole-line
/// boundary.
#[derive(Clone, Copy)]
pub(super) struct LineSpan {
    pub(super) top: Pixels,
    pub(super) bottom: Pixels,
    pub(super) line_height: Pixels,
}

/// An immutable Markdown projection prepared outside the UI thread.
///
/// Keeps the component's native marks, links and selection model. Preparing
/// parses synchronously; invoke this from a worker, never render or layout.
#[derive(Clone)]
pub struct PreparedMarkdown(Arc<ParsedContent>);

impl PreparedMarkdown {
    pub(crate) fn from_content(content: ParsedContent) -> Self {
        Self(Arc::new(content))
    }

    /// Parse a standalone fragment with the component's default Markdown rules.
    pub fn parse(source: &str) -> Result<Self, SharedString> {
        let mut node_cx = NodeContext::default();
        let document = format::markdown::parse(source, &mut node_cx)?;
        Ok(Self(Arc::new(ParsedContent {
            document,
            node_cx,
            ..ParsedContent::default()
        })))
    }

    pub(super) fn source(&self) -> SharedString {
        self.0.document.source.shared()
    }
}

/// The state of a TextView.
pub struct TextViewState {
    pub(super) focus_handle: FocusHandle,
    pub(super) list_state: ListState,

    /// The bounds of the text view
    bounds: Bounds<Pixels>,

    pub(super) selectable: bool,
    pub(super) selection_format: SelectionFormat,
    pub(super) scrollable: bool,
    pub(super) max_lines: Option<usize>,
    /// Line spans reported by `Inline` during prepaint (collected only while
    /// [`Self::max_lines`] is set); cleared by `TextView` at each frame start.
    pub(super) line_spans: Arc<Mutex<Vec<LineSpan>>>,
    /// Whether the last painted frame clipped content due to `max_lines`.
    pub(super) clamped: bool,
    pub(super) text_view_style: TextViewStyle,
    pub(super) code_block_actions: Option<std::sync::Arc<CodeBlockActionsFn>>,
    pub(super) table_actions: Option<std::sync::Arc<TableActionsFn>>,
    pub(super) link_click_handler: Option<std::sync::Arc<LinkClickHandlerFn>>,
    pub(super) markdown_extensions: Arc<MarkdownExtensions>,

    pub(super) is_selecting: bool,
    multi_click_selection: Option<TextViewMultiClickSelection>,
    selected_text_override: Option<String>,
    select_all: bool,
    pub(super) auto_scroll: AutoScroll,
    pub(super) selection_adapter: TextViewSelectionAdapter,

    pub(super) parsed_content: ParsedContent,
    prepared_snapshot: Option<PreparedMarkdown>,
    /// Content format (markdown / html), used for bounded synchronous parsing
    /// of small full-replace updates.
    format: TextViewFormat,
    pub(super) background_parse: bool,
    text: String,
    revision: usize,
    pub(super) selection_revision: usize,
    compatible_layout_update: bool,
    parsed_error: Option<SharedString>,
    tx: Publisher<UpdateOptions>,
    _parse_task: Task<()>,
    _receive_task: Task<()>,
}

impl TextViewState {
    /// Create a Markdown TextViewState.
    pub fn markdown(text: &str, cx: &mut Context<Self>) -> Self {
        Self::new(TextViewFormat::Markdown, text, cx)
    }

    /// Create a HTML TextViewState.
    pub fn html(text: &str, cx: &mut Context<Self>) -> Self {
        Self::new(TextViewFormat::Html, text, cx)
    }

    /// Create a new TextViewState.
    fn new(format: TextViewFormat, text: &str, cx: &mut Context<Self>) -> Self {
        Self::new_configured(format, text, Arc::default(), false, cx)
    }

    pub(super) fn new_configured(
        format: TextViewFormat,
        text: &str,
        markdown_extensions: Arc<MarkdownExtensions>,
        background_parse: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_inner(
            format,
            text,
            markdown_extensions,
            background_parse,
            None,
            cx,
        )
    }

    pub(super) fn new_prepared(prepared: PreparedMarkdown, cx: &mut Context<Self>) -> Self {
        Self::new_inner(
            TextViewFormat::Markdown,
            prepared.0.document.source.as_str(),
            Arc::default(),
            true,
            Some(prepared.clone()),
            cx,
        )
    }

    fn new_inner(
        format: TextViewFormat,
        text: &str,
        markdown_extensions: Arc<MarkdownExtensions>,
        background_parse: bool,
        prepared: Option<PreparedMarkdown>,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let selection_adapter = TextViewSelectionAdapter::new(cx.entity().downgrade(), cx);

        let (tx, rx) = pending_update::channel(UpdateOptions::merge);
        let (tx_result, mut rx_result) = pending_update::channel(ParsedUpdate::merge);
        let _receive_task = if prepared.is_none() {
            cx.spawn({
                async move |weak_self, cx| {
                    while let Some(parsed_update) = rx_result.next().await {
                        _ = weak_self.update(cx, |state, cx| {
                            state.accept_parsed_update(parsed_update, cx);
                        });
                    }
                }
            })
        } else {
            Task::ready(())
        };

        let _parse_task = if prepared.is_none() {
            cx.background_spawn(UpdateFuture::new(format, rx, tx_result))
        } else {
            Task::ready(())
        };

        let mut this = Self {
            focus_handle,
            bounds: Bounds::default(),
            multi_click_selection: None,
            selected_text_override: None,
            select_all: false,
            selectable: false,
            selection_format: SelectionFormat::default(),
            scrollable: false,
            max_lines: None,
            line_spans: Arc::default(),
            clamped: false,
            // Measure all blocks (not just visible ones) so the scrollbar
            // thumb size stays stable. Without this, off-screen blocks count
            // as zero height until scrolled into view, which makes the
            // scrollbar jitter as more blocks get measured during scrolling.
            list_state: ListState::new(0, gpui::ListAlignment::Top, px(1000.)).measure_all(),
            text_view_style: TextViewStyle::default(),
            code_block_actions: None,
            table_actions: None,
            link_click_handler: None,
            markdown_extensions,
            is_selecting: false,
            auto_scroll: AutoScroll::default(),
            selection_adapter,
            parsed_content: prepared
                .as_ref()
                .map_or_else(ParsedContent::default, |snapshot| (*snapshot.0).clone()),
            prepared_snapshot: prepared.clone(),
            format,
            background_parse,
            parsed_error: None,
            text: text.to_string(),
            revision: 0,
            selection_revision: 0,
            compatible_layout_update: false,
            tx,
            _parse_task,
            _receive_task,
        };
        if prepared.is_none() {
            this.increment_update(text, false, cx);
        }
        this
    }

    pub(super) fn set_prepared(&mut self, prepared: &PreparedMarkdown, cx: &mut Context<Self>) {
        if self
            .prepared_snapshot
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(&current.0, &prepared.0))
        {
            return;
        }
        self.prepared_snapshot = Some(prepared.clone());
        self.text = prepared.0.document.source.to_string();
        self.revision = self.revision.wrapping_add(1);
        self.selection_revision = self.selection_revision.wrapping_add(1);
        self.parsed_content = (*prepared.0).clone();
        self.parsed_error = None;
        self.compatible_layout_update = false;
        if !self.is_selecting {
            self.reset_selection_and_adapter(cx);
        }
        cx.notify();
    }

    fn accept_parsed_update(
        &mut self,
        parsed_update: ParsedUpdate,
        cx: &mut Context<Self>,
    ) -> bool {
        if parsed_update.revision != self.revision {
            return false;
        }
        if parsed_update.baseline_ack {
            debug_assert!(parsed_update.full_parse);
            return false;
        }

        match parsed_update.result {
            Ok(content) => {
                self.parsed_content = content;
                self.parsed_error = None;
                self.compatible_layout_update = parsed_update.selection_compatible;
            }
            Err(err) => {
                self.parsed_error = Some(err);
            }
        }
        // Don't interrupt an active drag-selection; the stored
        // positions remain valid for append-only updates and will
        // self-correct on the next mouse-move event.
        if !parsed_update.selection_compatible && !self.is_selecting {
            self.reset_selection_and_adapter(cx);
        }
        cx.notify();
        true
    }

    /// Get the text content.
    pub(crate) fn source(&self) -> SharedString {
        self.parsed_content.document.source.shared()
    }

    /// Set whether the text is selectable, default false.
    pub fn selectable(mut self, selectable: bool) -> Self {
        self.selectable = selectable;
        self
    }

    /// Set whether the text is selectable, default false.
    pub fn set_selectable(&mut self, selectable: bool, cx: &mut Context<Self>) {
        self.selectable = selectable;
        cx.notify();
    }

    /// Set the [`SelectionFormat`], default is [`SelectionFormat::Plain`].
    pub fn selection_format(mut self, selection_format: SelectionFormat) -> Self {
        self.selection_format = selection_format;
        self
    }

    /// Set the [`SelectionFormat`], default is [`SelectionFormat::Plain`].
    pub fn set_selection_format(
        &mut self,
        selection_format: SelectionFormat,
        cx: &mut Context<Self>,
    ) {
        self.selection_format = selection_format;
        cx.notify();
    }

    /// Set whether the text is selectable, default false.
    pub fn scrollable(mut self, scrollable: bool) -> Self {
        self.scrollable = scrollable;
        self
    }

    /// Set whether the text is selectable, default false.
    pub fn set_scrollable(&mut self, scrollable: bool, cx: &mut Context<Self>) {
        if !scrollable {
            self.reset_selection_and_adapter(cx);
        }
        self.scrollable = scrollable;
        cx.notify();
    }

    /// Whether the last painted frame clipped content because of
    /// [`TextView::max_lines`](crate::text::TextView::max_lines).
    pub fn is_clamped(&self) -> bool {
        self.clamped
    }

    /// Set the text content.
    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.text.as_str() == text {
            return;
        }

        self.text.clear();
        self.text.push_str(text);
        self.parsed_error = None;
        self.increment_update(text, false, cx);
    }

    /// Append partial text content to the existing text.
    pub fn push_str(&mut self, new_text: &str, cx: &mut Context<Self>) {
        if new_text.is_empty() {
            return;
        }
        self.text.push_str(new_text);
        self.increment_update(new_text, true, cx);
    }

    pub(crate) fn set_markdown_extensions(
        &mut self,
        markdown_extensions: Arc<MarkdownExtensions>,
        cx: &mut Context<Self>,
    ) {
        if self.markdown_extensions.revision() == markdown_extensions.revision() {
            return;
        }

        self.markdown_extensions = markdown_extensions;
        if self.format == TextViewFormat::Markdown {
            let text = self.text.clone();
            self.increment_update(&text, false, cx);
        }
    }

    /// Return the selected text, in the view's [`SelectionFormat`].
    pub fn selected_text(&self) -> String {
        self.selected_text_in(None)
    }

    /// The format to copy in, which is [`SelectionFormat::Plain`] whenever the
    /// requested one cannot be produced.
    ///
    /// Only a Markdown view can return source. Reconstructing HTML would mean
    /// spelling every attribute back out — a mark's color, an image's
    /// dimensions, a cell's alignment — with a new way to lose one at each
    /// step, and html5ever records no source offsets to fall back on (it
    /// reports only line numbers), so there is no original text to copy from
    /// either.
    fn effective_format(&self) -> SelectionFormat {
        match self.format {
            TextViewFormat::Markdown => self.selection_format,
            TextViewFormat::Html => SelectionFormat::Plain,
        }
    }

    /// Return the selected text, with `blocks` bounding which top-level blocks
    /// the selection covers.
    ///
    /// The range comes from the selection endpoints, which know their block
    /// even after it scrolls out of view; see
    /// [`ParsedDocument::selected_text`](crate::text::document::ParsedDocument).
    pub(super) fn selected_text_in(&self, blocks: Option<RangeInclusive<usize>>) -> String {
        let format = self.effective_format();

        if self.select_all {
            if format == SelectionFormat::Source {
                return self.source().to_string();
            }

            return self.parsed_content.document.text();
        }

        // A multi-click stores the plain text it selected, which is a shortcut
        // past the block walk. Source mode cannot take it: the word it stored
        // has lost its markup. The click also set the inline selection it came
        // from, so the walk reconstructs the same range with the markup intact.
        if format != SelectionFormat::Source
            && let Some(text) = &self.selected_text_override
        {
            return text.clone();
        }

        self.parsed_content.document.selected_text(format, blocks)
    }

    fn increment_update(&mut self, text: &str, append: bool, cx: &mut Context<Self>) {
        self.revision += 1;
        if !append {
            self.selection_revision = self.selection_revision.wrapping_add(1);
        }
        let parse_synchronously =
            !self.background_parse && !append && text.len() <= MAX_SYNC_FULL_REPLACE_BYTES;
        let update_options = UpdateOptions {
            revision: self.revision,
            append,
            mode: if append {
                ParseMode::Compatible
            } else if parse_synchronously {
                ParseMode::BaselineAck
            } else {
                ParseMode::Replace
            },
            pending_text: text.to_string(),
            markdown_extensions: self.markdown_extensions.clone(),
        };

        // Keep small full replacements synchronous so their first layout has
        // the exact content height. Larger replacements use the existing
        // background parser, bounding synchronous parser input on the UI thread.
        if parse_synchronously {
            match parse_content(self.format, ParsedContent::default(), &update_options) {
                Ok(content) => {
                    self.parsed_content = content;
                    self.parsed_error = None;
                    if !self.is_selecting {
                        self.reset_selection_and_adapter(cx);
                    }
                }
                Err(err) => {
                    self.parsed_error = Some(err);
                }
            }
            // Keep the background parser's accumulated document in sync so a
            // later append extends this baseline instead of parsing the delta
            // as a standalone document.
            _ = self.tx.try_send(update_options);
            cx.notify();
            return;
        }

        _ = self.tx.try_send(update_options);
    }

    /// Save bounds and unselect if bounds changed.
    pub(super) fn update_bounds(&mut self, bounds: Bounds<Pixels>, _cx: &mut App) {
        self.bounds = bounds;
    }

    /// The index of the top-level block at `content_y`, in this view's content
    /// coordinates (the same space the base selection endpoint stores its point in).
    ///
    /// Only laid-out blocks can be located, which is enough for a selection
    /// endpoint: the user can only put one where they can see it. Returns
    /// `None` for a view that is not virtualized, where every block paints and
    /// the range is not needed.
    pub(super) fn block_ix_at(&self, content_y: Pixels) -> Option<usize> {
        if !self.scrollable {
            return None;
        }

        let origin = self.bounds.origin.y + self.scroll_offset().y;
        let count = self.list_state.item_count();
        let mut ix = self.list_state.logical_scroll_top().item_ix;
        while ix < count {
            let bounds = self.list_state.bounds_for_item(ix)?;
            if content_y < bounds.bottom() - origin {
                return Some(ix);
            }
            ix += 1;
        }

        count.checked_sub(1)
    }

    pub(super) fn bounds(&self) -> Bounds<Pixels> {
        self.bounds
    }

    /// Whether this view has a view-local selection (select-all, multi-click, or override),
    /// independent of the window-level selection.
    pub(super) fn has_view_selection(&self) -> bool {
        self.select_all
            || self.multi_click_selection.is_some()
            || self.selected_text_override.is_some()
    }

    pub(super) fn stop_auto_scroll(&mut self) {
        self.auto_scroll.stop();
    }

    pub(super) fn reset_selection(&mut self) {
        self.multi_click_selection = None;
        self.selected_text_override = None;
        self.select_all = false;
        self.is_selecting = false;
        self.auto_scroll.stop();
        // Clear the inline selection state synchronously, so offscreen
        // (virtualized) views that won't repaint don't leak stale selection
        // text into a new cross-view copy.
        self.parsed_content.document.clear_selection();
    }

    fn reset_selection_and_adapter(&mut self, cx: &mut App) {
        self.reset_selection();
        self.selection_adapter.set_local_selection(false, cx);
    }

    /// Clear the current text selection.
    pub fn clear_selection(&mut self, cx: &mut Context<Self>) {
        self.reset_selection_and_adapter(cx);
        cx.notify();
    }

    pub(super) fn scroll_offset(&self) -> Point<Pixels> {
        if self.scrollable {
            self.list_state.scroll_px_offset_for_scrollbar()
        } else {
            Point::default()
        }
    }

    /// Select all rendered text in this view.
    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        self.multi_click_selection = None;
        self.selected_text_override = None;
        self.select_all = true;
        self.is_selecting = false;
        self.auto_scroll.stop();
        self.selection_adapter.set_local_selection(true, cx);
        cx.notify();
    }

    pub(crate) fn set_multi_click_selection(
        &mut self,
        pos: Point<Pixels>,
        kind: TextViewMultiClickKind,
        selected_text: String,
        cx: &mut App,
    ) {
        let scroll_offset = self.scroll_offset();
        let pos = pos - self.bounds.origin - scroll_offset;
        self.multi_click_selection = Some(TextViewMultiClickSelection { pos, kind });
        self.selected_text_override = Some(selected_text);
        self.select_all = false;
        self.is_selecting = false;
        self.auto_scroll.stop();
        self.selection_adapter.set_local_selection(true, cx);
    }

    pub(super) fn set_auto_scroll(&mut self, delta: Option<Pixels>, cx: &mut Context<Self>) {
        self.auto_scroll.set(delta, cx, |delta, state, cx| {
            state.list_state.scroll_by(delta);
            cx.notify();
        });
    }

    /// Return the window selection (anchor, cursor) in window coordinates if
    /// this view participates in it.
    ///
    /// Single-view fast path: when both endpoints are anchored inside one
    /// TextView, only that view participates (identical to the previous
    /// per-view behavior).
    pub(crate) fn selection_points(&self, cx: &App) -> Option<(Point<Pixels>, Point<Pixels>)> {
        if !self.selectable {
            return None;
        }
        self.selection_adapter.selection_points(cx)
    }

    pub(crate) fn has_selection(&self, cx: &App) -> bool {
        self.has_view_selection() || self.selection_points(cx).is_some()
    }

    pub(super) fn on_action_select_all(
        &mut self,
        _: &SelectAll,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.selectable {
            cx.propagate();
            return;
        }

        self.select_all(cx);
    }

    pub(crate) fn is_selectable(&self) -> bool {
        self.selectable
    }

    pub(crate) fn is_all_selected(&self) -> bool {
        self.select_all
    }

    pub(crate) fn multi_click_selection(&self) -> Option<TextViewMultiClickSelection> {
        let scroll_offset = self.scroll_offset();
        self.multi_click_selection.map(|selection| {
            let pos = selection.pos + scroll_offset + self.bounds.origin;
            TextViewMultiClickSelection { pos, ..selection }
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TextViewMultiClickSelection {
    pub(crate) pos: Point<Pixels>,
    pub(crate) kind: TextViewMultiClickKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TextViewMultiClickKind {
    Word,
    Paragraph,
}

impl Render for TextViewState {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = cx.entity();
        let document = self.parsed_content.document.clone();
        let mut node_cx = self.parsed_content.node_cx.clone();

        node_cx.code_block_actions = self.code_block_actions.clone();
        node_cx.table_actions = self.table_actions.clone();
        node_cx.link_click_handler = self.link_click_handler.clone();
        node_cx.markdown_extensions = self.markdown_extensions.clone();
        node_cx.style = self.text_view_style.clone();

        v_flex()
            .w_full()
            // Clamped content must keep its natural height: stretching it to
            // the capped box would hide the overflow the clamp has to measure.
            .when(self.max_lines.is_none(), |this| this.h_full())
            .map(|this| match &mut self.parsed_error {
                None => this.child(document.render_root(
                    if self.scrollable {
                        Some(self.list_state.clone())
                    } else {
                        None
                    },
                    &node_cx,
                    window,
                    cx,
                )),
                Some(err) => this.child(
                    v_flex()
                        .gap_1()
                        .child("Failed to parse content")
                        .child(err.to_string()),
                ),
            })
            .on_prepaint(move |bounds, window, cx| {
                let (
                    size_changed,
                    selection_involves_view,
                    has_selection_snapshot,
                    is_selecting,
                    compatible_layout_update,
                ) = {
                    let state = state.read(cx);
                    (
                        state.bounds().size != bounds.size,
                        state.selection_adapter.is_part_of_window_selection(cx),
                        state.selection_adapter.has_selection_snapshot(cx),
                        state.is_selecting,
                        state.compatible_layout_update,
                    )
                };
                let mut revision_changed = false;
                state.update(cx, |state, cx| {
                    revision_changed = state
                        .selection_adapter
                        .update_layout_revision(state.selection_revision, state.is_selecting);
                    state.update_bounds(bounds, cx);
                    state.compatible_layout_update = false;
                });
                if !is_selecting
                    && ((size_changed && selection_involves_view && !compatible_layout_update)
                        || (revision_changed && has_selection_snapshot))
                {
                    gpui_base::TextSelection::clear(window, cx);
                }
            })
    }
}

#[derive(Clone, PartialEq, Default)]
pub(crate) struct ParsedContent {
    pub(crate) document: ParsedDocument,
    pub(crate) node_cx: node::NodeContext,
    append_compatible: bool,
    /// Test-only grammar input volume, not a parse-time estimate. Required
    /// open-block reparses and conservative full-document reference fallback
    /// both contribute their actual source bytes.
    #[cfg(test)]
    grammar_input_bytes: usize,
}

struct UpdateFuture {
    format: TextViewFormat,
    content: ParsedContent,
    logical_source: super::document_storage::SourceSnapshot,
    basis_checked: bool,
    rx: Pin<Box<Publications<UpdateOptions>>>,
    tx_result: Publisher<ParsedUpdate>,
}

impl UpdateFuture {
    fn new(
        format: TextViewFormat,
        rx: Publications<UpdateOptions>,
        tx_result: Publisher<ParsedUpdate>,
    ) -> Self {
        Self {
            format,
            content: Default::default(),
            logical_source: Default::default(),
            basis_checked: true,
            rx: Box::pin(rx),
            tx_result,
        }
    }
    fn apply(&mut self, options: UpdateOptions) -> ParsedUpdate {
        if options.append {
            self.logical_source.append(&options.pending_text);
        } else {
            self.logical_source = options.pending_text.clone().into();
        }
        let recover = options.append && !self.basis_checked;
        let res = if recover {
            let complete = UpdateOptions {
                append: false,
                pending_text: self.logical_source.to_string(),
                mode: ParseMode::Replace,
                ..options.clone()
            };
            parse_content(self.format, ParsedContent::default(), &complete)
        } else {
            parse_content(self.format, self.content.clone(), &options)
        };
        self.basis_checked = res.is_ok();
        if let Ok(content) = &res {
            self.content = content.clone();
        }
        ParsedUpdate {
            revision: options.revision,
            full_parse: !options.append
                || recover
                || res.as_ref().is_ok_and(|content| !content.append_compatible),
            selection_compatible: !recover
                && res.as_ref().is_ok_and(|content| content.append_compatible),
            baseline_ack: options.mode == ParseMode::BaselineAck,
            result: res,
        }
    }
}

impl Future for UpdateFuture {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        match self.rx.as_mut().poll_next(cx) {
            Poll::Ready(Some(options)) => {
                let parsed = self.apply(options);
                _ = self.tx_result.try_send(parsed);
                // One parse per poll; a concurrent publisher cannot keep
                // this worker occupied indefinitely.
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
            Poll::Ready(None) => Poll::Ready(()),
            Poll::Pending => Poll::Pending,
        }
    }
}

#[derive(Clone)]
struct UpdateOptions {
    revision: usize,
    pending_text: String,
    append: bool,
    mode: ParseMode,
    markdown_extensions: Arc<MarkdownExtensions>,
}

impl UpdateOptions {
    fn merge(&mut self, next: UpdateOptions) {
        if next.append {
            self.pending_text.push_str(&next.pending_text);
            self.revision = next.revision;
            if self.mode != ParseMode::Replace {
                self.mode = ParseMode::Compatible;
            }
        } else {
            *self = next;
        }
    }
}

struct ParsedUpdate {
    revision: usize,
    full_parse: bool,
    selection_compatible: bool,
    baseline_ack: bool,
    result: Result<ParsedContent, SharedString>,
}

impl ParsedUpdate {
    fn merge(&mut self, mut next: Self) {
        // Each success is a complete cumulative document, so its content can
        // supersede the pending result. A skipped incompatible replacement
        // must still reset selection when its later appended result paints.
        // Baseline acknowledgements are already installed synchronously and
        // must not invalidate a selection made after that installation.
        next.selection_compatible &= self.baseline_ack || self.selection_compatible;
        *self = next;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ParseMode {
    BaselineAck,
    Replace,
    Compatible,
}

fn parse_content(
    format: TextViewFormat,
    mut content: ParsedContent,
    options: &UpdateOptions,
) -> Result<ParsedContent, SharedString> {
    let mut node_cx = if options.append {
        content.node_cx.clone()
    } else {
        NodeContext::default()
    };
    node_cx.markdown_extensions = options.markdown_extensions.clone();
    let prior_unresolved = node_cx.unresolved_references;
    let prior_reference_links = node_cx.reference_links_present;
    node_cx.link_refs.begin_update();
    node_cx.unresolved_references = false;
    node_cx.reference_links_present = false;

    // Re-parse the last block together with the appended text, so a block the
    // new text continues (an unclosed list, a fenced code block) is not split
    // in two. A block without a span cannot be located in `source` — the HTML
    // parser never records spans — so it is left in place and only the
    // appended text is parsed, positioned at the end of the current source.
    let last_span = options
        .append
        .then(|| {
            content
                .document
                .blocks
                .last()
                .and_then(|block| block.span())
        })
        .flatten();
    let open_reference_start = options
        .append
        .then(|| {
            node_cx
                .link_refs
                .open_tail_definition_start(&content.document.source)
        })
        .flatten();
    let reparse_start = match (last_span.map(|span| span.start), open_reference_start) {
        (Some(block), Some(reference)) => Some(block.min(reference)),
        (Some(block), None) => Some(block),
        (None, Some(reference)) => Some(reference),
        (None, None) => None,
    };

    let mut source = String::new();
    if let Some(start) = reparse_start {
        while content
            .document
            .blocks
            .last()
            .and_then(|block| block.span())
            .is_some_and(|span| span.start >= start || span.end > start)
        {
            content.document.blocks.pop();
        }
        node_cx.link_refs.truncate_from(start);
        node_cx.offset = start;
        source.push_str(
            &content
                .document
                .source
                .get(start..content.document.source.len())
                .unwrap(),
        );
        source.push_str(&options.pending_text);
    } else {
        if options.append {
            node_cx.offset = content.document.source.len();
        }
        source.push_str(&options.pending_text);
    }

    let new_document = match format {
        TextViewFormat::Markdown => format::markdown::parse(&source, &mut node_cx),
        TextViewFormat::Html => format::html::parse(&source, &mut node_cx),
    }?;

    #[cfg(test)]
    {
        content.grammar_input_bytes += source.len();
    }
    let suffix_unresolved = node_cx.unresolved_references;
    let reference_mapping_changed = node_cx.link_refs.finish_update();
    if options.append
        && (((prior_unresolved || prior_reference_links) && reference_mapping_changed)
            || (suffix_unresolved && !content.node_cx.link_refs.is_empty()))
    {
        // A new definition can change previously plain prose into a reference
        // link. A suffix reference may need definitions outside its parse
        // window. The Markdown parser owns that grammar, so reproject the true
        // complete source rather than inventing a separate bracket parser.
        let mut whole = content.document.source.clone();
        whole.append(&options.pending_text);
        let full = UpdateOptions {
            append: false,
            pending_text: whole.to_string(),
            mode: ParseMode::Replace,
            ..options.clone()
        };
        let mut result = parse_content(format, ParsedContent::default(), &full)?;
        #[cfg(test)]
        {
            result.grammar_input_bytes += content.grammar_input_bytes;
        }
        result.append_compatible = false;
        return Ok(result);
    }
    node_cx.unresolved_references |= prior_unresolved;
    node_cx.reference_links_present |= prior_reference_links;
    content.append_compatible = options.mode == ParseMode::Compatible;

    if options.append {
        // Test accounting includes the parse-buffer copy and the temporary
        // suffix document's source storage, not only the persistent append.
        #[cfg(test)]
        content
            .document
            .source
            .record_materialized_bytes(source.len() + new_document.source.copied_bytes());
        content.document.source.append(&options.pending_text);
        content.document.blocks.append(new_document.blocks);
    } else {
        content.document = new_document;
    }

    content.node_cx = node_cx;
    Ok(content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::MarkdownNode;
    use gpui::TestAppContext;

    fn replace_markdown(source: &str) -> ParsedContent {
        parse_content(
            TextViewFormat::Markdown,
            ParsedContent::default(),
            &UpdateOptions {
                revision: 1,
                pending_text: source.to_owned(),
                append: false,
                mode: ParseMode::Replace,
                markdown_extensions: Arc::default(),
            },
        )
        .unwrap()
    }

    fn append_markdown(content: ParsedContent, source: &str) -> ParsedContent {
        parse_content(
            TextViewFormat::Markdown,
            content,
            &UpdateOptions {
                revision: 2,
                pending_text: source.to_owned(),
                append: true,
                mode: ParseMode::Compatible,
                markdown_extensions: Arc::default(),
            },
        )
        .unwrap()
    }

    fn first_reference_link(content: &ParsedContent) -> &node::LinkMark {
        content
            .document
            .blocks
            .iter()
            .find_map(|block| {
                let node::BlockNode::Paragraph(paragraph) = block else {
                    return None;
                };
                paragraph
                    .children
                    .iter()
                    .flat_map(|run| &run.marks)
                    .find_map(|(_, mark)| {
                        mark.link.as_ref().filter(|link| link.identifier.is_some())
                    })
            })
            .expect("fixture contains a reference link")
    }

    #[test]
    fn append_reconciles_a_reference_definition_open_at_eof() {
        let initial = replace_markdown("[id]: fo");
        assert_eq!(
            initial.node_cx.link_refs.get(&"id".into()).unwrap().url,
            "fo"
        );

        let appended = append_markdown(initial, "o\n\n[link][id]");
        let definition = appended
            .node_cx
            .resolve_link_mark(first_reference_link(&appended).clone());
        assert_eq!(definition.url.as_str(), "foo");
        assert_eq!(appended.document.source.as_str(), "[id]: foo\n\n[link][id]");
        assert!(appended.append_compatible);
    }

    #[test]
    fn appended_definition_reprojects_an_earlier_unresolved_reference() {
        let initial = replace_markdown("[link][id]\n\nbody\n\n");
        assert!(initial.node_cx.unresolved_references);
        assert!(initial.document.text().contains("[link][id]"));

        let appended = append_markdown(initial, "[id]: dest");
        assert_eq!(
            appended
                .node_cx
                .resolve_link_mark(first_reference_link(&appended).clone())
                .url
                .as_str(),
            "dest"
        );
        assert!(!appended.append_compatible);
        assert_eq!(
            appended.document.source.as_str(),
            "[link][id]\n\nbody\n\n[id]: dest"
        );
        assert!(appended.grammar_input_bytes > appended.document.source.len());
    }

    #[test]
    fn appends_keep_reference_first_wins_order() {
        let initial = replace_markdown("[link][id]\n\n[id]: first");
        let appended = append_markdown(initial, "\n\n[id]: second");
        assert_eq!(
            appended
                .node_cx
                .resolve_link_mark(first_reference_link(&appended).clone())
                .url
                .as_str(),
            "first"
        );
        assert_eq!(
            appended.node_cx.link_refs.get(&"id".into()).unwrap().url,
            "first"
        );
    }

    #[test]
    fn appended_reference_uses_a_definition_retained_before_the_parse_window() {
        let initial = replace_markdown("[id]: dest\n\nbody\n\n");
        let appended = append_markdown(initial, "[link][id]");
        assert_eq!(
            appended
                .node_cx
                .resolve_link_mark(first_reference_link(&appended).clone())
                .url
                .as_str(),
            "dest"
        );
        assert!(!appended.append_compatible);
        assert!(appended.grammar_input_bytes > appended.document.source.len());
    }

    #[test]
    fn changing_an_open_winner_reprojects_existing_reference_links() {
        let initial = replace_markdown("[link][id]\n\n[id]: old");
        let appended = append_markdown(initial.clone(), "er");
        assert_eq!(
            appended
                .node_cx
                .resolve_link_mark(first_reference_link(&appended).clone())
                .url
                .as_str(),
            "older"
        );
        assert_eq!(
            initial.node_cx.link_refs.get(&"id".into()).unwrap().url,
            "old"
        );
        assert!(!appended.append_compatible);
        assert!(appended.grammar_input_bytes > appended.document.source.len());
    }

    #[test]
    fn worker_and_render_reference_work_uses_shared_production_lookup() {
        let mut source = String::new();
        for ix in 0..256 {
            source.push_str(&format!("[ref-{ix}]: dest-{ix}\n\n"));
        }
        source.push_str("[link][ref-0]\n\nbody");
        let mut content = replace_markdown(&source);
        let entry_clones = content.node_cx.link_refs.copied_entries();
        let environment_clones = content.node_cx.link_refs.environment_clones();

        // This is the same content clone and append path used by UpdateFuture
        // before a worker result is published to the UI state.
        for _ in 0..32 {
            content = append_markdown(content.clone(), "tail\n\n");
        }
        assert_eq!(content.node_cx.link_refs.copied_entries(), entry_clones);
        assert!(content.node_cx.link_refs.environment_clones() >= environment_clones + 32);

        let lookups = content.node_cx.link_refs.lookups();
        let resolved = content
            .node_cx
            .resolve_link_mark(first_reference_link(&content).clone());
        assert_eq!(resolved.url.as_str(), "dest-0");
        assert_eq!(content.node_cx.link_refs.lookups(), lookups + 1);
    }

    #[test]
    fn open_paragraph_and_fence_report_grammar_reparse_separately_from_map_clones() {
        for prefix in ["open", "```rust\ncode"] {
            let mut content = replace_markdown(prefix);
            let initial_bytes = content.document.source.len();
            for _ in 0..24 {
                content = append_markdown(content, "x");
            }
            let final_bytes = content.document.source.len();
            assert!(
                content.grammar_input_bytes > final_bytes * 3,
                "open blocks are reparsed by the Markdown grammar: grammar_input_bytes={} final_source_bytes={final_bytes}",
                content.grammar_input_bytes
            );
            assert!(content.document.source.copied_bytes() > final_bytes);
            assert!(final_bytes > initial_bytes);
            assert_eq!(content.node_cx.link_refs.copied_entries(), 0);
        }
    }

    #[test]
    fn prepared_heading_resolves_parent_reference_definitions_and_preserves_exact_source() {
        let extensions = Arc::new(MarkdownExtensions::default().block_parser(|node, context| {
            let markdown::mdast::Node::Heading(heading) = node else {
                return None;
            };
            Some(MarkdownNode::new(
                "derived",
                context.prepare_inline(&heading.children, "[Guide][id]"),
            ))
        }));
        let content = parse_content(
            TextViewFormat::Markdown,
            ParsedContent::default(),
            &UpdateOptions {
                revision: 1,
                pending_text: "## [Guide][id]\n\n[id]: src/lib.rs#L7".into(),
                append: false,
                mode: ParseMode::Replace,
                markdown_extensions: extensions,
            },
        )
        .unwrap();
        let node::BlockNode::Custom(custom) = &content.document.blocks[0] else {
            panic!("heading missing");
        };
        let projection = custom.data::<PreparedMarkdown>().unwrap();
        assert_eq!(projection.0.document.text().trim(), "Guide");
        assert_eq!(projection.source().as_str(), "[Guide][id]");
        assert!(projection.0.node_cx.link_refs.environment_clones() >= 1);
        let reference = projection.0.node_cx.link_refs.get(&"id".into()).unwrap();
        assert_eq!(reference.url.as_str(), "src/lib.rs#L7");
        let node::BlockNode::Paragraph(paragraph) = &projection.0.document.blocks[0] else {
            panic!("inline missing");
        };
        assert!(
            paragraph
                .children
                .iter()
                .flat_map(|run| &run.marks)
                .any(|(_, mark)| {
                    mark.link
                        .as_ref()
                        .is_some_and(|link| link.identifier.as_deref() == Some("id"))
                })
        );
    }

    #[gpui::test]
    fn prepared_heading_is_complete_before_executor_runs_and_keeps_native_links_and_copy(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::init);
        // The production heading extension performs this preparation in its
        // worker callback. Instantiating a TextView consumes only the snapshot.
        let prepared = PreparedMarkdown::parse("🦀 **Guide** [local](src/lib.rs#L7)").unwrap();
        let state = cx.update(|cx| cx.new(|cx| TextViewState::new_prepared(prepared.clone(), cx)));
        state.read_with(cx, |state, _| {
            assert_eq!(state.parsed_content.document.blocks.len(), 1);
            let node::BlockNode::Paragraph(paragraph) = &state.parsed_content.document.blocks[0]
            else {
                panic!("native inline paragraph missing");
            };
            assert_eq!(paragraph.text(), "🦀 Guide local");
            let destinations: Vec<_> = paragraph
                .children
                .iter()
                .flat_map(|run| run.marks.iter())
                .filter_map(|(_, mark)| mark.link.as_ref().map(|link| link.url.as_str()))
                .collect();
            assert_eq!(destinations, ["src/lib.rs#L7"]);
            // A prepared heading has no parser consumer or background receive
            // task: it must not depend on another executor turn to fill in.
            assert!(
                state
                    .tx
                    .try_send(UpdateOptions {
                        revision: 1,
                        pending_text: "unwanted parse".into(),
                        append: false,
                        mode: ParseMode::Replace,
                        markdown_extensions: Arc::default(),
                    })
                    .is_err()
            );
        });
        state.update(cx, |state, cx| {
            state.selectable = true;
            state.select_all(cx);
            assert_eq!(state.selected_text().trim(), "🦀 Guide local");
            state.selection_format = SelectionFormat::Source;
            assert_eq!(state.selected_text(), "🦀 **Guide** [local](src/lib.rs#L7)");
            let selected_revision = state.selection_revision;
            state.set_prepared(&prepared, cx);
            assert_eq!(state.selection_revision, selected_revision);
            assert!(state.select_all);
        });
        cx.run_until_parked();
        state.read_with(cx, |state, _| {
            assert_eq!(
                state.source().as_str(),
                "🦀 **Guide** [local](src/lib.rs#L7)"
            )
        });
    }

    #[gpui::test]
    fn distinct_prepared_semantics_replace_same_source_and_parent_offset_is_local(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::init);
        let first = PreparedMarkdown::parse("same").unwrap();
        let mut changed = (*first.0).clone();
        let mut paragraph = node::Paragraph::default();
        paragraph.push(
            node::InlineNode::new("same").marks(vec![(0..4, node::TextMark::default().bold())]),
        );
        changed.document.blocks = vec![node::BlockNode::Paragraph(paragraph)].into();
        let changed = PreparedMarkdown::from_content(changed);
        let state = cx.update(|cx| cx.new(|cx| TextViewState::new_prepared(first, cx)));
        state.update(cx, |state, cx| state.set_prepared(&changed, cx));
        state.read_with(cx, |state, _| {
            let node::BlockNode::Paragraph(paragraph) = &state.parsed_content.document.blocks[0]
            else {
                panic!("paragraph missing");
            };
            assert!(paragraph.children[0].marks[0].1.bold);
        });

        let extensions = Arc::new(MarkdownExtensions::default().block_parser(|node, context| {
            let markdown::mdast::Node::Heading(heading) = node else {
                return None;
            };
            assert!(
                context.offset() > 0,
                "fixture must exercise a nonzero parent offset"
            );
            Some(MarkdownNode::new(
                "offset-heading",
                context.prepare_inline(&heading.children, "<b>Guide</b> [id][target]"),
            ))
        }));
        let prefix = parse_content(
            TextViewFormat::Markdown,
            ParsedContent::default(),
            &UpdateOptions {
                revision: 1,
                pending_text: "first\n\nsecond\n\n".into(),
                append: false,
                mode: ParseMode::Replace,
                markdown_extensions: extensions.clone(),
            },
        )
        .unwrap();
        let content =
            parse_content(
                TextViewFormat::Markdown,
                prefix,
                &UpdateOptions {
                    revision: 2,
                    pending_text:
                        "## <b>Guide</b> [id][target]\n\n[target]: first.rs\n[target]: second.rs"
                            .into(),
                    append: true,
                    mode: ParseMode::Compatible,
                    markdown_extensions: extensions,
                },
            )
            .unwrap();
        let node = content
            .document
            .blocks
            .iter()
            .find_map(|block| match block {
                node::BlockNode::Custom(node) => Some(node),
                _ => None,
            });
        let Some(node) = node else {
            panic!("heading missing");
        };
        let projection = node.data::<PreparedMarkdown>().unwrap();
        assert_eq!(projection.0.node_cx.offset, 0);
        assert_eq!(
            projection
                .0
                .node_cx
                .link_refs
                .get(&"target".into())
                .unwrap()
                .url
                .as_str(),
            "first.rs"
        );
        assert_eq!(projection.0.document.blocks[0].span().unwrap().start, 0);
        let state =
            cx.update(|cx| cx.new(|cx| TextViewState::new_prepared(projection.clone(), cx)));
        state.update(cx, |state, cx| {
            state.selection_format = SelectionFormat::Source;
            state.select_all(cx);
            assert_eq!(state.selected_text(), "<b>Guide</b> [id][target]");
        });
    }

    #[gpui::test]
    fn prepared_heading_replacement_updates_without_worker_and_clears_old_selection(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::init);
        let first = PreparedMarkdown::parse("old [link](#old)").unwrap();
        let replacement = PreparedMarkdown::parse("**new** [guide](#guide)").unwrap();
        let state = cx.update(|cx| cx.new(|cx| TextViewState::new_prepared(first, cx)));
        state.update(cx, |state, cx| {
            state.select_all(cx);
            state.set_prepared(&replacement, cx);
            assert!(!state.select_all);
            assert_eq!(state.parsed_content.document.text().trim(), "new guide");
            assert_eq!(state.source().as_str(), "**new** [guide](#guide)");
        });
    }

    #[gpui::test]
    fn background_small_markdown_installs_extensions_once_before_parsing(cx: &mut TestAppContext) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        cx.update(crate::init);
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let extensions = Arc::new(MarkdownExtensions::default().block_parser(
            move |node, context| {
                let markdown::mdast::Node::Heading(heading) = node else {
                    return None;
                };
                observed.fetch_add(1, Ordering::SeqCst);
                Some(
                    MarkdownNode::new("heading", heading.depth)
                        .text("Hello 🦀")
                        .markdown(context.node_source(node).unwrap()),
                )
            },
        ));
        let source = "# Hello 🦀\n\n[local](src/lib.rs#L7)";
        let state = cx.update(|cx| {
            cx.new(|cx| {
                TextViewState::new_configured(
                    TextViewFormat::Markdown,
                    source,
                    extensions.clone(),
                    true,
                    cx,
                )
            })
        });
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        state.read_with(cx, |state, _| {
            assert!(state.parsed_content.document.blocks.is_empty())
        });
        cx.run_until_parked();
        state.read_with(cx, |state, _| {
            let blocks = &state.parsed_content.document.blocks;
            assert_eq!(blocks.len(), 2);
            let node::BlockNode::Custom(heading) = &blocks[0] else {
                panic!("custom heading missing");
            };
            assert_eq!(heading.data::<u8>(), Some(&1));
            assert_eq!(heading.as_text(), "Hello 🦀");
            let node::BlockNode::Paragraph(paragraph) = &blocks[1] else {
                panic!("link paragraph missing");
            };
            assert_eq!(paragraph.text(), "local");
            let destinations: Vec<_> = paragraph
                .children
                .iter()
                .flat_map(|run| run.marks.iter())
                .filter_map(|(_, mark)| mark.link.as_ref().map(|link| link.url.as_str()))
                .collect();
            assert_eq!(destinations, ["src/lib.rs#L7"]);
        });
        state.update(cx, |state, cx| {
            state.set_markdown_extensions(extensions, cx);
            state.set_text(source, cx);
        });
        cx.run_until_parked();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "unchanged source/options must reuse parse"
        );
    }

    #[gpui::test]
    fn held_old_completion_and_failure_cannot_publish_over_new_revision(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let state = cx.update(|cx| {
            cx.new(|cx| {
                TextViewState::new_configured(
                    TextViewFormat::Markdown,
                    "A",
                    Arc::default(),
                    true,
                    cx,
                )
            })
        });
        let held_a = parse_content(
            TextViewFormat::Markdown,
            ParsedContent::default(),
            &UpdateOptions {
                revision: 1,
                pending_text: "A".into(),
                append: false,
                mode: ParseMode::Replace,
                markdown_extensions: Arc::default(),
            },
        )
        .unwrap();
        state.update(cx, |state, cx| {
            state.set_text("B", cx);
            state.set_text("C", cx);
            assert!(!state.accept_parsed_update(
                ParsedUpdate {
                    revision: 1,
                    full_parse: true,
                    selection_compatible: false,
                    baseline_ack: false,
                    result: Ok(held_a),
                },
                cx
            ));
            assert!(!state.accept_parsed_update(
                ParsedUpdate {
                    revision: 2,
                    full_parse: true,
                    selection_compatible: false,
                    baseline_ack: false,
                    result: Err("obsolete B failure".into()),
                },
                cx
            ));
            assert!(state.source().is_empty());
            assert!(state.parsed_error.is_none());
        });
        cx.run_until_parked();
        state.read_with(cx, |state, _| {
            assert_eq!(state.source().as_str(), "C");
            assert!(state.parsed_error.is_none());
        });
    }

    #[gpui::test]
    fn background_replacements_publish_only_latest_unicode_source(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let state = cx.update(|cx| {
            cx.new(|cx| {
                TextViewState::new_configured(
                    TextViewFormat::Markdown,
                    "A",
                    Arc::default(),
                    true,
                    cx,
                )
            })
        });
        state.update(cx, |state, cx| {
            state.set_text("B", cx);
            state.set_text("**C 🦀**", cx);
        });
        state.read_with(cx, |state, _| assert!(state.source().is_empty()));
        cx.run_until_parked();
        state.read_with(cx, |state, _| {
            assert_eq!(state.source().as_str(), "**C 🦀**");
            assert_eq!(state.parsed_content.document.text().trim(), "C 🦀");
        });
    }

    #[gpui::test]
    fn background_bounded_large_readme_keeps_first_heading_and_unicode_tail(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::init);
        // The owner admits at most 512 KiB of README. Keep this fixture under
        // that bound while exercising a long Unicode paragraph and final link.
        let source = format!("# First\n\n{}\n\n[最後](#first)", "文 ".repeat(120_000));
        assert!(source.len() < 512 * 1024);
        let state = cx.update(|cx| {
            cx.new(|cx| {
                TextViewState::new_configured(
                    TextViewFormat::Markdown,
                    &source,
                    Arc::default(),
                    true,
                    cx,
                )
            })
        });
        state.read_with(cx, |state, _| {
            assert!(state.parsed_content.document.blocks.is_empty())
        });
        cx.run_until_parked();
        state.read_with(cx, |state, _| {
            assert_eq!(state.source().as_str(), source);
            assert_eq!(state.parsed_content.document.blocks.len(), 3);
            let rendered = state.parsed_content.document.text();
            assert!(rendered.starts_with("First"));
            assert!(rendered.trim_end().ends_with("最後"));
            assert_eq!(
                state.parsed_content.document.blocks[0]
                    .span()
                    .unwrap()
                    .start,
                0
            );
        });
    }

    #[gpui::test]
    fn small_full_replace_parses_before_background_executor_runs(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let markdown = "# ready";
        let state = cx.update(|cx| cx.new(|cx| TextViewState::markdown(markdown, cx)));

        state.read_with(cx, |state, _| {
            assert_eq!(state.source().as_str(), markdown);
            assert_eq!(state.parsed_content.document.blocks.len(), 1);
        });
    }

    #[gpui::test]
    fn large_markdown_and_html_full_replacements_wait_for_background_executor(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::init);
        let markdown = "# x\n\n".repeat(MAX_SYNC_FULL_REPLACE_BYTES / 5 + 1);
        let html = format!("<p>{}</p>", "x".repeat(MAX_SYNC_FULL_REPLACE_BYTES + 1));
        assert!(markdown.len() > MAX_SYNC_FULL_REPLACE_BYTES);
        assert!(html.len() > MAX_SYNC_FULL_REPLACE_BYTES);

        let (markdown_state, html_state) = cx.update(|cx| {
            (
                cx.new(|cx| TextViewState::markdown(&markdown, cx)),
                cx.new(|cx| TextViewState::html(&html, cx)),
            )
        });

        markdown_state.read_with(cx, |state, _| {
            assert_eq!(state.text.as_str(), markdown.as_str());
            assert!(state.source().as_str().is_empty());
            assert!(state.parsed_content.document.blocks.is_empty());
        });
        html_state.read_with(cx, |state, _| {
            assert_eq!(state.text.as_str(), html.as_str());
            assert!(state.source().as_str().is_empty());
            assert!(state.parsed_content.document.blocks.is_empty());
        });

        cx.run_until_parked();

        markdown_state.read_with(cx, |state, _| {
            assert_eq!(state.source().as_str(), markdown.as_str());
            assert!(!state.parsed_content.document.blocks.is_empty());
        });
        html_state.read_with(cx, |state, _| {
            assert_eq!(state.source().as_str(), html.as_str());
            assert!(!state.parsed_content.document.blocks.is_empty());
        });
    }

    #[gpui::test]
    fn async_full_replace_then_push_str_preserves_complete_source(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let state = cx.update(|cx| cx.new(|cx| TextViewState::markdown("old", cx)));
        cx.run_until_parked();

        let replacement = "x".repeat(MAX_SYNC_FULL_REPLACE_BYTES + 1);
        let expected = format!("{replacement} tail");
        state.update(cx, |state, cx| {
            state.set_text(&replacement, cx);
            state.push_str(" tail", cx);
        });
        cx.run_until_parked();

        state.read_with(cx, |state, _| {
            assert_eq!(state.text.as_str(), expected.as_str());
            assert_eq!(state.source().as_str(), expected.as_str());
        });
    }

    #[gpui::test]
    fn html_push_str_keeps_earlier_blocks(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let state = cx.update(|cx| cx.new(|cx| TextViewState::html("<p>first</p>", cx)));
        cx.run_until_parked();

        state.update(cx, |state, cx| {
            state.push_str("<p>second</p>", cx);
        });
        cx.run_until_parked();

        state.read_with(cx, |state, _| {
            assert_eq!(state.source().as_str(), "<p>first</p><p>second</p>");
            let text = state
                .parsed_content
                .document
                .blocks
                .iter()
                .map(|block| block.text())
                .collect::<String>();
            assert!(text.contains("first"), "lost the first block: {text:?}");
            assert!(text.contains("second"), "lost the appended block: {text:?}");
        });
    }

    #[gpui::test]
    fn set_text_then_push_str_appends_to_replaced_content(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let state = cx.update(|cx| cx.new(|cx| TextViewState::markdown("old", cx)));
        cx.run_until_parked();

        state.update(cx, |state, cx| {
            state.set_text("", cx);
            state.push_str("new", cx);
            state.push_str(" text", cx);
        });
        cx.run_until_parked();

        state.read_with(cx, |state, _| {
            assert_eq!(state.text.as_str(), "new text");
            assert_eq!(state.source().as_str(), "new text");
        });

        state.update(cx, |state, cx| {
            state.set_text("", cx);
        });
        cx.run_until_parked();

        state.read_with(cx, |state, _| {
            assert_eq!(state.text.as_str(), "");
            assert_eq!(state.source().as_str(), "");
        });
    }

    #[gpui::test]
    fn full_parse_coalesced_with_append_preserves_new_select_all(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let state = cx.update(|cx| cx.new(|cx| TextViewState::markdown("old", cx)));
        cx.run_until_parked();

        state.update(cx, |state, cx| {
            state.set_text("new", cx);
            state.push_str(" text", cx);
            state.select_all(cx);
        });
        cx.run_until_parked();

        state.read_with(cx, |state, _| {
            assert!(state.select_all);
            assert_eq!(state.selected_text().trim(), "new text");
        });
    }

    #[test]
    fn update_options_merge_keeps_latest_full_text() {
        let mut options = UpdateOptions {
            revision: 1,
            pending_text: "old".to_string(),
            append: true,
            mode: ParseMode::Compatible,
            markdown_extensions: Arc::default(),
        };

        options.merge(UpdateOptions {
            revision: 2,
            pending_text: "new".to_string(),
            append: false,
            mode: ParseMode::BaselineAck,
            markdown_extensions: Arc::default(),
        });
        options.merge(UpdateOptions {
            revision: 3,
            pending_text: " text".to_string(),
            append: true,
            mode: ParseMode::Compatible,
            markdown_extensions: Arc::default(),
        });

        assert_eq!(options.revision, 3);
        assert_eq!(options.pending_text, "new text");
        assert!(!options.append);
    }

    #[test]
    fn append_merged_into_async_replace_remains_a_replacement() {
        let mut options = UpdateOptions {
            revision: 1,
            pending_text: "new".to_string(),
            append: false,
            mode: ParseMode::Replace,
            markdown_extensions: Arc::default(),
        };

        options.merge(UpdateOptions {
            revision: 2,
            pending_text: " text".to_string(),
            append: true,
            mode: ParseMode::Compatible,
            markdown_extensions: Arc::default(),
        });

        assert_eq!(options.revision, 2);
        assert_eq!(options.pending_text, "new text");
        assert!(!options.append);
        assert_eq!(options.mode, ParseMode::Replace);
    }

    #[test]
    fn held_actual_worker_coalesces_replacement_and_repair_append_before_paint() {
        use std::sync::{
            Barrier,
            atomic::{AtomicBool, Ordering},
            mpsc,
        };
        let barrier = Arc::new(Barrier::new(2));
        let worker_barrier = barrier.clone();
        let (started, wait_started) = mpsc::channel();
        let entered = AtomicBool::new(false);
        let extensions = Arc::new(MarkdownExtensions::default().mdx().block_parser(
            move |_, context| {
                if context.source() == "A" && !entered.swap(true, Ordering::SeqCst) {
                    started.send(()).unwrap();
                    worker_barrier.wait();
                }
                None
            },
        ));
        let (tx, rx) = pending_update::channel(UpdateOptions::merge);
        let (tx_result, results) = pending_update::channel(ParsedUpdate::merge);
        let options = |revision, text: &str, append| UpdateOptions {
            revision,
            pending_text: text.into(),
            append,
            mode: if append {
                ParseMode::Compatible
            } else {
                ParseMode::Replace
            },
            markdown_extensions: extensions.clone(),
        };
        tx.try_send(options(1, "A", false)).unwrap();
        let worker = std::thread::spawn(move || {
            let mut worker = Box::pin(UpdateFuture::new(TextViewFormat::Markdown, rx, tx_result));
            let waker = futures::task::noop_waker();
            let mut context = std::task::Context::from_waker(&waker);
            assert!(worker.as_mut().poll(&mut context).is_pending());
            assert!(worker.as_mut().poll(&mut context).is_pending());
        });
        wait_started
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert!(results.try_recv().is_err());
        tx.try_send(options(2, "{invalid", false)).unwrap();
        tx.try_send(options(3, "}", true)).unwrap();
        tx.try_send(options(4, " tail", true)).unwrap();
        barrier.wait();
        worker.join().unwrap();
        let ready = results.try_recv().unwrap();
        assert_eq!(ready.revision, 4);
        assert!(!ready.selection_compatible);
        assert_eq!(
            ready.result.unwrap().document.source.as_str(),
            "{invalid} tail"
        );
        assert!(
            results.try_recv().is_err(),
            "no intermediate stale document may remain queued"
        );
    }

    #[test]
    fn real_incremental_parser_preserves_prefix_nodes_and_linear_source_copy_work() {
        let (_, rx) = pending_update::channel(UpdateOptions::merge);
        let (tx_result, _) = pending_update::channel(ParsedUpdate::merge);
        let mut worker = UpdateFuture::new(TextViewFormat::Markdown, rx, tx_result);
        let options = |revision, text: &str, append| UpdateOptions {
            revision,
            pending_text: text.into(),
            append,
            mode: if append {
                ParseMode::Compatible
            } else {
                ParseMode::Replace
            },
            markdown_extensions: Arc::default(),
        };
        let initial = worker
            .apply(options(1, "first\n\nsecond\n\n", false))
            .result
            .unwrap();
        let original_first = initial.document.blocks.get(0).unwrap() as *const node::BlockNode;
        let before = worker.content.document.source.copied_bytes();
        for revision in 2..=1025 {
            worker
                .apply(options(revision, "next\n\n", true))
                .result
                .unwrap();
        }
        assert_eq!(
            worker.content.document.blocks.get(0).unwrap() as *const node::BlockNode,
            original_first
        );
        assert_eq!(initial.document.blocks.len(), 2);
        assert_eq!(worker.content.document.blocks.len(), 1026);
        let copied = worker.content.document.source.copied_bytes() - before;
        assert!(
            copied < 1024 * 64,
            "copied {copied} bytes while appending only stable short paragraphs"
        );
        assert_eq!(
            worker.content.document.blocks.last().unwrap().text().trim(),
            "next"
        );
        assert!(
            worker
                .content
                .document
                .source
                .as_str()
                .starts_with("first\n\nsecond\n\n")
        );
    }

    #[test]
    fn failed_mdx_basis_recovers_true_logical_source_on_later_append() {
        let extensions = Arc::new(MarkdownExtensions::default().mdx());
        for (initial, failed, failed_append, expected) in [
            ("A", "{invalid", false, "{invalid}"),
            ("", "{invalid", false, "{invalid}"),
            ("A", "\n{invalid", true, "A\n{invalid}"),
        ] {
            let (_, rx) = pending_update::channel(UpdateOptions::merge);
            let (tx_result, _) = pending_update::channel(ParsedUpdate::merge);
            let mut worker = UpdateFuture::new(TextViewFormat::Markdown, rx, tx_result);
            let options = |revision, text: &str, append| UpdateOptions {
                revision,
                pending_text: text.into(),
                append,
                mode: if append {
                    ParseMode::Compatible
                } else {
                    ParseMode::Replace
                },
                markdown_extensions: extensions.clone(),
            };
            assert!(worker.apply(options(1, initial, false)).result.is_ok());
            let failed = worker.apply(options(2, failed, failed_append));
            assert!(failed.result.is_err());
            assert!(!worker.basis_checked);
            let repaired = worker.apply(options(3, "}", true));
            assert!(repaired.full_parse);
            assert!(!repaired.selection_compatible);
            assert_eq!(repaired.result.unwrap().document.source.as_str(), expected);
            assert!(worker.basis_checked);
        }
    }

    #[test]
    fn pending_completed_results_keep_latest_document_and_required_selection_reset() {
        let (tx, rx) = pending_update::channel(ParsedUpdate::merge);
        let parsed = |revision: usize,
                      source: &str,
                      append_compatible: bool,
                      baseline_ack: bool| ParsedUpdate {
            revision,
            full_parse: !append_compatible,
            selection_compatible: append_compatible,
            baseline_ack,
            result: parse_content(
                TextViewFormat::Markdown,
                ParsedContent::default(),
                &UpdateOptions {
                    revision,
                    pending_text: source.into(),
                    append: false,
                    mode: ParseMode::Replace,
                    markdown_extensions: Arc::default(),
                },
            ),
        };
        // The UI is busy while a replacement and two subsequent appended
        // publications complete. It must receive only the cumulative third.
        tx.try_send(parsed(1, "replacement", false, false)).unwrap();
        tx.try_send(parsed(2, "replacement α", true, false))
            .unwrap();
        tx.try_send(parsed(3, "replacement α β", true, false))
            .unwrap();
        let ready = rx.try_recv().unwrap();
        assert_eq!(ready.revision, 3);
        assert_eq!(
            ready.result.unwrap().document.text().trim(),
            "replacement α β"
        );
        assert!(
            !ready.selection_compatible,
            "skipped replacement still requires reset"
        );
        assert!(rx.try_recv().is_err());

        tx.try_send(parsed(4, "sync baseline", false, true))
            .unwrap();
        tx.try_send(parsed(5, "sync baseline appended", true, false))
            .unwrap();
        let ready = rx.try_recv().unwrap();
        assert!(
            ready.selection_compatible,
            "already installed baseline must preserve new selection"
        );
        assert!(!ready.baseline_ack);
    }

    #[test]
    fn pending_publications_replace_obsolete_snapshots_and_preserve_deltas() {
        let (tx, rx) = pending_update::channel(UpdateOptions::merge);
        let options = |revision, text: &str, append| UpdateOptions {
            revision,
            pending_text: text.into(),
            append,
            mode: if append {
                ParseMode::Compatible
            } else {
                ParseMode::Replace
            },
            markdown_extensions: Arc::default(),
        };
        tx.try_send(options(1, "A", false)).unwrap();
        let held = rx.try_recv().unwrap();
        tx.try_send(options(2, "B", false)).unwrap();
        tx.try_send(options(3, "C 🦀", false)).unwrap();
        tx.try_send(options(4, " tail", true)).unwrap();
        let latest = rx.try_recv().unwrap();
        assert_eq!(held.pending_text, "A");
        assert_eq!(latest.revision, 4);
        assert_eq!(latest.pending_text, "C 🦀 tail");
        assert!(!latest.append);
        assert_eq!(latest.mode, ParseMode::Replace);
        assert!(
            rx.try_recv().is_err(),
            "obsolete B must not have its own publication"
        );

        tx.try_send(options(5, " α", true)).unwrap();
        tx.try_send(options(6, " β", true)).unwrap();
        let delta = rx.try_recv().unwrap();
        assert!(delta.append);
        assert_eq!(delta.pending_text, " α β");
    }

    #[gpui::test]
    fn select_all_returns_rendered_text(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let state = cx.update(|cx| cx.new(|cx| TextViewState::markdown("**quick** value", cx)));
        cx.run_until_parked();

        state.update(cx, |state, cx| {
            state.select_all(cx);
        });

        state.read_with(cx, |state, _| {
            assert!(state.has_view_selection());
            assert_eq!(state.selected_text().trim(), "quick value");
        });

        state.update(cx, |state, cx| {
            state.clear_selection(cx);
        });

        state.read_with(cx, |state, _| {
            assert!(!state.has_view_selection());
            assert_eq!(state.selected_text(), "");
        });
    }

    #[gpui::test]
    fn select_all_in_source_format_returns_source(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let markdown = "**quick** value";
        let state = cx.update(|cx| cx.new(|cx| TextViewState::markdown(markdown, cx)));
        cx.run_until_parked();

        state.update(cx, |state, cx| state.select_all(cx));

        // The default (plain) mode strips the markup.
        state.read_with(cx, |state, _| {
            assert_eq!(state.selected_text().trim(), "quick value");
        });

        state.update(cx, |state, cx| {
            state.set_selection_format(SelectionFormat::Source, cx)
        });

        // Source mode yields the whole source verbatim.
        state.read_with(cx, |state, _| {
            assert_eq!(state.selected_text().trim(), markdown);
        });
    }

    #[gpui::test]
    fn set_markdown_extensions_reparses_existing_text(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let state = cx.update(|cx| cx.new(|cx| TextViewState::markdown("$TSLA.US", cx)));
        cx.run_until_parked();

        let extensions = MarkdownExtensions::default().block_parser(|node, cx| {
            let markdown::mdast::Node::Paragraph(paragraph) = node else {
                return None;
            };
            let [markdown::mdast::Node::Text(text)] = paragraph.children.as_slice() else {
                return None;
            };
            let symbol = text.value.strip_prefix('$')?.to_string();
            let node_text = format!("${symbol}");

            Some(
                MarkdownNode::new("ticker", symbol)
                    .text(node_text)
                    .markdown(cx.node_source(node).unwrap_or_default()),
            )
        });

        state.update(cx, |state, cx| {
            state.set_markdown_extensions(Arc::new(extensions), cx);
        });
        cx.run_until_parked();

        state.read_with(cx, |state, _| {
            let node::BlockNode::Custom(node) = &state.parsed_content.document.blocks[0] else {
                panic!("expected custom markdown node");
            };
            assert_eq!(node.name(), "ticker");
            assert_eq!(node.data::<String>().map(String::as_str), Some("TSLA.US"));
        });
    }
}

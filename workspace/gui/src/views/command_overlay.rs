//! `CommandOverlay` — the `?` shortcuts cheat sheet and the `cmd-shift-P`
//! command palette (GUI-PLAN §23.1, §23.3; docs/LIMITATIONS.md L15).
//!
//! # Why one view, two modes
//!
//! `main.rs` states the invariant this view exists to satisfy: "the `?` cheat
//! sheet and the command palette both render from the same registry … so a
//! binding cannot exist without being discoverable and cannot be documented
//! without working." Before this file, neither half of that sentence was
//! true — `OpenCommandPalette` and `ToggleShortcutsOverlay` were bound in
//! `app::keymaps`, named real `OverlayKind` variants, and had **zero**
//! `.on_action` handlers anywhere (L15).
//!
//! Both surfaces read the identical data — `app::keymaps::KEYMAP_REGISTRY` —
//! grouped by context. The only behavioural difference is what happens on
//! select:
//!
//! - **Shortcuts** (`?`): read-only. Rows exist so the reader can see what
//!   every binding does; clicking or confirming one does nothing beyond what
//!   Escape already does.
//! - **Palette** (`cmd-shift-P`): confirming the highlighted row dispatches
//!   that row's actual `Action` through the focused window — the same
//!   `Action` GPUI would run if the reader had typed the real keystroke — and
//!   then closes. There is no second, palette-only code path for "running a
//!   command"; it is literally `KeyBinding::action().boxed_clone()` handed to
//!   `Window::dispatch_action`, which is what keeps this view honest: it can
//!   never run something a keystroke could not.
//!
//! One deliberate asymmetry: Palette mode filters out the `"(Linux/Windows)"`
//! duplicate rows that exist only so `?` can show the alternate keystroke on
//! that platform — a command palette lists *commands*, and a command that
//! already has a row is not a second command. Shortcuts mode keeps them,
//! because teaching every real keystroke is the entire point of `?`.
//!
//! # Keyboard
//!
//! Navigation is not reinvented here. `escape` / `enter` / `up` / `down` are
//! already bound generically under the `"Overlay"` key-context in
//! `app::keymaps` (the same bindings `OmniSearch` uses) — this view's root
//! carries `key_context("<Mode> Overlay")` and registers `.on_action` for
//! `DismissOverlay` / `ConfirmOverlay` / `MoveSelectionUp` /
//! `MoveSelectionDown`, exactly the way `OmniSearch` does. No text input, no
//! fuzzy filter: v1 is a scrollable list. See `remaining` in the LOCKSMITH
//! report for why that scope line was drawn here.
//!
//! # This view never closes itself (§15)
//!
//! Same rule as `OmniSearch`: closing/focus-restoration is `Shell`'s job, not
//! this view's. `Escape` and a successful command execution both emit
//! [`CommandOverlayEvent::Dismiss`]; `Shell::close_overlay` pops the overlay
//! stack and returns focus to the pane.

use std::rc::Rc;

use gpui::{
    App, ClickEvent, Context, EventEmitter, FocusHandle, Focusable, InteractiveElement as _,
    IntoElement, Keystroke, ParentElement, Render, SharedString, StatefulInteractiveElement as _,
    Styled, Window, div, px, prelude::FluentBuilder as _,
};
use gpui_component::StyledExt as _;

use crate::app::actions::{ConfirmOverlay, DismissOverlay, MoveSelectionDown, MoveSelectionUp};
use crate::app::keymaps::{KEYMAP_REGISTRY, KeymapEntry};
use crate::theme::ext::ThemeExtAccessor as _;
use crate::ui::{KeyHint, SectionHeader};

// ─────────────────────────────────────────────────────────────────────────────
// Mode
// ─────────────────────────────────────────────────────────────────────────────

/// Which of the two surfaces a given `CommandOverlay` instance is.
///
/// A `bool` would work but would read as `CommandOverlay::new(true, ...)` at
/// every call site — exactly the "bool + comment" shape AGENTS-DOCTRINE §3
/// asks for an enum instead of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandOverlayMode {
    /// `?` — read-only cheat sheet, every binding including Linux alternates.
    Shortcuts,
    /// `cmd-shift-P` — confirming a row dispatches its `Action` and closes.
    Palette,
}

impl CommandOverlayMode {
    fn title(self) -> SharedString {
        match self {
            Self::Shortcuts => SharedString::from("Keyboard Shortcuts"),
            Self::Palette => SharedString::from("Command Palette"),
        }
    }

    /// The `key_context` token pair this mode's root div carries.
    ///
    /// `"Overlay"` is the shared token that already carries escape / enter /
    /// up / down (see the module doc); the first token exists for any future
    /// binding that wants to scope to exactly one of these two surfaces.
    fn key_context(self) -> &'static str {
        match self {
            Self::Shortcuts => "Shortcuts Overlay",
            Self::Palette => "CommandPalette Overlay",
        }
    }

    /// Whether a row is clickable/selectable, or purely informational.
    fn selectable(self) -> bool {
        matches!(self, Self::Palette)
    }
}

/// Turn a raw `KeymapEntry::context` string into a reader-facing group label.
///
/// A handful of contexts get a friendlier name; anything unrecognised is
/// shown verbatim rather than hidden — an unmapped context is a sign this
/// list needs a new case, not a reason to drop rows silently.
fn humanize_context(context: &str) -> SharedString {
    match context {
        "global" | "!InputFocused" => SharedString::from("Global"),
        "Overlay" => SharedString::from("Overlay"),
        "OmniSearch" => SharedString::from("Omni-Search"),
        "SymbolPage" => SharedString::from("Symbol Page"),
        "Pane" => SharedString::from("Tabs"),
        "GraphView" => SharedString::from("Graph View"),
        "ProjectPanel" => SharedString::from("Project Panel"),
        other => SharedString::from(other.to_string()),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// CommandOverlay
// ─────────────────────────────────────────────────────────────────────────────

/// One row of the palette / cheat sheet, resolved once at construction.
///
/// # Why this type exists
///
/// The struct it replaced held `&'static KeymapEntry` and let `render` do the
/// rest, under a doc comment claiming "`render` only walks `groups` and clones
/// `SharedString`s". That claim was false, and the instrumentation is what
/// found it: `command_overlay.render` measured **0.378 ms self-mean**, four
/// times every other region in the app, on a view whose content never changes
/// after construction.
///
/// The cost was `(entry.binding)()` — called once per row per frame. That
/// factory does not *look up* a binding, it *builds* one: `KeyBinding::new`
/// parses the keystroke string into `Keystroke`s and compiles the context
/// string into a `KeyBindingContextPredicate`, and the whole result was thrown
/// away after reading `.keystrokes()`. Seventy-nine of those per frame.
///
/// Holding the parsed keystrokes instead makes the original comment true rather
/// than merely aspirational, and doctrine §8's rule applies: when a comment and
/// the code disagree, the comment is the bug — and the expensive one, because
/// it tells the next reader not to look here.
struct PreparedCommand {
    /// The registry entry, kept for [`CommandOverlay::confirm`], which needs
    /// the real `Action` and is not on a per-frame path.
    entry: &'static KeymapEntry,
    /// The row's label, interned once.
    description: SharedString,
    /// The already-parsed chord, in the order `Kbd` chips must render.
    ///
    /// `Rc<[_]>` so handing it to a `KeyHint` each frame is a refcount bump.
    keystrokes: Rc<[Keystroke]>,
}

/// One context's worth of rows, as a *range* into [`CommandOverlay::rows`].
///
/// # Why a range and not a second `Vec` of entries
///
/// It used to be `Vec<(SharedString, Vec<&KeymapEntry>)>` beside a flat
/// `Vec<&KeymapEntry>`, with `selected` indexing the flat one and `render`
/// walking the grouped one — two containers that had to stay in exactly the
/// same order or the highlight would land on a different command than the one
/// `confirm` ran. Nothing in the types said so; a unit test
/// (`groups_flatten_to_the_same_entries_as_the_flat_row_list`) stood in for the
/// guarantee.
///
/// A range cannot disagree with the list it indexes. Doctrine §3: prefer the
/// shape in which the illegal state is unrepresentable over the test that
/// notices it.
struct CommandGroup {
    /// Reader-facing context label.
    label: SharedString,
    /// Half-open range of [`CommandOverlay::rows`] belonging to this group.
    rows: std::ops::Range<usize>,
}

/// Resolve the registry into display rows and their groups, once.
///
/// A free function rather than a method so it can be tested without a GPUI
/// `Context`: the tests below assert the ranges tile the row list exactly, and
/// a guarantee that can only be checked through a live window is a guarantee
/// nobody checks.
fn build_rows(mode: CommandOverlayMode) -> (Vec<PreparedCommand>, Vec<CommandGroup>) {
    let mut rows: Vec<PreparedCommand> = Vec::new();
    let mut groups: Vec<CommandGroup> = Vec::new();

    for entry in KEYMAP_REGISTRY.iter() {
        // Palette lists *commands*; a `"(Linux/Windows)"` row is the same
        // command with an alternate keystroke, not a second command.
        if mode == CommandOverlayMode::Palette && entry.description.ends_with("(Linux/Windows)") {
            continue;
        }

        let label = humanize_context(entry.context);
        let ix = rows.len();
        rows.push(PreparedCommand {
            entry,
            description: SharedString::from(entry.description),
            // `keystrokes()` (not a hand-parse of `entry.keystroke`) so a
            // chord like `"g d"` becomes two `Kbd` chips in the right order —
            // the factory already parsed it once, correctly. The point of
            // doing it *here* is that "once" now means once per overlay
            // rather than once per row per frame.
            keystrokes: (entry.binding)()
                .keystrokes()
                .iter()
                .map(|k| k.inner().clone())
                .collect(),
        });

        match groups.last_mut() {
            Some(last) if last.label == label => last.rows.end = ix + 1,
            _ => groups.push(CommandGroup {
                label,
                rows: ix..ix + 1,
            }),
        }
    }

    (rows, groups)
}

/// The `?` cheat sheet / command palette view.
///
/// Grouping, filtering, keystroke parsing and label interning all happen once,
/// in [`CommandOverlay::new`] — not in `render` — per this codebase's
/// zero-string-work-in-render convention (§1.1.4). `render` walks `groups`,
/// slices `rows`, and clones a `SharedString` and an `Rc` per row.
pub struct CommandOverlay {
    focus: FocusHandle,
    mode: CommandOverlayMode,
    /// Every row, in display order — the index space `selected` lives in.
    rows: Vec<PreparedCommand>,
    /// Contiguous slices of `rows`, one per context, in first-seen order.
    groups: Vec<CommandGroup>,
    /// Palette mode only: index into `rows` of the highlighted command.
    /// Ignored (but kept valid) in Shortcuts mode.
    selected: usize,
}

/// What this view reports upward. Same one-event shape as `OmniSearchEvent`'s
/// `Dismiss` — this view has no second event because *running* a command is
/// not something `Shell` needs to know about: this view dispatches the
/// `Action` itself, onto the same window `Shell` already owns, and only asks
/// to be closed afterward.
pub enum CommandOverlayEvent {
    Dismiss,
}

impl CommandOverlay {
    /// Build a view over the live keymap registry, in the given mode.
    pub fn new(mode: CommandOverlayMode, cx: &mut Context<Self>) -> Self {
        let (rows, groups) = build_rows(mode);
        Self {
            focus: cx.focus_handle(),
            mode,
            rows,
            groups,
            selected: 0,
        }
    }

    /// Move the highlighted row by `delta`, wrapping at both ends.
    ///
    /// No-op in Shortcuts mode's UI (nothing renders the selection there),
    /// but harmless to compute — keeping the state consistent means switching
    /// a hypothetical future toggle-between-modes control never has to
    /// reconcile a stale index.
    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            return;
        }
        let len = self.rows.len() as isize;
        let next = (self.selected as isize + delta).rem_euclid(len);
        self.selected = next as usize;
        cx.notify();
    }

    /// `ConfirmOverlay` (Enter) or a row click.
    ///
    /// Shortcuts mode has nothing to confirm — the row is informational — so
    /// this only dismisses. Palette mode dispatches the selected row's real
    /// `Action` onto `window` before dismissing, which is the one place this
    /// view does anything beyond rendering.
    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.mode.selectable() {
            if let Some(row) = self.rows.get(self.selected) {
                // `KeyBinding::action()` borrows from the temporary
                // `KeyBinding` the factory `fn` just built; `boxed_clone()`
                // takes an owned copy before that temporary drops, so this is
                // exactly the `Action` a real keystroke would have run.
                //
                // Still built on demand rather than cached: this runs once per
                // Enter, and caching a `Box<dyn Action>` per row would trade a
                // free path for eighty boxed actions held for the life of an
                // overlay that is usually dismissed without confirming.
                let action = (row.entry.binding)().action().boxed_clone();
                window.dispatch_action(action, cx);
            }
        }
        cx.emit(CommandOverlayEvent::Dismiss);
    }

    /// Build the row elements, section headers interleaved, for `render`.
    fn render_rows(&self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let (sp, colours) = {
            let ext = cx.theme_ext();
            (ext.space, ext.colours)
        };
        let selectable = self.mode.selectable();

        let mut out = Vec::with_capacity(self.rows.len() + self.groups.len());
        for group in &self.groups {
            out.push(
                div()
                    .px(sp.space_3)
                    .pt(sp.space_2)
                    .child(SectionHeader::new(group.label.clone()))
                    .into_any_element(),
            );
            // `group.rows` indexes `self.rows` by construction, so this slice
            // and `selected` are in the same index space with nothing to keep
            // in sync — see [`CommandGroup`].
            for (offset, row_data) in self.rows[group.rows.clone()].iter().enumerate() {
                let row_ix = group.rows.start + offset;
                let is_selected = selectable && row_ix == self.selected;

                let mut row = div()
                    .id(("command_overlay.row", row_ix))
                    .w_full()
                    .px(sp.space_3)
                    .py(sp.space_1)
                    .rounded(sp.r_sm)
                    .when(is_selected, |el| el.bg(colours.bg_active))
                    .when(selectable, |el| {
                        el.cursor_pointer().hover(|s| s.bg(colours.bg_hover))
                    })
                    // Two refcount bumps. Everything this row draws was
                    // resolved in `new` — see [`PreparedCommand`].
                    .child(KeyHint::new(
                        row_data.description.clone(),
                        row_data.keystrokes.clone(),
                    ));

                if selectable {
                    row = row.on_click(cx.listener(move |view, _: &ClickEvent, window, cx| {
                        view.selected = row_ix;
                        view.confirm(window, cx);
                    }));
                }

                out.push(row.into_any_element());
            }
        }
        out
    }
}

impl Focusable for CommandOverlay {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EventEmitter<CommandOverlayEvent> for CommandOverlay {}

impl Render for CommandOverlay {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _span = crate::perf::scope(crate::perf::Region::CommandOverlay);
        let (sp, ts, colours) = {
            let ext = cx.theme_ext();
            (ext.space, ext.type_scale, ext.colours)
        };
        let mode = self.mode;
        let rows = self.render_rows(cx);

        let panel = div()
            .id("command_overlay.panel")
            .w(px(480.0))
            .max_h(gpui::relative(0.6))
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded(sp.r_xl)
            .bg(colours.bg_raised)
            .border_1()
            .border_color(colours.border_default)
            .shadow_lg()
            // Modal in the mouse sense: a click inside the panel must not
            // fall through to the scrim behind it (same reasoning as
            // `OmniSearch`'s identical `.occlude()`).
            .occlude()
            .child(
                div()
                    .flex_none()
                    .px(sp.space_4)
                    .py(sp.space_2)
                    .border_b_1()
                    .border_color(colours.border_default)
                    .text_size(ts.ui.size)
                    .line_height(ts.ui.line_height)
                    .text_color(colours.fg_default)
                    .child(mode.title()),
            )
            .child(
                div()
                    .id("command_overlay.list")
                    .flex_1()
                    .overflow_y_scroll()
                    .v_flex()
                    .py(sp.space_2)
                    .children(rows),
            );

        div()
            .id("command_overlay")
            .track_focus(&self.focus)
            .key_context(mode.key_context())
            .on_action(cx.listener(|_view, _: &DismissOverlay, _window, cx| {
                cx.emit(CommandOverlayEvent::Dismiss);
            }))
            .on_action(cx.listener(|view, _: &ConfirmOverlay, window, cx| {
                view.confirm(window, cx);
            }))
            .on_action(cx.listener(|view, _: &MoveSelectionUp, _window, cx| {
                view.move_selection(-1, cx);
            }))
            .on_action(cx.listener(|view, _: &MoveSelectionDown, _window, cx| {
                view.move_selection(1, cx);
            }))
            .size_full()
            .flex()
            .justify_center()
            .items_start()
            .pt(sp.space_8)
            .child(panel)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// The group ranges tile the row list exactly: every row belongs to one
    /// group, in order, with no gap and no overlap.
    ///
    /// This is what `render_rows` (which slices `rows` by group) and `confirm`
    /// (which indexes `rows` by `selected`) both need in order to agree about
    /// which command a highlighted row is. The previous shape kept two parallel
    /// containers and this test compared them; the ranges make the disagreement
    /// unrepresentable, so what is left to check is that the tiling is total —
    /// a group whose range stopped short would silently hide the rows after it.
    #[test]
    fn group_ranges_tile_the_row_list_exactly() {
        for mode in [CommandOverlayMode::Shortcuts, CommandOverlayMode::Palette] {
            let (rows, groups) = build_rows(mode);
            assert!(!rows.is_empty(), "{mode:?}: the registry produced no rows");

            let mut next = 0usize;
            for group in &groups {
                assert_eq!(
                    group.rows.start, next,
                    "{mode:?}: group {:?} starts at {} but the previous group ended at {next}",
                    group.label, group.rows.start,
                );
                assert!(
                    group.rows.end > group.rows.start,
                    "{mode:?}: group {:?} is empty and would render a header over nothing",
                    group.label,
                );
                next = group.rows.end;
            }
            assert_eq!(
                next,
                rows.len(),
                "{mode:?}: the groups cover {next} of {} rows — the remainder \
                 would never render",
                rows.len(),
            );
        }
    }

    /// Every row carries a parsed chord and a label before any frame is drawn.
    ///
    /// This is the property that makes `render` free: the measurement that
    /// prompted this shape (`command_overlay.render` at 0.378 ms self-mean,
    /// four times every other region) was entirely `(entry.binding)()` being
    /// re-evaluated per row per frame. A row that reached `render` with nothing
    /// parsed would put that cost straight back.
    #[test]
    fn every_row_is_fully_resolved_before_render() {
        for mode in [CommandOverlayMode::Shortcuts, CommandOverlayMode::Palette] {
            let (rows, _) = build_rows(mode);
            for row in &rows {
                assert!(
                    !row.description.is_empty(),
                    "{mode:?}: a row reached the view with no label",
                );
                assert!(
                    !row.keystrokes.is_empty(),
                    "{mode:?}: {:?} carries no parsed keystroke, so its row would \
                     render a description with no chip",
                    row.description,
                );
            }
        }
    }

    /// Palette mode must exclude every `"(Linux/Windows)"` duplicate; a
    /// command palette lists commands, not per-platform keystroke variants.
    #[test]
    fn palette_mode_excludes_linux_alternate_rows() {
        let view_rows: Vec<&'static str> = KEYMAP_REGISTRY
            .iter()
            .filter(|e| !e.description.ends_with("(Linux/Windows)"))
            .map(|e| e.description)
            .collect();
        assert!(
            KEYMAP_REGISTRY.len() > view_rows.len(),
            "fixture assumption: the registry must contain at least one \
             Linux-alternate row for this test to mean anything"
        );
        assert!(
            view_rows.iter().all(|d| !d.ends_with("(Linux/Windows)")),
            "Palette's filter must remove every Linux-alternate row"
        );
    }

    /// `move_selection` wraps at both ends rather than saturating, so
    /// pressing Up on the first row reaches the last one (standard palette
    /// behaviour) instead of doing nothing.
    #[test]
    fn move_selection_wraps_at_both_ends() {
        // Pure index arithmetic, mirrored from `CommandOverlay::move_selection`
        // (which additionally needs a live `Context` this module-level test
        // does not have — see the GPUI-context version of this behaviour
        // exercised via `shell.rs`'s command-overlay integration test).
        fn wrapped(selected: usize, delta: isize, len: usize) -> usize {
            let len = len as isize;
            ((selected as isize + delta).rem_euclid(len)) as usize
        }
        let len = build_rows(CommandOverlayMode::Shortcuts).0.len();
        assert_eq!(wrapped(0, -1, len), len - 1, "Up from the first row wraps to the last");
        assert_eq!(wrapped(len - 1, 1, len), 0, "Down from the last row wraps to the first");
    }

    /// `humanize_context` never returns an empty label — a blank group
    /// header would be as silent a bug as `keymaps`'s own
    /// `every_entry_has_description` test guards against for descriptions.
    #[test]
    fn every_context_humanizes_to_a_nonempty_label() {
        for entry in KEYMAP_REGISTRY.iter() {
            let label = humanize_context(entry.context);
            assert!(
                !label.is_empty(),
                "context {:?} humanized to an empty label",
                entry.context
            );
        }
    }
}

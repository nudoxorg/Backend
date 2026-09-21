//! Typed adapters around GPUI CE components.
//!
//! GPUI CE owns interaction semantics: focus traversal, accessible roles,
//! disabled/loading behavior, managed tooltip placement, and popup lifetimes.
//! Nudox owns the visual language. These small builders are the seam between
//! the two: callers ask for a Nudox role and receive a real component with
//! the palette projected by [`crate::theme::sync_components`].

use crate::theme::Theme;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Radius, Space, TypeScale, hairline, radius, type_size};
use gpui::{
    App, Context, ElementId, Entity, FontWeight, InteractiveElement, IntoElement, SharedString,
    Styled, Window, px,
};
use gpui_component::button::{Button, ButtonRounded, ButtonVariants as _};
use gpui_component::input::{Input, InputState};
use gpui_component::list::ListItem;
use gpui_component::popover::{Popover, PopoverState};
use gpui_component::scroll::{Scrollable, ScrollableElement};
use gpui_component::setting::{SettingPage, Settings};
use gpui_component::tooltip::Tooltip;
use gpui_component::tree::{Tree, TreeEntry, TreeState};
use gpui_component::{Disableable as _, FocusableExt as _};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// Visual emphasis for a component-backed action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Weight {
    /// The single action that advances the current flow.
    Primary,
    /// A normal action beside content.
    Regular,
    /// A low emphasis action that should stay in the reading plane.
    Quiet,
}

/// Builds a GPUI CE button with Nudox semantic styling.
///
/// The returned value intentionally remains the concrete component type, so
/// a view may continue to add component behavior (`loading`, `disabled`,
/// `tooltip`, `on_click`, and `tab_index`) without losing the adapter's style.
pub(crate) fn button(
    theme: &Theme,
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    weight: Weight,
) -> Button {
    button_with_state(theme, id, label, weight, false, true)
}

/// Builds a CE button and registers its final disabled/visible state.
pub(crate) fn button_with_state(
    theme: &Theme,
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    weight: Weight,
    disabled: bool,
    visible: bool,
) -> Button {
    let id = id.into();
    let label = label.into();
    let (foreground, background, border) = paints(theme, weight);
    let id_element = ElementId::Name(id.clone());
    let button = Button::new(id_element)
        .label(label.clone())
        .rounded(ButtonRounded::Size(radius(Radius::Small)))
        .compact()
        .tab_index(0)
        .focus_ring(true)
        .text_size(type_size(TypeScale::Small))
        .font_weight(FontWeight::MEDIUM)
        .font_family(theme.ui_face())
        .px(theme.space(Space::Base))
        .py(px(4.0))
        .border(hairline())
        .border_color(border)
        .bg(background)
        .text_color(foreground)
        .focus_visible(|style| {
            style
                .border_color(theme.paint(Paint::Focus))
                .bg(theme.paint(Paint::GiltWash))
        })
        .disabled(disabled);
    theme.register_action(
        ActionMetadata::new(id.clone(), label.clone(), ActionRole::Button)
            .enabled(!disabled)
            .visible(visible),
    );
    match weight {
        Weight::Primary => button.primary(),
        Weight::Regular => button.secondary(),
        Weight::Quiet => button.text(),
    }
}

/// Builds an icon-only GPUI CE button with an explicit accessible name.
pub(crate) fn icon_button(
    theme: &Theme,
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
) -> Button {
    icon_button_with_state(theme, id, label, false, true)
}

/// Builds an icon button while preserving the final CE disabled/visible
/// semantics in the frame collector.
pub(crate) fn icon_button_with_state(
    theme: &Theme,
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    disabled: bool,
    visible: bool,
) -> Button {
    let id = id.into();
    let label = label.into();
    let button = Button::new(ElementId::Name(id.clone()))
        .accessibility_label(label.clone())
        .rounded(ButtonRounded::Size(radius(Radius::Small)))
        .compact()
        .tab_index(0)
        .focus_ring(true)
        .font_family(theme.ui_face())
        .w(px(24.0))
        .h(px(24.0))
        .border(hairline())
        .border_color(gpui::transparent_black())
        .bg(gpui::transparent_black())
        .text_color(theme.paint(Paint::TextDim))
        .focus_visible(|style| {
            style
                .border_color(theme.paint(Paint::Focus))
                .bg(theme.paint(Paint::GiltWash))
        })
        .disabled(disabled);
    theme.register_action(
        ActionMetadata::new(id, label, ActionRole::Button)
            .enabled(!disabled)
            .visible(visible),
    );
    button
}

/// Builds a component-backed text input with the Nudox control treatment.
///
/// `InputState` owns editing, IME composition, selection, clipboard routing,
/// and keyboard actions. This adapter only supplies the visual contract and
/// accessible name, so every input behaves like the component's reference
/// implementation while looking like Nudox.
pub(crate) fn input(
    theme: &Theme,
    state: &Entity<InputState>,
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
) -> Input {
    input_with_state(theme, state, id, label, SharedString::default(), false)
}

/// Builds an input while projecting the CE state's current value and focus
/// into the frame captured for accessibility and journey assertions.
pub(crate) fn input_with_state(
    theme: &Theme,
    state: &Entity<InputState>,
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    value: impl Into<SharedString>,
    focused: bool,
) -> Input {
    let id = id.into();
    let label = label.into();
    theme.register_action(
        ActionMetadata::new(id, label.clone(), ActionRole::Input)
            .focused(focused)
            .with_value(value),
    );
    input_element(theme, state, label)
}

fn input_element(theme: &Theme, state: &Entity<InputState>, label: SharedString) -> Input {
    Input::new(state)
        .aria_label(label)
        .focus_ring(true)
        .font_family(theme.ui_face())
        .text_size(type_size(TypeScale::Interface))
        .text_color(theme.paint(Paint::Text))
        .bg(theme.paint(Paint::Panel))
        .border_color(theme.paint(Paint::Hairline))
}

/// Search is an input with explicit search semantics and the same editing
/// behavior as every other text field. Keeping this distinction at the seam
/// lets command and package search share IME, focus, and clipboard behavior.
pub(crate) fn search_input(
    theme: &Theme,
    state: &Entity<InputState>,
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
) -> Input {
    search_input_with_state(theme, state, id, label, SharedString::default(), false)
}

/// Search variant of [`input_with_state`], preserving the current query in
/// the semantic node while CE remains the owner of editing and focus.
pub(crate) fn search_input_with_state(
    theme: &Theme,
    state: &Entity<InputState>,
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    value: impl Into<SharedString>,
    focused: bool,
) -> Input {
    let id = id.into();
    let label = label.into();
    theme.register_action(
        ActionMetadata::new(id, label.clone(), ActionRole::Search)
            .focused(focused)
            .with_value(value),
    );
    input_element(theme, state, label).prefix(crate::ui::icon::sized(
        theme,
        crate::ui::icon::Icon::Search,
        14.0,
        Paint::TextDim,
    ))
}

/// Builds the real GPUI CE tooltip view with Nudox surface tokens.
pub(crate) fn tooltip(theme: &Theme, text: impl Into<SharedString>) -> Tooltip {
    Tooltip::new(text.into())
        .font_family(theme.ui_face())
        .text_size(type_size(TypeScale::Small))
        .bg(theme.paint(Paint::Raised))
        .text_color(theme.paint(Paint::Text))
        .border(hairline())
        .border_color(theme.paint(Paint::HairlineStrong))
        .rounded(radius(Radius::Small))
        .px(theme.space(Space::Snug))
        .py(theme.space(Space::Tight))
}

/// Builds a component-managed popover. The component owns dismissal, focus
/// restoration, placement, and enter motion; the caller supplies only its
/// real trigger and content.
pub(crate) fn popover<T, E, F>(id: impl Into<ElementId>, trigger: T, content: F) -> Popover
where
    T: gpui_component::Selectable + IntoElement + 'static,
    E: IntoElement,
    F: Fn(&mut PopoverState, &mut Window, &mut Context<PopoverState>) -> E + 'static,
{
    Popover::new(id)
        .trigger(trigger)
        .content(content)
        .appearance(true)
}

/// Builds a popover trigger and records its disclosure semantics in the same
/// explicit frame as the real CE popup.
pub(crate) fn popover_with_action<T, E, F>(
    theme: &Theme,
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    trigger: T,
    content: F,
) -> Popover
where
    T: gpui_component::Selectable + IntoElement + 'static,
    E: IntoElement,
    F: Fn(&mut PopoverState, &mut Window, &mut Context<PopoverState>) -> E + 'static,
{
    let id = id.into();
    theme.register_action(ActionMetadata::new(
        id.clone(),
        label,
        ActionRole::Disclosure,
    ));
    popover(ElementId::Name(id), trigger, content)
}

/// Adds the CE scrollbar and wheel/track-scroll behavior to an existing
/// element while preserving its layout identity.
pub(crate) fn vertical_scroll<E>(element: E) -> Scrollable<E>
where
    E: ScrollableElement,
{
    element.overflow_y_scrollbar()
}

/// Builds the component tree renderer, retaining CE's keyboard navigation,
/// selection, expansion, context-menu, and virtualized scrolling behavior.
pub(crate) fn tree<R>(state: &Entity<TreeState>, render_item: R) -> Tree
where
    R: Fn(usize, &TreeEntry, bool, &mut Window, &mut App) -> ListItem + 'static,
{
    gpui_component::tree::tree(state, render_item)
}

/// Builds a tree and records its focusable root. Individual `ListItem`s can
/// be registered with [`list_item`] as they are rendered.
pub(crate) fn tree_with_action<R>(
    theme: &Theme,
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    state: &Entity<TreeState>,
    render_item: R,
) -> Tree
where
    R: Fn(usize, &TreeEntry, bool, &mut Window, &mut App) -> ListItem + 'static,
{
    theme.register_action(ActionMetadata::new(id, label, ActionRole::TreeItem));
    tree(state, render_item)
}

/// Builds a CE list item with final enabled/visible semantics.
pub(crate) fn list_item(
    theme: &Theme,
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    enabled: bool,
    visible: bool,
) -> ListItem {
    list_item_with_state(theme, id, label, enabled, visible, false)
}

/// Builds a CE list item with selection included in the same semantic frame.
pub(crate) fn list_item_with_state(
    theme: &Theme,
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    enabled: bool,
    visible: bool,
    selected: bool,
) -> ListItem {
    let id = id.into();
    theme.register_action(
        ActionMetadata::new(id.clone(), label, ActionRole::TreeItem)
            .enabled(enabled)
            .visible(visible)
            .selected(selected),
    );
    ListItem::new(ElementId::Name(id))
}

/// Builds the complete CE settings shell from real product setting pages.
/// Settings owns page selection, filtering, keyboard focus, and reset flow.
pub(crate) fn settings(
    id: impl Into<ElementId>,
    pages: impl IntoIterator<Item = SettingPage>,
) -> Settings {
    Settings::new(id).pages(pages)
}

/// Builds the CE settings shell and records its navigation affordance.
pub(crate) fn settings_with_action(
    theme: &Theme,
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    pages: impl IntoIterator<Item = SettingPage>,
) -> Settings {
    let id = id.into();
    theme.register_action(ActionMetadata::new(id.clone(), label, ActionRole::Setting));
    settings(ElementId::Name(id), pages)
}

/// Semantic role used by the in-process action tree. Native accessibility is
/// platform-dependent; this stable tree gives screenshot and journey harnesses
/// a deterministic contract for focus order, labels, and enabled actions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ActionRole {
    Button,
    Input,
    Search,
    Disclosure,
    Navigation,
    TreeItem,
    Setting,
}

/// Metadata for one focusable action in a Nudox route.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ActionMetadata {
    id: SharedString,
    label: SharedString,
    role: ActionRole,
    shortcut: Option<SharedString>,
    enabled: bool,
    visible: bool,
    focused: bool,
    selected: bool,
    expanded: bool,
    loading: bool,
    error: Option<SharedString>,
    value: Option<SharedString>,
    bounds: SemanticBounds,
    focus_order: usize,
}

/// Stable, capture-friendly bounds attached to a rendered semantic node.
///
/// GPUI only resolves pixel bounds after layout. The adapter therefore keeps a
/// small logical contract on the action itself and the capture harness can
/// replace `Deferred` with the measured rectangle at the frame boundary. This
/// makes a node complete in every frame without making render code borrow the
/// mutable window while it is still constructing the tree.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum SemanticBounds {
    /// Layout has not produced a rectangle for this node yet.
    #[default]
    Deferred,
    /// A logical rectangle in the capture viewport's coordinate space.
    Logical {
        /// Horizontal origin.
        x: u32,
        /// Vertical origin.
        y: u32,
        /// Width.
        width: u32,
        /// Height.
        height: u32,
    },
}

impl ActionMetadata {
    /// Creates an enabled action with a stable identity and visible label.
    pub(crate) fn new(
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        role: ActionRole,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            role,
            shortcut: None,
            enabled: true,
            visible: true,
            focused: false,
            selected: false,
            expanded: false,
            loading: false,
            error: None,
            value: None,
            bounds: SemanticBounds::Deferred,
            focus_order: 0,
        }
    }

    /// Adds the keyboard spelling shown by tooltips and the action inspector.
    pub(crate) fn shortcut(mut self, shortcut: impl Into<SharedString>) -> Self {
        self.shortcut = Some(shortcut.into());
        self
    }

    /// Marks an action unavailable without removing it from traversal metadata.
    pub(crate) const fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Marks an action absent from the current rendered tree.
    pub(crate) const fn visible(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }

    /// Records whether this node owns the concrete focus ring in the frame.
    pub(crate) const fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    /// Records the CE selection state for list/tree items and toggles.
    pub(crate) const fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Records whether this disclosure or tree node is expanded.
    pub(crate) const fn expanded(mut self, expanded: bool) -> Self {
        self.expanded = expanded;
        self
    }

    /// Records CE's loading state while retaining a stable semantic node.
    pub(crate) const fn loading(mut self, loading: bool) -> Self {
        self.loading = loading;
        self
    }

    /// Records a typed error state without replacing the action's label.
    pub(crate) fn error(mut self, error: impl Into<SharedString>) -> Self {
        self.error = Some(error.into());
        self
    }

    /// Records the value exposed by an input, combobox, or setting.
    pub(crate) fn with_value(mut self, value: impl Into<SharedString>) -> Self {
        self.value = Some(value.into());
        self
    }

    /// Replaces the deferred logical rectangle after a capture adapter has
    /// measured the corresponding CE element.
    pub(crate) const fn with_bounds(mut self, bounds: SemanticBounds) -> Self {
        self.bounds = bounds;
        self
    }

    pub(crate) fn id(&self) -> &SharedString {
        &self.id
    }

    pub(crate) fn label(&self) -> &SharedString {
        &self.label
    }

    pub(crate) const fn role(&self) -> ActionRole {
        self.role
    }

    pub(crate) fn shortcut_value(&self) -> Option<&SharedString> {
        self.shortcut.as_ref()
    }

    pub(crate) const fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub(crate) const fn is_visible(&self) -> bool {
        self.visible
    }

    /// Returns whether this action owns focus in the captured frame.
    pub(crate) const fn is_focused(&self) -> bool {
        self.focused
    }

    /// Returns whether CE marked this node selected.
    pub(crate) const fn is_selected(&self) -> bool {
        self.selected
    }

    /// Returns whether this node is expanded.
    pub(crate) const fn is_expanded(&self) -> bool {
        self.expanded
    }

    /// Returns whether this node is loading.
    pub(crate) const fn is_loading(&self) -> bool {
        self.loading
    }

    /// Returns the semantic error, if one is currently exposed.
    pub(crate) fn error_value(&self) -> Option<&SharedString> {
        self.error.as_ref()
    }

    /// Returns the value exposed by the rendered control.
    pub(crate) fn value(&self) -> Option<&SharedString> {
        self.value.as_ref()
    }

    /// Returns the capture rectangle contract.
    pub(crate) const fn bounds(&self) -> SemanticBounds {
        self.bounds
    }

    pub(crate) const fn focus_order(&self) -> usize {
        self.focus_order
    }
}

/// A deterministic semantic action tree for journeys, accessibility, and
/// focus assertions. It deliberately retains disabled actions so a user can
/// understand why a capability is unavailable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ActionTree {
    actions: Vec<ActionMetadata>,
    route: SharedString,
    revision: u64,
}

impl Default for ActionTree {
    fn default() -> Self {
        Self {
            actions: Vec::new(),
            route: SharedString::new_static("unbound"),
            revision: 0,
        }
    }
}

impl ActionTree {
    fn register(&mut self, action: ActionMetadata) -> bool {
        if action.id.is_empty() || action.label.is_empty() {
            return false;
        }
        if self.actions.iter().any(|current| current.id == action.id) {
            return false;
        }
        let mut action = action;
        action.focus_order = self.actions.len();
        self.actions.push(action);
        true
    }

    /// Starts a registration scope for rendered CE components.
    ///
    /// Routes should build their semantic tree through this scope while they
    /// instantiate the matching component. This keeps focus metadata next to
    /// the real control and makes a stale, hand-maintained action list
    /// impossible to mistake for the rendered UI.
    pub(crate) fn registrar(&mut self) -> ActionRegistrar<'_> {
        ActionRegistrar { tree: self }
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &ActionMetadata> {
        self.actions.iter()
    }

    pub(crate) fn focusable(&self) -> impl Iterator<Item = &ActionMetadata> {
        self.actions
            .iter()
            .filter(|action| action.enabled && action.visible)
    }

    pub(crate) fn len(&self) -> usize {
        self.actions.len()
    }
}

/// Owns semantic snapshots for one application/window owner.
///
/// A workspace keeps one instance and threads it through its `Theme`. Adapter
/// calls therefore carry an explicit window frame even when a CE popup renders
/// after the root element has been built. There is no process-global lock or
/// thread-local "current window" that can misattribute a second window.
#[derive(Clone, Debug, Default)]
pub(crate) struct ActionFrames {
    frames: Rc<RefCell<HashMap<u64, ActionTree>>>,
}

/// A registration capability for one rendered window frame. The revision is
/// checked on every write so a retained CE overlay from an older render cannot
/// leak actions into the next frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ActionFrameToken {
    window_id: u64,
    revision: u64,
}

impl ActionFrames {
    pub(crate) fn begin(
        &self,
        window: &Window,
        route: impl Into<SharedString>,
    ) -> ActionFrameToken {
        self.begin_id(window.window_handle().window_id().as_u64(), route)
    }

    fn begin_id(&self, window_id: u64, route: impl Into<SharedString>) -> ActionFrameToken {
        let mut frames = self.frames.borrow_mut();
        let revision = frames
            .get(&window_id)
            .map_or(1, |frame| frame.revision.saturating_add(1));
        frames.insert(
            window_id,
            ActionTree {
                actions: Vec::new(),
                route: route.into(),
                revision,
            },
        );
        ActionFrameToken {
            window_id,
            revision,
        }
    }

    pub(crate) fn register(&self, token: ActionFrameToken, action: ActionMetadata) {
        if let Some(frame) = self
            .frames
            .borrow_mut()
            .get_mut(&token.window_id)
            .filter(|frame| frame.revision == token.revision)
        {
            frame.registrar().record(action);
        }
    }

    pub(crate) fn snapshot(&self, window: &Window) -> ActionTree {
        self.frames
            .borrow()
            .get(&window.window_handle().window_id().as_u64())
            .cloned()
            .unwrap_or_default()
    }

    pub(crate) fn reset(&self, window: &Window) {
        self.frames
            .borrow_mut()
            .remove(&window.window_handle().window_id().as_u64());
    }

    fn snapshot_id(&self, window_id: u64) -> ActionTree {
        self.frames
            .borrow()
            .get(&window_id)
            .cloned()
            .unwrap_or_default()
    }
}

impl ActionTree {
    /// Returns the route identifier associated with this published frame.
    pub(crate) fn route(&self) -> &SharedString {
        &self.route
    }

    /// Returns the monotonically increasing frame revision.
    pub(crate) const fn revision(&self) -> u64 {
        self.revision
    }

    /// Returns the per-window render generation represented by this tree.
    pub(crate) const fn generation(&self) -> u64 {
        self.revision
    }
}

/// Registration boundary used while rendering component-backed controls.
///
/// The registrar deliberately returns the concrete GPUI CE element from every
/// builder. A route therefore records an action only in the same expression
/// that creates the focusable component; it does not maintain a second list
/// of labels or shortcuts.
pub(crate) struct ActionRegistrar<'a> {
    tree: &'a mut ActionTree,
}

impl ActionRegistrar<'_> {
    fn record(&mut self, action: ActionMetadata) -> bool {
        self.tree.register(action)
    }
}

fn paints(theme: &Theme, weight: Weight) -> (gpui::Hsla, gpui::Hsla, gpui::Hsla) {
    match weight {
        Weight::Primary => (
            theme.paint(Paint::Gilt),
            theme.paint(Paint::GiltWash),
            theme.paint(Paint::GiltDim),
        ),
        Weight::Regular => (
            theme.paint(Paint::Text),
            theme.paint(Paint::Panel),
            theme.paint(Paint::Hairline),
        ),
        Weight::Quiet => (
            theme.paint(Paint::TextDim),
            gpui::transparent_black(),
            gpui::transparent_black(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ActionMetadata, ActionRole, ActionTree, SemanticBounds, Weight,
    };

    #[test]
    fn action_tree_rejects_duplicate_or_empty_contracts_and_keeps_disabled_actions() {
        let mut tree = ActionTree::default();
        let mut registration = tree.registrar();
        assert!(registration.record(
            ActionMetadata::new("search", "Search packages", ActionRole::Search).shortcut("⌘K")
        ));
        assert!(!registration.record(ActionMetadata::new("search", "Again", ActionRole::Search,)));
        assert!(!registration.record(ActionMetadata::new("", "Missing id", ActionRole::Button,)));
        assert!(registration.record(
            ActionMetadata::new("settings", "Settings", ActionRole::Setting).enabled(false)
        ));
        drop(registration);
        assert_eq!(tree.len(), 2);
        assert_eq!(tree.focusable().count(), 1);
        assert_eq!(tree.iter().count(), 2);
    }

    #[test]
    fn action_roles_are_explicit_and_button_variants_remain_total() {
        assert_eq!(ActionRole::Button, ActionRole::Button);
        assert_ne!(ActionRole::Input, ActionRole::Search);
        assert_ne!(Weight::Primary, Weight::Quiet);
        assert_ne!(Weight::Primary, Weight::Regular);
        assert_ne!(Weight::Regular, Weight::Quiet);
    }

    #[test]
    fn frames_track_final_state_and_remove_stale_controls() {
        let frames = super::ActionFrames::default();
        let home = frames.begin_id(11, "home");
        frames.register(
            home,
            super::ActionMetadata::new("submit", "Submit", ActionRole::Button)
                .enabled(false)
                .visible(true),
        );
        let first = frames.snapshot_id(11);
        let action = first.iter().next().expect("disabled action is retained");
        assert!(!action.is_enabled());
        assert!(action.is_visible());
        assert_eq!(action.focus_order(), 0);

        frames.begin_id(11, "settings");
        let second = frames.snapshot_id(11);
        assert_eq!(second.route().as_ref(), "settings");
        assert_eq!(second.len(), 0);
        assert!(second.revision() > first.revision());
    }

    #[test]
    fn frames_are_separate_per_window_and_keep_input_search_roles() {
        let frames = super::ActionFrames::default();
        let search = frames.begin_id(21, "search");
        frames.register(
            search,
            super::ActionMetadata::new("query", "Query", ActionRole::Input)
                .focused(true)
                .with_value("serde"),
        );
        frames.register(
            search,
            super::ActionMetadata::new("package-search", "Search packages", ActionRole::Search),
        );
        let other_window = frames.begin_id(22, "other-window");
        frames.register(
            other_window,
            super::ActionMetadata::new("settings", "Settings", ActionRole::Setting),
        );
        let first = frames.snapshot_id(21);
        let second = frames.snapshot_id(22);
        assert_eq!(
            first.iter().map(|action| action.role()).collect::<Vec<_>>(),
            [ActionRole::Input, ActionRole::Search,]
        );
        assert_eq!(
            second.iter().next().map(|action| action.role()),
            Some(ActionRole::Setting)
        );
        assert_eq!(first.route().as_ref(), "search");
        assert_eq!(second.route().as_ref(), "other-window");
        let query = first.iter().next().expect("input action");
        assert!(query.is_focused());
        assert_eq!(query.value().map(|value| value.as_ref()), Some("serde"));
    }

    #[test]
    fn retained_frame_tokens_cannot_register_into_a_new_frame() {
        let frames = super::ActionFrames::default();
        let old = frames.begin_id(31, "home");
        let current = frames.begin_id(31, "settings");
        frames.register(
            old,
            super::ActionMetadata::new("stale", "Stale", ActionRole::Button),
        );
        assert_eq!(frames.snapshot_id(31).len(), 0);
        frames.register(
            current,
            super::ActionMetadata::new("current", "Current", ActionRole::Button),
        );
        assert_eq!(frames.snapshot_id(31).len(), 1);
    }

    #[test]
    fn overlay_registration_is_ordered_and_generation_scoped() {
        let frames = super::ActionFrames::default();
        let first = frames.begin_id(41, "package");
        frames.register(
            first,
            super::ActionMetadata::new("package-header", "Package", ActionRole::Navigation),
        );
        frames.register(
            first,
            super::ActionMetadata::new("source-overlay", "Source", ActionRole::Disclosure),
        );
        let visible = frames.snapshot_id(41);
        assert_eq!(
            visible
                .iter()
                .map(|action| action.id().as_ref())
                .collect::<Vec<_>>(),
            ["package-header", "source-overlay"]
        );

        let second = frames.begin_id(41, "package");
        frames.register(
            first,
            super::ActionMetadata::new("late-overlay", "Late", ActionRole::Disclosure),
        );
        frames.register(
            second,
            super::ActionMetadata::new("package-header", "Package", ActionRole::Navigation),
        );
        assert_eq!(
            frames
                .snapshot_id(41)
                .iter()
                .map(|action| action.id().as_ref())
                .collect::<Vec<_>>(),
            ["package-header"]
        );
    }

    #[test]
    fn semantic_nodes_retain_every_rendered_component_state() {
        let mut tree = ActionTree::default();
        let mut registration = tree.registrar();
        assert!(registration.record(
            ActionMetadata::new("package", "serde", ActionRole::TreeItem)
                .enabled(true)
                .visible(true)
                .focused(true)
                .selected(true)
                .expanded(true)
                .loading(true)
                .error("index unavailable")
                .with_value("serde 1.0.0")
                .with_bounds(SemanticBounds::Logical {
                    x: 8,
                    y: 16,
                    width: 320,
                    height: 36,
                }),
        ));
        let node = tree.iter().next().expect("semantic node");
        assert!(node.is_enabled());
        assert!(node.is_visible());
        assert!(node.is_focused());
        assert!(node.is_selected());
        assert!(node.is_expanded());
        assert!(node.is_loading());
        assert_eq!(
            node.error_value().map(|value| value.as_ref()),
            Some("index unavailable")
        );
        assert_eq!(node.value().map(|value| value.as_ref()), Some("serde 1.0.0"));
        assert_eq!(
            node.bounds(),
            SemanticBounds::Logical {
                x: 8,
                y: 16,
                width: 320,
                height: 36,
            }
        );
    }
}

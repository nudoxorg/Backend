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
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

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
    let (foreground, background, border) = paints(theme, weight);
    let id = id.into();
    let label = label.into();
    register_rendered(ActionMetadata::new(
        id.clone(),
        label.clone(),
        ActionRole::Button,
    ));
    let id = ElementId::Name(id);
    let button = Button::new(id)
        .label(label)
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
        .hover(|style| {
            style
                .bg(theme.paint(Paint::Hover))
                .border_color(theme.paint(Paint::Focus))
        })
        .focus_visible(|style| {
            style
                .border_color(theme.paint(Paint::Focus))
                .bg(theme.paint(Paint::GiltWash))
        });
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
    let id = id.into();
    let label = label.into();
    register_rendered(ActionMetadata::new(
        id.clone(),
        label.clone(),
        ActionRole::Button,
    ));
    Button::new(ElementId::Name(id))
        .accessibility_label(label)
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
        .hover(|style| {
            style
                .bg(theme.paint(Paint::Hover))
                .text_color(theme.paint(Paint::TextStrong))
        })
        .focus_visible(|style| {
            style
                .border_color(theme.paint(Paint::Focus))
                .bg(theme.paint(Paint::GiltWash))
        })
}

/// Applies an explicit disabled visual while keeping component semantics.
pub(crate) fn disabled(button: Button, disabled: bool) -> Button {
    button.disabled(disabled)
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
    label: impl Into<SharedString>,
) -> Input {
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
    label: impl Into<SharedString>,
) -> Input {
    input(theme, state, label).prefix(crate::ui::icon::sized(
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

/// Builds the complete CE settings shell from real product setting pages.
/// Settings owns page selection, filtering, keyboard focus, and reset flow.
pub(crate) fn settings(
    id: impl Into<ElementId>,
    pages: impl IntoIterator<Item = SettingPage>,
) -> Settings {
    Settings::new(id).pages(pages)
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
    focus_order: usize,
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

thread_local! {
    static ACTIVE_WINDOW: Cell<Option<u64>> = const { Cell::new(None) };
    static NEXT_REVISION: Cell<u64> = const { Cell::new(0) };
    static RENDERED_ACTIONS: RefCell<HashMap<u64, ActionTree>> = RefCell::new(HashMap::new());
}

/// Starts a fresh semantic snapshot for one window before its root renders.
/// The tree is render-local, so hidden routes and dismissed overlays cannot
/// remain in a later frame and windows cannot observe each other's controls.
pub(crate) fn begin_action_frame(window: &Window, route: impl Into<SharedString>) {
    let key = window.window_handle().window_id().as_u64();
    let route = route.into();
    let revision = NEXT_REVISION.with(|next| {
        let revision = next.get().saturating_add(1);
        next.set(revision);
        revision
    });
    ACTIVE_WINDOW.with(|active| active.set(Some(key)));
    RENDERED_ACTIONS.with(|frames| {
        frames.borrow_mut().insert(
            key,
            ActionTree {
                actions: Vec::new(),
                route,
                revision,
            },
        );
    });
}

/// Marks one root's current tree as complete and leaves it available to the
/// harness until the window's next frame begins.
pub(crate) fn publish_action_frame(window: &Window) {
    let key = window.window_handle().window_id().as_u64();
    ACTIVE_WINDOW.with(|active| {
        if active.get() == Some(key) {
            active.set(None);
        }
    });
}

fn register_rendered(action: ActionMetadata) {
    let key = ACTIVE_WINDOW.with(|active| active.get()).unwrap_or(0);
    RENDERED_ACTIONS.with(|frames| {
        let mut frames = frames.borrow_mut();
        frames.entry(key).or_default().registrar().record(action);
    });
}

/// Returns the semantic actions observed while real CE controls rendered.
///
/// Screenshot and journey harnesses use this snapshot to assert that every
/// focusable affordance has a stable name and role. The registry is updated by
/// the component adapters themselves, never by a manually maintained route
/// manifest.
pub(crate) fn rendered_action_tree(window: &Window) -> ActionTree {
    let key = window.window_handle().window_id().as_u64();
    RENDERED_ACTIONS.with(|frames| frames.borrow().get(&key).cloned().unwrap_or_default())
}

/// Clears one window's published tree between deterministic scenarios.
pub(crate) fn reset_rendered_action_tree(window: &Window) {
    let key = window.window_handle().window_id().as_u64();
    RENDERED_ACTIONS.with(|frames| {
        frames.borrow_mut().remove(&key);
    });
    ACTIVE_WINDOW.with(|active| {
        if active.get() == Some(key) {
            active.set(None);
        }
    });
}

#[cfg(test)]
fn test_rendered_action_tree() -> ActionTree {
    RENDERED_ACTIONS.with(|frames| frames.borrow().get(&0).cloned().unwrap_or_default())
}

#[cfg(test)]
fn reset_test_rendered_action_tree() {
    RENDERED_ACTIONS.with(|frames| {
        frames.borrow_mut().remove(&0);
    });
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

    fn register_button(
        &mut self,
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        enabled: bool,
    ) -> bool {
        self.tree
            .register(ActionMetadata::new(id, label, ActionRole::Button).enabled(enabled))
    }

    fn register_search(
        &mut self,
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        shortcut: impl Into<SharedString>,
    ) -> bool {
        self.tree
            .register(ActionMetadata::new(id, label, ActionRole::Search).shortcut(shortcut))
    }

    fn register_setting(
        &mut self,
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        enabled: bool,
    ) -> bool {
        self.tree
            .register(ActionMetadata::new(id, label, ActionRole::Setting).enabled(enabled))
    }

    pub(crate) fn button(
        &mut self,
        theme: &Theme,
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        weight: Weight,
    ) -> Button {
        let id = id.into();
        let label = label.into();
        self.register_button(id.clone(), label.clone(), true);
        crate::ui::components::button(theme, id, label, weight)
    }

    pub(crate) fn icon_button(
        &mut self,
        theme: &Theme,
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
    ) -> Button {
        let id = id.into();
        let label = label.into();
        self.register_button(id.clone(), label.clone(), true);
        crate::ui::components::icon_button(theme, id, label)
    }

    pub(crate) fn input(
        &mut self,
        theme: &Theme,
        state: &Entity<InputState>,
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
    ) -> Input {
        let id = id.into();
        let label = label.into();
        self.tree
            .register(ActionMetadata::new(id, label.clone(), ActionRole::Input));
        crate::ui::components::input(theme, state, label)
    }

    pub(crate) fn search_input(
        &mut self,
        theme: &Theme,
        state: &Entity<InputState>,
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
    ) -> Input {
        let id = id.into();
        let label = label.into();
        self.tree
            .register(ActionMetadata::new(id, label.clone(), ActionRole::Search));
        crate::ui::components::search_input(theme, state, label)
    }

    pub(crate) fn navigation(
        &mut self,
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
    ) -> bool {
        self.tree
            .register(ActionMetadata::new(id, label, ActionRole::Navigation))
    }

    pub(crate) fn disclosure(
        &mut self,
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
    ) -> bool {
        self.tree
            .register(ActionMetadata::new(id, label, ActionRole::Disclosure))
    }

    pub(crate) fn tree_item(
        &mut self,
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
    ) -> bool {
        self.tree
            .register(ActionMetadata::new(id, label, ActionRole::TreeItem))
    }

    pub(crate) fn setting(
        &mut self,
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
    ) -> bool {
        self.tree
            .register(ActionMetadata::new(id, label, ActionRole::Setting))
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
    use super::{ActionRole, ActionTree, Weight};

    #[test]
    fn action_tree_rejects_duplicate_or_empty_contracts_and_keeps_disabled_actions() {
        let mut tree = ActionTree::default();
        let mut registration = tree.registrar();
        assert!(registration.register_search("search", "Search packages", "⌘K"));
        assert!(!registration.register_search("search", "Again", "⌘K"));
        assert!(!registration.register_button("", "Missing id", true));
        assert!(registration.register_setting("settings", "Settings", false));
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
    fn rendered_component_registration_is_the_harness_source_of_truth() {
        super::reset_test_rendered_action_tree();
        let theme = crate::theme::Theme::default();
        let _button = super::button(&theme, "rendered-search", "Search", Weight::Regular);
        let tree = super::test_rendered_action_tree();
        let action = tree
            .iter()
            .find(|action| action.id().as_ref() == "rendered-search")
            .expect("CE button registration must be visible to the harness");
        assert_eq!(action.role(), ActionRole::Button);
        assert_eq!(action.label().as_ref(), "Search");
        super::reset_test_rendered_action_tree();
    }
}

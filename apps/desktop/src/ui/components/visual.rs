use super::{ActionMetadata, ActionRole, ActionState};
use crate::theme::Theme;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Space, TypeScale, hairline, type_size};
use gpui::{
    App, Context, ElementId, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement,
    SharedString, Styled, Window, px,
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
    let label = label.into();
    button_with_state_and_accessible(theme, id, label.clone(), label, weight, disabled, visible)
}

/// Builds a text button with a compact visual label and a complete spoken
/// name. Narrow chrome uses this to preserve discoverable controls after
/// shortening their on-screen labels for the available width.
pub(crate) fn button_with_state_and_accessible(
    theme: &Theme,
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    accessible_label: impl Into<SharedString>,
    weight: Weight,
    disabled: bool,
    visible: bool,
) -> Button {
    let id = id.into();
    let label = label.into();
    let accessible_label = accessible_label.into();
    let (foreground, background, border) = paints(theme, weight);
    let id_element = ElementId::Name(id.clone());
    let button = Button::new(id_element)
        .label(label.clone())
        .accessibility_label(accessible_label.clone())
        .accessibility_description(if disabled {
            "Unavailable in this view"
        } else {
            "Activates this command"
        })
        .compact()
        .tab_index(0)
        .tab_stop(theme.control_tab_stop())
        .focus_ring(true)
        .text_size(type_size(TypeScale::Small))
        .font_weight(FontWeight::MEDIUM)
        .font_family(theme.ui_face())
        .rounded(ButtonRounded::None)
        .px(theme.space(Space::Base))
        .py(px(4.0))
        .min_w(px(44.0))
        .min_h(px(44.0))
        .border(hairline())
        .border_color(border)
        .bg(background)
        .text_color(foreground)
        .focus_visible(|style| {
            style
                .border_color(theme.paint(Paint::Focus))
                .bg(theme.paint(Paint::MintSoft))
        })
        .disabled(disabled);
    theme.register_action(
        ActionMetadata::new(id.clone(), accessible_label, ActionRole::Button)
            .enabled(!disabled)
            .disabled_reason(disabled.then_some("Unavailable in this view"))
            .visible(visible),
    );
    let button = match weight {
        Weight::Primary => button.primary(),
        Weight::Regular => button.secondary(),
        Weight::Quiet => button.text(),
    };
    observe_button_focus(theme, id, button)
}

/// Binds a CE button's native focus handle to the action frame that registered
/// the same stable id. The callback is attached to the concrete element before
/// it is returned to the view, so a semantic export cannot invent ownership.
pub(crate) fn observe_button_focus(
    theme: &Theme,
    id: impl Into<SharedString>,
    button: Button,
) -> Button {
    let Some(token) = theme.action_frame_token() else {
        return button;
    };
    let frames = theme.action_frames_handle();
    let id = id.into();
    let focus_frames = frames.clone();
    let focus_id = id.clone();
    let hover_frames = frames.clone();
    let hover_id = id.clone();
    let press_frames = frames.clone();
    let press_id = id.clone();
    let bounds_frames = frames.clone();
    let bounds_id = id.clone();
    button
        .on_focus_observed(move |handle, focused, window, cx| {
            focus_frames.record_native_focus(token, focus_id.clone(), handle, focused, window, cx);
        })
        .on_hover_observed(move |hovered, _window, _cx| {
            hover_frames.record_pointer_state(
                token,
                hover_id.clone(),
                ActionState::Hovered,
                hovered,
            );
        })
        .on_press_observed(move |pressed, _window, _cx| {
            press_frames.record_pointer_state(
                token,
                press_id.clone(),
                ActionState::Pressed,
                pressed,
            );
        })
        .on_bounds_observed(move |bounds, _window, _cx| {
            bounds_frames.record_bounds(token, bounds_id.clone(), bounds);
        })
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
        .accessibility_description("Activates this command")
        .compact()
        .tab_index(0)
        .tab_stop(theme.control_tab_stop())
        .focus_ring(true)
        .font_family(theme.ui_face())
        .rounded(ButtonRounded::None)
        .w(px(24.0))
        .h(px(24.0))
        .min_w(px(44.0))
        .min_h(px(44.0))
        .border(hairline())
        .border_color(gpui::transparent_black())
        .bg(gpui::transparent_black())
        .text_color(theme.paint(Paint::Silver2))
        .focus_visible(|style| {
            style
                .border_color(theme.paint(Paint::Focus))
                .bg(theme.paint(Paint::MintSoft))
        })
        .disabled(disabled);
    theme.register_action(
        ActionMetadata::new(id.clone(), label, ActionRole::Button)
            .enabled(!disabled)
            .disabled_reason(disabled.then_some("Unavailable in this view"))
            .visible(visible),
    );
    observe_button_focus(theme, id, button)
}

/// Builds a content-bearing list row on the same CE button focus path as
/// ordinary commands. Product cards are still free to provide arbitrary
/// children, but their native focus handle is now observable and their 44px
/// minimum target is enforced by the shared adapter.
pub(crate) fn card_button(
    theme: &Theme,
    id: impl Into<SharedString>,
    accessible_label: impl Into<SharedString>,
    description: impl Into<SharedString>,
) -> Button {
    let id = id.into();
    let accessible_label = accessible_label.into();
    let description = description.into();
    let (foreground, background, border) = paints(theme, Weight::Regular);
    let button = Button::new(ElementId::Name(id.clone()))
        .role(gpui::Role::ListItem)
        .accessibility_label(accessible_label)
        .accessibility_description(description)
        .compact()
        .tab_index(0)
        .tab_stop(theme.control_tab_stop())
        .focus_ring(true)
        .w_full()
        .min_h(px(44.0))
        .items_start()
        .justify_start()
        .p(px(20.0))
        .border(hairline())
        .border_color(border)
        .bg(background)
        .text_color(foreground)
        .cursor_pointer()
        .focus_visible(|style| {
            style
                .border_color(theme.paint(Paint::Focus))
                .bg(theme.paint(Paint::MintSoft))
        });
    observe_button_focus(theme, id, button)
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
        ActionMetadata::new(id.clone(), label.clone(), ActionRole::TextInput)
            .focused(focused)
            .description("Editable text field")
            .with_value(value),
    );
    input_element(theme, state, id, label)
}

fn input_element(
    theme: &Theme,
    state: &Entity<InputState>,
    id: SharedString,
    label: SharedString,
) -> Input {
    let input = Input::new(state)
        .aria_label(label)
        .aria_description("Editable text field; use Tab to move focus")
        .tab_index(0)
        .tab_stop(theme.control_tab_stop())
        .focus_ring(true)
        .font_family(theme.ui_face())
        .text_size(type_size(TypeScale::Interface))
        .text_color(theme.paint(Paint::Silver1))
        .bg(theme.paint(Paint::Abyss1))
        .border_color(theme.paint(Paint::Rule1))
        .min_h(px(44.0));
    observe_input_focus(theme, id, input)
}

fn observe_input_focus(theme: &Theme, id: SharedString, input: Input) -> Input {
    let Some(token) = theme.action_frame_token() else {
        return input;
    };
    let frames = theme.action_frames_handle();
    let focus_frames = frames.clone();
    let focus_id = id.clone();
    let hover_frames = frames.clone();
    let hover_id = id.clone();
    let press_frames = frames.clone();
    let press_id = id.clone();
    input
        .on_focus_observed(move |handle, focused, window, cx| {
            focus_frames.record_native_focus(token, focus_id.clone(), handle, focused, window, cx);
        })
        .on_hover_observed(move |hovered, _window, _cx| {
            hover_frames.record_pointer_state(
                token,
                hover_id.clone(),
                ActionState::Hovered,
                hovered,
            );
        })
        .on_press_observed(move |pressed, _window, _cx| {
            press_frames.record_pointer_state(
                token,
                press_id.clone(),
                ActionState::Pressed,
                pressed,
            );
        })
}

/// Wraps one rendered control in a post-layout measurement boundary.
///
/// GPUI computes the final rectangle during prepaint. The wrapper observes its
/// direct child after layout and writes that measured rectangle into the same
/// per-window action frame that owns the semantic metadata. This keeps the
/// evidence tied to the actual rendered control, including responsive width,
/// large text, and modal repositioning.
pub(crate) fn measure<E>(theme: &Theme, id: impl Into<SharedString>, element: E) -> gpui::Div
where
    E: IntoElement,
{
    let id = id.into();
    let Some(token) = theme.action_frame_token() else {
        return gpui::div().child(element);
    };
    let frames = theme.action_frames_handle();
    gpui::div()
        .child(element)
        .on_children_prepainted(move |bounds, _window, _cx| {
            if let Some(bounds) = bounds.first().copied() {
                frames.record_bounds(token, id.clone(), bounds);
            }
        })
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
        ActionMetadata::new(id.clone(), label.clone(), ActionRole::Search)
            .focused(focused)
            .description("Searches the current documentation surface")
            .with_value(value),
    );
    input_element(theme, state, id, label).prefix(crate::ui::icon::sized(
        theme,
        crate::ui::icon::Icon::Search,
        14.0,
        Paint::Silver2,
    ))
}

/// Builds the real GPUI CE tooltip view with Nudox surface tokens.
pub(crate) fn tooltip(theme: &Theme, text: impl Into<SharedString>) -> Tooltip {
    Tooltip::new(text.into())
        .font_family(theme.ui_face())
        .text_size(type_size(TypeScale::Small))
        .bg(theme.paint(Paint::Abyss2))
        .text_color(theme.paint(Paint::Silver1))
        .border(hairline())
        .border_color(theme.paint(Paint::Rule3))
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
    theme.register_action(
        ActionMetadata::new(id.clone(), label, ActionRole::Disclosure)
            .description("Opens a popup surface")
            .expanded(false),
    );
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

fn paints(theme: &Theme, weight: Weight) -> (gpui::Hsla, gpui::Hsla, gpui::Hsla) {
    match weight {
        Weight::Primary => (
            theme.paint(Paint::Mint),
            theme.paint(Paint::MintSoft),
            theme.paint(Paint::Leaf),
        ),
        Weight::Regular => (
            theme.paint(Paint::Silver1),
            theme.paint(Paint::Abyss1),
            theme.paint(Paint::Rule1),
        ),
        Weight::Quiet => (
            theme.paint(Paint::Silver2),
            gpui::transparent_black(),
            gpui::transparent_black(),
        ),
    }
}

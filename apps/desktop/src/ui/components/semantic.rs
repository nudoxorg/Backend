use gpui::SharedString;
use std::collections::BTreeSet;

/// Semantic role used by the in-process action tree. Native accessibility is
/// platform-dependent; this stable tree gives screenshot and journey harnesses
/// a deterministic contract for focus order, labels, and enabled actions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ActionRole {
    Window,
    Dialog,
    List,
    ListItem,
    Tab,
    Status,
    Alert,
    Button,
    TextInput,
    Search,
    Disclosure,
    Navigation,
    TreeItem,
    Setting,
}

impl ActionRole {
    pub(crate) const fn is_focusable(self) -> bool {
        matches!(
            self,
            Self::Button
                | Self::TextInput
                | Self::Search
                | Self::Disclosure
                | Self::TreeItem
                | Self::ListItem
                | Self::Tab
                | Self::Setting
        )
    }
}

/// A typed cross-node relationship in the accessibility tree.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ActionRelation {
    LabelledBy,
    DescribedBy,
    Controls,
    Owns,
    FlowTo,
}

/// A semantic state retained alongside the concrete CE state.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum ActionState {
    Focused,
    Hovered,
    Pressed,
    Selected,
    Expanded,
    Disabled,
    Loading,
    Invalid,
    Inert,
}

/// Metadata for one focusable action in a Nudox route.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ActionMetadata {
    pub(super) id: SharedString,
    pub(super) label: SharedString,
    pub(super) role: ActionRole,
    description: Option<SharedString>,
    shortcut: Option<SharedString>,
    enabled: bool,
    pub(super) visible: bool,
    focused: bool,
    selected: bool,
    expanded: bool,
    loading: bool,
    error: Option<SharedString>,
    value: Option<SharedString>,
    disabled_reason: Option<SharedString>,
    parent: Option<SharedString>,
    relations: Vec<(ActionRelation, SharedString)>,
    states: BTreeSet<ActionState>,
    hit_target: Option<SemanticBounds>,
    bounds: SemanticBounds,
    pub(super) focus_order: usize,
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
            description: None,
            shortcut: None,
            enabled: true,
            visible: true,
            focused: false,
            selected: false,
            expanded: false,
            loading: false,
            error: None,
            value: None,
            disabled_reason: None,
            parent: None,
            relations: Vec::new(),
            states: BTreeSet::new(),
            // A target is unknown until a concrete GPUI element reports its
            // post-layout bounds. Keeping this deferred prevents a synthetic
            // 44x44 rectangle at the origin from masquerading as hit testing.
            hit_target: None,
            bounds: SemanticBounds::Deferred,
            focus_order: 0,
        }
    }

    /// Adds the keyboard spelling shown by tooltips and the action inspector.
    pub(crate) fn shortcut(mut self, shortcut: impl Into<SharedString>) -> Self {
        self.shortcut = Some(shortcut.into());
        self
    }

    /// Adds supplementary text for assistive technology.
    pub(crate) fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Marks an action unavailable without removing it from traversal metadata.
    pub(crate) fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        if !enabled {
            self.states.insert(ActionState::Disabled);
            if self.disabled_reason.is_none() {
                self.disabled_reason = Some(SharedString::new_static("Unavailable in this view"));
            }
        } else {
            self.states.remove(&ActionState::Disabled);
        }
        self
    }

    /// Records why a discoverable action cannot currently run.
    pub(crate) fn disabled_reason(mut self, reason: Option<&'static str>) -> Self {
        self.disabled_reason = reason.map(SharedString::new_static);
        self
    }

    /// Marks an action absent from the current rendered tree.
    pub(crate) const fn visible(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }

    /// Records whether this node owns the concrete focus ring in the frame.
    pub(crate) fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        if focused {
            self.states.insert(ActionState::Focused);
        } else {
            self.states.remove(&ActionState::Focused);
        }
        self
    }

    /// Records whether the pointer is currently over the concrete control.
    pub(crate) fn hovered(mut self, hovered: bool) -> Self {
        if hovered {
            self.states.insert(ActionState::Hovered);
        } else {
            self.states.remove(&ActionState::Hovered);
        }
        self
    }

    /// Records the momentary pointer press state of the concrete control.
    pub(crate) fn pressed(mut self, pressed: bool) -> Self {
        if pressed {
            self.states.insert(ActionState::Pressed);
        } else {
            self.states.remove(&ActionState::Pressed);
        }
        self
    }

    /// Records the CE selection state for list/tree items and toggles.
    pub(crate) fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        if selected {
            self.states.insert(ActionState::Selected);
        } else {
            self.states.remove(&ActionState::Selected);
        }
        self
    }

    /// Records whether this disclosure or tree node is expanded.
    pub(crate) fn expanded(mut self, expanded: bool) -> Self {
        self.expanded = expanded;
        if expanded {
            self.states.insert(ActionState::Expanded);
        } else {
            self.states.remove(&ActionState::Expanded);
        }
        self
    }

    /// Records CE's loading state while retaining a stable semantic node.
    pub(crate) fn loading(mut self, loading: bool) -> Self {
        self.loading = loading;
        if loading {
            self.states.insert(ActionState::Loading);
        } else {
            self.states.remove(&ActionState::Loading);
        }
        self
    }

    /// Records a typed error state without replacing the action's label.
    pub(crate) fn error(mut self, error: impl Into<SharedString>) -> Self {
        self.error = Some(error.into());
        self.states.insert(ActionState::Invalid);
        self
    }

    /// Records the value exposed by an input, combobox, or setting.
    pub(crate) fn with_value(mut self, value: impl Into<SharedString>) -> Self {
        self.value = Some(value.into());
        self
    }

    /// Links this node to a semantic parent.
    pub(crate) fn parent(mut self, parent: impl Into<SharedString>) -> Self {
        self.parent = Some(parent.into());
        self
    }

    /// Adds one explicit relationship to another semantic node.
    pub(crate) fn relation(
        mut self,
        kind: ActionRelation,
        target: impl Into<SharedString>,
    ) -> Self {
        self.relations.push((kind, target.into()));
        self
    }

    /// Marks the node inert while a modal owns keyboard focus.
    pub(crate) fn inert(mut self, inert: bool) -> Self {
        if inert {
            self.states.insert(ActionState::Inert);
        } else {
            self.states.remove(&ActionState::Inert);
        }
        self
    }

    /// Records the pointer/touch target rectangle.
    pub(crate) const fn hit_target(mut self, bounds: SemanticBounds) -> Self {
        self.hit_target = Some(bounds);
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

    pub(crate) fn description_value(&self) -> Option<&SharedString> {
        self.description.as_ref()
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

    pub(crate) fn is_hovered(&self) -> bool {
        self.states.contains(&ActionState::Hovered)
    }

    pub(crate) fn is_pressed(&self) -> bool {
        self.states.contains(&ActionState::Pressed)
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

    pub(crate) fn disabled_reason_value(&self) -> Option<&SharedString> {
        self.disabled_reason.as_ref()
    }

    pub(crate) fn parent_value(&self) -> Option<&SharedString> {
        self.parent.as_ref()
    }

    pub(crate) fn relations(&self) -> &[(ActionRelation, SharedString)] {
        &self.relations
    }

    pub(crate) fn states(&self) -> &BTreeSet<ActionState> {
        &self.states
    }

    pub(crate) const fn hit_target_bounds(&self) -> Option<SemanticBounds> {
        self.hit_target
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

    /// Whether this action participates in the current keyboard traversal.
    pub(crate) fn is_focusable(&self) -> bool {
        self.enabled
            && self.visible
            && !self.states.contains(&ActionState::Inert)
            && self.role.is_focusable()
    }
}

//! Deterministic accessibility evidence for rendered GPUI frames.
//!
//! GPUI/AccessKit owns the native tree. The harness keeps a small, stable
//! serialisation beside every screenshot so a capture can be reviewed and
//! verified on a machine that does not have the native accessibility client
//! attached. The source field is explicit: these probes are app metadata plus
//! GPUI post-layout measurements until a native AccessKit readback is attached.

use image::{Rgba, RgbaImage};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

/// Semantic evidence schema version.
pub const SEMANTIC_SCHEMA: u32 = 1;

/// Roles that may appear in a rendered accessibility snapshot.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SemanticRole {
    /// The application window or route shell.
    Window,
    /// A navigation landmark.
    Navigation,
    /// A push button or command.
    Button,
    /// A disclosure/control that exposes another surface.
    Disclosure,
    /// A single line or multiline text field.
    TextInput,
    /// A search field.
    Search,
    /// A route tab.
    Tab,
    /// A list or collection container.
    List,
    /// A list row or tree item.
    ListItem,
    /// A modal dialog.
    Dialog,
    /// A status region announced as it changes.
    Status,
    /// An error or warning announcement.
    Alert,
    /// A non-interactive heading retained for screen-reader hierarchy.
    Heading,
}

impl SemanticRole {
    /// Whether this role participates in keyboard focus traversal.
    #[must_use]
    pub const fn is_focusable(self) -> bool {
        matches!(
            self,
            Self::Button
                | Self::Disclosure
                | Self::TextInput
                | Self::Search
                | Self::Tab
                | Self::ListItem
        )
    }
}

/// State exposed to assistive technology and capture review.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SemanticState {
    /// The node owns keyboard focus.
    Focused,
    /// The pointer is currently over the node.
    Hovered,
    /// The pointer is currently pressing the node.
    Pressed,
    /// The node is selected.
    Selected,
    /// The node exposes children or a popup.
    Expanded,
    /// The node is disabled and cannot be activated.
    Disabled,
    /// Work is currently pending.
    Busy,
    /// The node has invalid input or an error.
    Invalid,
    /// The node is inert while an overlay owns focus.
    Inert,
}

/// Evidence source for a semantic probe.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SemanticSource {
    /// App metadata joined to rectangles observed during GPUI prepaint.
    GpuiPostLayout,
    /// A native AccessKit tree read back after the platform committed a frame.
    NativeAccessKit,
}

/// A logical rectangle in the screenshot viewport.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SemanticBounds {
    /// Horizontal origin in logical pixels.
    pub x: u32,
    /// Vertical origin in logical pixels.
    pub y: u32,
    /// Width in logical pixels.
    pub width: u32,
    /// Height in logical pixels.
    pub height: u32,
}

impl SemanticBounds {
    /// Returns whether the rectangle is non-empty.
    #[must_use]
    pub const fn is_non_empty(self) -> bool {
        self.width > 0 && self.height > 0
    }

    /// Returns a crop rectangle with symmetric padding, clamped to the image.
    #[must_use]
    pub fn padded(self, padding: u32, image: &RgbaImage) -> Self {
        let x = self.x.saturating_sub(padding).min(image.width());
        let y = self.y.saturating_sub(padding).min(image.height());
        let right = self
            .x
            .saturating_add(self.width)
            .saturating_add(padding)
            .min(image.width());
        let bottom = self
            .y
            .saturating_add(self.height)
            .saturating_add(padding)
            .min(image.height());
        Self {
            x,
            y,
            width: right.saturating_sub(x),
            height: bottom.saturating_sub(y),
        }
    }
}

/// Relationships from one semantic node to other nodes.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SemanticRelations {
    /// Nodes whose text labels this node.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labelled_by: Vec<String>,
    /// Nodes that provide supplementary instructions or error text.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub described_by: Vec<String>,
    /// A popup, panel, or content region controlled by this node.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub controls: Vec<String>,
    /// Nodes owned by this composite node.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owns: Vec<String>,
    /// The next logical reading/focus destination.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flow_to: Vec<String>,
}

/// One announcement emitted into a screen-reader live region.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SemanticAnnouncement {
    /// Stable announcement channel.
    pub id: String,
    /// Spoken text.
    pub text: String,
    /// Whether this announcement is an assertive error/warning.
    pub assertive: bool,
}

/// One semantic node paired with one rendered frame.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SemanticNode {
    /// Stable product identity, suitable for snapshots and journeys.
    pub id: String,
    /// Native accessibility role.
    pub role: SemanticRole,
    /// Accessible name.
    pub name: String,
    /// Supplementary accessible description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Current textual value, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// Current semantic states.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub states: BTreeSet<SemanticState>,
    /// Whether the node can currently be activated or edited.
    pub enabled: bool,
    /// Whether it is present in the rendered tree.
    pub visible: bool,
    /// Stable keyboard traversal order for focusable nodes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus_order: Option<u32>,
    /// Parent semantic node, if this is a child of a composite.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Cross-node relationships.
    #[serde(default)]
    pub relations: SemanticRelations,
    /// App-declared logical visual bounds, when the component provided them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_bounds: Option<SemanticBounds>,
    /// App-declared pointer/touch target bounds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_hit_target: Option<SemanticBounds>,
    /// Logical visual bounds measured from a concrete GPUI child after layout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measured_bounds: Option<SemanticBounds>,
    /// Actual pointer/touch target bounds measured from the concrete GPUI
    /// child. This can exceed the painted content bounds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measured_hit_target: Option<SemanticBounds>,
    /// Key spellings that activate or move within this node.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keyboard: Vec<String>,
    /// Human-readable reason an otherwise discoverable control is disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<String>,
}

impl SemanticNode {
    /// Creates an interactive node with a spoken name and 44px target.
    #[must_use]
    pub fn interactive(
        id: impl Into<String>,
        role: SemanticRole,
        name: impl Into<String>,
        focus_order: u32,
        bounds: SemanticBounds,
    ) -> Self {
        Self {
            id: id.into(),
            role,
            name: name.into(),
            description: None,
            value: None,
            states: BTreeSet::new(),
            enabled: true,
            visible: true,
            focus_order: Some(focus_order),
            parent: None,
            relations: SemanticRelations::default(),
            declared_bounds: Some(bounds),
            declared_hit_target: Some(SemanticBounds {
                width: bounds.width.max(44),
                height: bounds.height.max(44),
                ..bounds
            }),
            measured_bounds: None,
            measured_hit_target: None,
            keyboard: vec!["enter".to_owned(), "space".to_owned()],
            disabled_reason: None,
        }
    }

    /// Adds an explanatory description.
    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Adds a current value.
    #[must_use]
    pub fn value(mut self, value: impl Into<String>) -> Self {
        self.value = Some(value.into());
        self
    }

    /// Marks the node disabled and records why.
    #[must_use]
    pub fn disabled(mut self, reason: impl Into<String>) -> Self {
        self.enabled = false;
        self.states.insert(SemanticState::Disabled);
        self.disabled_reason = Some(reason.into());
        self
    }

    /// Marks the node as inert while a modal owns keyboard focus.
    #[must_use]
    pub fn inert(mut self) -> Self {
        self.states.insert(SemanticState::Inert);
        self
    }

    /// Attaches a rectangle observed after GPUI layout in a unit test or
    /// adapter that owns a concrete rendered element.
    #[must_use]
    pub fn measured(mut self, bounds: SemanticBounds) -> Self {
        self.measured_bounds = Some(bounds);
        self.measured_hit_target = Some(bounds);
        self
    }

    /// Returns whether the node is a keyboard tab stop in this frame.
    #[must_use]
    pub fn is_focusable(&self) -> bool {
        self.visible
            && self.enabled
            && !self.states.contains(&SemanticState::Inert)
            && self.role.is_focusable()
    }
}

/// Semantic tree and rendered evidence for one screenshot frame.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SemanticProbe {
    /// Schema version.
    pub schema: u32,
    /// Stable frame label.
    pub frame: String,
    /// Virtual capture time.
    pub time_ms: u64,
    /// Logical viewport dimensions.
    pub viewport: (u32, u32),
    /// Route/surface identity.
    pub route: String,
    /// Whether this probe is app metadata plus GPUI measurement or native
    /// AccessKit readback. The current desktop capture uses the former.
    pub source: SemanticSource,
    /// Nodes in deterministic DOM/paint order.
    pub nodes: Vec<SemanticNode>,
    /// Focus owner, if the window has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused: Option<String>,
    /// Focus owner observed from a concrete GPUI focus handle. This is native
    /// GPUI evidence, not an AccessKit tree readback; the distinction is
    /// preserved by [`Self::source`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_focus_owner: Option<String>,
    /// Modal root, when an overlay owns focus.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modal_root: Option<String>,
    /// Whether focus is constrained to `modal_root`.
    pub focus_trap: bool,
    /// Node restored when the modal closes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restore_focus: Option<String>,
    /// Live region announcements emitted for this frame.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub announcements: Vec<SemanticAnnouncement>,
    /// SHA-256 of the normalized screenshot pixels paired with this probe.
    pub screenshot_sha256: String,
}

impl SemanticProbe {
    /// Creates an empty probe for a frame.
    #[must_use]
    pub fn new(
        frame: impl Into<String>,
        time_ms: u64,
        viewport: (u32, u32),
        route: impl Into<String>,
        screenshot: &RgbaImage,
    ) -> Self {
        Self {
            schema: SEMANTIC_SCHEMA,
            frame: frame.into(),
            time_ms,
            viewport,
            route: route.into(),
            source: SemanticSource::GpuiPostLayout,
            nodes: Vec::new(),
            focused: None,
            native_focus_owner: None,
            modal_root: None,
            focus_trap: false,
            restore_focus: None,
            announcements: Vec::new(),
            screenshot_sha256: hash_png_pixels(screenshot),
        }
    }

    /// Validates the complete screen-reader and keyboard contract.
    pub fn validate(&self) -> Result<(), SemanticError> {
        if self.schema != SEMANTIC_SCHEMA {
            return Err(SemanticError::Schema(self.schema));
        }
        if self.frame.trim().is_empty() || self.route.trim().is_empty() {
            return Err(SemanticError::MissingFrameIdentity);
        }
        let mut by_id = BTreeMap::new();
        for node in &self.nodes {
            if node.id.trim().is_empty() || node.name.trim().is_empty() {
                return Err(SemanticError::MissingNodeName(node.id.clone()));
            }
            if by_id.insert(node.id.as_str(), node).is_some() {
                return Err(SemanticError::DuplicateNode(node.id.clone()));
            }
            if node.visible && node.role.is_focusable() {
                let bounds = node
                    .measured_bounds
                    .filter(|bounds| bounds.is_non_empty())
                    .ok_or_else(|| SemanticError::MissingBounds(node.id.clone()))?;
                let target = node
                    .measured_hit_target
                    .filter(|target| target.is_non_empty())
                    .ok_or_else(|| SemanticError::MissingHitTarget(node.id.clone()))?;
                if target.width < 44 || target.height < 44 {
                    return Err(SemanticError::SmallHitTarget {
                        id: node.id.clone(),
                        width: target.width,
                        height: target.height,
                    });
                }
                if bounds.width == 0 || bounds.height == 0 {
                    return Err(SemanticError::MissingBounds(node.id.clone()));
                }
                if bounds.x.saturating_add(bounds.width) > self.viewport.0
                    || bounds.y.saturating_add(bounds.height) > self.viewport.1
                {
                    return Err(SemanticError::BoundsOutsideViewport(node.id.clone()));
                }
                if target.x.saturating_add(target.width) > self.viewport.0
                    || target.y.saturating_add(target.height) > self.viewport.1
                {
                    return Err(SemanticError::HitTargetOutsideViewport(node.id.clone()));
                }
                if !node.enabled && node.disabled_reason.as_deref().map_or(true, str::is_empty) {
                    return Err(SemanticError::MissingDisabledReason(node.id.clone()));
                }
            }
            if node.enabled && node.states.contains(&SemanticState::Disabled) {
                return Err(SemanticError::StateMismatch(node.id.clone()));
            }
            if !node.enabled && !node.states.contains(&SemanticState::Disabled) {
                return Err(SemanticError::StateMismatch(node.id.clone()));
            }
        }

        let mut orders = self
            .nodes
            .iter()
            .filter(|node| node.is_focusable())
            .map(|node| {
                node.focus_order
                    .ok_or_else(|| SemanticError::MissingFocusOrder(node.id.clone()))
                    .map(|order| (order, node.id.clone()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        orders.sort_by_key(|(order, _)| *order);
        for (expected, (actual, _)) in orders.iter().enumerate() {
            if *actual != expected as u32 {
                return Err(SemanticError::NonContiguousFocusOrder {
                    expected: expected as u32,
                    actual: *actual,
                });
            }
        }

        // A post-layout observer must identify the concrete control it wrapped.
        // If two independent focus stops report one rectangle, the capture is
        // lying about hit testing (the old synthetic origin fallback made this
        // pass trivially). Reject the frame before it can become evidence.
        let focus_targets = self
            .nodes
            .iter()
            .filter(|node| node.is_focusable())
            .filter_map(|node| node.measured_hit_target.map(|bounds| (&node.id, bounds)))
            .collect::<Vec<_>>();
        for (index, (left_id, left)) in focus_targets.iter().enumerate() {
            for (right_id, right) in focus_targets.iter().skip(index + 1) {
                if rectangles_overlap(*left, *right) {
                    return Err(SemanticError::OverlappingFocusTargets {
                        left: (*left_id).clone(),
                        right: (*right_id).clone(),
                    });
                }
            }
        }

        let focused_nodes = self
            .nodes
            .iter()
            .filter(|node| node.states.contains(&SemanticState::Focused))
            .collect::<Vec<_>>();
        if focused_nodes.len() > 1 {
            return Err(SemanticError::MultipleFocusOwners(
                focused_nodes.iter().map(|node| node.id.clone()).collect(),
            ));
        }
        if let Some(focused) = self.focused.as_deref() {
            let node = by_id
                .get(focused)
                .ok_or_else(|| SemanticError::UnknownRelation(focused.to_owned()))?;
            if !node.is_focusable() {
                return Err(SemanticError::FocusedNodeUnavailable(focused.to_owned()));
            }
            if !node.states.contains(&SemanticState::Focused) {
                return Err(SemanticError::FocusStateMismatch(focused.to_owned()));
            }
        } else if !focused_nodes.is_empty() {
            return Err(SemanticError::FocusStateMismatch(
                focused_nodes[0].id.clone(),
            ));
        }

        if self.source == SemanticSource::GpuiPostLayout
            && self.focused.is_some()
            && self.native_focus_owner.is_none()
        {
            return Err(SemanticError::MissingNativeFocusEvidence);
        }

        if let Some(native_focus) = self.native_focus_owner.as_deref() {
            let node = by_id
                .get(native_focus)
                .ok_or_else(|| SemanticError::UnknownNativeFocusOwner(native_focus.to_owned()))?;
            if !node.is_focusable() {
                return Err(SemanticError::NativeFocusOwnerUnavailable(
                    native_focus.to_owned(),
                ));
            }
            if self.focused.as_deref() != Some(native_focus) {
                return Err(SemanticError::NativeFocusMismatch {
                    declared: self.focused.clone(),
                    native: native_focus.to_owned(),
                });
            }
        }

        if let Some(modal) = self.modal_root.as_deref() {
            let modal_node = by_id
                .get(modal)
                .ok_or_else(|| SemanticError::UnknownRelation(modal.to_owned()))?;
            if modal_node.role != SemanticRole::Dialog {
                return Err(SemanticError::ModalRootNotDialog(modal.to_owned()));
            }
            if self.focus_trap {
                let focusable = self.nodes.iter().filter(|node| node.is_focusable());
                for node in focusable {
                    if !is_descendant(node, modal, &by_id) {
                        return Err(SemanticError::FocusEscapesModal(node.id.clone()));
                    }
                }
                let restore = self
                    .restore_focus
                    .as_deref()
                    .ok_or(SemanticError::MissingRestoreFocus)?;
                if !by_id.contains_key(restore)
                    || is_descendant(by_id[restore], modal, &by_id)
                    || restore == modal
                {
                    return Err(SemanticError::UnknownRelation(restore.to_owned()));
                }
            }
        } else if self.focus_trap {
            return Err(SemanticError::TrapWithoutModal);
        }

        for node in &self.nodes {
            for target in relation_targets(node) {
                if !by_id.contains_key(target.as_str()) {
                    return Err(SemanticError::UnknownRelation(format!(
                        "{} -> {target}",
                        node.id
                    )));
                }
            }
        }
        for announcement in &self.announcements {
            if announcement.id.trim().is_empty() || announcement.text.trim().is_empty() {
                return Err(SemanticError::EmptyAnnouncement);
            }
        }
        Ok(())
    }

    /// Returns the nodes that can be reached by Tab in order.
    #[must_use]
    pub fn tab_order(&self) -> Vec<&SemanticNode> {
        let mut nodes = self
            .nodes
            .iter()
            .filter(|node| node.is_focusable())
            .collect::<Vec<_>>();
        nodes.sort_by_key(|node| node.focus_order.unwrap_or(u32::MAX));
        nodes
    }
}

fn relation_targets(node: &SemanticNode) -> impl Iterator<Item = String> {
    node.parent
        .iter()
        .chain(node.relations.labelled_by.iter())
        .chain(node.relations.described_by.iter())
        .chain(node.relations.controls.iter())
        .chain(node.relations.owns.iter())
        .chain(node.relations.flow_to.iter())
        .cloned()
}

fn is_descendant(
    node: &SemanticNode,
    ancestor: &str,
    by_id: &BTreeMap<&str, &SemanticNode>,
) -> bool {
    let mut current = node.parent.as_deref();
    while let Some(id) = current {
        if id == ancestor {
            return true;
        }
        current = by_id.get(id).and_then(|parent| parent.parent.as_deref());
    }
    node.id == ancestor
}

fn rectangles_overlap(left: SemanticBounds, right: SemanticBounds) -> bool {
    let left_right = u64::from(left.x) + u64::from(left.width);
    let right_right = u64::from(right.x) + u64::from(right.width);
    let left_bottom = u64::from(left.y) + u64::from(left.height);
    let right_bottom = u64::from(right.y) + u64::from(right.height);
    u64::from(left.x) < right_right
        && u64::from(right.x) < left_right
        && u64::from(left.y) < right_bottom
        && u64::from(right.y) < left_bottom
}

/// Semantic evidence validation failures.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum SemanticError {
    /// The artifact uses an unsupported schema.
    #[error("unsupported semantic schema {0}")]
    Schema(u32),
    /// Frame and route labels must be present.
    #[error("semantic probe has no frame or route identity")]
    MissingFrameIdentity,
    /// A node has no stable id or spoken name.
    #[error("semantic node {0:?} has no id or accessible name")]
    MissingNodeName(String),
    /// IDs are unique within a frame.
    #[error("semantic node {0:?} is duplicated")]
    DuplicateNode(String),
    /// Focusable visible nodes have measured bounds.
    #[error("focusable node {0:?} has no non-empty bounds")]
    MissingBounds(String),
    /// Focusable visible nodes have an explicit hit target.
    #[error("focusable node {0:?} has no hit target")]
    MissingHitTarget(String),
    /// Measured bounds remain inside the logical capture viewport.
    #[error("measured bounds for node {0:?} escape the viewport")]
    BoundsOutsideViewport(String),
    /// Measured hit targets remain inside the logical capture viewport.
    #[error("measured hit target for node {0:?} escapes the viewport")]
    HitTargetOutsideViewport(String),
    /// Pointer/touch targets meet the minimum size.
    #[error("hit target {id:?} is {width}x{height}; minimum is 44x44")]
    SmallHitTarget { id: String, width: u32, height: u32 },
    /// Disabled controls explain why they cannot be activated.
    #[error("disabled node {0:?} has no disabled reason")]
    MissingDisabledReason(String),
    /// Enabled and disabled state metadata agree.
    #[error("enabled/disabled state mismatch for node {0:?}")]
    StateMismatch(String),
    /// Every keyboard stop has an explicit traversal ordinal.
    #[error("focusable node {0:?} has no focus order")]
    MissingFocusOrder(String),
    /// Focus order is contiguous and deterministic.
    #[error("focus order expected {expected}, found {actual}")]
    NonContiguousFocusOrder { expected: u32, actual: u32 },
    /// A focused node must be present.
    #[error("focused node {0:?} is unavailable")]
    FocusedNodeUnavailable(String),
    /// Focus state agrees with the focused id.
    #[error("focused node {0:?} is missing the focused state")]
    FocusStateMismatch(String),
    /// A frame cannot expose two simultaneous keyboard focus owners.
    #[error("semantic frame has multiple focus owners: {0:?}")]
    MultipleFocusOwners(Vec<String>),
    /// Native GPUI focus evidence must identify a known rendered control.
    #[error("native focus owner {0:?} is unavailable")]
    UnknownNativeFocusOwner(String),
    /// A GPUI post-layout probe with a focus owner must include native handle
    /// evidence instead of repeating app-declared metadata.
    #[error("focused GPUI post-layout probe has no native focus evidence")]
    MissingNativeFocusEvidence,
    /// Native GPUI focus evidence must identify a focusable rendered control.
    #[error("native focus owner {0:?} is not focusable")]
    NativeFocusOwnerUnavailable(String),
    /// Declared and native GPUI focus owners must agree.
    #[error("declared focus owner {declared:?} differs from native owner {native:?}")]
    NativeFocusMismatch {
        declared: Option<String>,
        native: String,
    },
    /// Independent focus stops may not claim the same rendered hit region.
    #[error("focus targets {left:?} and {right:?} overlap")]
    OverlappingFocusTargets { left: String, right: String },
    /// Modal roots are dialogs.
    #[error("modal root {0:?} does not have dialog role")]
    ModalRootNotDialog(String),
    /// Focus traps require a restorable owner.
    #[error("modal focus trap has no restore focus target")]
    MissingRestoreFocus,
    /// A focusable node is outside a declared modal trap.
    #[error("focusable node {0:?} escapes the active modal")]
    FocusEscapesModal(String),
    /// A trap cannot exist without a modal root.
    #[error("focus trap is set without a modal root")]
    TrapWithoutModal,
    /// A relationship points to a missing node.
    #[error("semantic relationship points to unknown node {0:?}")]
    UnknownRelation(String),
    /// Live announcements have stable ids and spoken text.
    #[error("semantic announcement is empty")]
    EmptyAnnouncement,
}

/// Returns a deterministic SHA-256 over screenshot pixels.
#[must_use]
pub fn hash_png_pixels(image: &RgbaImage) -> String {
    let mut hasher = Sha256::new();
    hasher.update(image.width().to_le_bytes());
    hasher.update(image.height().to_le_bytes());
    hasher.update(image.as_raw());
    format!("{:x}", hasher.finalize())
}

/// Relative luminance of an sRGB colour.
#[must_use]
pub fn relative_luminance(rgb: [u8; 3]) -> f32 {
    fn linear(channel: u8) -> f32 {
        let value = f32::from(channel) / 255.0;
        if value <= 0.03928 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * linear(rgb[0]) + 0.7152 * linear(rgb[1]) + 0.0722 * linear(rgb[2])
}

/// WCAG contrast ratio for two opaque sRGB colours.
#[must_use]
pub fn contrast_ratio(foreground: [u8; 3], background: [u8; 3]) -> f32 {
    let first = relative_luminance(foreground);
    let second = relative_luminance(background);
    let (light, dark) = if first >= second {
        (first, second)
    } else {
        (second, first)
    };
    (light + 0.05) / (dark + 0.05)
}

/// Returns whether a colour pair meets WCAG AA for normal or large text.
#[must_use]
pub fn meets_wcag_aa(foreground: [u8; 3], background: [u8; 3], large_text: bool) -> bool {
    contrast_ratio(foreground, background) >= if large_text { 3.0 } else { 4.5 }
}

/// Crops a focus-ring inspection region from a screenshot.
pub fn crop_focus_ring(
    image: &RgbaImage,
    bounds: SemanticBounds,
    padding: u32,
) -> Result<RgbaImage, SemanticError> {
    let crop = bounds.padded(padding, image);
    if !crop.is_non_empty() {
        return Err(SemanticError::MissingBounds("focus-ring-crop".to_owned()));
    }
    Ok(image::imageops::crop_imm(image, crop.x, crop.y, crop.width, crop.height).to_image())
}

/// Counts pixels that differ from a reference colour in a crop.
#[must_use]
pub fn changed_pixels(crop: &RgbaImage, background: Rgba<u8>) -> usize {
    crop.pixels().filter(|pixel| **pixel != background).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image() -> RgbaImage {
        RgbaImage::from_pixel(120, 80, Rgba([0, 0, 0, 255]))
    }

    fn button(id: &str, order: u32) -> SemanticNode {
        SemanticNode::interactive(
            id,
            SemanticRole::Button,
            id,
            order,
            SemanticBounds {
                x: order * 48,
                y: 8,
                width: 40,
                height: 28,
            },
        )
        .measured(SemanticBounds {
            x: order * 48,
            y: 8,
            width: 44,
            height: 44,
        })
    }

    #[test]
    fn validates_complete_focus_order_and_modal_relationships() {
        let mut open = button("open", 0);
        open.states.insert(SemanticState::Inert);
        let mut dialog = SemanticNode {
            id: "dialog".to_owned(),
            role: SemanticRole::Dialog,
            name: "Settings".to_owned(),
            description: Some("Application settings".to_owned()),
            value: None,
            states: BTreeSet::new(),
            enabled: true,
            visible: true,
            focus_order: None,
            parent: None,
            relations: SemanticRelations::default(),
            declared_bounds: Some(SemanticBounds {
                x: 10,
                y: 10,
                width: 80,
                height: 60,
            }),
            declared_hit_target: None,
            measured_bounds: None,
            measured_hit_target: None,
            keyboard: vec![],
            disabled_reason: None,
        };
        let mut close = button("close", 0);
        close.parent = Some("dialog".to_owned());
        dialog.relations.owns.push("close".to_owned());
        let mut probe = SemanticProbe::new("settled", 80, (120, 80), "settings", &image());
        probe.nodes = vec![dialog, close, open];
        probe.focused = Some("close".to_owned());
        probe.native_focus_owner = Some("close".to_owned());
        probe.modal_root = Some("dialog".to_owned());
        probe.focus_trap = true;
        probe.restore_focus = Some("open".to_owned());
        probe.nodes[1].states.insert(SemanticState::Focused);
        assert!(probe.validate().is_ok());

        // Underlying action cannot remain a focusable stop in a modal trap.
        probe.nodes[2].states.remove(&SemanticState::Inert);
        probe.nodes[2].states.insert(SemanticState::Focused);
        assert!(matches!(
            probe.validate(),
            Err(SemanticError::FocusEscapesModal(_))
        ));
    }

    #[test]
    fn requires_disabled_reason_and_large_hit_targets() {
        let mut probe = SemanticProbe::new("start", 0, (120, 80), "home", &image());
        probe.nodes = vec![button("disabled", 0).disabled("Index is still loading")];
        assert!(probe.validate().is_ok());
        probe.nodes[0].disabled_reason = None;
        assert!(matches!(
            probe.validate(),
            Err(SemanticError::MissingDisabledReason(_))
        ));
        probe.nodes[0].disabled_reason = Some("loading".to_owned());
        probe.nodes[0].measured_hit_target = Some(SemanticBounds {
            width: 24,
            height: 24,
            ..SemanticBounds::default()
        });
        assert!(matches!(
            probe.validate(),
            Err(SemanticError::SmallHitTarget { .. })
        ));
    }

    #[test]
    fn measured_controls_are_distinct_and_focus_owner_is_rendered_owner() {
        let mut first = button("first", 0);
        first.states.insert(SemanticState::Focused);
        let second = button("second", 1);
        let mut probe = SemanticProbe::new("focus", 0, (120, 80), "home", &image());
        probe.nodes = vec![first, second];
        probe.focused = Some("first".to_owned());
        probe.native_focus_owner = Some("first".to_owned());
        assert!(probe.validate().is_ok());

        probe.nodes[1].measured_bounds = probe.nodes[0].measured_bounds;
        probe.nodes[1].measured_hit_target = probe.nodes[0].measured_hit_target;
        assert!(matches!(
            probe.validate(),
            Err(SemanticError::OverlappingFocusTargets { .. })
        ));
    }

    #[test]
    fn focused_gpui_probe_requires_native_focus_evidence() {
        let mut probe = SemanticProbe::new("focus", 0, (120, 80), "home", &image());
        let mut first = button("first", 0);
        first.states.insert(SemanticState::Focused);
        probe.nodes = vec![first, button("second", 1)];
        probe.focused = Some("first".to_owned());
        assert!(matches!(
            probe.validate(),
            Err(SemanticError::MissingNativeFocusEvidence)
        ));
    }

    #[test]
    fn contrast_and_focus_crop_are_deterministic() {
        assert!(meets_wcag_aa([255, 255, 255], [0, 0, 0], false));
        assert!((contrast_ratio([255, 255, 255], [0, 0, 0]) - 21.0).abs() < 0.01);
        let mut source = image();
        source.put_pixel(20, 20, Rgba([255, 255, 255, 255]));
        let crop = crop_focus_ring(
            &source,
            SemanticBounds {
                x: 20,
                y: 20,
                width: 1,
                height: 1,
            },
            2,
        )
        .expect("focus crop");
        assert_eq!(crop.dimensions(), (5, 5));
        assert!(changed_pixels(&crop, Rgba([0, 0, 0, 255])) > 0);
    }
}

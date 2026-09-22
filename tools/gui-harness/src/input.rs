//! Deterministic user input scripts used before a capture.

use gpui::{Modifiers, Pixels, Point, point, px};
use serde::{Deserialize, Serialize};
use std::ops::Range;
use std::time::Duration;
use thiserror::Error;

/// A user-level action accepted by the GPUI driver.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum InputStep {
    /// Dispatch one GPUI keystroke, such as `cmd-p`, `tab`, or `escape`.
    Key {
        /// GPUI keystroke spelling.
        value: String,
    },
    /// Type text through the keyboard input path.
    Text {
        /// Text to insert.
        value: String,
    },
    /// Move focus to the next focusable element.
    FocusNext,
    /// Move focus to the previous focusable element.
    FocusPrevious,
    /// Move the deterministic clock and let due tasks run.
    Wait {
        /// Milliseconds to advance.
        milliseconds: u64,
    },
    /// Move the pointer without depending on a physical display.
    PointerMove {
        /// Horizontal position in logical pixels.
        x: f32,
        /// Vertical position in logical pixels.
        y: f32,
        /// Button held during a drag, if any.
        #[serde(default)]
        pressed_button: Option<String>,
    },
    /// Press a mouse button at a logical coordinate.
    PointerDown {
        /// Horizontal position.
        x: f32,
        /// Vertical position.
        y: f32,
        /// `left`, `right`, `middle`, `back`, or `forward`.
        button: String,
    },
    /// Release a mouse button at a logical coordinate.
    PointerUp {
        /// Horizontal position.
        x: f32,
        /// Vertical position.
        y: f32,
        /// `left`, `right`, `middle`, `back`, or `forward`.
        button: String,
    },
    /// Press and release a mouse button at a logical coordinate.
    Click {
        /// Horizontal position.
        x: f32,
        /// Vertical position.
        y: f32,
        /// `left`, `right`, `middle`, `back`, or `forward`.
        button: String,
    },
    /// Scroll a wheel/trackpad by an exact pixel delta.
    Scroll {
        /// Horizontal pointer position.
        x: f32,
        /// Vertical pointer position.
        y: f32,
        /// Horizontal delta in pixels.
        delta_x: f32,
        /// Vertical delta in pixels.
        delta_y: f32,
    },
    /// Seed the deterministic clipboard with text.
    Clipboard {
        /// Clipboard text.
        value: String,
    },
    /// Route a paste shortcut through the keyboard path.
    Paste,
    /// Send marked/composed text through the input path.
    ImeText {
        /// Composed text.
        value: String,
    },
    /// Update the marked portion of the focused editor through the native IME
    /// composition path. Kept separate from commit/cancel so a journey can
    /// prove each phase independently.
    ImeCompose {
        /// Text currently being composed.
        value: String,
    },
    /// Commit text through the native IME commit path.
    ImeCommit {
        /// Text to commit.
        value: String,
    },
    /// Cancel the active marked composition without inserting text.
    ImeCancel,
    /// Change the active modifier set for subsequent pointer/keyboard checks.
    Modifiers {
        /// Shift modifier.
        shift: bool,
        /// Control modifier.
        control: bool,
        /// Alt/option modifier.
        alt: bool,
        /// Command/super modifier.
        command: bool,
    },
    /// Drag a pointer from one logical coordinate to another.
    Drag {
        /// Horizontal start position.
        from_x: f32,
        /// Vertical start position.
        from_y: f32,
        /// Horizontal end position.
        to_x: f32,
        /// Vertical end position.
        to_y: f32,
        /// Button held during the drag.
        button: String,
    },
    /// Send a trackpad pinch gesture through GPUI's gesture path.
    Pinch {
        /// Horizontal gesture center.
        x: f32,
        /// Vertical gesture center.
        y: f32,
        /// Zoom delta; positive values zoom in.
        delta: f32,
    },
    /// Resize the headless window at this point in the input timeline.
    Resize {
        /// New logical width.
        width: u32,
        /// New logical height.
        height: u32,
    },
    /// Change the headless device scale factor for subsequent layout.
    Scale {
        /// New device scale factor. GPUI CE currently supports integral factors in the harness.
        factor: u8,
    },
    /// Tell a product adapter to switch its rendered theme.
    Theme {
        /// Stable theme identifier owned by the product adapter.
        value: String,
    },
    /// Tell a product adapter to switch its rendered locale.
    Locale {
        /// BCP-47 locale identifier owned by the product adapter.
        value: String,
    },
    /// Change the product interface text scale through its real preference
    /// path. The adapter owns the font/layout mutation; the harness records
    /// the event in the same deterministic timeline as resize and theme.
    TextScale {
        /// Percentage of the default interface size, from 50 through 300.
        percent: u16,
    },
    /// Deliver a native window focus transition to the product adapter.
    ///
    /// GPUI's headless platform cannot activate another native application,
    /// so the driver records this as an explicit adapter event.
    WindowFocus {
        /// Whether the window is receiving platform focus.
        focused: bool,
    },
}

/// The operation that was actually delivered to the focused GPUI CE editor.
///
/// Keeping this separate from [`InputStep`] makes an input transcript useful
/// when an adapter rejects a native capability: a successful step records the
/// operation and its post-edit evidence, while a failed step is represented by
/// the typed [`InputError::Unsupported`] returned by the adapter.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ImeOperation {
    /// Direct text insertion through `EntityInputHandler::replace_text_in_range`.
    Text,
    /// Mark or update the active composition.
    Compose,
    /// Commit the active composition exactly once.
    Commit,
    /// Clear the active composition without inserting text.
    Cancel,
}

impl ImeOperation {
    /// Returns the operation represented by an input step, if it is an IME
    /// step.
    #[must_use]
    pub const fn from_step(step: &InputStep) -> Option<Self> {
        match step {
            InputStep::ImeText { .. } => Some(Self::Text),
            InputStep::ImeCompose { .. } => Some(Self::Compose),
            InputStep::ImeCommit { .. } => Some(Self::Commit),
            InputStep::ImeCancel => Some(Self::Cancel),
            _ => None,
        }
    }
}

/// Post-dispatch evidence returned by the live desktop adapter for one IME
/// step. The fields are read from the focused CE state after the operation;
/// they are never synthesized from the journey or semantic action tree.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ImeObservation {
    /// The operation delivered by the adapter.
    pub operation: ImeOperation,
    /// Whether the same GPUI focus handle remained focused for the operation.
    pub focused: bool,
    /// Full editor value before dispatch.
    pub text_before: String,
    /// Full editor value after dispatch.
    pub text_after: String,
    /// UTF-16 marked range before dispatch.
    pub marked_range_before: Option<Range<usize>>,
    /// UTF-16 marked range after dispatch.
    pub marked_range_after: Option<Range<usize>>,
    /// Marked text read back through `EntityInputHandler::text_for_range`.
    pub marked_text_before: Option<String>,
    /// Marked text read back through `EntityInputHandler::text_for_range`.
    pub marked_text_after: Option<String>,
    /// UTF-16 selection after dispatch.
    pub selected_range_after: Option<Range<usize>>,
    /// Number of text-commit calls made for this step. The live adapter sets
    /// this to one for `ImeText`/`ImeCommit` and zero for composition/cancel;
    /// CE may still close an internal undo transaction while cancelling.
    pub commit_count: u8,
}

impl ImeObservation {
    /// Checks that a successful observation proves the requested IME
    /// transition took place. This rejects silent no-ops before a screenshot
    /// can be marked green.
    pub fn validate_for(&self, step: &InputStep) -> Result<(), InputError> {
        let Some(operation) = ImeOperation::from_step(step) else {
            return Ok(());
        };
        if self.operation != operation {
            return Err(InputError::Ime(format!(
                "adapter reported {:?} for {:?}",
                self.operation, operation
            )));
        }
        if !self.focused {
            return Err(InputError::Unsupported {
                capability: "ime-focused-editor".to_owned(),
                evidence: "the GPUI focus handle was not focused while dispatching the IME step"
                    .to_owned(),
            });
        }
        match (operation, step) {
            (ImeOperation::Compose, InputStep::ImeCompose { value }) => {
                if value.is_empty() {
                    return Err(InputError::Ime(
                        "an empty composition is a cancellation; use ime-cancel".to_owned(),
                    ));
                }
                let Some(marked) = self.marked_range_after.as_ref() else {
                    return Err(InputError::Unsupported {
                        capability: "ime-marked-range".to_owned(),
                        evidence: "composition returned no marked UTF-16 range".to_owned(),
                    });
                };
                let marked_len = marked.end.saturating_sub(marked.start);
                let expected_len = value.encode_utf16().count();
                if marked_len != expected_len
                    || self.marked_text_after.as_deref() != Some(value.as_str())
                {
                    return Err(InputError::Unsupported {
                        capability: "ime-marked-text-readback".to_owned(),
                        evidence: format!(
                            "marked text/range mismatch: expected {:?} ({expected_len} UTF-16 units), got {:?} / {:?}",
                            value, self.marked_text_after, self.marked_range_after
                        ),
                    });
                }
                if self.commit_count != 0 {
                    return Err(InputError::Ime(
                        "composition update committed before ime-commit".to_owned(),
                    ));
                }
            }
            (ImeOperation::Commit, InputStep::ImeCommit { value })
            | (ImeOperation::Text, InputStep::ImeText { value }) => {
                if value.is_empty() {
                    return Err(InputError::Ime(
                        "empty ime-text/ime-commit would be an unobservable no-op".to_owned(),
                    ));
                }
                if self.commit_count != 1 {
                    return Err(InputError::Ime(format!(
                        "{} must commit exactly once, observed {} commits",
                        match operation {
                            ImeOperation::Commit => "ime-commit",
                            ImeOperation::Text => "ime-text",
                            _ => unreachable!(),
                        },
                        self.commit_count
                    )));
                }
                if self.marked_range_after.is_some() || self.marked_text_after.is_some() {
                    return Err(InputError::Ime(
                        "committed IME text left a marked range behind".to_owned(),
                    ));
                }
                let commit_was_a_noop = !value.is_empty()
                    && self.text_before == self.text_after
                    && (operation == ImeOperation::Text || self.marked_range_before.is_none());
                if commit_was_a_noop {
                    return Err(InputError::Ime(
                        "text input reported success without changing the editor value".to_owned(),
                    ));
                }
            }
            (ImeOperation::Cancel, InputStep::ImeCancel) => {
                if self.marked_range_before.is_none() {
                    return Err(InputError::Ime(
                        "ime-cancel was dispatched without an active marked composition".to_owned(),
                    ));
                }
                if self.commit_count != 0 {
                    return Err(InputError::Ime(
                        "ime-cancel must not commit replacement text".to_owned(),
                    ));
                }
                if self.text_before != self.text_after {
                    return Err(InputError::Ime(
                        "ime-cancel changed the editor value".to_owned(),
                    ));
                }
                if self.marked_range_after.is_some() || self.marked_text_after.is_some() {
                    return Err(InputError::Ime(
                        "ime-cancel left marked text behind".to_owned(),
                    ));
                }
            }
            _ => return Err(InputError::Ime("IME observation/step mismatch".to_owned())),
        }
        Ok(())
    }
}

/// One input event and its live evidence. Waits carry their deterministic
/// clock position; every dispatched action has a transcript row, including a
/// successful IME observation.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct InputTranscriptEntry {
    /// Original action index in the journey.
    pub index: usize,
    /// Deterministic virtual time at dispatch.
    pub virtual_time_ms: u64,
    /// Exact action delivered to GPUI/the product adapter.
    pub step: InputStep,
    /// Live IME evidence for IME steps.
    pub ime: Option<ImeObservation>,
}

impl InputStep {
    /// A compact action constructor for tests and adapters.
    #[must_use]
    pub fn key(value: impl Into<String>) -> Self {
        Self::Key {
            value: value.into(),
        }
    }

    /// Returns the elapsed time represented by a step.
    #[must_use]
    pub const fn duration(&self) -> Duration {
        match self {
            Self::Wait { milliseconds } => Duration::from_millis(*milliseconds),
            _ => Duration::ZERO,
        }
    }
}

/// A named transition sequence from one visual state to another.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TransitionScript {
    /// Stable script id used in artifact paths.
    pub id: String,
    /// State before the first input event.
    pub from: String,
    /// State expected after all events settle.
    pub to: String,
    /// Ordered user events.
    pub steps: Vec<InputStep>,
}

/// Semantic destination of a keyboard action.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ActionTarget {
    /// Whole workspace shell.
    Workspace,
    /// Omnibar text field.
    Omnibar,
    /// Library/shelf navigation.
    Library,
    /// Reader tabs and page body.
    Reader,
    /// Palette overlay.
    Palette,
    /// Settings sheet.
    Settings,
}

/// One action exposed by the desktop keymap.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ActionDescriptor {
    /// Stable action id.
    pub id: String,
    /// User-facing action label.
    pub label: String,
    /// Keybinding spelling, when one exists.
    pub shortcut: Option<String>,
    /// Focus/context owner that receives the action.
    pub target: ActionTarget,
}

/// The semantic action tree used by keyboard journey tests and reports.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ActionTree {
    /// Action descriptors in deterministic keymap order.
    pub actions: Vec<ActionDescriptor>,
}

impl ActionTree {
    /// Builds a tree from the product action descriptors emitted by the live
    /// adapter. The harness owns no parallel, stale keymap.
    #[must_use]
    pub fn from_actions(actions: impl IntoIterator<Item = ActionDescriptor>) -> Self {
        Self {
            actions: actions.into_iter().collect(),
        }
    }

    /// Returns an action by stable id.
    #[must_use]
    pub fn find(&self, id: &str) -> Option<&ActionDescriptor> {
        self.actions.iter().find(|action| action.id == id)
    }
}

impl TransitionScript {
    /// Builds a transition script.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        from: impl Into<String>,
        to: impl Into<String>,
        steps: Vec<InputStep>,
    ) -> Self {
        Self {
            id: id.into(),
            from: from.into(),
            to: to.into(),
            steps,
        }
    }

    /// The baseline keyboard/focus transitions required on every desktop page.
    #[must_use]
    pub fn baseline() -> Vec<Self> {
        vec![
            Self::new(
                "focus-forward",
                "browse",
                "browse",
                vec![InputStep::FocusNext],
            ),
            Self::new(
                "focus-backward",
                "browse",
                "browse",
                vec![InputStep::FocusPrevious],
            ),
            Self::new(
                "palette-open-dismiss",
                "browse",
                "browse--palette",
                vec![InputStep::key("cmd-p"), InputStep::key("escape")],
            ),
            Self::new(
                "settings-open-dismiss",
                "browse",
                "browse--settings-appearance",
                vec![InputStep::key("cmd-,"), InputStep::key("escape")],
            ),
            Self::new(
                "omnibar-text",
                "browse",
                "browse",
                vec![
                    InputStep::key("cmd-l"),
                    InputStep::Text {
                        value: "serde".to_owned(),
                    },
                    InputStep::key("escape"),
                ],
            ),
        ]
    }

    /// Stress scripts for motion interruption, responsive resize, and focus loss.
    #[must_use]
    pub fn stress() -> Vec<Self> {
        vec![
            Self::new(
                "resize-during-animation",
                "browse",
                "browse",
                vec![
                    InputStep::key("cmd-p"),
                    InputStep::Wait { milliseconds: 50 },
                    InputStep::Resize {
                        width: 640,
                        height: 480,
                    },
                    InputStep::Wait { milliseconds: 50 },
                    InputStep::Resize {
                        width: 1_440,
                        height: 900,
                    },
                    InputStep::key("escape"),
                ],
            ),
            Self::new(
                "rapid-input-reversal",
                "browse",
                "browse",
                vec![
                    InputStep::key("cmd-p"),
                    InputStep::key("escape"),
                    InputStep::key("cmd-p"),
                    InputStep::key("escape"),
                    InputStep::key("cmd-p"),
                    InputStep::key("escape"),
                ],
            ),
            Self::new(
                "lost-focus-restoration",
                "browse",
                "browse",
                vec![
                    InputStep::key("cmd-p"),
                    InputStep::WindowFocus { focused: false },
                    InputStep::WindowFocus { focused: true },
                    InputStep::key("escape"),
                ],
            ),
            Self::new(
                "reduced-motion",
                "browse",
                "browse",
                vec![
                    InputStep::key("cmd-shift-m"),
                    InputStep::key("cmd-p"),
                    InputStep::key("escape"),
                ],
            ),
        ]
    }

    /// IME contract journeys. These are deliberately separate from the
    /// visual motion stress set so a desktop adapter can run them against a
    /// focused CE `Input`, `Textarea`, or `Editor` and preserve the live input
    /// transcript as evidence.
    #[must_use]
    pub fn ime_contract() -> Vec<Self> {
        vec![
            Self::new(
                "ime-ascii-compose-update-commit",
                "browse",
                "browse",
                vec![
                    InputStep::FocusNext,
                    InputStep::ImeCompose {
                        value: "n".to_owned(),
                    },
                    InputStep::ImeCompose {
                        value: "ni".to_owned(),
                    },
                    InputStep::ImeCommit {
                        value: "你".to_owned(),
                    },
                ],
            ),
            Self::new(
                "ime-non-latin-emoji-surrogate-boundary",
                "browse",
                "browse",
                vec![
                    InputStep::FocusNext,
                    InputStep::ImeCompose {
                        value: "你".to_owned(),
                    },
                    InputStep::ImeCompose {
                        value: "你😀".to_owned(),
                    },
                    InputStep::ImeCommit {
                        value: "你😀".to_owned(),
                    },
                ],
            ),
            Self::new(
                "ime-replace-selection",
                "browse",
                "browse",
                vec![
                    InputStep::FocusNext,
                    InputStep::key("cmd-a"),
                    InputStep::ImeCompose {
                        value: "新".to_owned(),
                    },
                    InputStep::ImeCommit {
                        value: "新".to_owned(),
                    },
                ],
            ),
            Self::new(
                "ime-cancel-without-insertion",
                "browse",
                "browse",
                vec![
                    InputStep::FocusNext,
                    InputStep::ImeCompose {
                        value: "候".to_owned(),
                    },
                    InputStep::ImeCancel,
                ],
            ),
            Self::new(
                "ime-focus-loss",
                "browse",
                "browse",
                vec![
                    InputStep::FocusNext,
                    InputStep::ImeCompose {
                        value: "a".to_owned(),
                    },
                    InputStep::WindowFocus { focused: false },
                    InputStep::WindowFocus { focused: true },
                    InputStep::ImeCancel,
                ],
            ),
            Self::new(
                "ime-modal-focus",
                "browse",
                "browse--palette",
                vec![
                    InputStep::key("cmd-p"),
                    InputStep::ImeCompose {
                        value: "候".to_owned(),
                    },
                    InputStep::ImeCancel,
                    InputStep::key("escape"),
                ],
            ),
            Self::new(
                "ime-no-focused-editor-rejected",
                "browse",
                "browse",
                vec![
                    InputStep::WindowFocus { focused: false },
                    InputStep::ImeText {
                        value: "rejected".to_owned(),
                    },
                ],
            ),
        ]
    }

    /// Validates that the script has a meaningful identity and finite timing.
    pub fn validate(&self) -> Result<(), InputError> {
        if self.id.trim().is_empty() {
            return Err(InputError::EmptyId);
        }
        if self.from.trim().is_empty() || self.to.trim().is_empty() {
            return Err(InputError::MissingEndpoint(self.id.clone()));
        }
        if self.steps.len() > 10_000 {
            return Err(InputError::TooManySteps(self.id.clone()));
        }
        if self
            .steps
            .iter()
            .any(|step| {
                matches!(step, InputStep::TextScale { percent } if !(50_u16..=300).contains(percent))
            })
        {
            return Err(InputError::InvalidTextScale);
        }
        if self.elapsed() > Duration::from_secs(10 * 60) {
            return Err(InputError::TooLong(self.id.clone()));
        }
        Ok(())
    }

    /// Total deterministic clock advancement in this script.
    #[must_use]
    pub fn elapsed(&self) -> Duration {
        self.steps.iter().map(InputStep::duration).sum()
    }
}

/// Script validation failures.
#[derive(Debug, Error)]
pub enum InputError {
    /// Script ids are artifact keys and cannot be empty.
    #[error("input script id is empty")]
    EmptyId,
    /// Both endpoints are required for transition reports.
    #[error("input script {0:?} is missing a state endpoint")]
    MissingEndpoint(String),
    /// Protects the harness from accidental unbounded input generation.
    #[error("input script {0:?} has too many steps")]
    TooManySteps(String),
    /// Protects capture runs from an accidental multi-hour virtual wait.
    #[error("input script {0:?} advances the virtual clock for too long")]
    TooLong(String),
    /// GPUI rejected a keystroke spelling.
    #[error("invalid GPUI keystroke: {0}")]
    Keystroke(String),
    /// A pointer script named an unsupported mouse button.
    #[error("invalid mouse button: {0}")]
    MouseButton(String),
    /// A text scale outside the stress matrix is unsafe for deterministic
    /// layout comparison.
    #[error("text scale must be between 50% and 300%")]
    InvalidTextScale,
    /// A native or product input capability cannot be expressed by the
    /// active GPUI platform. Journey runners must preserve this error as a
    /// red/unsupported result rather than treating it as a successful no-op.
    #[error("unsupported input capability {capability:?}: {evidence}")]
    Unsupported {
        /// Stable capability name used by journey reports.
        capability: String,
        /// Concrete platform/adapter evidence for the rejection.
        evidence: String,
    },
    /// The adapter or observation violated the CE input transition contract.
    #[error("invalid IME transition: {0}")]
    Ime(String),
}

/// Converts a serialised modifier set to GPUI's modifier type.
#[must_use]
pub const fn modifiers(shift: bool, control: bool, alt: bool, command: bool) -> Modifiers {
    Modifiers {
        shift,
        control,
        alt,
        platform: command,
        function: false,
    }
}

/// Converts logical coordinates into GPUI points.
#[must_use]
pub fn position(x: f32, y: f32) -> Point<Pixels> {
    point(px(x), px(y))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_scripts_are_named_and_bounded() {
        for script in TransitionScript::baseline() {
            script.validate().expect("baseline script should validate");
            assert!(script.steps.len() < 10);
        }
    }

    #[test]
    fn stress_scripts_cover_resize_reversal_focus_loss_and_reduced_motion() {
        let scripts = TransitionScript::stress();
        for script in &scripts {
            script.validate().expect("stress script should validate");
        }
        assert_eq!(scripts.len(), 4);
        assert!(scripts.iter().any(|script| {
            script
                .steps
                .iter()
                .any(|step| matches!(step, InputStep::Resize { .. }))
        }));
        assert!(scripts.iter().any(|script| {
            script
                .steps
                .iter()
                .any(|step| matches!(step, InputStep::WindowFocus { focused: false }))
        }));
    }

    #[test]
    fn ime_contract_journeys_cover_live_composition_edges() {
        let scripts = TransitionScript::ime_contract();
        for script in &scripts {
            script.validate().expect("IME journey should validate");
        }
        assert_eq!(scripts.len(), 7);
        assert!(scripts.iter().any(|script| {
            script
                .steps
                .iter()
                .any(|step| matches!(step, InputStep::ImeCompose { value } if value == "ni"))
        }));
        assert!(scripts.iter().any(|script| {
            script
                .steps
                .iter()
                .any(|step| matches!(step, InputStep::ImeCompose { value } if value.contains('😀')))
        }));
        assert!(scripts.iter().any(|script| {
            script
                .steps
                .iter()
                .any(|step| matches!(step, InputStep::ImeCancel))
        }));
        assert!(scripts.iter().any(|script| {
            script
                .steps
                .iter()
                .any(|step| matches!(step, InputStep::Key { value } if value == "cmd-a"))
        }));
        assert!(scripts.iter().any(|script| {
            script
                .steps
                .iter()
                .any(|step| matches!(step, InputStep::Key { value } if value == "cmd-p"))
        }));
        assert!(scripts.iter().any(|script| {
            script
                .steps
                .iter()
                .any(|step| matches!(step, InputStep::WindowFocus { focused: false }))
        }));
    }

    #[test]
    fn ime_observation_rejects_silent_dispatch_and_preserves_utf16_ranges() {
        let step = InputStep::ImeCompose {
            value: "😀".to_owned(),
        };
        let mut observation = ImeObservation {
            operation: ImeOperation::Compose,
            focused: true,
            text_before: String::new(),
            text_after: "😀".to_owned(),
            marked_range_before: None,
            marked_range_after: Some(0..2),
            marked_text_before: None,
            marked_text_after: Some("😀".to_owned()),
            selected_range_after: Some(2..2),
            commit_count: 0,
        };
        observation
            .validate_for(&step)
            .expect("emoji uses two UTF-16 code units");
        observation.marked_range_after = None;
        observation.marked_text_after = None;
        assert!(matches!(
            observation.validate_for(&step),
            Err(InputError::Unsupported { .. })
        ));
    }

    #[test]
    fn ime_cancel_requires_unchanged_text_and_no_marked_range() {
        let step = InputStep::ImeCancel;
        let observation = ImeObservation {
            operation: ImeOperation::Cancel,
            focused: true,
            text_before: "seed".to_owned(),
            text_after: "seed".to_owned(),
            marked_range_before: Some(4..5),
            marked_range_after: None,
            marked_text_before: Some("候".to_owned()),
            marked_text_after: None,
            selected_range_after: Some(4..4),
            commit_count: 0,
        };
        observation
            .validate_for(&step)
            .expect("cancel removes marked text without insertion");
    }

    #[test]
    fn duration_only_counts_waits() {
        let script = TransitionScript::new(
            "wait",
            "browse",
            "browse",
            vec![InputStep::key("tab"), InputStep::Wait { milliseconds: 12 }],
        );
        assert_eq!(script.elapsed(), Duration::from_millis(12));
    }

    #[test]
    fn semantic_action_tree_preserves_product_descriptors() {
        let tree = ActionTree::from_actions([
            ActionDescriptor {
                id: "dismiss".to_owned(),
                label: "Dismiss".to_owned(),
                shortcut: Some("escape".to_owned()),
                target: ActionTarget::Workspace,
            },
            ActionDescriptor {
                id: "focus-omnibar".to_owned(),
                label: "Focus omnibar".to_owned(),
                shortcut: Some("cmd-k".to_owned()),
                target: ActionTarget::Omnibar,
            },
        ]);
        assert_eq!(
            tree.find("dismiss").map(|action| action.target),
            Some(ActionTarget::Workspace)
        );
        assert_eq!(
            tree.find("focus-omnibar").map(|action| action.target),
            Some(ActionTarget::Omnibar)
        );
    }

    #[test]
    fn text_scale_stress_steps_are_serialisable_and_bounded() {
        let step = InputStep::TextScale { percent: 200 };
        let encoded = serde_json::to_string(&step).expect("text scale json");
        assert!(encoded.contains("text-scale"));
        assert!(matches!(
            serde_json::from_str::<InputStep>(&encoded),
            Ok(InputStep::TextScale { percent: 200 })
        ));
    }
}

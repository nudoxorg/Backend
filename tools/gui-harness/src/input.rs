//! Deterministic user input scripts used before a capture.

use gpui::{Modifiers, Pixels, Point, point, px};
use serde::{Deserialize, Serialize};
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

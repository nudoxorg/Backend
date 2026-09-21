//! Visible-window journey contract.
//!
//! Direct offscreen rendering proves pixels.  This separate contract names the
//! compositor/native-window checks that must run in an actual visible GPUI
//! process: titlebar routing, clipboard, focus activation, and resize.

use crate::{CaptureError, Viewport};
use serde::{Deserialize, Serialize};

/// One visible-window operation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum VisibleJourneyStep {
    /// Window was shown with the native titlebar and app-owned content.
    ShowWindow,
    /// Window received platform focus.
    Activate,
    /// Keyboard shortcut traversed the real platform event path.
    Keyboard,
    /// Clipboard write/read crossed the platform integration.
    Clipboard,
    /// Pointer focus was moved between controls.
    PointerFocus,
    /// Window was resized through the compositor.
    Resize,
    /// Window closed and released native resources.
    Close,
}

/// Named visible-window journey and its required viewport checks.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VisibleJourney {
    /// Stable journey id.
    pub id: String,
    /// Required ordered operations.
    pub steps: Vec<VisibleJourneyStep>,
    /// Responsive sizes to exercise through native resizing.
    pub viewports: Vec<Viewport>,
}

impl VisibleJourney {
    /// Returns the canonical platform journey.
    pub fn required() -> Result<Self, CaptureError> {
        Ok(Self {
            id: "visible-window-platform".to_owned(),
            steps: vec![
                VisibleJourneyStep::ShowWindow,
                VisibleJourneyStep::Activate,
                VisibleJourneyStep::Keyboard,
                VisibleJourneyStep::Clipboard,
                VisibleJourneyStep::PointerFocus,
                VisibleJourneyStep::Resize,
                VisibleJourneyStep::Close,
            ],
            viewports: Viewport::required(),
        })
    }

    /// Checks that the platform journey cannot omit a critical integration.
    pub fn validate(&self) -> Result<(), CaptureError> {
        let required = [
            VisibleJourneyStep::ShowWindow,
            VisibleJourneyStep::Activate,
            VisibleJourneyStep::Keyboard,
            VisibleJourneyStep::Clipboard,
            VisibleJourneyStep::PointerFocus,
            VisibleJourneyStep::Resize,
            VisibleJourneyStep::Close,
        ];
        if required.iter().any(|step| !self.steps.contains(step)) || self.viewports.is_empty() {
            return Err(CaptureError::Gpui(format!(
                "visible journey {:?} is incomplete",
                self.id
            )));
        }
        Ok(())
    }
}

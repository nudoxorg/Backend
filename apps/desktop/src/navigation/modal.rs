//! Modal stack with typed focus restoration.

use super::focus::FocusId;

/// Closed modal vocabulary owned by the shell.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ModalId {
    /// Command palette.
    CommandPalette,
    /// Settings sheet.
    Settings,
    /// Source viewer.
    Source,
    /// Error/fault sheet.
    Fault,
}

impl ModalId {
    /// Converts the modal into its stable focus node.
    #[must_use]
    pub const fn focus(self) -> FocusId {
        // Modal(0) is reserved as the shell's non-modal sentinel; the focus
        // tree registers concrete modal nodes from one upward.
        FocusId::Modal(self as u16 + 1)
    }
}

/// One modal frame and the focus that launched it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModalFrame {
    /// Modal identity.
    pub id: ModalId,
    /// Focus to restore when dismissed.
    pub restore: FocusId,
}

/// Top-first modal stack.
#[derive(Clone, Debug, Default)]
pub struct ModalStack {
    frames: Vec<ModalFrame>,
}

impl ModalStack {
    /// Pushes a modal and returns its frame.
    pub fn push(&mut self, id: ModalId, restore: FocusId) -> ModalFrame {
        let frame = ModalFrame { id, restore };
        self.frames.push(frame);
        frame
    }

    /// Pops the top modal.
    pub fn pop(&mut self) -> Option<ModalFrame> {
        self.frames.pop()
    }

    /// Returns the top modal.
    #[must_use]
    pub fn top(&self) -> Option<ModalId> {
        self.frames.last().map(|frame| frame.id)
    }

    /// Returns the stack depth.
    #[must_use]
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// Returns whether the stack is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modal_ids_map_to_registered_focus_nodes() {
        assert_eq!(ModalId::CommandPalette.focus(), FocusId::Modal(1));
        assert_eq!(ModalId::Fault.focus(), FocusId::Modal(4));
    }
}

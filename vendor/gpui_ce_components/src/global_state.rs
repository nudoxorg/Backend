use gpui::{App, Entity, Global};

use crate::text::{LinkAdmission, TextViewState};

pub use gpui_base::GlobalState;

pub(crate) fn init(cx: &mut App) {
    // Preserve the legacy initialization point while `gpui_base::init` remains
    // after Root initialization for focus-trap ordering compatibility.
    GlobalState::init(cx);
    cx.set_global(UiGlobalState::new());
}

/// UI-only global state whose types cannot cross into `gpui-base`.
struct TextViewScope {
    state: Entity<TextViewState>,
    // Snapshot the UI predicate before rendering this entity. Reading the
    // entity again from its own Render callback would alias its mutable borrow.
    admission: Option<LinkAdmission>,
    clamped: bool,
}

pub(crate) struct UiGlobalState {
    text_view_state_stack: Vec<TextViewScope>,
    selection_document_order: u64,
}

impl Global for UiGlobalState {}

impl UiGlobalState {
    fn new() -> Self {
        Self {
            text_view_state_stack: Vec::new(),
            selection_document_order: 1,
        }
    }

    pub(crate) fn global(cx: &App) -> &Self {
        cx.global::<Self>()
    }

    pub(crate) fn global_mut(cx: &mut App) -> &mut Self {
        cx.global_mut::<Self>()
    }

    pub(crate) fn text_view_state(&self) -> Option<&Entity<TextViewState>> {
        self.text_view_state_stack.last().map(|scope| &scope.state)
    }

    pub(crate) fn text_view_admission(&self) -> Option<&LinkAdmission> {
        self.text_view_state_stack
            .last()
            .and_then(|scope| scope.admission.as_ref())
    }

    pub(crate) fn text_view_clamped(&self) -> bool {
        self.text_view_state_stack.iter().any(|scope| scope.clamped)
    }

    pub(crate) fn push_text_view(
        &mut self,
        state: Entity<TextViewState>,
        admission: Option<LinkAdmission>,
        clamped: bool,
    ) {
        self.text_view_state_stack.push(TextViewScope {
            state,
            admission,
            clamped,
        });
    }

    pub(crate) fn pop_text_view(&mut self) {
        self.text_view_state_stack.pop();
    }

    pub(crate) fn begin_selection_frame(&mut self) {
        self.selection_document_order = 1;
    }

    pub(crate) fn next_selection_document_order(&mut self) -> u64 {
        let order = self.selection_document_order;
        self.selection_document_order = self.selection_document_order.wrapping_add(1);
        order
    }
}

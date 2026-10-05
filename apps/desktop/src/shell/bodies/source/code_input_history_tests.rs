#![allow(clippy::expect_used, clippy::panic)]

use super::tests::LongSource;
use crate::model::pages::{Known, PageValue, ReadFailure, RelationKind};
use crate::navigation::presentation::{ReadingChange, ReadingSession, SourceLineDraft};
use crate::navigation::{Intent, View};
use crate::runtime::reads::{PageReader, ReadContext, ReadPool, ReadRequest};
use crate::shell::tests::{native_bounds, rig_with_reads, view_route};
use gpui::{AppContext as _, TestAppContext};
use gpui_component::WindowExt as _;
use std::sync::Arc;

struct CalledBySource;

impl PageReader for CalledBySource {
    fn read(&mut self, request: &ReadRequest, context: &ReadContext<'_>) -> Result<PageValue, ReadFailure> {
        let mut fixture = LongSource;
        let mut value = fixture.read(request, context)?;
        if let PageValue::Symbol(page) = &mut value {
            let mut caller = page.rose.down.known().expect("fixture relation")[0].clone();
            caller.kind = RelationKind::Semantic(backend_library::SemanticLinkKind::Calls);
            page.rose.left = Known::Known(Arc::from([caller]));
        }
        Ok(value)
    }
}

fn draft(rig: &mut crate::shell::tests::Rig) -> String {
    rig.graph.store.read_with(rig.cx, |store, _| {
        store.snapshot().session().reading.current.presentation.controls()
            .source_line_draft.as_str().to_owned()
    })
}

fn visit(rig: &mut crate::shell::tests::Rig) -> crate::navigation::presentation::VisitId {
    rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().session().reading.current.id)
}

fn focus_line_input(rig: &mut crate::shell::tests::Rig) -> gpui::Entity<gpui_component::input::InputState> {
    let targets = rig.shell.read_with(rig.cx, |shell, cx| shell.reader_targets(cx));
    targets.focus("source-jump-field");
    rig.cx.update(|window, cx| {
        assert!(targets.focus_native("source-jump-field", window, cx), "mounted line editor");
    });
    // Focus changes the native handle and requests a frame. The component
    // registers its focused InputState during that frame's Input::render,
    // rather than synchronously inside FocusHandle::focus.
    rig.draw_frame();
    rig.cx.update(|window, cx| {
        assert_eq!(targets.native_focused(window).as_deref(), Some("source-jump-field"),
            "the rendered line editor retains the native focus we requested");
        window.focused_input(cx).and_then(|input| input.as_input().cloned())
            .expect("the focused Reader target is the actual input engine")
    })
}

#[test]
fn line_draft_is_bounded_and_owned_by_its_reading_visit() {
    for invalid in ["12345678901234567", "17\n", "17a", "١٧"] {
        assert!(SourceLineDraft::new(invalid).is_none(), "invalid line input: {invalid:?}");
    }
    assert_eq!(SourceLineDraft::new("1234567890123456").expect("bounded digits").as_str(),
        "1234567890123456");

    let code = view_route("RelationLabel", View::Code);
    let relation = view_route("RelationDirection", View::Code);
    let mut reading = ReadingSession::new(&code);
    let original = reading.current.id;
    assert!(reading.current.presentation.apply(ReadingChange::SourceLineDraft(
        SourceLineDraft::new("17").expect("line draft"))));
    let saved = reading.current.clone();
    assert!(reading.fresh(&relation));
    assert_eq!(reading.current.presentation.controls().source_line_draft.as_str(), "");
    assert!(reading.restore(saved, &code));
    assert_eq!(reading.current.id, original);
    assert_eq!(reading.current.presentation.controls().source_line_draft.as_str(), "17");
}

#[gpui::test]
fn native_tab_edits_code_line_immediately_and_history_restores_its_draft(
    cx: &mut TestAppContext,
) {
    let code = view_route("RelationLabel", View::Code);
    let pool = ReadPool::start(2, |_| CalledBySource).expect("source read pool");
    let mut rig = rig_with_reads(cx, Some(code.clone()), 1440.0, 900.0, pool);
    let original = visit(&mut rig);
    let targets = rig.shell.read_with(rig.cx, |shell, cx| shell.reader_targets(cx));
    assert!(targets.native_keys().iter().any(|id| id == "source-jump-field"),
        "the line target must register its actual editor handle");
    rig.cx.update(|window, cx| {
        assert!(targets.focus_native("source-page-top-previous", window, cx),
            "positive native predecessor control");
    });
    rig.keys("tab");
    assert_eq!(rig.cx.update(|window, _| targets.native_focused(window)).as_deref(),
        Some("source-jump-field"), "Tab must land on the text engine, not its outer plate");
    assert!(rig.cx.update(|window, cx| window.has_focused_input(cx)),
        "the native editor must own typing before Return");
    rig.keys("1 7");
    assert_eq!(draft(&mut rig), "17", "typing after Tab writes the mounted visit");

    rig.go(Intent::OpenCommandPalette);
    assert_eq!(draft(&mut rig), "17", "Ask must not rewrite the covered Code draft");
    assert_ne!(rig.cx.update(|window, _| targets.native_focused(window)).as_deref(),
        Some("source-jump-field"), "Ask owns native input while it covers Code");
    rig.go(Intent::DismissOverlay);
    assert_eq!(draft(&mut rig), "17", "dismissing Ask retains the same visit draft");

    let called_by = native_bounds(&mut rig, "Button", "Typed · calls", true)
        .expect("mounted native Called by control");
    rig.cx.simulate_click(called_by.center(), gpui::Modifiers::none());
    rig.settle();
    assert_ne!(rig.route(), code, "the relation must actually open");
    let relation = rig.route();
    assert_ne!(visit(&mut rig), original);
    assert_eq!(draft(&mut rig), "", "a fresh relation has its own blank line draft");

    rig.go(Intent::Back);
    assert_eq!(rig.route(), code);
    assert_eq!(visit(&mut rig), original);
    assert_eq!(draft(&mut rig), "17");
    let targets = rig.shell.read_with(rig.cx, |shell, cx| shell.reader_targets(cx));
    rig.cx.update(|window, cx| assert!(targets.focus_native("source-jump-field", window, cx)));
    rig.keys("enter");
    assert!(rig.said().iter().any(|line| line.contains("Lines 17")),
        "the restored native editor must submit its hydrated value");

    rig.go(Intent::Forward);
    assert_eq!(rig.route(), relation);
    rig.go(Intent::Back);
    assert_eq!(rig.route(), code);
    assert_eq!(draft(&mut rig), "17", "Forward/Back returns the same visit draft");
    rig.go(Intent::Forward);
    rig.go(Intent::Navigate(code.clone()));
    assert_eq!(rig.route(), code);
    assert_ne!(visit(&mut rig), original);
    assert_eq!(draft(&mut rig), "", "a newly navigated Code visit must not inherit the old form");
    rig.go(Intent::Back);
    rig.go(Intent::Back);
    assert_eq!(rig.route(), code);
    assert_eq!(visit(&mut rig), original);
    assert_eq!(draft(&mut rig), "17", "an older visit survives a later fresh Code visit");
}

#[gpui::test]
fn owner_loss_keeps_the_mounted_line_draft_but_denies_source_jump(cx: &mut TestAppContext) {
    let code = view_route("RelationLabel", View::Code);
    let pool = ReadPool::start(2, |_| LongSource).expect("source read pool");
    let mut rig = rig_with_reads(cx, Some(code.clone()), 900.0, 700.0, pool);
    let original = visit(&mut rig);
    let _input = focus_line_input(&mut rig);
    let targets = rig.shell.read_with(rig.cx, |shell, cx| shell.reader_targets(cx));
    rig.graph.store.update(rig.cx, |store, cx| store.owner_failed(
        &crate::runtime::owner::OwnerFault::Lost("source owner unavailable".into()), cx,
    ));
    assert!(rig.graph.store.read_with(rig.cx, |store, _| store.current_owner_attachment().is_none()),
        "the source producer lease is genuinely revoked");
    // The producer receipt is revoked now, before a repaint can replace the
    // still-mounted native editor with a read-only retained source page.
    rig.cx.simulate_input("17");
    rig.cx.simulate_keystrokes("enter");
    assert_eq!(targets.focused().as_deref(), Some("source-jump-field"),
        "Return must not jump to line 17 through the revoked source lease");
    rig.cx.run_until_parked();
    assert_eq!(draft(&mut rig), "17", "local visit editing outlives owner authority");
    rig.settle();
    rig.go(Intent::Back);
    rig.go(Intent::Forward);
    assert_eq!(rig.route(), code);
    assert_eq!(visit(&mut rig), original);
    assert_eq!(draft(&mut rig), "17", "the returned unavailable visit retains the draft");
}

#[gpui::test]
fn latent_editor_change_under_a_settings_page_cannot_rewrite_code_visit(cx: &mut TestAppContext) {
    let code = view_route("RelationLabel", View::Code);
    let pool = ReadPool::start(2, |_| LongSource).expect("source read pool");
    let mut rig = rig_with_reads(cx, Some(code.clone()), 900.0, 700.0, pool);
    let input = focus_line_input(&mut rig);
    rig.cx.simulate_input("17");
    rig.settle();
    assert_eq!(draft(&mut rig), "17");

    let reader = rig.shell.read_with(rig.cx, |shell, _| shell.reader_entity());
    let links = reader.read_with(rig.cx, |reader, _| reader.navigation_links());
    rig.cx.update(|_, cx| links.dispatch(
        Intent::OpenSettings(crate::navigation::SettingsPage::Help), cx,
    ));
    rig.cx.run_until_parked();
    assert!(rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().page_overlay().is_some()),
        "the local page owns input before the old editor can report a change");
    // replace_all emits InputEvent::Change, unlike set_value; the old Pager
    // subscription remains alive until its prior painted body is retired.
    rig.cx.update(|window, cx| input.update(cx, |input, cx| input.replace_all("99", window, cx)));
    rig.cx.run_until_parked();
    assert_eq!(input.read_with(rig.cx, |input, _| input.value().to_string()), "99", "positive latent Change source");
    assert_eq!(draft(&mut rig), "17", "a covered Code page cannot accept latent editor text");
    rig.go(Intent::DismissOverlay);
    assert_eq!(rig.route(), code);
    assert_eq!(draft(&mut rig), "17");
}

#[gpui::test]
fn settings_returns_to_the_native_line_editor_even_without_a_logical_selection(
    cx: &mut TestAppContext,
) {
    let code = view_route("RelationLabel", View::Code);
    let pool = ReadPool::start(2, |_| LongSource).expect("source read pool");
    let mut rig = rig_with_reads(cx, Some(code.clone()), 900.0, 700.0, pool);
    let original = visit(&mut rig);
    let input = focus_line_input(&mut rig);
    rig.keys("1 7");
    assert_eq!(draft(&mut rig), "17");
    let targets = rig.shell.read_with(rig.cx, |shell, cx| shell.reader_targets(cx));
    targets.clear_focus();
    assert_eq!(rig.cx.update(|window, _| targets.native_focused(window)).as_deref(),
        Some("source-jump-field"), "native focus, rather than Recall, owns this return");
    rig.keys("secondary-,");
    rig.keys("escape");
    assert_eq!(rig.route(), code);
    assert_eq!(visit(&mut rig), original);
    assert_eq!(rig.cx.update(|window, _| targets.native_focused(window)).as_deref(),
        Some("source-jump-field"), "Settings must return to the mounted native editor");
    assert!(rig.cx.update(|window, cx| window.has_focused_input(cx)),
        "the actual text engine receives subsequent typing");
    rig.keys("8");
    assert_eq!(input.read_with(rig.cx, |input, _| input.value().to_string()), "178");
    assert_eq!(draft(&mut rig), "178");
    rig.keys("enter");
    assert!(rig.said().iter().any(|line| line.contains("Lines 178")),
        "Return submits the restored editor without clicking it again");
}

#[gpui::test]
fn settings_native_return_cannot_steal_focus_after_a_later_explicit_blur(cx: &mut TestAppContext) {
    let code = view_route("RelationLabel", View::Code);
    let pool = ReadPool::start(2, |_| LongSource).expect("source read pool");
    let mut rig = rig_with_reads(cx, Some(code.clone()), 900.0, 700.0, pool);
    let _input = focus_line_input(&mut rig);
    rig.keys("secondary-,");
    let reader = rig.shell.read_with(rig.cx, |shell, _| shell.reader_entity());
    let links = reader.read_with(rig.cx, |reader, _| reader.navigation_links());
    rig.cx.update(|_, cx| links.dispatch(Intent::DismissOverlay, cx));
    assert_eq!(rig.route(), code);
    assert!(rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().overlay().is_none()));
    rig.cx.update(|window, _| window.blur());
    rig.settle();
    assert!(rig.cx.update(|window, cx| window.focused(cx).is_none()),
        "the landing Code page must respect a newer explicit blur");
    assert!(!rig.cx.update(|window, cx| window.has_focused_input(cx)),
        "the former line editor cannot reacquire native input after that choice");
}

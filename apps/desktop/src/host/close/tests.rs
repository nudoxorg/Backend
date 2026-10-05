use super::*;
use crate::model::{DensityPreference, PersistentState};
use crate::navigation::Intent;
use crate::runtime::persistence_writer::{PersistenceWriter, WriteFailure};
use crate::shell::tests::{Rig, rig};
use gpui::TestAppContext;
use std::sync::mpsc;

fn closing(rig: &mut Rig) -> Entity<GracefulClose> {
    rig._window.root(rig.cx).expect("component root").read_with(rig.cx, |root, cx| {
        root.view().clone().downcast::<CloseView>().expect("real close view").read(cx).close.clone()
    })
}

struct HeldSave { entered: mpsc::Receiver<()>, release: Option<mpsc::Sender<()>>, persistence: PersistentState }
impl Drop for HeldSave {
    fn drop(&mut self) { if let Some(release) = self.release.take() { let _ = release.send(()); } }
}
fn held_writer(rig: &mut Rig) -> HeldSave {
    let path = crate::host::scratch_base().join(format!("nx-close-{}-{}.json", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("clock").as_nanos()));
    let directory = path.with_extension("");
    crate::host::private_dir(&directory).expect("private fixture");
    let persistence = PersistentState::at(directory.join("desktop.json"));
    let saving = persistence.clone();
    let (started, entered) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let mut first = true;
    let writer = PersistenceWriter::testing(saving.path().to_owned(), move |state| {
        if std::mem::take(&mut first) {
            let _ = started.send(());
            released.recv_timeout(Duration::from_secs(20)).map_err(|error| WriteFailure { message: error.to_string().into() })?;
        }
        saving.save(state).map_err(|error| WriteFailure { message: error.to_string().into() })
    }).expect("writer");
    rig.graph.root.update(rig.cx, |root, _| root.replace_close_writer(writer));
    HeldSave { entered, release: Some(release), persistence }
}
fn pending(close: &Entity<GracefulClose>, rig: &mut Rig) -> bool {
    close.read_with(rig.cx, |close, _| matches!(close.phase, Phase::Saving(_)))
}
fn release(held: &mut HeldSave) { held.release.take().expect("held save").send(()).expect("release"); }

#[derive(Clone, Copy)]
enum Entry { RedClose, CmdW, MenuClose, CmdQ, MenuQuit, NativeQuit }
fn deliver(entry: Entry, rig: &mut Rig) {
    match entry {
        Entry::RedClose => assert!(!rig.cx.simulate_close(), "native close is deferred"),
        Entry::CmdW => rig.cx.simulate_keystrokes("cmd-w"),
        Entry::CmdQ => rig.cx.simulate_keystrokes("cmd-q"),
        Entry::MenuClose => rig.cx.cx.update(|cx| cx.dispatch_action(&super::super::menus::CloseWindow)),
        Entry::MenuQuit => rig.cx.cx.update(|cx| cx.dispatch_action(&super::super::menus::Quit)),
        Entry::NativeQuit => assert!(!rig.cx.cx.simulate_native_quit(), "OS termination is deferred"),
    }
    rig.cx.run_until_parked();
}
fn held_native_path(cx: &mut TestAppContext, entry: Entry) {
    let mut rig = rig(cx, None, 1440.0, 900.0);
    rig.cx.cx.update(super::super::menus::install);
    let close = closing(&mut rig);
    let mut held = held_writer(&mut rig);
    deliver(entry, &mut rig);
    held.entered.recv_timeout(Duration::from_secs(1)).expect("actual writer entered");
    assert!(pending(&close, &mut rig));
    // The actual GPUI clock advances past shutdown's 200ms while the window
    // still paints and owns a close input surface. No sleep stands in for ack.
    rig.cx.cx.executor().advance_clock(Duration::from_millis(350));
    rig.draw();
    assert!(pending(&close, &mut rig));
    assert_eq!(rig.cx.cx.windows().len(), 1, "no premature native close");
    assert_eq!(rig.cx.cx.platform_quit_requests(), 0, "shutdown has not started during a held save");
    assert!(rig.cx.update(|window, _| window.painted_texts().iter().any(|text| text.text.contains("Saving your latest changes"))));
    release(&mut held);
    crate::runtime::wait::until("close checkpoint acknowledged", || {
        rig.cx.run_until_parked();
        close.read_with(rig.cx, |close, _| close.phase == Phase::Committed)
    });
    assert!(held.persistence.load().is_ok(), "real durable state is readable after approval");
    if matches!(entry, Entry::NativeQuit) { assert_eq!(rig.cx.cx.native_quit_reply(), Some(true)); }
    assert!(rig.cx.cx.platform_quit_requests() > 0, "last-window policy and application quit follow acknowledgement");
    if matches!(entry, Entry::RedClose | Entry::CmdW | Entry::MenuClose) {
        assert!(rig.cx.cx.windows().is_empty());
    }
    let _ = std::fs::remove_file(held.persistence.path());
}

#[gpui::test] fn red_close_waits_beyond_shutdown_cap(cx: &mut TestAppContext) { held_native_path(cx, Entry::RedClose); }
#[gpui::test] fn cmd_w_waits_beyond_shutdown_cap(cx: &mut TestAppContext) { held_native_path(cx, Entry::CmdW); }
#[gpui::test] fn menu_close_waits_beyond_shutdown_cap(cx: &mut TestAppContext) { held_native_path(cx, Entry::MenuClose); }
#[gpui::test] fn cmd_q_waits_beyond_shutdown_cap(cx: &mut TestAppContext) { held_native_path(cx, Entry::CmdQ); }
#[gpui::test] fn menu_quit_waits_beyond_shutdown_cap(cx: &mut TestAppContext) { held_native_path(cx, Entry::MenuQuit); }
#[gpui::test] fn native_quit_waits_and_replies_only_after_ack(cx: &mut TestAppContext) { held_native_path(cx, Entry::NativeQuit); }

#[gpui::test]
fn duplicate_close_coalesces_and_quit_escalates_the_same_attempt(cx: &mut TestAppContext) {
    let mut rig = rig(cx, None, 1440.0, 900.0);
    rig.cx.cx.update(super::super::menus::install);
    let close = closing(&mut rig);
    let mut held = held_writer(&mut rig);
    deliver(Entry::RedClose, &mut rig);
    held.entered.recv_timeout(Duration::from_secs(1)).expect("write entered");
    let first = close.read_with(rig.cx, |close, _| close.phase.clone());
    deliver(Entry::MenuClose, &mut rig);
    deliver(Entry::CmdQ, &mut rig);
    assert_eq!(close.read_with(rig.cx, |close, _| close.phase.clone()), first);
    assert_eq!(close.read_with(rig.cx, |close, _| close.target), CloseTarget::Application);
    close.update(rig.cx, |close, cx| close.cancel(cx));
    release(&mut held);
}

#[gpui::test]
fn cancel_reopens_editing_and_late_ack_never_closes_the_new_edit(cx: &mut TestAppContext) {
    let mut rig = rig(cx, None, 1440.0, 900.0);
    let close = closing(&mut rig);
    let mut held = held_writer(&mut rig);
    deliver(Entry::RedClose, &mut rig);
    held.entered.recv_timeout(Duration::from_secs(1)).expect("write entered");
    // Escape goes through the focused native close surface, not a direct state helper.
    rig.cx.simulate_keystrokes("escape");
    rig.cx.run_until_parked();
    assert_eq!(close.read_with(rig.cx, |close, _| close.phase.clone()), Phase::Open);
    rig.graph.root.update(rig.cx, |root, cx| root.dispatch(Intent::SetDensity(DensityPreference::Dense), cx));
    release(&mut held);
    crate::runtime::wait::until("new edit ordered after cancelled checkpoint", || {
        rig.cx.run_until_parked();
        held.persistence.load().is_ok_and(|state| state.density == crate::model::persistence::PersistedDensity::Dense)
    });
    assert_eq!(rig.cx.cx.windows().len(), 1);
    assert_eq!(close.read_with(rig.cx, |close, _| close.phase.clone()), Phase::Open);
    let _ = std::fs::remove_file(held.persistence.path());
}

#[gpui::test]
fn deadline_keeps_the_window_open_and_native_cancel_replies_no(cx: &mut TestAppContext) {
    let mut rig = rig(cx, None, 1440.0, 900.0);
    let close = closing(&mut rig);
    let mut held = held_writer(&mut rig);
    deliver(Entry::NativeQuit, &mut rig);
    held.entered.recv_timeout(Duration::from_secs(1)).expect("write entered");
    rig.cx.cx.executor().advance_clock(Duration::from_secs(6));
    rig.draw();
    assert!(close.read_with(rig.cx, |close, _| matches!(close.phase, Phase::NeedsDecision(_, _))));
    assert_eq!(rig.cx.cx.windows().len(), 1);
    assert!(rig.cx.update(|window, _| window.painted_texts().iter().any(|text| text.text.contains("Previously saved state"))));
    rig.cx.simulate_keystrokes("escape");
    rig.cx.run_until_parked();
    assert_eq!(rig.cx.cx.native_quit_reply(), Some(false));
    release(&mut held);
}

#[gpui::test]
fn last_window_policy_never_requests_quit_before_the_native_close_ack(cx: &mut TestAppContext) {
    let mut rig = rig(cx, None, 1440.0, 900.0);
    let close = closing(&mut rig);
    let mut held = held_writer(&mut rig);
    deliver(Entry::RedClose, &mut rig);
    held.entered.recv_timeout(Duration::from_secs(1)).expect("last-window checkpoint entered");
    assert!(pending(&close, &mut rig));
    assert_eq!(rig.cx.cx.platform_quit_requests(), 0);
    release(&mut held);
    crate::runtime::wait::until("last-window policy commits after acknowledgement", || {
        rig.cx.run_until_parked();
        close.read_with(rig.cx, |close, _| close.phase == Phase::Committed)
    });
    assert!(rig.cx.cx.windows().is_empty());
    assert!(rig.cx.cx.platform_quit_requests() > 0);
    assert!(held.persistence.load().is_ok());
}

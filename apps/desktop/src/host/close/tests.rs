use super::*;
use crate::model::{DensityPreference, PersistentState};
use crate::navigation::Intent;
use crate::runtime::persistence_writer::{PersistenceWriter, WriteFailure};
use crate::shell::tests::{Rig, rig};
use gpui::TestAppContext;
use std::sync::mpsc;

fn closing(rig: &mut Rig) -> Entity<GracefulClose> {
    rig._window
        .root(rig.cx)
        .expect("component root")
        .read_with(rig.cx, |root, cx| {
            root.view()
                .clone()
                .downcast::<CloseView>()
                .expect("real close view")
                .read(cx)
                .close
                .clone()
        })
}

struct HeldSave {
    entered: mpsc::Receiver<()>,
    release: Option<mpsc::Sender<()>>,
    persistence: PersistentState,
}
impl Drop for HeldSave {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
    }
}
fn held_writer(rig: &mut Rig) -> HeldSave {
    let path = crate::host::scratch_base().join(format!(
        "nx-close-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
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
            released
                .recv_timeout(Duration::from_secs(20))
                .map_err(|error| WriteFailure {
                    message: error.to_string().into(),
                })?;
        }
        saving.save(state).map_err(|error| WriteFailure {
            message: error.to_string().into(),
        })
    })
    .expect("writer");
    rig.graph
        .root
        .update(rig.cx, |root, _| root.replace_close_writer(writer));
    HeldSave {
        entered,
        release: Some(release),
        persistence,
    }
}
fn pending(close: &Entity<GracefulClose>, rig: &mut Rig) -> bool {
    close.read_with(rig.cx, |close, _| matches!(close.phase, Phase::Saving(_)))
}
fn release(held: &mut HeldSave) {
    held.release
        .take()
        .expect("held save")
        .send(())
        .expect("release");
}

#[derive(Clone, Copy)]
enum Entry {
    RedClose,
    CmdW,
    MenuClose,
    CmdQ,
    MenuQuit,
    NativeQuit,
}
fn deliver(entry: Entry, rig: &mut Rig) {
    match entry {
        Entry::RedClose => assert!(!rig.cx.simulate_close(), "native close is deferred"),
        Entry::CmdW => rig.cx.simulate_keystrokes("cmd-w"),
        Entry::CmdQ => rig.cx.simulate_keystrokes("cmd-q"),
        Entry::MenuClose => rig
            .cx
            .cx
            .update(|cx| cx.dispatch_action(&super::super::menus::CloseWindow)),
        Entry::MenuQuit => rig
            .cx
            .cx
            .update(|cx| cx.dispatch_action(&super::super::menus::Quit)),
        Entry::NativeQuit => assert!(
            !rig.cx.cx.simulate_native_quit(),
            "OS termination is deferred"
        ),
    }
    rig.cx.run_until_parked();
}
fn held_native_path(cx: &mut TestAppContext, entry: Entry) {
    let mut rig = rig(cx, None, 1440.0, 900.0);
    rig.cx.cx.update(super::super::menus::install);
    let close = closing(&mut rig);
    let mut held = held_writer(&mut rig);
    deliver(entry, &mut rig);
    held.entered
        .recv_timeout(Duration::from_secs(1))
        .expect("actual writer entered");
    assert!(pending(&close, &mut rig));
    // The actual GPUI clock advances past shutdown's 200ms while the window
    // still paints and owns a close input surface. No sleep stands in for ack.
    rig.cx
        .cx
        .executor()
        .advance_clock(Duration::from_millis(350));
    rig.draw();
    assert!(pending(&close, &mut rig));
    assert_eq!(rig.cx.cx.windows().len(), 1, "no premature native close");
    assert_eq!(
        rig.cx.cx.platform_quit_requests(),
        0,
        "shutdown has not started during a held save"
    );
    assert!(rig.cx.update(|window, _| {
        window
            .painted_texts()
            .iter()
            .any(|text| text.text.contains("Saving your latest changes"))
    }));
    release(&mut held);
    crate::runtime::wait::until("close checkpoint acknowledged", || {
        rig.cx
            .cx
            .executor()
            .advance_clock(Duration::from_millis(25));
        rig.cx.run_until_parked();
        close.read_with(rig.cx, |close, _| close.phase == Phase::Committed)
    });
    assert!(
        held.persistence.load().is_ok(),
        "real durable state is readable after approval"
    );
    if matches!(entry, Entry::NativeQuit) {
        assert_eq!(rig.cx.cx.native_quit_reply(), Some(true));
    }
    assert!(
        rig.cx.cx.platform_quit_requests() > 0,
        "last-window policy and application quit follow acknowledgement"
    );
    if matches!(entry, Entry::RedClose | Entry::CmdW | Entry::MenuClose) {
        assert!(rig.cx.cx.windows().is_empty());
    }
    let _ = std::fs::remove_file(held.persistence.path());
}

#[gpui::test]
fn red_close_waits_beyond_shutdown_cap(cx: &mut TestAppContext) {
    held_native_path(cx, Entry::RedClose);
}
#[gpui::test]
fn cmd_w_waits_beyond_shutdown_cap(cx: &mut TestAppContext) {
    held_native_path(cx, Entry::CmdW);
}
#[gpui::test]
fn menu_close_waits_beyond_shutdown_cap(cx: &mut TestAppContext) {
    held_native_path(cx, Entry::MenuClose);
}
#[gpui::test]
fn cmd_q_waits_beyond_shutdown_cap(cx: &mut TestAppContext) {
    held_native_path(cx, Entry::CmdQ);
}
#[gpui::test]
fn menu_quit_waits_beyond_shutdown_cap(cx: &mut TestAppContext) {
    held_native_path(cx, Entry::MenuQuit);
}
#[gpui::test]
fn native_quit_waits_and_replies_only_after_ack(cx: &mut TestAppContext) {
    held_native_path(cx, Entry::NativeQuit);
}

#[gpui::test]
fn duplicate_close_coalesces_and_quit_escalates_the_same_attempt(cx: &mut TestAppContext) {
    let mut rig = rig(cx, None, 1440.0, 900.0);
    rig.cx.cx.update(super::super::menus::install);
    let close = closing(&mut rig);
    let mut held = held_writer(&mut rig);
    deliver(Entry::RedClose, &mut rig);
    held.entered
        .recv_timeout(Duration::from_secs(1))
        .expect("write entered");
    let first = close.read_with(rig.cx, |close, _| close.phase.clone());
    deliver(Entry::MenuClose, &mut rig);
    deliver(Entry::CmdQ, &mut rig);
    assert_eq!(
        close.read_with(rig.cx, |close, _| close.phase.clone()),
        first
    );
    assert_eq!(
        close.read_with(rig.cx, |close, _| close.target),
        CloseTarget::Application
    );
    close.update(rig.cx, |close, cx| close.cancel(cx));
    release(&mut held);
}

#[gpui::test]
fn cancel_reopens_editing_and_late_ack_never_closes_the_new_edit(cx: &mut TestAppContext) {
    let mut rig = rig(cx, None, 1440.0, 900.0);
    let close = closing(&mut rig);
    let mut held = held_writer(&mut rig);
    deliver(Entry::RedClose, &mut rig);
    held.entered
        .recv_timeout(Duration::from_secs(1))
        .expect("write entered");
    // Escape goes through the focused native close surface, not a direct state helper.
    rig.cx.simulate_keystrokes("escape");
    rig.cx.run_until_parked();
    assert_eq!(
        close.read_with(rig.cx, |close, _| close.phase.clone()),
        Phase::Open
    );
    rig.graph.root.update(rig.cx, |root, cx| {
        root.dispatch(Intent::SetDensity(DensityPreference::Dense), cx)
    });
    release(&mut held);
    crate::runtime::wait::until("new edit ordered after cancelled checkpoint", || {
        rig.cx.run_until_parked();
        held.persistence
            .load()
            .is_ok_and(|state| state.density == crate::model::persistence::PersistedDensity::Dense)
    });
    assert_eq!(rig.cx.cx.windows().len(), 1);
    assert_eq!(
        close.read_with(rig.cx, |close, _| close.phase.clone()),
        Phase::Open
    );
    let _ = std::fs::remove_file(held.persistence.path());
}

#[gpui::test]
fn deadline_keeps_the_window_open_and_native_cancel_replies_no(cx: &mut TestAppContext) {
    let mut rig = rig(cx, None, 1440.0, 900.0);
    let close = closing(&mut rig);
    let mut held = held_writer(&mut rig);
    deliver(Entry::NativeQuit, &mut rig);
    held.entered
        .recv_timeout(Duration::from_secs(1))
        .expect("write entered");
    rig.cx.cx.executor().advance_clock(Duration::from_secs(6));
    rig.draw();
    assert!(close.read_with(rig.cx, |close, _| matches!(
        close.phase,
        Phase::NeedsDecision(_, _)
    )));
    assert_eq!(rig.cx.cx.windows().len(), 1);
    assert!(rig.cx.update(|window, _| {
        window
            .painted_texts()
            .iter()
            .any(|text| text.text.contains("Previously saved state"))
    }));
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
    held.entered
        .recv_timeout(Duration::from_secs(1))
        .expect("last-window checkpoint entered");
    assert!(pending(&close, &mut rig));
    assert_eq!(rig.cx.cx.platform_quit_requests(), 0);
    release(&mut held);
    crate::runtime::wait::until("last-window policy commits after acknowledgement", || {
        rig.cx
            .cx
            .executor()
            .advance_clock(Duration::from_millis(25));
        rig.cx.run_until_parked();
        close.read_with(rig.cx, |close, _| close.phase == Phase::Committed)
    });
    assert!(rig.cx.cx.windows().is_empty());
    assert!(rig.cx.cx.platform_quit_requests() > 0);
    assert!(held.persistence.load().is_ok());
}

#[gpui::test]
fn an_old_close_choice_cannot_approve_or_cancel_a_later_attempt(cx: &mut TestAppContext) {
    let mut rig = rig(cx, None, 1440.0, 900.0);
    let close = closing(&mut rig);
    let mut held = held_writer(&mut rig);
    deliver(Entry::RedClose, &mut rig);
    held.entered
        .recv_timeout(Duration::from_secs(1))
        .expect("first write entered");
    let first = close.read_with(rig.cx, |close, _| match close.phase {
        Phase::Saving(attempt) => attempt,
        _ => panic!("first close saving"),
    });
    close.update(rig.cx, |close, cx| {
        close.choose(first, Choice::ContinueEditing, cx)
    });
    deliver(Entry::RedClose, &mut rig);
    let current = close.read_with(rig.cx, |close, _| close.phase.clone());
    assert_ne!(current, Phase::Saving(first));
    close.update(rig.cx, |close, cx| {
        close.choose(first, Choice::CloseWithPreviousState, cx);
        close.choose(first, Choice::ContinueEditing, cx);
    });
    assert_eq!(
        close.read_with(rig.cx, |close, _| close.phase.clone()),
        current
    );
    assert_eq!(rig.cx.cx.platform_quit_requests(), 0);
    close.update(rig.cx, |close, cx| close.cancel(cx));
    release(&mut held);
}

#[gpui::test]
fn unfinished_workers_keep_one_responsive_finish_job_and_cannot_resume_editing(
    cx: &mut TestAppContext,
) {
    let mut rig = rig(cx, None, 1440.0, 900.0);
    let close = closing(&mut rig);
    let mut held = held_writer(&mut rig);
    let (release_worker, worker_released) = mpsc::channel();
    // A real, finite held thread exercises join ownership independently of the
    // production reader's cancellation implementation, not a fake save ack.
    let worker = std::thread::spawn(move || {
        let _ = worker_released.recv_timeout(Duration::from_secs(20));
    });
    close.update(rig.cx, |close, _| {
        close.extra_finish = Some(crate::runtime::worker_finish::WorkerFinish::from_workers(
            vec![worker],
        ));
    });
    deliver(Entry::NativeQuit, &mut rig);
    held.entered
        .recv_timeout(Duration::from_secs(1))
        .expect("checkpoint entered");
    release(&mut held);
    crate::runtime::wait::until("durable checkpoint enters worker finish", || {
        rig.cx.run_until_parked();
        close.read_with(rig.cx, |close, _| {
            matches!(close.phase, Phase::Finishing(_))
        })
    });
    rig.cx.cx.executor().advance_clock(Duration::from_secs(6));
    rig.draw();
    let attempt = close.read_with(rig.cx, |close, _| match close.phase {
        Phase::WorkerBlocked(attempt) => attempt,
        _ => panic!("worker deadline is visible"),
    });
    assert!(
        held.persistence.load().is_ok(),
        "durable save preceded worker stop"
    );
    assert_eq!(rig.cx.cx.windows().len(), 1);
    assert_eq!(rig.cx.cx.platform_quit_requests(), 0);
    assert_eq!(rig.cx.cx.native_quit_reply(), None);
    assert!(rig.cx.update(|window, _| {
        window
            .painted_texts()
            .iter()
            .any(|text| text.text.contains("background request has not stopped"))
    }));
    rig.cx.simulate_keystrokes("escape");
    close.update(rig.cx, |close, cx| {
        assert!(close.join_task.is_some());
        close.choose(attempt, Choice::WaitAgain, cx);
        close.choose(attempt, Choice::WaitAgain, cx); // coalesced: one monitor, same join job
        assert!(close.join_task.is_some());
        assert!(close.extra_finish.is_none());
        assert_eq!(close.phase, Phase::Finishing(attempt));
    });
    release_worker.send(()).expect("release worker");
    crate::runtime::wait::until("tracked workers finish before native approval", || {
        rig.cx
            .cx
            .executor()
            .advance_clock(Duration::from_millis(25));
        rig.cx.run_until_parked();
        close.read_with(rig.cx, |close, _| close.phase == Phase::Committed)
    });
    assert_eq!(rig.cx.cx.native_quit_reply(), Some(true));
    assert!(close.read_with(rig.cx, |close, _| close.join_task.is_none()
        && close.joined.is_none()));
}

#[gpui::test]
fn long_close_failure_at_200_percent_stays_inside_viewport_and_traps_native_tab(
    cx: &mut TestAppContext,
) {
    let mut rig = rig(cx, None, 1440.0, 900.0);
    let display = rig.shell.read_with(rig.cx, |shell, _| shell.display_key());
    rig.go(Intent::ZoomTo {
        display,
        percent: 200,
    });
    let close = closing(&mut rig);
    let mut held = held_writer(&mut rig);
    deliver(Entry::RedClose, &mut rig);
    held.entered
        .recv_timeout(Duration::from_secs(1))
        .expect("save entered");
    close.update(rig.cx, |close, cx| {
        let Phase::Saving(attempt) = close.phase else {
            panic!("saving attempt");
        };
        close.task = None;
        close.phase = Phase::NeedsDecision(attempt, "CloseError ".repeat(410));
        cx.notify();
    });
    let labels = ["Keep editing", "Try again", "Close anyway"];
    for (width, height) in [(320.0, 568.0), (360.0, 320.0), (640.0, 568.0)] {
        rig.cx.simulate_resize(gpui::size(px(width), px(height)));
        rig.draw();
        rig.cx
            .update(|window, cx| close.read(cx).focus.focus(window, cx));
        let mut focused = Vec::new();
        for _ in 0..4 {
            rig.cx.simulate_keystrokes("tab");
            rig.draw();
            let focus = rig.cx.update(|window, cx| {
                let trap = close.read(cx).focus.clone();
                assert!(
                    trap.contains_focused(window, cx),
                    "native focus never escapes to the underlying shell"
                );
                let mut observed = 0;
                for text in window.painted_texts().iter().filter(|text| {
                    text.text.contains("CloseError") || labels.contains(&text.text.as_ref())
                }) {
                    observed += 1;
                    assert!(text.bounds.origin.x >= px(0.0));
                    assert!(text.bounds.origin.y >= px(0.0));
                    assert!(text.bounds.origin.x + text.bounds.size.width <= px(width));
                    assert!(text.bounds.origin.y + text.bounds.size.height <= px(height));
                }
                assert!(observed > 0, "bounded close text is actually painted");
                window.focused(cx).expect("native control focus")
            });
            focused.push(focus);
        }
        assert_ne!(focused[0], focused[1]);
        assert_ne!(focused[1], focused[2]);
        assert_eq!(
            focused[0], focused[3],
            "Tab cycles the three mounted native decisions"
        );
    }
    rig.cx
        .update(|window, cx| close.read(cx).focus.focus(window, cx));
    rig.cx.simulate_keystrokes("tab");
    rig.native_press("space");
    rig.cx.run_until_parked();
    assert_eq!(
        close.read_with(rig.cx, |close, _| close.phase.clone()),
        Phase::Open,
        "the focused Keep editing button receives native Space activation"
    );
    release(&mut held);
}

#[gpui::test]
fn a_cancelled_attempt_deadline_cannot_fail_its_newer_attempt(cx: &mut TestAppContext) {
    let mut rig = rig(cx, None, 1440.0, 900.0);
    let close = closing(&mut rig);
    let mut held = held_writer(&mut rig);
    deliver(Entry::RedClose, &mut rig);
    held.entered
        .recv_timeout(Duration::from_secs(1))
        .expect("held first checkpoint");
    rig.cx.cx.executor().advance_clock(Duration::from_secs(4));
    close.update(rig.cx, |close, cx| close.cancel(cx));
    deliver(Entry::RedClose, &mut rig);
    let second = close.read_with(rig.cx, |close, _| close.phase.clone());
    assert!(matches!(second, Phase::Saving(_)));
    rig.cx.cx.executor().advance_clock(Duration::from_secs(2));
    rig.draw();
    assert_eq!(
        close.read_with(rig.cx, |close, _| close.phase.clone()),
        second,
        "only the current attempt owns the five-second save deadline"
    );
    assert_eq!(rig.cx.cx.windows().len(), 1);
    close.update(rig.cx, |close, cx| close.cancel(cx));
    release(&mut held);
}

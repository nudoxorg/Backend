//! Actual GPUI paint and native-handle tests for the shared target registry.
//! These component schedules do not supply an index or prove live owner reads.

use super::*;
use gpui::{
    AppContext as _, Context, ParentElement, Render, Styled, TestAppContext, VisualContext as _,
    VisualTestContext,
};

struct Mounted {
    targets: Targets,
    destination: Route,
    source: Option<SymbolRef>,
    shown: bool,
    paint_control: bool,
}

impl Render for Mounted {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.targets.begin();
        let mut view = div().w(px(320.0)).h(px(180.0));
        if self.shown {
            let id: SharedString = "same-row".into();
            let action = TargetAction::new(Rc::new(|_| true), Rc::new(|_, _| {}))
                .with_payload(self.destination.clone());
            self.targets.push(Target {
                id: id.clone(),
                label: "Open item".into(),
                action: action.clone(),
                peek: None,
                source: self.source.clone(),
            });
            let handle = self.targets.native_handle(&id, cx);
            if self.paint_control {
                let button = native_control(
                    id.clone(),
                    "Open item",
                    gpui::Role::Button,
                    Some(handle),
                    action.callback(),
                )
                .w(px(120.0))
                .h(px(36.0))
                .child("Open item");
                view = view.child(self.targets.track(id, button));
            }
        }
        self.targets.finish_native();
        view
    }
}

fn draw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
    });
}

fn mounted(cx: &mut TestAppContext) -> (gpui::Entity<Mounted>, &mut VisualTestContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        let _ = facet::fonts::install(cx);
    });
    cx.add_window_view(|_, _| Mounted {
        targets: Targets::named("native-mount-test"),
        destination: Route::World,
        source: None,
        shown: true,
        paint_control: true,
    })
}

#[gpui::test]
fn mount_receipt_survives_native_repaint_but_not_unmount_or_same_label_payload_change(
    cx: &mut TestAppContext,
) {
    let (view, cx) = mounted(cx);
    draw(cx);
    let targets = view.read_with(cx, |view, _| view.targets.clone());
    let original = targets
        .mount_claim("same-row")
        .expect("actually painted native button");
    let before = targets.hint_frame();
    assert!(cx.update(|window, _| targets.admits_mount(&original, window)));

    view.update(cx, |_, cx| cx.notify());
    draw(cx);
    assert_ne!(targets.hint_frame(), before);
    assert!(
        cx.update(|window, _| targets.admits_mount(&original, window)),
        "a freshly painted frame retains the same original native owner"
    );

    view.update(cx, |view, cx| {
        view.destination = Route::Orbit(crate::navigation::OrbitRoute::Home);
        cx.notify();
    });
    draw(cx);
    assert!(
        !cx.update(|window, _| targets.admits_mount(&original, window)),
        "same key/label/native handle cannot borrow a different typed row payload"
    );
    let changed = targets
        .mount_claim("same-row")
        .expect("replacement payload painted");
    assert!(cx.update(|window, _| targets.admits_mount(&changed, window)));

    view.update(cx, |view, cx| {
        view.shown = false;
        cx.notify();
    });
    draw(cx);
    assert!(!cx.update(|window, _| targets.admits_mount(&changed, window)));
    view.update(cx, |view, cx| {
        view.shown = true;
        cx.notify();
    });
    draw(cx);
    assert!(
        !cx.update(|window, _| targets.admits_mount(&changed, window)),
        "an absent frame retires the old identity even when the exact words return"
    );
    let returned = targets
        .mount_claim("same-row")
        .expect("new physical button painted");
    assert!(cx.update(|window, _| targets.admits_mount(&returned, window)));
}

#[gpui::test]
fn logical_registration_without_native_paint_cannot_upgrade_a_cached_mount_receipt(
    cx: &mut TestAppContext,
) {
    let (view, cx) = mounted(cx);
    draw(cx);
    let targets = view.read_with(cx, |view, _| view.targets.clone());
    let original = targets
        .mount_claim("same-row")
        .expect("first physical native paint");
    view.update(cx, |view, cx| {
        view.paint_control = false;
        cx.notify();
    });
    draw(cx);
    assert!(
        targets
            .placed()
            .iter()
            .any(|(target, _)| target.id == "same-row"),
        "the actual preceding bounds remain cached; they are not mount authority"
    );
    assert!(
        targets.mount_claim("same-row").is_none(),
        "registration alone is not a new native paint"
    );
    assert!(!cx.update(|window, _| targets.admits_mount(&original, window)));
    view.update(cx, |view, cx| {
        view.paint_control = true;
        cx.notify();
    });
    draw(cx);
    assert!(
        !cx.update(|window, _| targets.admits_mount(&original, window)),
        "the new physical mount cannot upgrade the cached origin into current authority"
    );
    let fresh = targets
        .mount_claim("same-row")
        .expect("newly painted control");
    assert!(cx.update(|window, _| targets.admits_mount(&fresh, window)));
}

#[gpui::test]
fn same_label_changed_source_retires_the_original_native_target(cx: &mut TestAppContext) {
    let (view, cx) = mounted(cx);
    view.update(cx, |view, cx| {
        view.source =
            Some(SymbolRef::new("/fixture/app::app.rs:1::first").expect("first declaration"));
        cx.notify();
    });
    draw(cx);
    let targets = view.read_with(cx, |view, _| view.targets.clone());
    let original = targets
        .mount_claim("same-row")
        .expect("first source mounted");
    view.update(cx, |view, cx| {
        view.source =
            Some(SymbolRef::new("/fixture/app::app.rs:2::second").expect("second declaration"));
        cx.notify();
    });
    draw(cx);
    assert!(
        !cx.update(|window, _| targets.admits_mount(&original, window)),
        "same key/label/native owner cannot lend another declaration its source identity"
    );
    let fresh = targets
        .mount_claim("same-row")
        .expect("replacement source painted");
    assert!(cx.update(|window, _| targets.admits_mount(&fresh, window)));
}

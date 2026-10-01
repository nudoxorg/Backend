//! Editing is exercised with the native input's events and actual GPUI timer.
use super::*;
use crate::theme::{Facet, set_facet};
use gpui::{Focusable, Render, TestAppContext, VisualTestContext};
use std::cell::RefCell;

struct Host {
    state: Entity<State>,
}
impl Render for Host {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        Input::new(&self.state.read(cx).input).appearance(false)
    }
}

struct MountedFind {
    state: Entity<State>,
    scroll: ScrollHandle,
    model: Arc<Model>,
    actions: Actions,
}
impl Render for MountedFind {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::probe::draw_started(cx);
        let id: ElementId = "mounted-find".into();
        let measure = Measure::new(px(360.0), &cx.facet());
        div()
            .id("mounted-find-scroll")
            .w(px(360.0))
            .h(px(220.0))
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .child(Find {
                id,
                model: Arc::clone(&self.model),
                actions: self.actions.clone(),
                measure,
                test_state: Some(self.state.clone()),
            })
    }
}

fn draw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.simulate_next_frame(cx);
        window.draw(cx).clear(cx);
    });
}

fn assert_package_visible(cx: &mut VisualTestContext, scroll: &ScrollHandle, name: &str) {
    let viewport = scroll.bounds();
    let ledger = cx.update(|_, cx| crate::probe::take(cx));
    let text = ledger
        .texts
        .iter()
        .find(|text| text.content == name && text.key.contains("candidate-"))
        .expect("selected package row paints natively");
    let box_ = &text.bounds;
    assert!(
        box_.y >= f32::from(viewport.top()) - 0.5
            && box_.y + box_.height <= f32::from(viewport.bottom()) + 0.5,
        "selected {name} must fit the real scroll viewport: {text:?}, {viewport:?}"
    );
}

#[gpui::test]
fn mounted_find_arrows_reveal_offscreen_choice_without_pointer_auto_scroll(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| {
        gpui_component::init(cx);
        set_facet(
            Facet {
                reduced_motion: true,
                ..Facet::default()
            },
            cx,
        );
        crate::probe::enable(cx);
    });
    let reads = Rc::new(RefCell::new(vec![]));
    let opened = Rc::new(RefCell::new(vec![]));
    let scroll = ScrollHandle::new();
    let mut actions = actions(&reads, &opened);
    actions.scroll = scroll.clone();
    let candidates = (0..8)
        .map(|at| Candidate {
            key: format!("package-{at}").into(),
            name: format!("Package {at}").into(),
            version: None,
            indexed: true,
            summary: None,
            facts: vec![],
            answers: vec![],
            offer: None,
        })
        .collect();
    let model = Arc::new(Model {
        query: "package".into(),
        candidates,
        loose: vec![],
        coverage: vec![],
        more_answers: false,
        loading: false,
    });
    let (host, cx) = cx.add_window_view(|window, cx| {
        let state = cx.new(|cx| State::new(model.query.clone(), actions.clone(), window, cx));
        MountedFind {
            state,
            scroll: scroll.clone(),
            model,
            actions,
        }
    });
    draw(cx);
    let state = host.read_with(cx, |host, _| host.state.clone());
    assert_eq!(scroll.offset().y, px(0.0));
    // Pointer selection changes the inspector but does not hijack scrolling.
    state.update(cx, |state, cx| {
        state.selected = Some(Selection::Package("package-7".into()));
        cx.notify();
    });
    draw(cx);
    assert_eq!(scroll.offset().y, px(0.0));
    state.update(cx, |state, cx| {
        state.selected = Some(Selection::Package("package-0".into()));
        cx.notify();
    });
    draw(cx);
    cx.update(|window, cx| {
        state
            .read(cx)
            .input
            .clone()
            .update(cx, |input, cx| input.focus(window, cx))
    });
    for at in 1..8 {
        cx.simulate_keystrokes("down");
        draw(cx);
        draw(cx);
        assert_eq!(
            state.read_with(cx, |state, _| state.selected.clone()),
            Some(Selection::Package(format!("package-{at}").into()))
        );
        assert_package_visible(cx, &scroll, &format!("Package {at}"));
    }
    assert_eq!(
        state.read_with(cx, |state, _| state.selected.clone()),
        Some(Selection::Package("package-7".into()))
    );
    let below = scroll.offset().y;
    assert!(
        below < px(0.0),
        "keyboard-selected package must be revealed by the real scroll container"
    );
    for at in (0..7).rev() {
        cx.simulate_keystrokes("up");
        draw(cx);
        draw(cx);
        assert_eq!(
            state.read_with(cx, |state, _| state.selected.clone()),
            Some(Selection::Package(format!("package-{at}").into()))
        );
        assert_package_visible(cx, &scroll, &format!("Package {at}"));
    }
    assert_eq!(
        state.read_with(cx, |state, _| state.selected.clone()),
        Some(Selection::Package("package-0".into()))
    );
    assert!(
        scroll.offset().y > below,
        "walking back upward must reveal the first row from a pre-scrolled viewport"
    );
}

fn model(query: &str, loading: bool) -> Arc<Model> {
    Arc::new(Model {
        query: query.to_owned().into(),
        candidates: vec![],
        loose: vec![],
        coverage: vec![],
        more_answers: false,
        loading,
    })
}
fn actions(
    reads: &Rc<RefCell<Vec<SharedString>>>,
    opened: &Rc<RefCell<Vec<SharedString>>>,
) -> Actions {
    let reads = Rc::clone(reads);
    let opened = Rc::clone(opened);
    Actions {
        scroll: ScrollHandle::new(),
        initial_held: vec![],
        persist_held: Rc::new(|_, _| {}),
        refine: Rc::new(move |query, _| reads.borrow_mut().push(query)),
        open_symbol: Rc::new(move |key, _, _| opened.borrow_mut().push(key)),
        open_code: Rc::new(|_, _, _| {}),
        open_package: Rc::new(|_, _, _| {}),
        compare: Rc::new(|_, _, _| {}),
        acquire: None,
    }
}
fn edit(state: &Entity<State>, text: &str, cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        let input = state.read(cx).input.clone();
        input.update(cx, |input, cx| {
            input.set_value(text.to_owned(), window, cx);
            // set_value is deliberately quiet; Change is what native typing emits.
            cx.emit(InputEvent::Change);
        });
    });
    cx.run_until_parked();
}
fn advance(cx: &mut VisualTestContext, millis: u64) {
    cx.executor().advance_clock(Duration::from_millis(millis));
    cx.run_until_parked();
}

#[gpui::test]
fn latest_query_wins_clear_cancels_and_enter_never_opens_a_stale_answer(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let reads = Rc::new(RefCell::new(vec![]));
    let opened = Rc::new(RefCell::new(vec![]));
    let callbacks = actions(&reads, &opened);
    let initial = model("from_str", false);
    let (host, cx) = cx.add_window_view(|window, cx| Host {
        state: cx.new(|cx| {
            let mut state = State::new(initial.query.clone(), callbacks.clone(), window, cx);
            state.accept(&initial, &callbacks, window, cx);
            state.selected = Some(Selection::Answer("old::from_str".into()));
            state
        }),
    });
    let state = host.read_with(cx, |host, _| host.state.clone());
    edit(&state, "toml", cx);
    advance(cx, 60);
    edit(&state, "serde", cx);
    advance(cx, 109);
    assert!(reads.borrow().is_empty());
    advance(cx, 1);
    assert_eq!(reads.borrow().as_slice(), &[SharedString::from("serde")]);

    edit(&state, "later", cx);
    edit(&state, "", cx);
    advance(cx, 110);
    assert_eq!(reads.borrow().last().map(AsRef::as_ref), Some(""));
    assert!(!reads.borrow().iter().any(|query| query == "later"));

    edit(&state, "value", cx);
    cx.update(|_, cx| {
        state.read(cx).input.clone().update(cx, |_, cx| {
            cx.emit(InputEvent::PressEnter {
                secondary: false,
                shift: false,
            })
        })
    });
    cx.run_until_parked();
    assert_eq!(reads.borrow().last().map(AsRef::as_ref), Some("value"));
    assert!(
        opened.borrow().is_empty(),
        "pending Enter must submit the edited query"
    );
    let count = reads.borrow().len();
    advance(cx, 200);
    assert_eq!(
        reads.borrow().len(),
        count,
        "Enter cancels the old debounce"
    );
}

#[gpui::test]
fn pending_read_preserves_snapshot_and_input_but_external_back_cancels_dirty_timer(
    cx: &mut TestAppContext,
) {
    cx.update(gpui_component::init);
    let reads = Rc::new(RefCell::new(vec![]));
    let opened = Rc::new(RefCell::new(vec![]));
    let callbacks = actions(&reads, &opened);
    let initial = model("from_str", false);
    let (host, cx) = cx.add_window_view(|window, cx| Host {
        state: cx.new(|cx| {
            let mut state = State::new(initial.query.clone(), callbacks.clone(), window, cx);
            state.accept(&initial, &callbacks, window, cx);
            state
        }),
    });
    let state = host.read_with(cx, |host, _| host.state.clone());
    let input = state.read_with(cx, |state, _| state.input.clone());
    cx.update(|window, cx| input.update(cx, |input, cx| input.focus(window, cx)));
    edit(&state, "toml", cx);
    advance(cx, 110);
    edit(&state, "toml_edit", cx);
    cx.update(|window, cx| {
        state.update(cx, |state, cx| {
            state.accept(&model("toml", true), &callbacks, window, cx)
        })
    });
    state.read_with(cx, |state, cx| {
        assert_eq!(state.input, input, "IME engine survives query admission");
        assert_eq!(state.input.read(cx).value(), "toml_edit");
        assert!(Arc::ptr_eq(state.snapshot.as_ref().unwrap(), &initial));
        assert!(
            state.loaded_query.is_none(),
            "pending snapshot is not an admitted Enter target"
        );
    });
    cx.update(|window, cx| {
        assert!(
            input.read(cx).focus_handle(cx).is_focused(window),
            "pending query preserves native input focus"
        )
    });
    cx.update(|window, cx| {
        state.update(cx, |state, cx| {
            state.accept(&model("Deserialize", false), &callbacks, window, cx)
        })
    });
    cx.run_until_parked();
    let count = reads.borrow().len();
    advance(cx, 200);
    state.read_with(cx, |state, cx| {
        assert_eq!(state.input.read(cx).value(), "Deserialize")
    });
    assert_eq!(
        reads.borrow().len(),
        count,
        "external navigation cancels the dirty local query"
    );
}

/// W-Acquire: adding a release changes its address (the offer's package URL
/// becomes the tree the owner indexed); the selection follows the release.
#[gpui::test]
fn the_selection_follows_a_release_the_library_just_added(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let reads = Rc::new(RefCell::new(vec![]));
    let opened = Rc::new(RefCell::new(vec![]));
    let callbacks = actions(&reads, &opened);
    let offer = |library: Option<&str>| crate::browse::acquire::Offer {
        release: "pkg:cargo/smallvec@1.16.2".into(),
        label: "smallvec 1.16.2".into(),
        place: crate::browse::acquire::Place::Unpacked,
        library: library.map(Into::into),
    };
    let candidate = |key: &str, library: Option<&str>| Candidate {
        key: key.into(),
        name: "smallvec".into(),
        version: Some("1.16.2".into()),
        indexed: library.is_some(),
        summary: None,
        facts: vec![],
        answers: vec![],
        offer: Some(offer(library)),
    };
    let older = Candidate {
        key: "/cache/smallvec-1.16.0".into(),
        name: "smallvec".into(),
        version: Some("1.16.0".into()),
        indexed: true,
        summary: None,
        facts: vec![],
        answers: vec![],
        offer: None,
    };
    let before = Arc::new(Model {
        query: "smallvec".into(),
        candidates: vec![candidate("pkg:cargo/smallvec@1.16.2", None), older.clone()],
        loose: vec![],
        coverage: vec![],
        more_answers: false,
        loading: false,
    });
    let after = Arc::new(Model {
        query: "smallvec".into(),
        candidates: vec![
            older,
            candidate("/cache/smallvec-1.16.2", Some("/cache/smallvec-1.16.2")),
        ],
        loose: vec![],
        coverage: vec![],
        more_answers: false,
        loading: false,
    });
    let (host, cx) = cx.add_window_view(|window, cx| Host {
        state: cx.new(|cx| {
            let mut state = State::new(before.query.clone(), callbacks.clone(), window, cx);
            state.accept(&before, &callbacks, window, cx);
            state.selected = Some(Selection::Package("pkg:cargo/smallvec@1.16.2".into()));
            state
        }),
    });
    let state = host.read_with(cx, |host, _| host.state.clone());
    cx.update(|window, cx| {
        state.update(cx, |state, cx| state.accept(&after, &callbacks, window, cx))
    });
    assert_eq!(
        state.read_with(cx, |state, _| state.selected.clone()),
        Some(Selection::Package("/cache/smallvec-1.16.2".into())),
        "the added release stays selected at its new address, not the first candidate"
    );
}

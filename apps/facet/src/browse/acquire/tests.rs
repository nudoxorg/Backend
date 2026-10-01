//! The add control says what the shell says, in words a person reads, and
//! only an offer whose source is on this machine can be added.

#![allow(clippy::expect_used)]

use super::*;
use crate::theme::{Facet, set_facet};
use gpui::{Context, Modifiers, Render, TestAppContext, VisualTestContext, point, px};
use std::cell::RefCell;

struct Host {
    offer: Offer,
    actions: AddActions,
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::probe::draw_started(cx);
        let measure = Measure::new(px(480.0), &cx.facet());
        div().w(px(480.0)).child(add_control(
            "offer",
            &self.offer,
            &self.actions,
            &measure,
            cx,
        ))
    }
}

fn offer(place: Place) -> Offer {
    Offer {
        release: "pkg:cargo/anyhow@1.0.104".into(),
        label: "anyhow 1.0.104".into(),
        place,
        library: None,
    }
}

/// Draws `offer` in `state`; returns every painted text and what `add` got.
fn drawn(
    offer: Offer,
    state: Adding,
    click: Option<&str>,
    cx: &mut TestAppContext,
) -> (Vec<(String, String)>, Vec<SharedString>) {
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
    let added = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&added);
    let actions = AddActions {
        state: Rc::new(move |_, _| state.clone()),
        add: Rc::new(move |release, _, _| sink.borrow_mut().push(release)),
        open: Rc::new(|_, _, _| {}),
    };
    let (_host, cx): (_, &mut VisualTestContext) =
        cx.add_window_view(|_, _| Host { offer, actions });
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.simulate_next_frame(cx);
        window.draw(cx).clear(cx);
    });
    let ledger = cx.update(|_, cx| crate::probe::take(cx));
    let texts = ledger
        .texts
        .iter()
        .map(|text| (text.key.clone(), text.content.clone()))
        .collect::<Vec<_>>();
    if let Some(part) = click {
        let keys = ledger
            .targets
            .iter()
            .map(|target| target.key.clone())
            .collect::<Vec<_>>();
        let target = ledger
            .targets
            .iter()
            .find(|target| target.key.contains(part))
            .unwrap_or_else(|| panic!("no `{part}` target among {keys:?}"));
        let at = point(
            px(target.bounds.x + target.bounds.width / 2.0),
            px(target.bounds.y + target.bounds.height / 2.0),
        );
        cx.simulate_click(at, Modifiers::none());
        cx.run_until_parked();
    }
    let added = added.borrow().clone();
    (texts, added)
}

fn said(texts: &[(String, String)]) -> Vec<&str> {
    texts.iter().map(|(_, content)| content.as_str()).collect()
}

#[gpui::test]
fn an_offer_on_this_machine_is_added_by_its_button(cx: &mut TestAppContext) {
    let (texts, added) = drawn(offer(Place::Unpacked), Adding::Idle, Some("add"), cx);
    assert!(
        said(&texts).contains(&"anyhow 1.0.104 · on this machine"),
        "{texts:?}"
    );
    assert_eq!(
        added,
        [SharedString::from("pkg:cargo/anyhow@1.0.104")],
        "the click adds the release it offers"
    );
}

#[gpui::test]
fn an_offer_that_needs_a_download_says_so_and_cannot_be_added(cx: &mut TestAppContext) {
    let (texts, added) = drawn(offer(Place::Download), Adding::Idle, Some("add"), cx);
    assert!(
        said(&texts).contains(&"anyhow 1.0.104 · needs a download"),
        "{texts:?}"
    );
    assert!(
        added.is_empty(),
        "nothing is fetched: the button is disabled"
    );
}

#[gpui::test]
fn adding_says_the_step_it_is_on(cx: &mut TestAppContext) {
    let (texts, _) = drawn(
        offer(Place::Archive),
        Adding::Working(Step::Unpacking),
        None,
        cx,
    );
    assert!(
        said(&texts).contains(&"Unpacking its archive · anyhow 1.0.104"),
        "{texts:?}"
    );
    assert!(
        !said(&texts).contains(&"Add to library"),
        "no second add while one runs"
    );
    let unpacking = seam_stages(Place::Archive, Step::Unpacking);
    assert_eq!(
        unpacking
            .iter()
            .map(|stage| stage.state)
            .collect::<Vec<_>>(),
        [StageState::Done, StageState::Now, StageState::Todo]
    );
    assert_eq!(
        seam_stages(Place::Unpacked, Step::Indexing).len(),
        2,
        "no unpack step when cargo unpacked it"
    );
}

#[gpui::test]
fn added_says_it_is_in_the_library(cx: &mut TestAppContext) {
    let (texts, _) = drawn(
        offer(Place::Unpacked),
        Adding::Added {
            open: "/cache/anyhow-1.0.104".into(),
        },
        None,
        cx,
    );
    assert!(
        said(&texts).contains(&"anyhow 1.0.104 is in your library"),
        "{texts:?}"
    );
}

#[gpui::test]
fn a_failure_says_why_in_the_owners_words_and_can_be_tried_again(cx: &mut TestAppContext) {
    let (texts, added) = drawn(
        offer(Place::Unpacked),
        Adding::Failed("the index refused anyhow 1.0.104: no compiler".into()),
        Some("retry"),
        cx,
    );
    assert!(
        said(&texts).contains(&"anyhow 1.0.104 was not added"),
        "{texts:?}"
    );
    assert!(
        said(&texts).contains(&"the index refused anyhow 1.0.104: no compiler"),
        "the owner's words are shown"
    );
    assert_eq!(added.len(), 1, "trying again adds it again");
}

#[gpui::test]
fn an_offer_already_in_the_library_opens_instead(cx: &mut TestAppContext) {
    let mut offer = offer(Place::Unpacked);
    offer.library = Some("/cache/anyhow-1.0.104".into());
    let (texts, _) = drawn(offer, Adding::Idle, None, cx);
    assert!(
        said(&texts).contains(&"anyhow 1.0.104 is in your library"),
        "{texts:?}"
    );
    assert!(!said(&texts).contains(&"Add to library"));
}

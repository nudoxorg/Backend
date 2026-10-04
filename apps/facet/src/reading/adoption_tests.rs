//! The actual folio, peek and failure renderers publish their painted words.
//! Native TestSupport fixtures do not prove live-owner or platform acceptance.
use super::{ControlKind, Intent};
use crate::folio::cards::{CardFacts, symbol_card};
use crate::folio::module::module;
use crate::folio::state::Use;
use crate::graph::World;
use crate::marks::badges::{Badge, Glyph, Ink};
use crate::overlay::peek::{self, Peek, SymbolPeek};
use crate::overlay::text::Sig;
use crate::semantics::fails::Section;
use crate::{ActiveFacet as _, Measure};
use gpui::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, Render, StatefulInteractiveElement as _, Styled as _,
    TestAppContext, VisualTestContext, Window, div, px,
};
use std::{cell::Cell, rc::Rc, sync::Arc, time::Duration};

type TestResult<T = ()> = Result<T, &'static str>;

enum Scene {
    Card(Rc<CardFacts>),
    ControlledModule(Rc<CardFacts>),
    Peek(Peek),
    Failure(Section),
}

struct Page {
    scene: Scene,
    owner: u32,
    covered: bool,
    calls: Rc<Cell<usize>>,
}

impl Render for Page {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let measure = Measure::new(px(720.), &cx.facet());
        let owner = gpui::ElementId::Name(format!("adopted-reading-owner-{}", self.owner).into());
        let content = match &self.scene {
            Scene::Card(facts) => symbol_card(owner.clone(), facts.clone(), &measure)
                .width(px(300.))
                .into_any_element(),
            Scene::ControlledModule(facts) => {
                let words = facts.name.clone();
                let calls = self.calls.clone();
                let scope = owner.clone();
                module(
                    owner.clone(),
                    "current-package",
                    "current-module",
                    vec![facts.clone()],
                    &measure,
                )
                .card_name_intent(Intent::NameOfExistingControl)
                .wrap(move |index, card| {
                    let calls = calls.clone();
                    let id = gpui::ElementId::NamedChild(
                        Arc::new(scope.clone()),
                        format!("existing-control-{index}").into(),
                    );
                    super::control(words.clone(), ControlKind::Button, div().id(id).child(card))
                        .on_click(move |_, _, _| calls.set(calls.get() + 1))
                        .into_any_element()
                })
                .into_any_element()
            }
            Scene::Peek(peek) => div()
                .id(owner.clone())
                .child(peek::card(peek, &measure, window, cx))
                .into_any_element(),
            Scene::Failure(section) => crate::anatomy::fails::fails(
                owner.clone(),
                section.clone(),
                Arc::new(World::new(vec![], vec![], vec![], vec![]).expect("empty observed world")),
                &measure,
                &crate::anatomy::text::Links::plain(),
            )
            .into_any_element(),
        };
        let content = if self.covered {
            gpui::inert(owner, "The current surface owns input", content).into_any_element()
        } else {
            content
        };
        div().size_full().child(content)
    }
}

fn init(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::fonts::install(cx).expect("production font cuts install");
        crate::set_facet(
            crate::Facet {
                reduced_motion: true,
                ..crate::Facet::default()
            },
            cx,
        );
        cx.set_global(gpui::TextTrace);
    });
}

fn mount(cx: &mut TestAppContext, scene: Scene) -> (gpui::Entity<Page>, &mut VisualTestContext) {
    cx.add_window_view(|window, _| {
        window.set_a11y_forced(true);
        Page {
            scene,
            owner: 1,
            covered: false,
            calls: Rc::new(Cell::new(0)),
        }
    })
}

fn draw(cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
    });
}

fn nodes(cx: &mut VisualTestContext) -> Vec<(gpui::accesskit::NodeId, gpui::accesskit::Node)> {
    cx.update(|window, _| {
        window
            .a11y_tree()
            .expect("forced native tree")
            .nodes
            .clone()
    })
}

fn assert_reading(cx: &mut VisualTestContext, role: gpui::Role, words: &str) {
    let found = nodes(cx);
    let matching: Vec<_> = found
        .iter()
        .filter(|(_, node)| node.role() == role && node.label() == Some(words))
        .collect();
    assert_eq!(matching.len(), 1, "one current reading node for {words}");
    for action in [
        gpui::AccessibleAction::SetValue,
        gpui::AccessibleAction::Click,
        gpui::AccessibleAction::Focus,
    ] {
        assert!(!matching[0].1.supports_action(action));
    }
    assert!(
        cx.update(|window, _| window
            .painted_texts()
            .iter()
            .any(|run| run.text.as_ref() == words && run.alpha > 0.)),
        "native words must also paint"
    );
}

fn click(cx: &mut VisualTestContext, id: gpui::accesskit::NodeId) {
    cx.update(|window, cx| {
        window.simulate_a11y_action(
            gpui::accesskit::ActionRequest {
                action: gpui::AccessibleAction::Click,
                target_tree: gpui::accesskit::TreeId::ROOT,
                target_node: id,
                data: None,
            },
            cx,
        )
    });
}

fn facts() -> Rc<CardFacts> {
    Rc::new(CardFacts {
        name: "état_🧭".into(),
        kind: crate::icons::Kind::Variable,
        word: "variable".into(),
        doc: Some(
            format!(
                "{} HIDDEN_CARD_SOURCE_TAIL",
                "Current observed prose café 日本語. ".repeat(40)
            )
            .into(),
        ),
        badges: vec![Badge {
            glyph: Glyph::Unsafe,
            word: "unsafe".into(),
            tip: "Captured declaration is unsafe.".into(),
            ink: Ink::Amber,
        }],
        yours: Use::Elsewhere,
        change: None,
    })
}

#[gpui::test]
fn adopted_folio_card_words_are_readonly_and_the_summary_follows_its_real_clamp(
    cx: &mut TestAppContext,
) {
    let result = check_adopted_folio_card_words_are_readonly_and_the_summary_follows_its_real_clamp(cx);
    assert!(result.is_ok(), "fixture failed: {result:?}");
}

fn check_adopted_folio_card_words_are_readonly_and_the_summary_follows_its_real_clamp(
    cx: &mut TestAppContext,
) -> TestResult {
    init(cx);
    let (_page, cx) = mount(cx, Scene::Card(facts()));
    draw(cx);
    assert_reading(cx, gpui::Role::Heading, "état_🧭");
    assert_reading(cx, gpui::Role::Label, "variable");
    assert_reading(cx, gpui::Role::Label, "unsafe");
    let current = nodes(cx);
    let summaries: Vec<_> = current
        .iter()
        .filter(|(_, node)| {
            node.label()
                .is_some_and(|words| words.starts_with("Current observed prose"))
        })
        .collect();
    assert_eq!(summaries.len(), 1);
    let words = summaries[0].1.label().ok_or("current clamped folio summary")?;
    assert!(!words.contains("HIDDEN_CARD_SOURCE_TAIL"));
    assert!(cx.update(|window, _| {
        window
            .painted_texts()
            .iter()
            .any(|run| run.text.as_ref() == words)
    }));
    assert!(
        !summaries[0]
            .1
            .supports_action(gpui::AccessibleAction::SetValue)
    );
    Ok(())
}

#[gpui::test]
fn adopted_module_card_name_stays_on_its_existing_control_through_cover_and_owner_key_change(
    cx: &mut TestAppContext,
) {
    let result = check_adopted_module_card_name_stays_on_its_existing_control_through_cover_and_owner_key_change(cx);
    assert!(result.is_ok(), "fixture failed: {result:?}");
}

fn check_adopted_module_card_name_stays_on_its_existing_control_through_cover_and_owner_key_change(
    cx: &mut TestAppContext,
) -> TestResult {
    init(cx);
    let (page, cx) = mount(cx, Scene::ControlledModule(facts()));
    draw(cx);
    let current = nodes(cx);
    let named: Vec<_> = current
        .iter()
        .filter(|(_, node)| node.label() == Some("état_🧭"))
        .collect();
    assert_eq!(
        named.len(),
        1,
        "the card's native control is the only owner of its name"
    );
    let (old_id, control) = named[0];
    assert_eq!(control.role(), gpui::Role::Button);
    assert!(!control.supports_action(gpui::AccessibleAction::SetValue));
    click(cx, *old_id);
    let calls = page.read_with(cx, |page, _| page.calls.clone());
    assert_eq!(calls.get(), 1);
    page.update(cx, |page, cx| {
        page.covered = true;
        cx.notify();
    });
    draw(cx);
    let covered = nodes(cx);
    let (_, control) = covered
        .iter()
        .find(|(_, node)| node.label() == Some("état_🧭"))
        .ok_or("covered module card control")?;
    assert!(!control.supports_action(gpui::AccessibleAction::Click));
    click(cx, *old_id);
    assert_eq!(calls.get(), 1);
    page.update(cx, |page, cx| {
        page.owner = 2;
        page.covered = false;
        cx.notify();
    });
    draw(cx);
    cx.executor().advance_clock(Duration::from_millis(400));
    cx.run_until_parked();
    draw(cx);
    let next = nodes(cx);
    let (current_id, _) = next
        .iter()
        .find(|(_, node)| node.label() == Some("état_🧭"))
        .ok_or("replacement module card control")?;
    assert_ne!(
        current_id, old_id,
        "existing qualified host keys distinguish mounted identities"
    );
    click(cx, *current_id);
    assert_eq!(calls.get(), 2);
    Ok(())
}

#[gpui::test]
fn adopted_peek_reads_its_actual_prose_and_unsplit_signature_without_inventing_link_actions(
    cx: &mut TestAppContext,
) {
    init(cx);
    let peek = Peek::Symbol(SymbolPeek {
        name: "état_🧭".into(),
        place: "enum in `present::module`".into(),
        signature: Some(Sig::new().kw("pub").p(" ").val("état_🧭")),
        sentence: Some("A *current* `observed` paragraph.".into()),
        uses: Some(7),
        ..SymbolPeek::default()
    });
    let (_page, cx) = mount(cx, Scene::Peek(peek));
    draw(cx);
    assert_reading(cx, gpui::Role::Heading, "état_🧭");
    assert_reading(cx, gpui::Role::Label, "enum in present::module");
    assert_reading(cx, gpui::Role::Label, "pub état_🧭");
    assert_reading(cx, gpui::Role::Label, "A current observed paragraph.");
    assert_reading(cx, gpui::Role::Label, "7");
    assert!(
        !nodes(cx)
            .iter()
            .any(|(_, node)| node.role() == gpui::Role::Link),
        "hover affordances do not invent native navigation actions"
    );
}

#[gpui::test]
fn adopted_failure_footer_reads_the_observed_value_without_becoming_editable(
    cx: &mut TestAppContext,
) {
    init(cx);
    let section = Section {
        rows: vec![],
        can: 7,
        told: 0,
        column: 1,
    };
    let expected = section.foot();
    let (_page, cx) = mount(cx, Scene::Failure(section));
    draw(cx);
    assert_reading(cx, gpui::Role::Label, &expected);
}

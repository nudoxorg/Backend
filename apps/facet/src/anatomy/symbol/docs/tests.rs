//! Mounted document semantics, using actual page compilation and native draw.
use super::{TextKind, text};
use crate::anatomy::symbol::facts::Site;
use crate::anatomy::symbol::view::{Block, Kind, Lang, Uses};
use crate::anatomy::symbol::{self, Chrome, Facts, Fixed, Ui, View};
use crate::{ActiveFacet as _, Facet, Measure, set_facet};
use gpui::{
    AppContext as _, Context, Element as _, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, StatefulInteractiveElement as _, Styled as _, TestAppContext,
    VisualTestContext, Window, div, px,
};

/// A retained painted copy uses the desktop host's actual semantic boundary:
/// accessibility suppression in every phase, with inert native registration.
/// This fixture tests that projection, not physical occlusion by a real modal.
struct PaintedCopy {
    child: gpui::AnyElement,
}

impl IntoElement for PaintedCopy {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl gpui::Element for PaintedCopy {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<gpui::ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut gpui::App,
    ) -> (gpui::LayoutId, ()) {
        (
            window.with_a11y_suppressed(|window| self.child.request_layout(window, cx)),
            (),
        )
    }

    fn prepaint(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        _: gpui::Bounds<gpui::Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut gpui::App,
    ) {
        window.with_a11y_suppressed(|window| self.child.prepaint(window, cx));
    }

    fn paint(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        _: gpui::Bounds<gpui::Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut gpui::App,
    ) {
        window.with_a11y_suppressed(|window| self.child.paint(window, cx));
    }
}

fn painted_copy(id: &'static str, child: impl IntoElement) -> gpui::AnyElement {
    PaintedCopy {
        child: gpui::inert(id, "The live surface owns interaction", child).into_any_element(),
    }
    .into_any_element()
}

struct Document {
    view: View,
    uses: Uses,
    ui: Ui,
    covered: bool,
    width: f32,
}
impl Render for Document {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let facet = cx.facet();
        let measure = Measure::new(px(self.width), &facet);
        let title = text(
            "document-title",
            self.view.head.name.clone(),
            TextKind::Heading,
            |words| div().child(words),
        )
        .into_any_element();
        let page = symbol::page(
            &self.view,
            &self.uses,
            Chrome {
                gem: symbol::gem(&self.view, &measure, facet.palette()),
                title,
            },
            &measure,
            facet.palette(),
            &Fixed::new(self.ui.clone()).with_open([symbol::key::FoldKey::Example(0)]),
            &crate::fluid::Modes::new(),
        );
        let page = if self.covered {
            painted_copy("covered-document", page)
        } else {
            page
        };
        let _ = window;
        div()
            .id("document-scroll")
            .size_full()
            .overflow_y_scroll()
            .child(page)
    }
}

fn tree(cx: &mut VisualTestContext) -> serde_json::Value {
    cx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
        serde_json::from_str(
            &window
                .debug_a11y_tree_json()
                .expect("forced native document tree"),
        )
        .expect("native tree JSON")
    })
}
fn native_node(
    cx: &mut VisualTestContext,
    role: gpui::Role,
    words: &str,
) -> gpui::accesskit::NodeId {
    cx.update(|window, _| {
        window
            .a11y_tree()
            .expect("committed native tree")
            .nodes
            .iter()
            .find(|(_, node)| node.role() == role && node.label() == Some(words))
            .map(|(id, _)| *id)
            .expect("actual mounted semantic node")
    })
}
fn accessible_click(cx: &mut VisualTestContext, node: gpui::accesskit::NodeId) {
    cx.update(|window, cx| {
        window.simulate_a11y_action(
            gpui::accesskit::ActionRequest {
                action: gpui::AccessibleAction::Click,
                target_tree: gpui::accesskit::TreeId::ROOT,
                target_node: node,
                data: None,
            },
            cx,
        )
    });
}
fn has(tree: &serde_json::Value, role: &str, words: &str) -> bool {
    tree["nodes"]
        .as_object()
        .expect("native nodes")
        .values()
        .any(|node| {
            node["aria"]["role"].as_str() == Some(role)
                && node["aria"]["label"].as_str() == Some(words)
        })
}

#[gpui::test]
fn compiled_symbol_kinds_and_languages_expose_the_painted_words_at_large_text(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| {
        gpui_component::init(cx);
        let _ = crate::fonts::install(cx);
        set_facet(
            Facet {
                reduced_motion: true,
                ..Facet::default()
            },
            cx,
        );
    });
    let facts = Facts::new("initial", Kind::Other, Lang::Rust, "current-package");
    let (document, cx) = cx.add_window_view(|window, _| {
        window.set_a11y_forced(true);
        Document {
            view: symbol::compile(&facts),
            uses: Uses::default(),
            ui: Ui::default(),
            covered: false,
            width: 1440.,
        }
    });
    cx.simulate_resize(gpui::size(px(1800.), px(2400.)));
    let title = "état_🧭_current".repeat(6);
    let prose = "Current observed tick — café, Ελληνικά, 日本語.";
    let code = "let état = signal.tick.saturating_add(1);";
    let kinds = [
        Kind::Function,
        Kind::Method,
        Kind::Enum,
        Kind::Struct,
        Kind::Trait,
        Kind::Alias,
        Kind::Constant,
        Kind::Module,
        Kind::Other,
        Kind::Variable,
    ];
    let languages = [
        Lang::Rust,
        Lang::Python,
        Lang::JavaScript,
        Lang::TypeScript,
        Lang::Go,
        Lang::Java,
        Lang::CSharp,
        Lang::C,
        Lang::Cpp,
        Lang::Other,
    ];
    for scale in [1.0, 2.0] {
        cx.update(|_, cx| {
            set_facet(
                Facet {
                    text_scale: scale,
                    reduced_motion: true,
                    ..Facet::default()
                },
                cx,
            )
        });
        for lang in languages {
            for kind in kinds {
                let mut facts = Facts::new(&title, kind, lang, "current-package");
                facts.docs = vec![
                    Block::Para(prose.into()),
                    Block::Head("Example".into()),
                    Block::Code(code.into()),
                ];
                facts.site = Some(Site {
                    file: "src/cadence.rs".into(),
                    line: 9,
                    open: Some("/current/src/cadence.rs".into()),
                });
                document.update(cx, |document, cx| {
                    document.view = symbol::compile(&facts);
                    cx.notify();
                });
                let native = tree(cx);
                assert!(
                    has(&native, "Heading", &title),
                    "{kind:?}/{lang:?}/{scale}: current title"
                );
                assert!(
                    has(&native, "Label", prose),
                    "{kind:?}/{lang:?}/{scale}: authored lede"
                );
                assert!(
                    has(&native, "Label", "Reference information has not been read."),
                    "missing reference evidence is not a zero-users claim"
                );
                assert!(has(&native, "Heading", "IN YOUR WORKSPACE"));
                assert!(
                    has(&native, "Link", "src/cadence.rs:9"),
                    "actual source endpoint"
                );
                assert!(has(&native, "Button", "Example"), "actual disclosure label");
                assert!(
                    has(&native, "Label", code),
                    "the open example paints its code"
                );
                assert!(
                    !has(&native, "Heading", "initial"),
                    "a prior page cannot remain in the native tree"
                );
                document.update(cx, |document, cx| {
                    document.covered = true;
                    cx.notify();
                });
                let native = tree(cx);
                assert!(
                    !has(&native, "Heading", &title),
                    "covered pages have no semantic copy"
                );
                assert!(!has(&native, "Label", prose));
                document.update(cx, |document, cx| {
                    document.covered = false;
                    cx.notify();
                });
                assert!(
                    has(&tree(cx), "Heading", &title),
                    "release restores the actual document"
                );
            }
        }
    }
}

/// Projection test: pixels may remain during a transition without supplying
/// a second accessible document. Real desktop modal coverage is a later gate.
#[gpui::test]
fn retained_document_keeps_its_painted_words_without_a_semantic_copy(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        let _ = crate::fonts::install(cx);
        cx.set_global(gpui::TextTrace);
        set_facet(
            Facet {
                reduced_motion: true,
                ..Facet::default()
            },
            cx,
        );
    });
    let prose = "A retained document still paints these owned words.";
    let mut facts = Facts::new("retained_tick", Kind::Constant, Lang::Rust, "signal");
    facts.docs = vec![Block::Para(prose.into())];
    let (document, cx) = cx.add_window_view(|window, _| {
        window.set_a11y_forced(true);
        Document {
            view: symbol::compile(&facts),
            uses: Uses::default(),
            ui: Ui::default(),
            covered: true,
            width: 900.,
        }
    });
    cx.simulate_resize(gpui::size(px(900.), px(1200.)));
    let native = tree(cx);
    assert!(!has(&native, "Heading", "retained_tick"));
    assert!(!has(&native, "Label", prose));
    assert!(
        cx.update(|window, _| window
            .painted_texts()
            .iter()
            .any(|run| run.text == prose && run.alpha > 0.)),
        "suppressed semantics do not remove the retained pixels"
    );
    document.update(cx, |document, cx| {
        document.covered = false;
        cx.notify();
    });
    let native = tree(cx);
    assert!(has(&native, "Heading", "retained_tick"));
    assert!(
        has(&native, "Label", prose),
        "the live projection restores the owned document"
    );
}

#[test]
fn popup_openers_admit_touch_and_keyboard_but_never_repeat_mouse_release() {
    use super::Activation;
    let mouse = gpui::ClickEvent::Mouse(gpui::MouseClickEvent::default());
    let touch = gpui::ClickEvent::Touch(gpui::TouchClickEvent::default());
    let keyboard = gpui::ClickEvent::default();
    for event in [&mouse, &touch, &keyboard] {
        assert!(Activation::Release.admits_click(event));
    }
    assert!(!Activation::PointerDown.admits_click(&mouse));
    assert!(Activation::PointerDown.admits_click(&touch));
    assert!(Activation::PointerDown.admits_click(&keyboard));
}

struct NativeControl {
    focus: gpui::FocusHandle,
    calls: std::rc::Rc<std::cell::Cell<usize>>,
    covered: bool,
}
impl Render for NativeControl {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let calls = self.calls.clone();
        let act: super::super::host::Act = std::rc::Rc::new(move |_, _| calls.set(calls.get() + 1));
        let leaf = super::control(
            "all 2 packages",
            super::ControlKind::Filter(false),
            div()
                .id("package-picker")
                .w(px(180.))
                .h(px(40.))
                .child("all 2 packages"),
        );
        let leaf = super::Activation::PointerDown.pointer_down(leaf, act.clone());
        let leaf = crate::controls::button::native_button_with_event(
            leaf,
            &self.focus,
            move |event, window, cx| {
                if super::Activation::PointerDown.admits_click(event) {
                    act(window, cx);
                }
            },
        );
        if self.covered {
            painted_copy("covered-control", leaf)
        } else {
            leaf.into_any_element()
        }
    }
}

#[gpui::test]
fn native_opener_counts_one_press_and_clean_keyboard_release(cx: &mut TestAppContext) {
    let calls = std::rc::Rc::new(std::cell::Cell::new(0));
    let (control, cx) = cx.add_window_view(|window, cx| {
        window.set_a11y_forced(true);
        NativeControl {
            focus: cx.focus_handle(),
            calls: calls.clone(),
            covered: false,
        }
    });
    assert!(has(&tree(cx), "Button", "all 2 packages"));
    let at = gpui::point(px(20.), px(20.));
    cx.simulate_mouse_down(at, gpui::MouseButton::Left, gpui::Modifiers::none());
    assert_eq!(calls.get(), 1, "pointer down opens once");
    cx.simulate_event(gpui::MouseUpEvent {
        position: at,
        button: gpui::MouseButton::Left,
        modifiers: gpui::Modifiers::none(),
        click_count: 1,
    });
    assert_eq!(
        calls.get(),
        1,
        "mouse release cannot reopen/toggle the popup"
    );
    cx.update(|window, cx| {
        let focus = control.read(cx).focus.clone();
        window.focus(&focus, cx);
    });
    tree(cx);
    for (stroke, expected) in [("enter", 2), ("space", 3)] {
        crate::test_input::native_press(cx, stroke);
        assert_eq!(calls.get(), expected, "native {stroke} releases once");
    }
    let node = native_node(cx, gpui::Role::Button, "all 2 packages");
    accessible_click(cx, node);
    assert_eq!(
        calls.get(),
        4,
        "native AX opens once without mouse synthesis"
    );
    control.update(cx, |control, cx| {
        control.covered = true;
        cx.notify();
    });
    assert!(!has(&tree(cx), "Button", "all 2 packages"));
    cx.simulate_click(at, gpui::Modifiers::none());
    for key in ["enter", "space"] {
        crate::test_input::native_press(cx, key);
    }
    accessible_click(cx, node);
    assert_eq!(
        calls.get(),
        4,
        "actual native inert scope denies all input including retained AX"
    );
}

struct Paragraph {
    words: gpui::SharedString,
    destination: gpui::SharedString,
    calls: std::rc::Rc<std::cell::Cell<usize>>,
    covered: bool,
}
impl Render for Paragraph {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let calls = self.calls.clone();
        let body = super::rich_text(
            "current-paragraph",
            self.words.clone(),
            vec![],
            vec![super::Link {
                range: 0..self.words.len(),
                destination: self.destination.clone(),
                activate: std::rc::Rc::new(move |_, _| calls.set(calls.get() + 1)),
                admission: None,
            }],
        );
        let body = div().w(px(260.)).text_size(px(24.)).child(body);
        if self.covered {
            painted_copy("covered-prose", body)
        } else {
            body.into_any_element()
        }
    }
}
#[gpui::test]
fn wrapped_unicode_link_uses_painted_body_native_focus_and_new_destination_identity(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| {
        let _ = crate::fonts::install(cx);
        cx.set_global(gpui::TextTrace);
    });
    let calls = std::rc::Rc::new(std::cell::Cell::new(0));
    let words: gpui::SharedString =
        "café Ελληνικά 日本語 🧭 current observed tick documentation wraps onto several lines"
            .into();
    let (paragraph, cx) = cx.add_window_view(|window, _| {
        window.set_a11y_forced(true);
        Paragraph {
            words: words.clone(),
            destination: "https://docs.rs/current".into(),
            calls: calls.clone(),
            covered: false,
        }
    });
    assert!(has(&tree(cx), "Link", &words));
    assert!(cx.update(|window, _| {
        window
            .painted_texts()
            .iter()
            .any(|run| run.text == words && run.alpha > 0.)
    }));
    cx.simulate_click(gpui::point(px(8.), px(12.)), gpui::Modifiers::none());
    assert_eq!(
        calls.get(),
        1,
        "actual glyph pointer admission activates once"
    );
    cx.update(|window, cx| window.focus_next(cx));
    let native = tree(cx);
    let focus = native["gpui_focus"]
        .as_str()
        .expect("mounted link is a native tab stop");
    assert_eq!(native["nodes"][focus]["aria"]["role"], "Link");
    for key in ["enter", "space"] {
        crate::test_input::native_press(cx, key);
    }
    assert_eq!(
        calls.get(),
        3,
        "clean native link key releases activate once each"
    );
    let old_node = native_node(cx, gpui::Role::Link, &words);
    accessible_click(cx, old_node);
    assert_eq!(
        calls.get(),
        4,
        "native AX link activates once without pointer synthesis"
    );
    paragraph.update(cx, |paragraph, cx| {
        paragraph.destination = "https://docs.rs/new-page".into();
        cx.notify();
    });
    tree(cx);
    crate::test_input::native_press(cx, "enter");
    accessible_click(cx, old_node);
    assert_eq!(
        calls.get(),
        4,
        "a new destination cannot inherit the old link's focus"
    );
    let current_node = native_node(cx, gpui::Role::Link, &words);
    paragraph.update(cx, |paragraph, cx| {
        paragraph.covered = true;
        cx.notify();
    });
    assert!(!has(&tree(cx), "Link", &words));
    cx.simulate_click(gpui::point(px(8.), px(12.)), gpui::Modifiers::none());
    crate::test_input::native_press(cx, "space");
    accessible_click(cx, current_node);
    assert_eq!(
        calls.get(),
        4,
        "covered prose has no native or glyph activation"
    );
}

#[gpui::test]
fn actual_usage_locations_code_and_package_filters_are_native_named_controls(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| {
        gpui_component::init(cx);
        let _ = crate::fonts::install(cx);
        set_facet(
            Facet {
                reduced_motion: true,
                ..Facet::default()
            },
            cx,
        );
    });
    let facts = Facts::new("tick", Kind::Constant, Lang::Rust, "signal");
    let mut uses = Uses {
        all: ["alpha", "café_🧭"]
            .into_iter()
            .map(|package| symbol::view::Use {
                package: package.into(),
                file: format!("src/{package}.rs"),
                path: format!("/current/{package}/src/lib.rs"),
                line: 9,
                text: "signal.tick".into(),
                mark: Some(7..11),
                verb: symbol::view::Verb::Reads,
                member: None,
                ctx: symbol::view::Ctx::Code,
                fill: None,
                approx: false,
            })
            .collect(),
        evidence: symbol::view::UseEvidence::Reported { reported: 3, readable: 3 },
        elsewhere: None,
    };
    uses.all.push(symbol::view::Use {
        package: "alpha".into(),
        file: "src/alpha_tests.rs".into(),
        path: "/current/alpha/src/alpha_tests.rs".into(),
        line: 12,
        text: "assert!(signal.tick > 0);".into(),
        mark: None,
        verb: symbol::view::Verb::Reads,
        member: None,
        ctx: symbol::view::Ctx::Test,
        fill: None,
        approx: false,
    });
    let (document, cx) = cx.add_window_view(|window, _| {
        window.set_a11y_forced(true);
        Document {
            view: symbol::compile(&facts),
            uses,
            ui: Ui::default(),
            covered: false,
            width: 1440.,
        }
    });
    cx.simulate_resize(gpui::size(px(1800.), px(1800.)));
    let native = tree(cx);
    for location in ["src/alpha.rs:9", "src/café_🧭.rs:9"] {
        assert!(
            has(&native, "Link", location),
            "actual usage destination {location}"
        );
    }
    assert!(has(&native, "Label", "signal.tick"));
    assert!(has(&native, "Button", "all 2 packages"));
    assert!(
        has(&native, "Button", "reads"),
        "actual typed Reads relation, not an inferred call"
    );
    assert!(has(&native, "Button", "include tests"));
    assert!(
        !has(&native, "Link", "src/alpha_tests.rs:12"),
        "test-context rows start excluded"
    );
    document.update(cx, |document, cx| {
        document.ui = document.ui.clone().apply(&symbol::Change::Tests);
        cx.notify();
    });
    let native = tree(cx);
    assert!(has(&native, "Button", "include tests"));
    assert!(
        has(&native, "Link", "src/alpha_tests.rs:12"),
        "including tests projects the actual test destination"
    );
    document.update(cx, |document, cx| {
        document
            .uses
            .all
            .retain(|place| place.ctx != symbol::view::Ctx::Test);
        cx.notify();
    });
    assert!(
        !has(&tree(cx), "Button", "include tests"),
        "no test-use evidence means no include-tests control"
    );
}

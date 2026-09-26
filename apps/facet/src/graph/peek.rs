//! A symbol of the world as the float layer's peek card: the one card the
//! graph raises on hover and the page raises on a link.

use super::model::{Kind, NodeId, World};
use crate::controls::button;
use crate::measure::Control;
use crate::overlay::float::{self, FloatKind, FloatRequest, Side};
use crate::overlay::peek::{self, Peek, SymbolPeek};
use crate::overlay::text::{Role, Sig};
use gpui::{
    App, Bounds, ElementId, IntoElement, ParentElement, Pixels, SharedString, Styled, Window, div,
    px,
};
use std::cell::OnceCell;
use std::fmt::Write as _;
use std::rc::Rc;
use std::sync::Arc;

/// An explicit action on the symbol described by a graph peek.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Fly to the symbol and gather its relations.
    Focus,
    /// Open the symbol's reading page.
    Open,
}

/// Dispatches a card action using the card's own node, independent of the
/// current canvas hover or focus.
pub type ActionHandler = Rc<dyn Fn(NodeId, Action, &mut Window, &mut App)>;

/// A cheap symbol handle. Semantic content is built once only after the
/// float's hover intent opens it; pointer movement never scans the world.
#[derive(Clone)]
pub struct Prepared {
    node: NodeId,
    world: Arc<World>,
    content: Rc<OnceCell<Peek>>,
}

impl Prepared {
    /// Retain a symbol without preparing its signature or relations.
    #[must_use]
    pub fn new(world: Arc<World>, node: NodeId) -> Self {
        Self {
            node,
            world,
            content: Rc::new(OnceCell::new()),
        }
    }

    /// Which symbol this content describes.
    #[must_use]
    pub const fn node(&self) -> NodeId {
        self.node
    }

    /// A moving-anchor float request with useful actions on this symbol.
    pub fn request(
        &self,
        key: ElementId,
        anchor: Bounds<Pixels>,
        side: Side,
        open_page: bool,
        action: ActionHandler,
    ) -> FloatRequest {
        let content = self.content.clone();
        let world = self.world.clone();
        let node = self.node;
        FloatRequest::new(key, anchor, FloatKind::Peek, move |measure, window, cx| {
            let content = content.get_or_init(|| Peek::Symbol(symbol_preview(&world, node)));
            let card = peek::card(content, measure, window, cx);
            let pinned = float::surface(window, cx) == float::Surface::Pinned;
            let focus = action.clone();
            let open = action.clone();
            let mut actions = div()
                .flex()
                .flex_wrap()
                .gap(px(8.0 * measure.scale()))
                .px(px(if pinned { 0.0 } else { 16.0 } * measure.scale()))
                .pb(px(if pinned { 0.0 } else { 14.0 } * measure.scale()))
                .child(crate::probe::target(
                    ("graph-peek-focus", node as usize),
                    crate::probe::Target {
                        clickable: true,
                        focusable: true,
                        ..crate::probe::Target::default()
                    },
                    button(("graph-peek-focus", node as usize), "Focus", measure)
                        .size(Control::Small)
                        .edge()
                        .on_click(move |window, cx| {
                            float::close_all(window, cx);
                            focus(node, Action::Focus, window, cx);
                        }),
                ));
            if open_page {
                actions = actions.child(crate::probe::target(
                    ("graph-peek-open", node as usize),
                    crate::probe::Target {
                        clickable: true,
                        focusable: true,
                        ..crate::probe::Target::default()
                    },
                    button(("graph-peek-open", node as usize), "Open page", measure)
                        .size(Control::Small)
                        .ghost()
                        .on_click(move |window, cx| {
                            float::close_all(window, cx);
                            open(node, Action::Open, window, cx);
                        }),
                ));
            }
            div()
                .flex()
                .flex_col()
                .child(card)
                .child(actions)
                .into_any_element()
        })
        .side(side)
    }
}

const PREVIEW_KIDS: usize = 64;
const PREVIEW_CHARS: usize = 2048;

struct Preview {
    text: String,
    remaining: usize,
    truncated: bool,
}

impl Preview {
    fn new(limit: usize) -> Self {
        Self {
            text: String::new(),
            remaining: limit,
            truncated: false,
        }
    }
    fn finish(mut self) -> String {
        if self.truncated {
            self.text.pop();
            self.text.push('…');
        }
        self.text
    }
}

impl std::fmt::Write for Preview {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        if self.truncated {
            return Ok(());
        }
        for ch in text.chars() {
            if self.remaining == 0 {
                self.truncated = true;
                break;
            }
            self.text.push(ch);
            self.remaining -= 1;
        }
        Ok(())
    }
}

fn bounded_text(text: &str, limit: usize) -> String {
    let mut out = Preview::new(limit);
    let _ = out.write_str(text);
    out.finish()
}

#[cfg(test)]
thread_local! { static SEMANTIC_VISITS: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) }; }

fn signature_preview(world: &World, i: NodeId) -> String {
    let node = world.node(i);
    let mut out = Preview::new(PREVIEW_CHARS);
    match node.kind {
        Kind::Field | Kind::Variant if node.ty.is_some() => {
            let ty = node.ty.as_deref().unwrap_or("");
            match (node.kind, node.shape.as_deref()) {
                (Kind::Variant, Some("record")) => {
                    let _ = write!(out, "{} {{ {ty} }}", node.name);
                }
                (Kind::Variant, _) => {
                    let _ = write!(out, "{}({ty})", node.name);
                }
                _ => {
                    let _ = write!(out, "{}: {ty}", node.name);
                }
            }
        }
        Kind::Struct | Kind::Enum => {
            let kids = world.kids(i);
            let parts: Vec<NodeId> = kids
                .iter()
                .take(PREVIEW_KIDS)
                .copied()
                .filter(|&j| {
                    #[cfg(test)]
                    SEMANTIC_VISITS.with(|visits| {
                        let (kids, edges) = visits.get();
                        visits.set((kids + 1, edges));
                    });
                    world.node(j).kind.is_part()
                })
                .collect();
            if parts.is_empty() && kids.len() <= PREVIEW_KIDS {
                let _ = out.write_str(node.sig.as_deref().unwrap_or(&node.name));
            } else {
                if node.kind == Kind::Struct {
                    let _ = out.write_str("{ ");
                }
                for (n, &j) in parts.iter().enumerate() {
                    if n > 0 {
                        let _ = out.write_str(if node.kind == Kind::Struct {
                            ", "
                        } else {
                            " | "
                        });
                    }
                    let part = world.node(j);
                    if node.kind == Kind::Struct {
                        let _ = write!(out, "{}: {}", part.name, part.ty.as_deref().unwrap_or(""));
                    } else {
                        match (&part.ty, part.shape.as_deref()) {
                            (Some(_), Some("record")) => {
                                let _ = write!(out, "{} {{…}}", part.name);
                            }
                            (Some(ty), _) => {
                                let _ = write!(out, "{}({ty})", part.name);
                            }
                            (None, _) => {
                                let _ = out.write_str(&part.name);
                            }
                        }
                    }
                    if out.truncated {
                        break;
                    }
                }
                if kids.len() > PREVIEW_KIDS {
                    let _ = out.write_str(if parts.is_empty() {
                        "…"
                    } else if node.kind == Kind::Struct {
                        ", …"
                    } else {
                        " | …"
                    });
                }
                if node.kind == Kind::Struct {
                    let _ = out.write_str(" }");
                }
            }
        }
        _ => {
            let _ = out.write_str(node.sig.as_deref().unwrap_or(&node.name));
        }
    }
    out.finish()
}

fn symbol_preview(world: &World, i: NodeId) -> SymbolPeek {
    let node = world.node(i);
    let mut name = Preview::new(256);
    if let Some(parent) = node.parent {
        let _ = write!(
            name,
            "{}{}",
            world.node(parent).name,
            if node.kind == Kind::Field { "." } else { "::" }
        );
    }
    let _ = name.write_str(&node.name);
    let mut path = Preview::new(512);
    let _ = path.write_str(world.package_short(node.pkg));
    let module = &world.modules[node.module as usize].path;
    if !module.is_empty() {
        let _ = write!(path, "::{module}");
    }
    if let Some(parent) = node.parent {
        let _ = write!(path, "::{}", world.node(parent).name);
    }
    let path = path.finish();
    let uses = if world.yours(i) {
        None
    } else {
        let count = your_uses(world, i);
        (count > 0).then_some(count)
    };
    SymbolPeek {
        kind: Some(crate::anatomy::icon_kind(node.kind)),
        name: name.finish().into(),
        place: format!("{} in `{path}`", node.kind.text()).into(),
        path: path.into(),
        signature: Some(Sig::new().span(signature_preview(world, i), Role::Plain)),
        sentence: node.doc.as_deref().map(|doc| bounded_text(doc, 512).into()),
        uses,
        ..SymbolPeek::default()
    }
}

/// The signature the prototype's peek shows (app.js `sigOf`): a part spells
/// its type, an enum its variants, a struct its fields, anything else its
/// written signature.
#[must_use]
pub fn signature(world: &World, i: NodeId) -> String {
    let node = world.node(i);
    let part = |j: NodeId| {
        let n = world.node(j);
        match (&n.ty, n.kind) {
            (Some(ty), Kind::Variant) if n.shape.as_deref() == Some("record") => {
                format!("{} {{ {ty} }}", n.name)
            }
            (Some(ty), Kind::Variant) => format!("{}({ty})", n.name),
            (Some(ty), _) => format!("{}: {ty}", n.name),
            (None, _) => n.name.to_string(),
        }
    };
    match node.kind {
        Kind::Field | Kind::Variant if node.ty.is_some() => part(i),
        Kind::Enum | Kind::Struct => {
            let parts: Vec<NodeId> = world
                .kids(i)
                .iter()
                .copied()
                .filter(|&j| {
                    #[cfg(test)]
                    SEMANTIC_VISITS.with(|visits| {
                        let (kids, edges) = visits.get();
                        visits.set((kids + 1, edges));
                    });
                    world.node(j).kind.is_part()
                })
                .collect();
            if parts.is_empty() {
                return node
                    .sig
                    .as_ref()
                    .map_or_else(|| node.name.to_string(), ToString::to_string);
            }
            if node.kind == Kind::Enum {
                parts
                    .iter()
                    .map(|&j| {
                        let n = world.node(j);
                        match (&n.ty, n.shape.as_deref()) {
                            (Some(_), Some("record")) => format!("{} {{…}}", n.name),
                            (Some(ty), _) => format!("{}({ty})", n.name),
                            (None, _) => n.name.to_string(),
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" | ")
            } else {
                let fields: Vec<String> = parts
                    .iter()
                    .map(|&j| {
                        let n = world.node(j);
                        format!("{}: {}", n.name, n.ty.as_deref().unwrap_or(""))
                    })
                    .collect();
                format!("{{ {} }}", fields.join(", "))
            }
        }
        _ => node
            .sig
            .as_ref()
            .map_or_else(|| node.name.to_string(), ToString::to_string),
    }
}

/// How many relations from your code land on `i` (app.js `yoursUses`):
/// item-level for an item, member-level for a member.
#[must_use]
pub fn your_uses(world: &World, i: NodeId) -> usize {
    let yours = |&(j, _): &(NodeId, super::model::Rel)| {
        #[cfg(test)]
        SEMANTIC_VISITS.with(|visits| {
            let (kids, edges) = visits.get();
            visits.set((kids, edges + 1));
        });
        world.yours(j)
    };
    if world.is_item(i) {
        world.item_ins(i).filter(yours).count()
    } else {
        world.in_edges(i).filter(yours).count()
    }
}

/// The peek card for `i`: kind, name, where, one line of signature, the
/// doc's first sentence and your uses.
#[must_use]
pub fn symbol_peek(world: &World, i: NodeId) -> SymbolPeek {
    let node = world.node(i);
    let place = world.qual(i);
    let uses = your_uses(world, i);
    SymbolPeek {
        kind: Some(crate::anatomy::icon_kind(node.kind)),
        name: world.name_of(i),
        place: SharedString::from(format!("{} in `{place}`", node.kind.text())),
        path: place,
        signature: Some(Sig::new().span(signature(world, i), Role::Plain)),
        sentence: node.doc.clone(),
        uses: (uses > 0 && !world.yours(i)).then_some(uses),
        ..SymbolPeek::default()
    }
}

#[cfg(test)]
mod tests {
    use super::{signature, symbol_peek, your_uses};
    use crate::graph::model::tests::tiny;

    #[test]
    fn a_struct_spells_its_fields_and_a_callable_its_signature() {
        let mut world = tiny();
        world.nodes[1].ty = Some("Vec<Relation>".into());
        world.nodes[3].sig = Some("pub fn from_str<T>(s: &str) -> Result<T>".into());
        assert_eq!(signature(&world, 0), "{ relations: Vec<Relation> }");
        assert_eq!(
            signature(&world, 3),
            "pub fn from_str<T>(s: &str) -> Result<T>"
        );
        assert_eq!(signature(&world, 1), "relations: Vec<Relation>");
    }

    #[test]
    fn the_peek_counts_only_your_uses() {
        let world = tiny();
        // Error (5) is held by Page.relations and given by Page::new (yours:
        // one rolled-up item edge) and given by from_str (not yours).
        assert_eq!(your_uses(&world, 5), 1);
        let peek = symbol_peek(&world, 5);
        assert_eq!(peek.uses, Some(1));
        assert_eq!(peek.path.as_ref(), "lib::de");
        assert_eq!(peek.place.as_ref(), "enum in `lib::de`");
        // Your own symbols do not count your own uses.
        assert_eq!(symbol_peek(&world, 0).uses, None);
    }

    struct PeekHost;

    struct HoverHost {
        world: std::sync::Arc<crate::graph::model::World>,
    }

    impl gpui::Render for HoverHost {
        fn render(
            &mut self,
            window: &mut gpui::Window,
            cx: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            use gpui::{ParentElement, Styled};
            let world = self.world.clone();
            gpui::div()
                .size_full()
                .child(
                    gpui::canvas(
                        |_, _, _| (),
                        move |_, (), window, _| {
                            window.on_mouse_event(
                                move |event: &gpui::MouseMoveEvent, phase, window, cx| {
                                    if phase != gpui::DispatchPhase::Capture {
                                        return;
                                    }
                                    let node = if event.position.x < gpui::px(450.0) {
                                        0
                                    } else {
                                        5
                                    };
                                    let prepared = super::Prepared::new(world.clone(), node);
                                    super::float::rest(
                                        prepared.request(
                                            gpui::ElementId::NamedInteger(
                                                "rapid-node".into(),
                                                u64::from(node),
                                            ),
                                            gpui::Bounds::new(
                                                event.position,
                                                gpui::size(gpui::px(20.0), gpui::px(20.0)),
                                            ),
                                            super::Side::Below,
                                            false,
                                            std::rc::Rc::new(|_, _, _, _| {}),
                                        ),
                                        window,
                                        cx,
                                    );
                                },
                            );
                        },
                    )
                    .size_full(),
                )
                .child(super::float::layer(window, cx))
        }
    }

    fn high_degree_world() -> std::sync::Arc<crate::graph::model::World> {
        use crate::graph::model::{Edge, Kind, Node, Rel, World};
        let world = tiny();
        let (packages, modules, mut nodes, mut edges) =
            (world.packages, world.modules, world.nodes, world.edges);
        let ty: gpui::SharedString = "Long名".repeat(4096).into();
        for n in 0..512 {
            let mut field = Node::new(Kind::Field, format!("field{n}"), 0, 0).member_of(0);
            field.ty = Some(ty.clone());
            nodes.push(field);
            let mut variant = Node::new(Kind::Variant, format!("variant{n}"), 1, 1).member_of(5);
            variant.ty = Some(ty.clone());
            nodes.push(variant);
            let source = u32::try_from(nodes.len()).expect("fixture node id");
            nodes.push(Node::new(Kind::Function, format!("use{n}"), 0, 0));
            edges.push(Edge {
                from: source,
                to: 5,
                rel: Rel::USES,
            });
        }
        std::sync::Arc::new(
            World::new(packages, modules, nodes, edges).expect("high-degree fixture"),
        )
    }

    #[gpui::test]
    fn rapid_native_alternating_high_degree_nodes_scan_no_semantics_until_original_intent_opens(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::set_facet(
                crate::theme::Facet {
                    reduced_motion: true,
                    ..crate::theme::Facet::default()
                },
                cx,
            );
        });
        let world = high_degree_world();
        assert!(
            world.kids(0).len() > super::PREVIEW_KIDS && world.kids(5).len() > super::PREVIEW_KIDS
        );
        assert!(world.item_ins(5).len() >= 512);
        let (_, cx) = cx.add_window_view(|_, _| HoverHost {
            world: world.clone(),
        });
        cx.simulate_resize(gpui::size(gpui::px(900.0), gpui::px(700.0)));
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        super::SEMANTIC_VISITS.with(|visits| visits.set((0, 0)));
        let clock = cx.executor();
        cx.update(|window, cx| {
            for n in 0..1000 {
                clock.advance_clock(std::time::Duration::from_millis(1));
                window.dispatch_event(
                    gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
                        position: gpui::point(
                            gpui::px(if n % 2 == 0 { 200.0 } else { 600.0 }),
                            gpui::px(100.0),
                        ),
                        pressed_button: None,
                        modifiers: gpui::Modifiers::none(),
                    }),
                    cx,
                );
            }
        });
        cx.run_until_parked();
        assert_eq!(
            super::SEMANTIC_VISITS.with(std::cell::Cell::get),
            (0, 0),
            "rapid actual pointer identities must scan no signature children or incoming relations while intent is pending"
        );
        clock.advance_clock(std::time::Duration::from_millis(349));
        cx.run_until_parked();
        let key = gpui::ElementId::NamedInteger("rapid-node".into(), 5);
        assert!(!cx.update(|window, cx| super::float::is_open(&key, window, cx)));
        assert_eq!(super::SEMANTIC_VISITS.with(std::cell::Cell::get), (0, 0));
        clock.advance_clock(std::time::Duration::from_millis(1));
        cx.run_until_parked();
        assert!(
            cx.update(|window, cx| super::float::is_open(&key, window, cx)),
            "lazy semantics must preserve the existing 350 ms intent deadline"
        );
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        let visits = super::SEMANTIC_VISITS.with(std::cell::Cell::get);
        assert_eq!(
            visits.0,
            super::PREVIEW_KIDS,
            "opened preview scans only the bounded first member window"
        );
        assert!(
            visits.1 >= 512,
            "exact uses are computed only for the actual opened card"
        );
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        assert_eq!(
            super::SEMANTIC_VISITS.with(std::cell::Cell::get),
            visits,
            "the opened card's shared content is initialized once"
        );
    }

    #[test]
    fn a_large_unicode_signature_has_a_bounded_preview_and_keeps_the_full_signature_api() {
        let world = high_degree_world();
        for node in [0, 5] {
            super::SEMANTIC_VISITS.with(|visits| visits.set((0, 0)));
            let preview = super::signature_preview(&world, node);
            assert!(preview.chars().count() <= super::PREVIEW_CHARS && preview.ends_with('…'));
            assert_eq!(
                super::SEMANTIC_VISITS.with(std::cell::Cell::get).0,
                super::PREVIEW_KIDS
            );
            assert!(super::signature(&world, node).chars().count() > super::PREVIEW_CHARS);
        }
    }

    impl gpui::Render for PeekHost {
        fn render(
            &mut self,
            window: &mut gpui::Window,
            cx: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            use crate::theme::ActiveFacet;
            use gpui::{ParentElement, Styled};
            let measure = crate::Measure::new(gpui::px(252.0), &cx.facet());
            gpui::div()
                .size_full()
                .child(super::float::pinned_column(&measure, window, cx))
                .child(super::float::layer(window, cx))
        }
    }

    #[gpui::test]
    fn native_buttons_route_the_cards_own_symbol_and_keep_pinned_actions(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::set_facet(
                crate::theme::Facet {
                    reduced_motion: true,
                    ..crate::theme::Facet::default()
                },
                cx,
            );
            crate::probe::enable(cx);
        });
        for (open_page, pinned, wanted) in [
            (false, false, super::Action::Focus),
            (true, false, super::Action::Open),
            (true, true, super::Action::Focus),
        ] {
            let invoked = std::rc::Rc::new(std::cell::Cell::new(None));
            let record = invoked.clone();
            let prepared = super::Prepared::new(std::sync::Arc::new(tiny()), 5);
            let (_, cx) = cx.add_window_view(|_, _| PeekHost);
            cx.simulate_resize(gpui::size(gpui::px(900.0), gpui::px(700.0)));
            cx.update(|window, cx| {
                super::float::open(
                    prepared.request(
                        "own-symbol".into(),
                        gpui::Bounds::new(
                            gpui::point(gpui::px(400.0), gpui::px(100.0)),
                            gpui::size(gpui::px(20.0), gpui::px(20.0)),
                        ),
                        super::Side::Below,
                        open_page,
                        std::rc::Rc::new(move |node, action, _, _| {
                            record.set(Some((node, action)))
                        }),
                    ),
                    window,
                    cx,
                );
                if pinned {
                    assert!(super::float::pin_top(window, cx));
                }
            });
            cx.run_until_parked();
            cx.update(|window, cx| {
                window.draw(cx).clear(cx);
            });
            let targets = cx.update(|_, cx| crate::probe::take(cx)).targets;
            assert!(
                targets
                    .iter()
                    .any(|target| target.key == "graph-peek-focus-5")
            );
            assert_eq!(
                targets
                    .iter()
                    .any(|target| target.key == "graph-peek-open-5"),
                open_page,
                "standalone peeks must never offer an unavailable page action"
            );
            let key = if wanted == super::Action::Focus {
                "graph-peek-focus-5"
            } else {
                "graph-peek-open-5"
            };
            let target = targets
                .iter()
                .rev()
                .find(|target| target.key == key)
                .expect("actual measured native action");
            cx.simulate_click(
                gpui::point(
                    gpui::px(target.bounds.x + target.bounds.width * 0.5),
                    gpui::px(target.bounds.y + target.bounds.height * 0.5),
                ),
                gpui::Modifiers::none(),
            );
            assert_eq!(
                invoked.get(),
                Some((5, wanted)),
                "native action must use its prepared symbol after float closure"
            );
        }
    }
}

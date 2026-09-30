//! The hand arranged from declarations and relations in the selected local
//! index. A missing relation is shown as unavailable; it never falls back to
//! the design-system graph fixture.

use super::indexed_world::{self, Key, Projection};
use super::offload::{Answer, Asker, Memo};
use crate::model::AppSnapshot;
use crate::model::hand::{Hand, Held};
use crate::model::pages::{DeclRef, PackageRef};
use facet::graph::{NodeId, World};
use facet::semantics::recipes::{PreparedRecipes, Recipes};
use gpui::{Context, Global, SharedString};
use std::cell::RefCell;
use std::collections::HashSet;
use std::num::NonZeroUsize;
use std::sync::{Arc, OnceLock};

const HANDS_KEPT: usize = 8;

/// One hand card as the shell draws it.
#[derive(Clone, Debug)]
pub(crate) struct Card {
    pub(crate) held: Held,
    pub(crate) name: SharedString,
    pub(crate) kind: facet::icons::Kind,
}

/// A road of cards and the recorded relations between them.
#[derive(Clone, Debug)]
pub(crate) struct RoadView {
    pub(crate) cards: Vec<usize>,
    pub(crate) verbs: Vec<SharedString>,
    pub(crate) sentence: String,
}

/// The hand arranged by the current index. `status` is present while its
/// exact graph is loading or when the owner could not provide that graph.
#[derive(Clone, Debug, Default)]
pub(crate) struct HandView {
    pub(crate) cards: Vec<Card>,
    pub(crate) roads: Vec<RoadView>,
    pub(crate) apart: Vec<usize>,
    pub(crate) status: Option<Arc<str>>,
}

#[derive(Clone, Debug)]
struct HandKey {
    held: Vec<Held>,
}

impl PartialEq for HandKey {
    fn eq(&self, other: &Self) -> bool {
        self.held.len() == other.held.len()
            && self.held.iter().zip(&other.held).all(|(left, right)| left.same(right))
    }
}
impl Eq for HandKey {}
impl std::hash::Hash for HandKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        for card in &self.held {
            card.package.hash(state);
            card.id.hash(state);
        }
    }
}

struct Shown {
    held: Vec<Held>,
    view: Arc<HandView>,
}

struct Hands {
    key: Key,
    memo: Memo<HandKey, HandView>,
    shown: RefCell<Option<Shown>>,
}
impl Global for Hands {}

impl Hands {
    fn over(key: Key, projection: Arc<Projection>) -> Self {
        let recipes = Arc::new(OnceLock::<PreparedRecipes>::new());
        let world = Arc::clone(&projection.world);
        let identities = Arc::clone(&projection.identities);
        let memo = Memo::new(
            NonZeroUsize::new(HANDS_KEPT).expect("positive cache capacity"),
            move |key: &HandKey| {
                let prepared = recipes.get_or_init(|| Recipes::new(&world).into_prepared());
                arrange(&world, &identities, &Recipes::from_prepared(prepared.clone()), &key.held)
            },
        );
        Self { key, memo, shown: RefCell::new(None) }
    }
}

/// A stable project preference for bounded world projections.
pub(crate) fn preferred(snapshot: &AppSnapshot) -> Option<PackageRef> {
    snapshot
        .workspace()
        .active
        .as_ref()
        .or(snapshot.workspace().host.as_ref())
        .and_then(|project| PackageRef::parse(project.as_str()).ok())
}

/// Reads or arranges `hand` without blocking the window. The current root is
/// exact; touching a held card does not rerun the relation walk.
pub(crate) fn hand_view_for<T: 'static>(
    hand: &Hand,
    snapshot: &AppSnapshot,
    cx: &mut Context<T>,
) -> Arc<HandView> {
    if hand.is_empty() {
        return Arc::new(HandView::default());
    }
    let Some(key) = indexed_world::key(snapshot.key(), preferred(snapshot), cx) else {
        return standing_apart(hand.held(), Some(Arc::from("Waiting for the local index connection.")));
    };
    let projection = match indexed_world::get(&key, cx) {
        indexed_world::State::Reading => {
            return standing_apart(hand.held(), Some(Arc::from("Reading declarations and relations from the current index…")));
        }
        indexed_world::State::Waiting => {
            return standing_apart(hand.held(), Some(Arc::from("Waiting for an available index-read slot before reading declarations and relations…")));
        }
        indexed_world::State::Unavailable(reason) => {
            return standing_apart(hand.held(), Some(Arc::from(format!("The current index graph is unavailable: {reason}"))));
        }
        indexed_world::State::Ready(projection) => projection,
    };
    if cx.try_global::<Hands>().is_none_or(|hands| hands.key != key) {
        cx.set_global(Hands::over(key.clone(), projection));
    }
    let key = HandKey { held: hand.held().to_vec() };
    let (memo, previous) = {
        let hands = cx.global::<Hands>();
        (
            hands.memo.clone(),
            hands
                .shown
                .borrow()
                .as_ref()
                .map(|shown| (shown.held.clone(), Arc::clone(&shown.view))),
        )
    };
    let asker = Asker::View(cx.entity_id());
    let view = match memo.ask(&key, asker, cx) {
        Answer::Ready(view) => refresh(&view, hand.held()),
        Answer::Reading => match previous {
            Some((held, view)) if same_held(&held, hand.held()) && view.status.is_some() => view,
            _ => standing_apart(hand.held(), Some(Arc::from("Ordering these cards from indexed relations…"))),
        },
        Answer::Deferred => standing_apart(hand.held(), Some(Arc::from("Waiting for an available index-read slot before arranging these cards…"))),
        Answer::Failed(fault) => standing_apart(
            hand.held(),
            Some(Arc::from(format!("Could not order these cards from indexed relations: {fault}"))),
        ),
    };
    if view.status.is_none() {
        *cx.global::<Hands>().shown.borrow_mut() = Some(Shown {
            held: hand.held().to_vec(),
            view: Arc::clone(&view),
        });
    }
    view
}

fn same_held(left: &[Held], right: &[Held]) -> bool {
    left.len() == right.len() && left.iter().zip(right).all(|(a, b)| a.same(b))
}

fn arrange(
    world: &World,
    identities: &crate::shell::bodies::graph::identity::IdentityAdapter,
    recipes: &Recipes,
    held: &[Held],
) -> HandView {
    let nodes: Vec<Option<NodeId>> = held
        .iter()
        .map(|card| {
            let id = card.id.as_ref()?;
            let symbol = crate::model::pages::SymbolRef::new(id.as_str()).ok()?;
            let package = PackageRef::parse(card.package.as_str()).ok()?;
            let Some(decl) = DeclRef::from_label(symbol.as_str(), None, None, None) else {
                return None;
            };
            match identities.candidates(&decl, &package).as_slice() {
                [node] if (*node as usize) < world.nodes.len() => Some(*node),
                _ => None,
            }
        })
        .collect();
    let connector = facet::semantics::recipes::chain::Connector::new(recipes, world);
    let known: Vec<NodeId> = nodes.iter().flatten().copied().collect();
    let arrangement = connector.arrange(&known);
    let card_of = |node: NodeId| {
        held.iter()
            .zip(&nodes)
            .find(|(_, candidate)| **candidate == Some(node))
            .map(|(card, _)| card.clone())
    };
    let make = |held: Held, node: Option<NodeId>| Card {
        name: node.map_or_else(|| name_of(&held), |node| world.name_of(node)),
        kind: node.map_or(facet::icons::Kind::Unknown, |node| {
            crate::shell::kit::world_kind(world.node(node).kind)
        }),
        held,
    };
    let unresolved = nodes.iter().any(Option::is_none);
    let mut view = HandView {
        status: unresolved.then(|| {
            Arc::from("Some held cards have no exact declaration in the selected index and stand apart.")
        }),
        ..HandView::default()
    };
    let twins: HashSet<String> = {
        let mut seen = HashSet::new();
        known
            .iter()
            .map(|&node| world.node(node).name.to_string())
            .filter(|name| !seen.insert(name.clone()))
            .collect()
    };
    for road in &arrangement.roads {
        let mut cards = Vec::new();
        let mut verbs = Vec::new();
        for link in &road.links {
            if let Some(join) = &link.join {
                verbs.push(match &join.how {
                    facet::semantics::recipes::chain::JoinHow::Direct => SharedString::default(),
                    facet::semantics::recipes::chain::JoinHow::As => SharedString::new_static("as"),
                    facet::semantics::recipes::chain::JoinHow::Steps(_) => connector.verbs(join).join(" · ").into(),
                });
            }
            if let Some(card) = card_of(link.piece.node) {
                cards.push(view.cards.len());
                view.cards.push(make(card, Some(link.piece.node)));
            }
        }
        view.roads.push(RoadView { cards, verbs, sentence: connector.sentence(road, &twins).text() });
    }
    let shown: Vec<Held> = view.cards.iter().map(|card| card.held.clone()).collect();
    for (card, node) in held.iter().zip(&nodes) {
        if shown.iter().any(|seen| seen.same(card)) {
            continue;
        }
        view.apart.push(view.cards.len());
        view.cards.push(make(card.clone(), *node));
    }
    view
}

fn standing_apart(held: &[Held], status: Option<Arc<str>>) -> Arc<HandView> {
    Arc::new(HandView {
        cards: held.iter().map(|card| Card { held: card.clone(), name: name_of(card), kind: facet::icons::Kind::Unknown }).collect(),
        roads: Vec::new(),
        apart: (0..held.len()).collect(),
        status,
    })
}

fn name_of(held: &Held) -> SharedString {
    match &held.id {
        Some(id) => SharedString::from(backend_present::Identity::parse(id.as_str()).name().to_owned()),
        None => PackageRef::parse(held.package.as_str()).map_or_else(
            |_| SharedString::from(held.package.as_str().to_owned()),
            |package| SharedString::from(package.display_name().to_owned()),
        ),
    }
}

fn refresh(view: &HandView, held: &[Held]) -> Arc<HandView> {
    let mut view = view.clone();
    for card in &mut view.cards {
        if let Some(now) = held.iter().find(|now| now.same(&card.held)) {
            card.held = now.clone();
        }
    }
    Arc::new(view)
}

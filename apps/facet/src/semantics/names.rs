//! Resolving a written type name to a symbol in the world.
//!
//! The prototype resolves by last segment alone (`page.js` `resolveName`):
//! same package first, then importance. That links `fmt::Formatter` to
//! serde_json's `Formatter` trait. Here a qualified name must also agree
//! with its qualifier: the segment before the name has to be the
//! candidate's module (or its last segment) or its package; a qualifier
//! nothing in the world matches leaves the name a [`Target::Path`] for the
//! shell to ask the index about.

use super::bounds::generics;
use super::types::{Resolve, Scope, Target, TypeExpr, parse};
use crate::graph::model::{Kind, NodeId, World};
use gpui::SharedString;
use std::collections::HashMap;

/// Every top-level type-like symbol by name (built once per world).
pub struct Names {
    by_name: HashMap<SharedString, Vec<NodeId>>,
}

impl Names {
    /// Indexes `world`'s structs, enums, traits, aliases and unions.
    #[must_use]
    pub fn new(world: &World) -> Self {
        let mut by_name: HashMap<SharedString, Vec<NodeId>> = HashMap::new();
        for &i in &world.items {
            let node = world.node(i);
            if matches!(node.kind, Kind::Struct | Kind::Enum | Kind::Trait | Kind::Type | Kind::Union) {
                by_name.entry(node.name.clone()).or_default().push(i);
            }
        }
        Self { by_name }
    }

    /// The candidates named `name`.
    #[must_use]
    pub fn candidates(&self, name: &str) -> &[NodeId] {
        self.by_name.get(name).map_or(&[], Vec::as_slice)
    }
}

/// Resolves names written inside the symbol `from`.
pub struct InWorld<'w> {
    /// The world.
    pub world: &'w World,
    /// Its name index.
    pub names: &'w Names,
    /// Where the name was written.
    pub from: NodeId,
}

impl InWorld<'_> {
    fn qualifier_matches(&self, candidate: NodeId, qualifier: &str) -> bool {
        if matches!(qualifier, "crate" | "self" | "super" | "Self") {
            return true;
        }
        let node = self.world.node(candidate);
        let module = self.world.modules[node.module as usize].path.as_ref();
        let package = self.world.packages[node.pkg as usize].name.replace('-', "_");
        module == qualifier
            || module.rsplit("::").next() == Some(qualifier)
            || package == qualifier
            || self.world.package_short(node.pkg).replace('-', "_") == qualifier
    }

    /// The best candidate for `path`, as the prototype ranks them.
    fn best(&self, path: &[String], kind: Option<Kind>) -> Option<NodeId> {
        let name = path.last()?;
        let here = self.world.node(self.from).pkg;
        let qualifier = (path.len() >= 2).then(|| path[path.len() - 2].as_str());
        self.names
            .candidates(name)
            .iter()
            .copied()
            .filter(|&c| kind.is_none_or(|k| self.world.node(c).kind == k))
            .filter(|&c| qualifier.is_none_or(|q| self.qualifier_matches(c, q)))
            .max_by(|&a, &b| {
                let same = |c: NodeId| self.world.node(c).pkg == here;
                same(a)
                    .cmp(&same(b))
                    .then_with(|| self.world.importance(a).partial_cmp(&self.world.importance(b)).unwrap_or(std::cmp::Ordering::Equal))
                    // Equal rank: the first in node order wins, as a stable sort would keep it.
                    .then_with(|| b.cmp(&a))
            })
    }
}

impl Resolve for InWorld<'_> {
    fn named(&self, path: &[String]) -> Option<Target> {
        self.best(path, None).map(Target::Node)
    }

    fn alias(&self, path: &[String], arity: usize) -> Option<(Vec<String>, TypeExpr)> {
        let alias = self.best(path, Some(Kind::Type))?;
        let sig = self.world.node(alias).sig.clone()?;
        // `type Result<T> = result::Result<T, Error>;`
        let (head, body) = sig.split_once('=')?;
        let params: Vec<String> = head
            .split_once('<')
            .map(|(_, rest)| rest.trim_end().trim_end_matches('>'))
            .map(|list| generics(&[list], "").into_iter().map(|g| g.name).collect())
            .unwrap_or_default();
        (params.len() == arity).then(|| (params, parse(body.trim().trim_end_matches(';'))))
    }
}

/// The spelling scope for types written in `node`: its own generics, its
/// parent's, its where-clause's, and `Self` as its type (or itself).
#[must_use]
pub fn scope<'w>(resolver: &'w InWorld<'w>, node: NodeId) -> Scope<'w> {
    let world = resolver.world;
    let n = world.node(node);
    let parent = n.parent.map(|p| world.node(p));
    let lists: Vec<&str> = [n.generics.as_deref(), parent.and_then(|p| p.generics.as_deref())].into_iter().flatten().collect();
    let names = generics(&lists, n.where_.as_deref().unwrap_or("")).into_iter().map(|g| g.name);
    let owner = n.parent.unwrap_or(node);
    let owner_node = world.node(owner);
    let scope = Scope::new(resolver).generics(names);
    if matches!(owner_node.kind, Kind::Function | Kind::Method | Kind::Macro | Kind::Constant) {
        scope
    } else {
        scope.owner(Target::Node(owner), owner_node.name.clone())
    }
}

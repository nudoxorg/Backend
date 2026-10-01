//! A page link keeps the fixture node's exact identity while its admitted
//! package outline loads. Its spelling is only a Find fallback, never a route.

use super::bodies::graph::identity::{IdentityAdapter, ResolvedSymbol};
use super::bodies::graph::open_value;
use crate::core::{Resource, VersionedRoot};
use crate::model::pages::{PackageDossier, PackageRef, SearchQuery};
use crate::navigation::Route;
use facet::graph::{NodeId, World};
use std::sync::Arc;

#[derive(Clone)]
pub(crate) struct Request {
    pub query: SearchQuery,
    pub package: Option<PackageRef>,
    pub node: NodeId,
    pub route: Route,
    pub root: VersionedRoot,
    pub generation: u64,
    world: Arc<World>,
    identities: Arc<IdentityAdapter>,
}

#[derive(Debug)]
pub(crate) enum Step {
    Waiting,
    Resolved(ResolvedSymbol),
    Find,
}

impl Request {
    pub(crate) fn new(
        node: NodeId,
        route: Route,
        root: VersionedRoot,
        generation: u64,
        world: Arc<World>,
        identities: Arc<IdentityAdapter>,
    ) -> Option<Self> {
        let name = world.nodes.get(node as usize)?.name.as_ref();
        let query = SearchQuery::new(name, 200).ok()?;
        let package = identities.package_for_node(node);
        Some(Self {
            query,
            package,
            node,
            route,
            root,
            generation,
            world,
            identities,
        })
    }

    /// A route event invalidates the generation even if Back later restores
    /// the same route. A changed fixture world cannot settle an old node ID.
    pub(crate) fn accepts(
        &self,
        generation: u64,
        route: &Route,
        root: VersionedRoot,
        current: Option<&(Arc<World>, Arc<IdentityAdapter>)>,
    ) -> bool {
        self.generation == generation
            && self.route == *route
            && self.root == root
            && current.is_some_and(|(world, identities)| {
                Arc::ptr_eq(&self.world, world) && Arc::ptr_eq(&self.identities, identities)
            })
    }

    /// Ranked search is only a contextual Find door. Exact navigation needs
    /// one admitted package identity and that package's complete outline.
    pub(crate) fn step(&self, resource: &Resource<PackageDossier>) -> Step {
        let Some(package) = &self.package else {
            return Step::Find;
        };
        let dossier = match open_value(resource, self.root) {
            Ok(Some(dossier)) => dossier,
            Ok(None) => return Step::Waiting,
            Err(_) => return Step::Find,
        };
        if &dossier.package != package {
            return Step::Find;
        }
        let Some(tree) = dossier.outline.known().filter(|tree| tree.complete) else {
            return Step::Find;
        };
        let Some(symbol) = self.identities.outline_symbol(self.node, package, tree) else {
            return Step::Find;
        };
        Step::Resolved(ResolvedSymbol {
            symbol,
            package: package.clone(),
            line: self
                .world
                .nodes
                .get(self.node as usize)
                .map(|node| node.line),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::pages::{DeclRef, Known, OutlineNode, OutlineTree, PackageRef};
    use facet::graph::{Kind, Module, Node, Package};

    fn fixture() -> (Arc<World>, Arc<IdentityAdapter>) {
        let mut node = Node::new(Kind::Struct, "Value", 0, 0);
        node.line = 7;
        let world = Arc::new(
            World::new(
                vec![Package {
                    name: "one".into(),
                    version: "1.0.0".into(),
                    yours: true,
                    external: false,
                    deps: vec![],
                }],
                vec![Module {
                    pkg: 0,
                    path: "".into(),
                    file: "src/lib.rs".into(),
                }],
                vec![node],
                vec![],
            )
            .expect("world"),
        );
        let identities = Arc::new(IdentityAdapter::synthetic(
            &world,
            PackageRef::parse("/fixture/one").expect("package"),
        ));
        (world, identities)
    }

    fn row(package: &str) -> DeclRef {
        let path = "src/lib.rs";
        DeclRef::from_label(
            &format!("{package}::{path}:7::Value"),
            None,
            None,
            Some((path, 7)),
        )
        .expect("decl")
    }

    fn page_at(
        root: VersionedRoot,
        package: &str,
        rows: Vec<DeclRef>,
        complete: bool,
    ) -> Resource<PackageDossier> {
        fn unknown<T>() -> Known<T> {
            Known::unknown(crate::model::pages::GapReason::NotCaptured, "test")
        }
        Resource::loaded_at(
            PackageDossier {
                package: PackageRef::parse(package).expect("package"),
                record: unknown(),
                versions: unknown(),
                dependencies: unknown(),
                dependents: unknown(),
                outline: Known::Known(OutlineTree {
                    roots: rows
                        .into_iter()
                        .map(|decl| OutlineNode {
                            decl,
                            children: Arc::from([]),
                        })
                        .collect::<Vec<_>>()
                        .into(),
                    complete,
                }),
                readme: unknown(),
            },
            root,
        )
    }

    fn page(root: VersionedRoot, rows: Vec<DeclRef>, complete: bool) -> Resource<PackageDossier> {
        page_at(root, "/fixture/one", rows, complete)
    }

    #[test]
    fn a_link_is_cancelled_by_any_visit_or_world_replacement() {
        let root = VersionedRoot::synthetic(
            backend_library::view_state_root(&[("symbol".to_owned(), "links".to_owned())]),
            1,
        );
        let (world, identities) = fixture();
        let request = Request::new(
            0,
            Route::World,
            root,
            4,
            Arc::clone(&world),
            Arc::clone(&identities),
        )
        .expect("link");
        let current = (world, identities);
        assert!(request.accepts(4, &Route::World, root, Some(&current)));
        assert!(
            !request.accepts(5, &Route::World, root, Some(&current)),
            "returning to the same route cannot revive a cancelled request"
        );
        assert!(!request.accepts(4, &Route::World, root.with_generation(2), Some(&current)));
        assert!(!request.accepts(4, &Route::World, root, None));
        let replacement = fixture();
        assert!(!request.accepts(4, &Route::World, root, Some(&replacement)));
    }

    #[test]
    fn a_partial_or_ambiguous_outline_cannot_open_even_its_only_exact_candidate() {
        let root = VersionedRoot::synthetic(
            backend_library::view_state_root(&[("symbol".to_owned(), "partial".to_owned())]),
            1,
        );
        let (world, identities) = fixture();
        let request = Request::new(0, Route::World, root, 1, world, identities).expect("link");
        let exact = row("/fixture/one");
        assert!(
            matches!(request.step(&Resource::not_yet()), Step::Waiting),
            "cold outline is not a missing symbol"
        );
        assert!(
            matches!(
                request.step(&page(root, vec![exact.clone()], false)),
                Step::Find
            ),
            "one exact row in a partial outline is not enough"
        );

        let (world, identities) = fixture();
        let request = Request::new(0, Route::World, root, 2, world, identities).expect("link");
        assert!(matches!(
            request.step(&page(root, vec![row("/other")], true)),
            Step::Find
        ));
        assert!(
            matches!(
                request.step(&page(root, vec![exact.clone(), exact], true)),
                Step::Find
            ),
            "ambiguous matches never choose a first row"
        );
    }

    #[test]
    fn only_a_complete_current_root_can_resolve_the_recorded_source() {
        let root = VersionedRoot::synthetic(
            backend_library::view_state_root(&[("symbol".to_owned(), "complete".to_owned())]),
            1,
        );
        let (world, identities) = fixture();
        let request = Request::new(0, Route::World, root, 1, world, identities).expect("link");
        let complete = page(root, vec![row("/fixture/one")], true);
        assert!(matches!(
            request.step(&page(root, vec![row("/fixture/one")], true).waiting()),
            Step::Waiting
        ));
        assert!(matches!(
            request.step(&page(
                root.with_generation(2),
                vec![row("/fixture/one")],
                true
            )),
            Step::Find
        ));
        match request.step(&complete) {
            Step::Resolved(resolved) => assert_eq!(resolved.package.as_str(), "/fixture/one"),
            other => panic!("expected exact source, got {other:?}"),
        }
    }

    #[test]
    fn cross_package_node_uses_its_admitted_package_outline() {
        let root = VersionedRoot::synthetic(
            backend_library::view_state_root(&[("symbol".to_owned(), "cross-package".to_owned())]),
            1,
        );
        let left = PackageRef::parse("/fixture/left").expect("left");
        let right = PackageRef::parse("/fixture/right").expect("right");
        let mut node = Node::new(Kind::Struct, "Value", 1, 1);
        node.line = 7;
        let world = Arc::new(
            World::new(
                vec![
                    Package {
                        name: "left".into(),
                        version: "1.0.0".into(),
                        yours: true,
                        external: false,
                        deps: vec![],
                    },
                    Package {
                        name: "right".into(),
                        version: "1.0.0".into(),
                        yours: true,
                        external: false,
                        deps: vec![],
                    },
                ],
                vec![
                    Module {
                        pkg: 0,
                        path: "".into(),
                        file: "src/lib.rs".into(),
                    },
                    Module {
                        pkg: 1,
                        path: "".into(),
                        file: "src/lib.rs".into(),
                    },
                ],
                vec![node],
                vec![],
            )
            .expect("world"),
        );
        let identities = Arc::new(IdentityAdapter::synthetic_catalog(
            &world,
            vec![left.clone(), right.clone()],
        ));
        let route = Route::Package(crate::navigation::PackageRoute {
            project: None,
            package: crate::core::PackageId::new(left.as_str()).expect("route package"),
            lane: crate::navigation::PackageLane::Overview,
            selected: None,
            at: None,
        });
        let request =
            Request::new(0, route, root, 1, world, identities).expect("cross-package link");
        assert_eq!(request.package.as_ref(), Some(&right));
        match request.step(&page_at(
            root,
            right.as_str(),
            vec![row(right.as_str())],
            true,
        )) {
            Step::Resolved(symbol) => assert_eq!(symbol.package, right),
            other => panic!("expected exact cross-package outline match, got {other:?}"),
        }
    }
}

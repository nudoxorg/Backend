//! A navigable graph surface.
//!
//! The graph data source will grow independently of this view. The important
//! contract here is already real: the surface owns a focused root, renders a
//! viewport, and every advertised graph action changes visible state.

use gpui::prelude::*;
use std::collections::BTreeSet;

use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable, IntoElement, Render, SharedString,
    Subscription, Window, div,
};
use gpui_component::{Icon, IconName, h_flex, v_flex};

use crate::app::actions::{ExpandNeighbors, FitGraphToView, PinGraphNode};
use crate::stores::events::PackagesChanged;
use crate::stores::package::{PackageAccess, PackageRow};
use crate::theme::ext::ThemeExtAccessor as _;
use crate::workspace::item::{NavEntry, WorkspaceItem};

/// Render-ready graph node derived from the corpus package snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
struct GraphNode {
    label: SharedString,
    is_package: bool,
}

fn graph_nodes(rows: &[PackageRow]) -> Vec<GraphNode> {
    let packages: BTreeSet<SharedString> = rows.iter().map(|row| row.name.clone()).collect();
    let dependencies: BTreeSet<SharedString> = rows
        .iter()
        .flat_map(|row| row.metadata.dependencies.iter())
        .map(|dependency| SharedString::from(dependency.clone()))
        .collect();

    packages
        .iter()
        .map(|name| GraphNode {
            label: name.clone(),
            is_package: true,
        })
        .chain(
            dependencies
                .difference(&packages)
                .cloned()
                .map(|label| GraphNode {
                    label,
                    is_package: false,
                }),
        )
        .collect()
}

/// The graph reader's focused, keyboard-navigable surface.
pub struct GraphView<P: PackageAccess> {
    _store: Entity<P>,
    nodes: Vec<GraphNode>,
    neighbors_expanded: bool,
    _sub: Subscription,
    focus: FocusHandle,
    fit_revision: u64,
    selected_pinned: bool,
}

impl<P: PackageAccess> GraphView<P> {
    pub fn new(store: Entity<P>, cx: &mut Context<Self>) -> Self {
        let nodes = graph_nodes(&store.read(cx).rows());
        let sub = cx.subscribe(&store, |view, store, _: &PackagesChanged, cx| {
            view.nodes = graph_nodes(&store.read(cx).rows());
            cx.notify();
        });
        Self {
            _store: store,
            nodes,
            neighbors_expanded: false,
            _sub: sub,
            focus: cx.focus_handle(),
            fit_revision: 0,
            selected_pinned: false,
        }
    }

    pub fn visible_nodes(&self) -> usize {
        self.nodes
            .iter()
            .filter(|node| self.neighbors_expanded || node.is_package)
            .count()
    }

    pub fn fit_revision(&self) -> u64 {
        self.fit_revision
    }

    pub fn selected_pinned(&self) -> bool {
        self.selected_pinned
    }

    fn fit(&mut self) {
        self.fit_revision += 1;
    }

    fn expand_neighbors(&mut self) {
        self.neighbors_expanded = true;
    }

    fn toggle_pin(&mut self) {
        self.selected_pinned = !self.selected_pinned;
    }
}

impl<P: PackageAccess> Focusable for GraphView<P> {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl<P: PackageAccess> WorkspaceItem for GraphView<P> {
    fn tab_content(&self, cx: &App) -> AnyElement {
        let colours = cx.theme_ext().colours;
        h_flex()
            .gap(cx.theme_ext().space.space_2)
            .child(Icon::new(IconName::Frame).text_color(colours.accent))
            .child("Graph")
            .into_any_element()
    }

    fn telemetry_id(&self) -> &'static str {
        "graph-view"
    }

    fn nav_entry(&self) -> Option<NavEntry> {
        Some(NavEntry {
            kind: "graph-view".into(),
            key: "corpus".into(),
            scroll_fraction: 0.0,
        })
    }
}

impl<P: PackageAccess> Render for GraphView<P> {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (sp, ts, colours) = {
            let ext = cx.theme_ext();
            (ext.space, ext.type_scale, ext.colours)
        };
        let nodes: Vec<GraphNode> = self
            .nodes
            .iter()
            .filter(|node| self.neighbors_expanded || node.is_package)
            .cloned()
            .collect();
        let node_count = nodes.len();
        let fit_revision = self.fit_revision;
        let pinned = self.selected_pinned;

        div()
            .id("graph.view")
            .track_focus(&self.focus)
            .key_context("GraphView")
            .on_action(cx.listener(|view, _: &FitGraphToView, _, cx| {
                view.fit();
                cx.notify();
            }))
            .on_action(cx.listener(|view, _: &ExpandNeighbors, _, cx| {
                view.expand_neighbors();
                cx.notify();
            }))
            .on_action(cx.listener(|view, _: &PinGraphNode, _, cx| {
                view.toggle_pin();
                cx.notify();
            }))
            .size_full()
            .p(sp.space_5)
            .flex()
            .flex_col()
            .gap(sp.space_3)
            .child(
                h_flex()
                    .gap(sp.space_2)
                    .child(
                        div()
                            .text_size(ts.title.size)
                            .text_color(colours.fg_default)
                            .child("Graph"),
                    )
                    .child(
                        div()
                            .text_size(ts.caption.size)
                            .text_color(colours.fg_muted)
                            .child(format!("{node_count} nodes · fit {fit_revision}")),
                    ),
            )
            .child(
                v_flex()
                    .id("graph.viewport")
                    .size_full()
                    .min_h(gpui::px(0.0))
                    .p(sp.space_5)
                    .gap(sp.space_2)
                    .rounded(sp.r_md)
                    .bg(colours.bg_sunken)
                    .border_1()
                    .border_color(colours.border_default)
                    .child(
                        div()
                            .text_size(ts.caption.size)
                            .text_color(colours.fg_faint)
                            .child(if pinned {
                                "Selected node pinned"
                            } else {
                                "Selected node follows layout"
                            }),
                    )
                    .children(nodes.into_iter().map(|node| {
                        h_flex()
                            .gap(sp.space_2)
                            .child(
                                div()
                                    .size(sp.space_3)
                                    .rounded(sp.space_3)
                                    .bg(colours.accent),
                            )
                            .child(
                                div()
                                    .text_size(ts.ui.size)
                                    .text_color(colours.fg_default)
                                    .child(node.label),
                            )
                    })),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{GraphView, graph_nodes};
    use crate::app::keymaps;
    use crate::motion::tokens::MotionTokens;
    use crate::stores::events::PackagesChanged;
    use crate::stores::package::{PackageAccess, PackageRow, PackageStatus};
    use crate::theme::ext::NudoxThemeExt;
    use gpui::{
        App, AppContext as _, EventEmitter, Focusable as _, SharedString, TestAppContext,
        WindowOptions,
    };
    use nudox_engine::wire::{
        EcosystemId, Gen, PackageLineageId, PackageName, VersionEvent, VersionList,
    };

    struct TestPackages {
        rows: Vec<PackageRow>,
    }

    impl PackageAccess for TestPackages {
        fn rows(&self) -> Vec<PackageRow> {
            self.rows.clone()
        }

        fn versions(&self, package: &PackageLineageId) -> VersionList {
            VersionList {
                package: package.clone(),
                versions: std::sync::Arc::from(vec![]),
            }
        }

        fn select_version(
            &self,
            package: PackageLineageId,
            version: &str,
            generation: Gen,
        ) -> flume::Receiver<VersionEvent> {
            let (tx, rx) = flume::bounded(1);
            let _ = tx.try_send(VersionEvent::NotLoaded {
                package,
                version: version.into(),
                generation,
            });
            rx
        }

        fn summary_label(&self) -> SharedString {
            SharedString::from(format!("{} packages", self.rows.len()))
        }
    }

    impl EventEmitter<PackagesChanged> for TestPackages {}

    fn row(name: &str, dependencies: &[&str]) -> PackageRow {
        PackageRow {
            name: SharedString::from(name),
            status: PackageStatus::Ready { symbols: 1 },
            metadata: nudox_engine::PackageMetadata {
                dependencies: dependencies.iter().map(|name| (*name).to_owned()).collect(),
                ..Default::default()
            },
            current_version: None,
            lineage: None,
            root: None,
        }
    }

    #[test]
    fn graph_nodes_are_projected_from_package_dependencies() {
        let nodes = graph_nodes(&[row("axum", &["tower"]), row("hyper", &["tower"])]);
        assert_eq!(
            nodes.into_iter().map(|node| node.label).collect::<Vec<_>>(),
            ["axum", "hyper", "tower"],
            "the graph labels must come from PackageStore rows and metadata",
        );
    }

    #[gpui::test]
    fn graph_keys_reach_the_focused_view(cx: &mut TestAppContext) {
        cx.update(|cx: &mut App| {
            gpui_component::init(cx);
            NudoxThemeExt::init(cx).expect("bundled themes parse");
            cx.set_global(MotionTokens::new(1.0));
            cx.bind_keys(keymaps::all_bindings());
        });
        let cell = std::sync::Arc::new(std::sync::Mutex::new(None));
        let cell_w = cell.clone();
        let window = cx
            .update(|cx: &mut App| {
                cx.open_window(WindowOptions::default(), move |_window, cx| {
                    let packages = cx.new(|_| TestPackages {
                        rows: vec![row("axum", &["tower", "hyper"])],
                    });
                    let view = cx.new(|cx| GraphView::new(packages, cx));
                    *cell_w.lock().unwrap() = Some(view.clone());
                    view
                })
            })
            .expect("graph window opens");
        let view = cell.lock().unwrap().take().expect("graph view exists");
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| {
            let focus = view.read(cx).focus_handle(cx);
            window.focus(&focus, cx);
        });
        vcx.run_until_parked();
        assert_eq!(
            view.read_with(&mut vcx, |view, _| {
                view.nodes
                    .iter()
                    .map(|node| node.label.clone())
                    .collect::<Vec<_>>()
            }),
            ["axum", "hyper", "tower"],
        );
        vcx.simulate_keystrokes("e");
        vcx.run_until_parked();
        assert_eq!(view.read_with(&mut vcx, |view, _| view.visible_nodes()), 3);
    }
}

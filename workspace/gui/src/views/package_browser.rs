//! A navigable package/module browser.

use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable, IntoElement, Render, SharedString,
    Subscription, Window, div, prelude::*,
};
use gpui_component::{Icon, IconName, h_flex, v_flex};

use crate::{
    app::actions::{CollapseTreeNode, DiffAgainstPrevious, ExpandTreeNode, SwitchFocusTreeTable},
    stores::{
        events::PackagesChanged,
        package::{PackageAccess, PackageRow, PackageStatus},
    },
    theme::ext::ThemeExtAccessor as _,
    workspace::item::{NavEntry, WorkspaceItem},
};

/// Which half of the browser currently owns keyboard navigation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowserFocus {
    Tree,
    Table,
}

/// Package metadata and its module tree in one centre-pane item.
pub struct PackageBrowser<P: PackageAccess> {
    _store: Entity<P>,
    packages: Vec<BrowserPackage>,
    summary: SharedString,
    _sub: Subscription,
    focus: FocusHandle,
    tree_expanded: bool,
    browser_focus: BrowserFocus,
    diff_requested: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct BrowserPackage {
    name: SharedString,
    detail: SharedString,
    table_label: SharedString,
}

fn package_rows(rows: &[PackageRow]) -> Vec<BrowserPackage> {
    rows.iter()
        .map(|row| {
            let detail = match &row.status {
                PackageStatus::Pending => SharedString::from("Loading"),
                PackageStatus::Ready { symbols } => {
                    SharedString::from(format!("{symbols} symbols"))
                }
                PackageStatus::Failed { message } => message.clone(),
            };
            BrowserPackage {
                table_label: SharedString::from(format!("{} · {}", row.name, detail)),
                name: row.name.clone(),
                detail,
            }
        })
        .collect()
}

impl<P: PackageAccess> PackageBrowser<P> {
    pub fn new(store: Entity<P>, cx: &mut Context<Self>) -> Self {
        let (packages, summary) = {
            let store = store.read(cx);
            (package_rows(&store.rows()), store.summary_label())
        };
        let sub = cx.subscribe(&store, |view, store, _: &PackagesChanged, cx| {
            let store = store.read(cx);
            view.packages = package_rows(&store.rows());
            view.summary = store.summary_label();
            cx.notify();
        });
        Self {
            _store: store,
            packages,
            summary,
            _sub: sub,
            focus: cx.focus_handle(),
            tree_expanded: true,
            browser_focus: BrowserFocus::Tree,
            diff_requested: false,
        }
    }

    pub fn tree_expanded(&self) -> bool {
        self.tree_expanded
    }

    pub fn browser_focus(&self) -> BrowserFocus {
        self.browser_focus
    }

    pub fn diff_requested(&self) -> bool {
        self.diff_requested
    }

    fn collapse(&mut self) {
        self.tree_expanded = false;
    }

    fn expand(&mut self) {
        self.tree_expanded = true;
    }

    fn toggle_focus(&mut self) {
        self.browser_focus = match self.browser_focus {
            BrowserFocus::Tree => BrowserFocus::Table,
            BrowserFocus::Table => BrowserFocus::Tree,
        };
    }

    fn request_diff(&mut self) {
        self.diff_requested = true;
    }
}

impl<P: PackageAccess> Focusable for PackageBrowser<P> {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl<P: PackageAccess> WorkspaceItem for PackageBrowser<P> {
    fn tab_content(&self, _: &App) -> AnyElement {
        h_flex()
            .gap(gpui::px(8.0))
            .child(Icon::new(IconName::Inbox))
            .child("Package")
            .into_any_element()
    }

    fn telemetry_id(&self) -> &'static str {
        "package-browser"
    }

    fn nav_entry(&self) -> Option<NavEntry> {
        Some(NavEntry {
            kind: "package-browser".into(),
            key: "corpus".into(),
            scroll_fraction: 0.0,
        })
    }
}

impl<P: PackageAccess> Render for PackageBrowser<P> {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (sp, ts, colours) = {
            let ext = cx.theme_ext();
            (ext.space, ext.type_scale, ext.colours)
        };
        let expanded = self.tree_expanded;
        let focus = self.browser_focus;
        let diff = self.diff_requested;
        let packages = self.packages.clone();
        let summary = self.summary.clone();

        div()
            .id("package.browser")
            .track_focus(&self.focus)
            .key_context("PackageBrowser")
            .on_action(cx.listener(|view, _: &DiffAgainstPrevious, _, cx| {
                view.request_diff();
                cx.notify();
            }))
            .on_action(cx.listener(|view, _: &CollapseTreeNode, _, cx| {
                view.collapse();
                cx.notify();
            }))
            .on_action(cx.listener(|view, _: &ExpandTreeNode, _, cx| {
                view.expand();
                cx.notify();
            }))
            .on_action(cx.listener(|view, _: &SwitchFocusTreeTable, _, cx| {
                view.toggle_focus();
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
                            .child("Package browser"),
                    )
                    .child(
                        div()
                            .text_size(ts.caption.size)
                            .text_color(colours.fg_muted)
                            .child(if diff {
                                "Comparison ready".into()
                            } else {
                                summary
                            }),
                    ),
            )
            .child(
                h_flex()
                    .size_full()
                    .min_h(gpui::px(0.0))
                    .gap(sp.space_3)
                    .child(
                        v_flex()
                            .id("package.browser.tree")
                            .w(gpui::px(240.0))
                            .h_full()
                            .p(sp.space_3)
                            .gap(sp.space_2)
                            .bg(colours.bg_sunken)
                            .border_1()
                            .border_color(if focus == BrowserFocus::Tree {
                                colours.accent
                            } else {
                                colours.border_default
                            })
                            .child("Packages")
                            .when(expanded, |tree| {
                                tree.children(packages.iter().map(|package| {
                                    h_flex().gap(sp.space_2).child(package.name.clone()).child(
                                        div()
                                            .text_size(ts.caption.size)
                                            .text_color(colours.fg_muted)
                                            .child(package.detail.clone()),
                                    )
                                }))
                            }),
                    )
                    .child(
                        v_flex()
                            .id("package.browser.table")
                            .flex_1()
                            .h_full()
                            .p(sp.space_3)
                            .gap(sp.space_2)
                            .bg(colours.bg_raised)
                            .border_1()
                            .border_color(if focus == BrowserFocus::Table {
                                colours.accent
                            } else {
                                colours.border_default
                            })
                            .child("Symbols")
                            .children(packages.iter().map(|package| package.table_label.clone())),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{BrowserFocus, PackageBrowser, package_rows};
    use crate::{
        app::keymaps,
        motion::tokens::MotionTokens,
        stores::{
            events::PackagesChanged,
            package::{PackageAccess, PackageRow, PackageStatus},
        },
        theme::ext::NudoxThemeExt,
    };
    use gpui::{
        App, AppContext as _, EventEmitter, Focusable as _, SharedString, TestAppContext,
        WindowOptions,
    };
    use nudox_engine::wire::{Gen, PackageLineageId, VersionEvent, VersionList};

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

    fn row(name: &str, status: PackageStatus) -> PackageRow {
        PackageRow {
            name: SharedString::from(name),
            status,
            metadata: nudox_engine::PackageMetadata::default(),
            current_version: None,
            lineage: None,
            root: None,
        }
    }

    #[test]
    fn package_browser_rows_are_projected_from_store_rows() {
        let packages = package_rows(&[
            row("axum", PackageStatus::Ready { symbols: 4_220 }),
            row("tower", PackageStatus::Failed {
                message: SharedString::from("producer unavailable"),
            }),
        ]);
        assert_eq!(packages[0].name, "axum");
        assert_eq!(packages[0].detail, "4220 symbols");
        assert_eq!(packages[1].table_label, "tower · producer unavailable");
    }

    #[gpui::test]
    fn package_keys_reach_the_focused_view(cx: &mut TestAppContext) {
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
                        rows: vec![row("axum", PackageStatus::Ready { symbols: 4_220 })],
                    });
                    let view = cx.new(|cx| PackageBrowser::new(packages, cx));
                    *cell_w.lock().unwrap() = Some(view.clone());
                    view
                })
            })
            .expect("package browser window opens");
        let view = cell.lock().unwrap().take().expect("package browser exists");
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| {
            let focus = view.read(cx).focus_handle(cx);
            window.focus(&focus, cx);
        });
        vcx.run_until_parked();
        assert_eq!(
            view.read_with(&mut vcx, |view, _| view.packages[0].table_label.clone()),
            "axum · 4220 symbols",
        );

        vcx.simulate_keystrokes("d");
        vcx.simulate_keystrokes("tab");
        vcx.simulate_keystrokes("left");
        vcx.run_until_parked();

        view.read_with(&mut vcx, |view, _| {
            assert!(view.diff_requested());
            assert_eq!(view.browser_focus(), BrowserFocus::Table);
            assert!(!view.tree_expanded());
        });
    }
}

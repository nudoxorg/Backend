//! Native, local-first package and documentation browser.
//!
//! The desktop surface follows the mental model of crates.io and docs.rs: the
//! package is the home, documentation is a deep link from that package, and
//! source is another view of the same immutable indexed object.  There is no
//! administration shell in this module.  The local service remains the
//! authority; this file only projects its current view root into a fast,
//! bounded native view.

use crate::{DesktopHost, HostMode, Model, UnixSubscriptionTransport};
use backend_client::Session;
use backend_library::{PackageReference, SemanticVersionRecord, ViewRoot, encode_id};
use gpui::{
    Animation, AnimationExt, App, Bounds, Context, Entity, Focusable, Hsla, KeyBinding,
    Orientation, Role, ScrollStrategy, SharedString, Subscription, Task, UniformListScrollHandle,
    Window, WindowBounds, WindowOptions, actions, div, ease_out_quint, prelude::*, px, rgb, size,
    uniform_list,
};
use gpui_elements::editable_text::{
    EditableTextState, StringStorage, TextChanged,
    actions::{DEFAULT_INPUT_CONTEXT, default_bindings},
    text_input,
};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

mod bootstrap;
mod browse;
mod catalog;
mod docs;
mod package;
mod package_metadata;
mod search;
mod shell;

pub(crate) use bootstrap::run;
use search::search_catalog;
use shell::{back_button, badge, code_block, doc_block, section_heading, side_card, version_card};

#[cfg(test)]
use catalog::language_for;
use catalog::{Catalog, DocBlock, DocRow, SourceLink, SourcePreview};

const MAX_SOURCE_BYTES: usize = 1024 * 1024;
const SOURCE_CONTEXT_LINES: u32 = 22;

actions!(nudox, [FocusGlobalSearch]);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Route {
    Package,
    Docs,
    Search,
    Source,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PackageTab {
    Readme,
    Code,
    Versions,
    Dependencies,
    Dependents,
    Security,
}

impl PackageTab {
    const ALL: [Self; 6] = [
        Self::Readme,
        Self::Code,
        Self::Versions,
        Self::Dependencies,
        Self::Dependents,
        Self::Security,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Readme => "Readme",
            Self::Code => "Code",
            Self::Versions => "Versions",
            Self::Dependencies => "Dependencies",
            Self::Dependents => "Dependents",
            Self::Security => "Security",
        }
    }
}

struct NativeApp {
    _host: DesktopHost,
    endpoint: PathBuf,
    project_path: PathBuf,
    mode: HostMode,
    root: ViewRoot,
    catalog: Catalog,
    visible: Vec<usize>,
    selected: Option<SharedString>,
    docs_filter: Option<SharedString>,
    route: Route,
    package_tab: PackageTab,
    search: Entity<EditableTextState>,
    status: SharedString,
    source_preview: Option<SourcePreview>,
    source_loading: bool,
    source_error: Option<SharedString>,
    source_task: Option<Task<()>>,
    search_task: Option<Task<()>>,
    semantic_versions: Arc<[SemanticVersionRecord]>,
    semantic_versions_loading: bool,
    semantic_versions_error: Option<SharedString>,
    semantic_version_task: Option<Task<()>>,
    source_scroll: UniformListScrollHandle,
    _index_task: Task<()>,
    _search_subscription: Subscription,
    _live_task: Task<()>,
}

impl NativeApp {
    fn new(
        host: DesktopHost,
        root: ViewRoot,
        model: Model,
        transport: UnixSubscriptionTransport,
        cx: &mut Context<Self>,
    ) -> Self {
        let endpoint = host.endpoint().to_path_buf();
        let project_path = host.project().to_path_buf();
        let mode = host.mode();
        let catalog = Catalog::from_root(&root, &project_path);
        let selected = None;
        let is_empty = root.row_count() == 0;
        let visible = catalog.order.clone();
        let search = cx.new(|cx| EditableTextState::new(StringStorage::default(), cx));
        let search_subscription =
            cx.subscribe(&search, |this: &mut Self, input, _: &TextChanged, cx| {
                let query = input.read(cx).as_str().to_owned();
                this.route = if query.is_empty() {
                    if this.route == Route::Search {
                        Route::Package
                    } else {
                        this.route
                    }
                } else {
                    Route::Search
                };
                this.search_index(&query, cx);
            });
        let index_endpoint = endpoint.clone();
        let index_project = project_path.clone();
        let index_task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let mut session = Session::connect(index_endpoint)
                        .map_err(|error| format!("connect local index: {error}"))?;
                    session
                        .index(&index_project.to_string_lossy())
                        .map_err(|error| format!("index project: {error}"))?;
                    session
                        .health()
                        .map_err(|error| format!("read index readiness: {error}"))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.status = match result {
                    Ok(_) if this.root.row_count() == 0 => {
                        "Index committed · preparing documentation…".into()
                    }
                    Ok(readiness) => format!(
                        "Indexed locally · owner reports {} rows",
                        readiness.row_count()
                    )
                    .into(),
                    Err(error) => format!("Index unavailable · {error}").into(),
                };
                cx.notify();
            });
        });
        let live_task = Self::live_updates(model, transport, endpoint.clone(), cx);
        Self {
            _host: host,
            endpoint,
            project_path,
            mode,
            root,
            catalog,
            visible,
            selected,
            docs_filter: None,
            route: Route::Package,
            package_tab: PackageTab::Readme,
            search,
            status: if is_empty {
                "Opening package · indexing locally…".into()
            } else {
                "Indexed locally · refreshing…".into()
            },
            source_preview: None,
            source_loading: false,
            source_error: None,
            source_task: None,
            search_task: None,
            semantic_versions: Arc::from([]),
            semantic_versions_loading: false,
            semantic_versions_error: None,
            semantic_version_task: None,
            source_scroll: UniformListScrollHandle::new(),
            _index_task: index_task,
            _search_subscription: search_subscription,
            _live_task: live_task,
        }
    }

    fn live_updates(
        mut model: Model,
        mut transport: UnixSubscriptionTransport,
        endpoint: PathBuf,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        const LEASE_MS: u64 = 30_000;
        cx.spawn(async move |this, cx| {
            let mut delay = Duration::from_millis(80);
            let mut had_error = false;
            let mut lease = None;
            loop {
                let before = model.root().version();
                let (next_model, next_transport, next_lease, result) = cx
                    .background_spawn(async move {
                        let response = if let Some(lease) = lease {
                            if model.snapshot_pending() {
                                transport.snapshot_page(
                                    lease,
                                    model.snapshot_next_token().unwrap_or_default().to_vec(),
                                    model.credit(),
                                )
                            } else {
                                transport.resume_lease(
                                    lease,
                                    model.cursor(),
                                    model.credit(),
                                    LEASE_MS,
                                )
                            }
                        } else {
                            transport.open_lease(Some(model.cursor()), model.credit(), LEASE_MS)
                        };
                        let result = response.and_then(|response| {
                            let phase = match &response {
                                backend_replication::LocalSubscriptionResponse::Opened {
                                    ..
                                }
                                | backend_replication::LocalSubscriptionResponse::Resumed {
                                    ..
                                } => "current",
                                backend_replication::LocalSubscriptionResponse::Batch {
                                    ..
                                } => "delta",
                                backend_replication::LocalSubscriptionResponse::SnapshotPage {
                                    ..
                                }
                                | backend_replication::LocalSubscriptionResponse::ResetWithRoot {
                                    ..
                                } => "snapshot",
                                backend_replication::LocalSubscriptionResponse::Acked {
                                    ..
                                }
                                | backend_replication::LocalSubscriptionResponse::Renewed {
                                    ..
                                }
                                | backend_replication::LocalSubscriptionResponse::Cancelled {
                                    ..
                                } => "control",
                            };
                            let response_lease = response.lease();
                            if let Some(peer) = transport.authenticated_peer() {
                                model.reduce_lease_response_with_verifier(response, peer)?;
                            } else {
                                let capability = model.root().capability();
                                model.reduce_lease_response(response, capability)?;
                            }
                            lease = Some(response_lease);
                            Ok(phase)
                        });
                        (model, transport, lease, result)
                    })
                    .await;
                model = next_model;
                transport = next_transport;
                lease = next_lease;
                let changed = before != model.root().version();
                let hydrating = model.snapshot_pending();
                let recovered = result.is_ok() && had_error;
                let phase = result.as_ref().ok().copied();
                let live_root = model.root().clone();
                had_error = result.is_err();
                let error = result.err().map(|error| error.to_string());
                if this
                    .update(cx, |this, cx| {
                        // Compare the entity's admitted revision too. A native
                        // window can coalesce the first background repaint
                        // while it is being installed; the next lease poll
                        // must repair that missed handoff even when the wire
                        // model itself has not advanced again.
                        if this.root.version() != live_root.version() {
                            this.install_root(live_root, cx);
                            this.status = "Indexed locally · live".into();
                        } else if hydrating {
                            this.status = "Loading indexed documentation…".into();
                            cx.notify();
                        } else if let Some(error) = error {
                            this.status = format!("Live feed paused · {error}").into();
                            cx.notify();
                        } else if recovered {
                            this.status = "Live connection restored · waiting for index…".into();
                            cx.notify();
                        } else if phase == Some("delta") || phase == Some("snapshot") {
                            this.status = format!("Applying indexed {phase:?}…").into();
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
                delay = if hydrating {
                    Duration::from_millis(1)
                } else if changed || recovered {
                    Duration::from_millis(80)
                } else {
                    (delay * 2).min(Duration::from_secs(2))
                };
                if had_error {
                    model.abort_snapshot();
                    lease = None;
                    if let Ok(reconnected) = UnixSubscriptionTransport::connect(&endpoint) {
                        transport = reconnected;
                    }
                }
                cx.background_executor().timer(delay).await;
            }
        })
    }

    fn install_root(&mut self, root: ViewRoot, cx: &mut Context<Self>) {
        self.root = root;
        self.catalog = Catalog::from_root(&self.root, &self.project_path);
        let query = self.search.read(cx).as_str().to_owned();
        self.search_index(&query, cx);
        if self
            .selected
            .as_ref()
            .is_some_and(|key| self.catalog.get(key).is_none())
        {
            self.selected = None;
        }
        if self.package_tab == PackageTab::Versions {
            self.load_semantic_versions(cx);
            return;
        }
        cx.notify();
    }

    fn search_index(&mut self, query: &str, cx: &mut Context<Self>) {
        let query = query.trim();
        if query.is_empty() {
            self.search_task = None;
            self.visible.clone_from(&self.catalog.order);
            self.status = "Indexed locally · live".into();
            cx.notify();
            return;
        }
        let endpoint = self.endpoint.clone();
        let requested = query.to_owned();
        self.status = "Searching indexed documentation…".into();
        self.search_task = Some(cx.spawn(async move |this, cx| {
            let search_query = requested.clone();
            let result = cx
                .background_spawn(async move { search_catalog(&endpoint, &search_query) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.search.read(cx).as_str().trim() != requested {
                    return;
                }
                match result {
                    Ok(ids) => {
                        this.visible = this.catalog.indices_for_ids(&ids);
                        this.status = format!(
                            "{} indexed result{} · immutable local view",
                            this.visible.len(),
                            if this.visible.len() == 1 { "" } else { "s" }
                        )
                        .into();
                    }
                    Err(error) => {
                        this.visible.clear();
                        this.status = format!("Search unavailable · {error}").into();
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn selected_row(&self) -> Option<DocRow> {
        self.selected
            .as_ref()
            .and_then(|key| self.catalog.get(key))
            .cloned()
    }

    fn choose(&mut self, key: SharedString, cx: &mut Context<Self>) {
        self.selected = Some(key);
        self.docs_filter = None;
        self.route = Route::Docs;
        self.source_preview = None;
        self.source_error = None;
        cx.notify();
    }

    fn set_package_tab(&mut self, tab: PackageTab, cx: &mut Context<Self>) {
        self.package_tab = tab;
        self.route = Route::Package;
        if tab == PackageTab::Versions {
            self.load_semantic_versions(cx);
            return;
        }
        if tab == PackageTab::Code
            && self.source_preview.is_none()
            && let Some(source) = self
                .catalog
                .order
                .iter()
                .filter_map(|index| self.catalog.rows.get(*index))
                .find_map(|row| row.source.clone())
        {
            self.load_source_preview(&source, Route::Package, cx);
            return;
        }
        cx.notify();
    }

    fn semantic_package(&self) -> Result<PackageReference, String> {
        PackageReference::parse(self.project_path.to_string_lossy().into_owned())
            .map_err(|error| format!("local package identity: {error}"))
    }

    fn load_semantic_versions(&mut self, cx: &mut Context<Self>) {
        let endpoint = self.endpoint.clone();
        let package = match self.semantic_package() {
            Ok(package) => package,
            Err(error) => {
                self.semantic_versions_error = Some(error.into());
                cx.notify();
                return;
            }
        };
        self.semantic_versions_loading = true;
        self.semantic_versions_error = None;
        self.semantic_version_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let mut session = Session::connect(endpoint)
                        .map_err(|error| format!("connect semantic history: {error}"))?;
                    session
                        .semantic_versions(package)
                        .map_err(|error| format!("read semantic history: {error}"))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.semantic_versions_loading = false;
                match result {
                    Ok(mut records) => {
                        records.sort_by(|left, right| {
                            right
                                .selected
                                .cmp(&left.selected)
                                .then_with(|| left.coordinate.cmp(&right.coordinate))
                                .then_with(|| left.profile.cmp(&right.profile))
                                .then_with(|| right.generation.cmp(&left.generation))
                        });
                        this.semantic_versions = records.into();
                        this.semantic_versions_error = None;
                    }
                    Err(error) => {
                        this.semantic_versions = Arc::from([]);
                        this.semantic_versions_error = Some(error.into());
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn select_semantic_version(&mut self, record: SemanticVersionRecord, cx: &mut Context<Self>) {
        let endpoint = self.endpoint.clone();
        self.semantic_versions_loading = true;
        self.semantic_versions_error = None;
        self.status = "Selecting immutable compiler generation…".into();
        self.semantic_version_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let mut session = Session::connect(endpoint)
                        .map_err(|error| format!("connect semantic selection: {error}"))?;
                    session
                        .select_semantic_version(
                            record.package,
                            record.coordinate,
                            record.profile,
                            record.generation,
                        )
                        .map_err(|error| format!("select semantic generation: {error}"))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.semantic_versions_loading = false;
                match result {
                    Ok(selected) => {
                        let selected_id = selected.generation;
                        this.semantic_versions = this
                            .semantic_versions
                            .iter()
                            .cloned()
                            .map(|mut record| {
                                record.selected = record.generation == selected_id;
                                record
                            })
                            .collect::<Vec<_>>()
                            .into();
                        this.status = "Immutable compiler generation selected · live".into();
                    }
                    Err(error) => {
                        this.semantic_versions_error = Some(error.clone().into());
                        this.status = format!("Semantic selection unavailable · {error}").into();
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn open_source_preview(&mut self, source: &SourceLink, cx: &mut Context<Self>) {
        self.load_source_preview(source, Route::Source, cx);
    }

    fn open_code_preview(&mut self, source: &SourceLink, cx: &mut Context<Self>) {
        self.package_tab = PackageTab::Code;
        self.load_source_preview(source, Route::Package, cx);
    }

    fn load_source_preview(
        &mut self,
        source: &SourceLink,
        destination: Route,
        cx: &mut Context<Self>,
    ) {
        let path = source.path.clone();
        let display_path = source.display_path.clone();
        let line = source.line;
        self.route = destination;
        self.source_loading = true;
        self.source_error = None;
        self.source_preview = None;
        let read_path = path.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { read_source(&read_path) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.source_loading = false;
                match result {
                    Ok(text) => {
                        let lines = text
                            .lines()
                            .map(|line| SharedString::from(line.to_owned()))
                            .collect::<Vec<_>>()
                            .into();
                        this.source_preview = Some(SourcePreview {
                            path: display_path,
                            lines,
                            line,
                        });
                        this.source_scroll.scroll_to_item_strict(
                            usize::try_from(line.saturating_sub(1)).unwrap_or(usize::MAX),
                            ScrollStrategy::Center,
                        );
                    }
                    Err(error) => this.source_error = Some(error.into()),
                }
                cx.notify();
            });
        });
        self.source_task = Some(task);
        cx.notify();
    }

    fn package_name(&self) -> SharedString {
        self.catalog.package_name.clone()
    }

    fn focus_global_search(
        &mut self,
        _: &FocusGlobalSearch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.search.focus_handle(cx), cx);
    }

    fn open_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.route = Route::Search;
        window.focus(&self.search.focus_handle(cx), cx);
        cx.notify();
    }

    fn top_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("app-banner")
            .role(Role::Banner)
            .aria_label("Nudox package navigation")
            .h(px(68.0))
            .flex_none()
            .px_6()
            .flex()
            .items_center()
            .gap_4()
            .border_b_1()
            .border_color(rgb(0x0027_303d))
            .bg(rgb(0x000d_1117))
            .child(
                div()
                    .id("brand")
                    .role(Role::Button)
                    .aria_label("Open package overview")
                    .flex()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.route = Route::Package;
                        this.package_tab = PackageTab::Readme;
                        cx.notify();
                    }))
                    .child(
                        div()
                            .w(px(28.0))
                            .h(px(28.0))
                            .rounded_lg()
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(rgb(0x00f5_9e0b))
                            .text_color(rgb(0x0017_120a))
                            .font_weight(gpui::FontWeight::BOLD)
                            .child("N"),
                    )
                    .child(
                        div()
                            .text_size(px(18.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("nudox"),
                    ),
            )
            .child(div().h(px(28.0)).w(px(1.0)).bg(rgb(0x002c_3544)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(px(14.0))
                    .text_color(rgb(0x00c4_ccda))
                    .child(self.package_name())
                    .child(badge("local", 0x0020_3c31, 0x008e_e1b7)),
            )
            .child(div().flex_1())
            .child(
                text_input("global-search")
                    .state(self.search.downgrade())
                    .role(Role::SearchInput)
                    .aria_label("Search packages and symbols")
                    .aria_description("Search the current package and indexed documentation")
                    .aria_placeholder("Search packages and symbols")
                    .aria_value(self.search.read(cx).as_str().to_owned())
                    .caret_blink_interval_500ms()
                    .placeholder("Search packages and symbols…  ⌘K")
                    .w(px(430.0))
                    .px_3()
                    .py_2()
                    .rounded_full()
                    .border_1()
                    .border_color(Hsla::from(rgb(0x0039_465a)))
                    .bg(rgb(0x0015_1b24))
                    .whitespace_nowrap(),
            )
    }

    fn docs_top_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let package = self.package_name();
        div()
            .id("docs-banner")
            .role(Role::Banner)
            .aria_label("Documentation navigation")
            .h(px(34.0))
            .flex_none()
            .flex()
            .items_center()
            .border_b_1()
            .border_color(rgb(0x0050_5050))
            .bg(rgb(0x0030_3030))
            .text_size(px(12.0))
            .child(
                div()
                    .id("docs-home")
                    .role(Role::Button)
                    .aria_label("Open package overview")
                    .h_full()
                    .px_4()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_r_1()
                    .border_color(rgb(0x0050_5050))
                    .cursor_pointer()
                    .hover(|item| item.bg(rgb(0x003a_3a3a)))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.route = Route::Package;
                        this.package_tab = PackageTab::Readme;
                        cx.notify();
                    }))
                    .child("⬡ NUDOX.DOCS"),
            )
            .child(
                div()
                    .h_full()
                    .px_4()
                    .flex()
                    .items_center()
                    .border_r_1()
                    .border_color(rgb(0x0050_5050))
                    .child(format!("{}-{}  ▼", package, self.catalog.package.version)),
            )
            .child(
                div()
                    .h_full()
                    .px_4()
                    .flex()
                    .items_center()
                    .border_r_1()
                    .border_color(rgb(0x0050_5050))
                    .child("⚙ Platform  ▼"),
            )
            .child(
                div()
                    .h_full()
                    .px_4()
                    .flex()
                    .items_center()
                    .border_r_1()
                    .border_color(rgb(0x0050_5050))
                    .child("⚑ Feature flags"),
            )
            .child(div().flex_1())
            .child(
                div()
                    .h_full()
                    .px_4()
                    .flex()
                    .items_center()
                    .border_l_1()
                    .border_color(rgb(0x0050_5050))
                    .child("docs  ▼"),
            )
            .child(
                div()
                    .h_full()
                    .px_4()
                    .flex()
                    .items_center()
                    .border_l_1()
                    .border_color(rgb(0x0050_5050))
                    .child("Rust  ▼"),
            )
            .child(
                text_input("docs-search")
                    .state(self.search.downgrade())
                    .role(Role::SearchInput)
                    .aria_label("Find package or item")
                    .aria_placeholder("Find crate")
                    .caret_blink_interval_500ms()
                    .placeholder("⌕  Find crate")
                    .w(px(150.0))
                    .h_full()
                    .px_3()
                    .bg(rgb(0x0030_3030)),
            )
    }

    fn package_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut tabs = div()
            .id("package-tabs")
            .role(Role::TabList)
            .aria_label("Package views")
            .aria_orientation(Orientation::Horizontal)
            .h(px(48.0))
            .flex_none()
            .px_8()
            .flex()
            .items_center()
            .gap_1()
            .border_b_1()
            .border_color(rgb(0x0065_6563))
            .bg(rgb(0x0030_302f));
        for tab in PackageTab::ALL {
            let active = self.route == Route::Package && self.package_tab == tab;
            tabs = tabs.child(
                div()
                    .id(format!("package-tab-{}", tab.label()))
                    .role(Role::Tab)
                    .aria_label(tab.label())
                    .aria_selected(active)
                    .h_full()
                    .px_3()
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .text_size(px(13.0))
                    .text_color(if active {
                        rgb(0x00ff_d18a)
                    } else {
                        rgb(0x009d_a8ba)
                    })
                    .when(active, |item| {
                        item.border_b_1().border_color(rgb(0x00f5_9e0b))
                    })
                    .hover(|item| item.text_color(rgb(0x00f7_f9fc)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_package_tab(tab, cx);
                    }))
                    .child(tab.label()),
            );
        }
        tabs.child(div().flex_1()).child(
            div()
                .id("documentation-tab")
                .role(Role::Tab)
                .aria_label("Documentation")
                .aria_selected(self.route == Route::Docs)
                .px_3()
                .py_2()
                .rounded_lg()
                .cursor_pointer()
                .text_size(px(13.0))
                .text_color(if self.route == Route::Docs {
                    rgb(0x00ff_d18a)
                } else {
                    rgb(0x009d_a8ba)
                })
                .when(self.route == Route::Docs, |item| item.bg(rgb(0x002a_2116)))
                .hover(|item| item.bg(rgb(0x0021_1d18)))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.route = Route::Docs;
                    this.selected = None;
                    this.docs_filter = None;
                    cx.notify();
                }))
                .child("Documentation"),
        )
    }
}

fn read_source(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("Unable to read source: {error}"))?;
    let truncated = bytes.len() > MAX_SOURCE_BYTES;
    let mut text =
        String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_SOURCE_BYTES)]).into_owned();
    if truncated {
        text.push_str("\n\n// source preview truncated at 1 MiB");
    }
    Ok(text)
}

fn source_lines(lines: &[SharedString], focus: u32) -> Vec<(u32, SharedString)> {
    let total = u32::try_from(lines.len()).unwrap_or(u32::MAX);
    let start = focus.saturating_sub(SOURCE_CONTEXT_LINES).max(1);
    let end = focus
        .saturating_add(SOURCE_CONTEXT_LINES)
        .min(total.max(focus));
    lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            u32::try_from(index)
                .ok()
                .and_then(|index| index.checked_add(1))
                .map(|index| (index, line.clone()))
        })
        .filter(|(line, _)| *line >= start && *line <= end)
        .collect()
}

fn source_text_color(line: &str) -> Hsla {
    let trimmed = line.trim_start();
    if trimmed.starts_with("//")
        || trimmed.starts_with('#')
        || trimmed.starts_with("/*")
        || trimmed.starts_with('*')
    {
        Hsla::from(rgb(0x009a_a587))
    } else {
        Hsla::from(rgb(0x00c3_d1e4))
    }
}

fn revision_short(bytes: &[u8; 32]) -> String {
    let encoded = encode_id(bytes);
    encoded.chars().take(12).collect()
}

fn host_mode_label(mode: HostMode) -> &'static str {
    match mode {
        HostMode::Embedded => "embedded owner",
        HostMode::Attached => "attached owner",
    }
}

fn short_coordinate(coordinate: &str) -> String {
    coordinate
        .rsplit_once("::")
        .map_or_else(|| coordinate.to_owned(), |(_, tail)| tail.to_owned())
}

/// Build a deterministic, compact suffix for an accessibility element id.
///
/// AccessKit uses GPUI's composed element ids to preserve node identity across
/// frames.  Content-derived ids must therefore be stable without depending on
/// a process-randomized hasher or on list position.
fn a11y_key(value: &str) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::{language_for, source_lines};
    use gpui::SharedString;

    #[test]
    fn language_projection_covers_product_rows() {
        assert_eq!(language_for("pkg::src/main.ts:3::turing"), "TypeScript");
        assert_eq!(language_for("pkg::src/lib.rs:2::Ferris"), "Rust");
    }

    #[test]
    fn source_window_keeps_focus_and_bounds_memory() {
        let text = (1..100)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .into_iter()
            .map(SharedString::from)
            .collect::<Vec<_>>();
        let lines = source_lines(&text, 50);
        assert_eq!(lines.first().map(|(line, _)| *line), Some(28));
        assert_eq!(lines.last().map(|(line, _)| *line), Some(72));
        assert!(lines.iter().any(|(line, _)| *line == 50));
    }
}

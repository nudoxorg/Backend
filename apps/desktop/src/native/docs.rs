//! docs.rs-style declaration navigation and documentation reader.

use super::package_metadata::ReadmeBlock;
use super::{
    Context, DocRow, FluentBuilder, Hsla, InteractiveElement, IntoElement, NativeApp,
    ParentElement, Role, SourceLink, StatefulInteractiveElement, Styled, badge, code_block, div,
    doc_block, px, rgb, section_heading, short_coordinate,
};
use backend_library::RowId;
use gpui::SharedString;

#[derive(Clone)]
enum SidebarTarget {
    Overview,
    Source(SourceLink),
    Kind(SharedString),
}

impl NativeApp {
    fn docs_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut kinds = std::collections::BTreeMap::<SharedString, usize>::new();
        for row in self
            .catalog
            .order
            .iter()
            .filter_map(|index| self.catalog.rows.get(*index))
        {
            *kinds.entry(row.kind.clone()).or_default() += 1;
        }
        let source = self
            .catalog
            .order
            .iter()
            .filter_map(|index| self.catalog.rows.get(*index))
            .filter_map(|row| row.source.clone())
            .min_by_key(|source| {
                let path = source.display_path.as_ref();
                (
                    !path.ends_with("src/lib.rs"),
                    !path.ends_with("lib.rs"),
                    path.len(),
                )
            });
        let mut kinds = kinds.into_iter().collect::<Vec<_>>();
        kinds.sort_by_key(|(kind, _)| docs_kind_rank(kind));
        let mut sidebar = div()
            .id("documentation-sections")
            .flex_1()
            .min_h_0()
            .h_full()
            .w_full()
            .overflow_y_scroll()
            .px_4()
            .py_4()
            .flex()
            .flex_col()
            .gap_2()
            .child(sidebar_heading("Crate"))
            .child(self.sidebar_action(
                cx,
                "All Items",
                format!("{} indexed", self.catalog.symbol_count()),
                self.selected.is_none() && self.docs_filter.is_none(),
                SidebarTarget::Overview,
            ))
            .child(sidebar_heading("Sections"))
            .child(self.sidebar_action(
                cx,
                "Documentation",
                "crate overview",
                self.selected.is_none() && self.docs_filter.is_none(),
                SidebarTarget::Overview,
            ))
            .child(sidebar_heading("Crate Items"));
        if let Some(source) = source {
            sidebar = sidebar.child(self.sidebar_action(
                cx,
                "Source",
                "indexed files",
                false,
                SidebarTarget::Source(source),
            ));
        }
        sidebar
            .children(kinds.into_iter().map(|(kind, count)| {
                let active = self.docs_filter.as_ref() == Some(&kind) && self.selected.is_none();
                self.sidebar_action(
                    cx,
                    kind.clone(),
                    format!("{count}"),
                    active,
                    SidebarTarget::Kind(kind),
                )
            }))
            .child(sidebar_heading("Crates"))
            .child(self.sidebar_action(
                cx,
                self.package_name(),
                format!("v{}", self.catalog.package.version),
                self.selected.is_none() && self.docs_filter.is_none(),
                SidebarTarget::Overview,
            ))
    }

    fn sidebar_action(
        &self,
        cx: &mut Context<Self>,
        label: impl Into<SharedString>,
        meta: impl Into<SharedString>,
        active: bool,
        target: SidebarTarget,
    ) -> gpui::AnyElement {
        let label = label.into();
        let meta = meta.into();
        div()
            .id(format!(
                "docs-nav-{}",
                super::a11y_key(&format!("{label}-{meta}"))
            ))
            .role(Role::Link)
            .aria_label(format!("{label}, {meta}"))
            .px_2()
            .py_1()
            .rounded_sm()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .cursor_pointer()
            .when(active, |item| item.bg(rgb(0x0044_4444)))
            .hover(|item| item.bg(rgb(0x003e_3e3e)))
            .on_click(cx.listener(move |this, _, _, cx| {
                match &target {
                    SidebarTarget::Overview => {
                        this.route = super::Route::Docs;
                        this.selected = None;
                        this.docs_filter = None;
                    }
                    SidebarTarget::Source(source) => {
                        this.open_source_preview(source, cx);
                        return;
                    }
                    SidebarTarget::Kind(kind) => {
                        this.route = super::Route::Docs;
                        this.selected = None;
                        this.docs_filter = Some(kind.clone());
                    }
                }
                cx.notify();
            }))
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(0x00df_a84e))
                    .child(label),
            )
            .child(
                div()
                    .text_size(px(10.0))
                    .text_color(rgb(0x009d_9d9d))
                    .child(meta),
            )
            .into_any_element()
    }

    pub(super) fn docs_shell(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let selected = self.selected_row();
        div()
            .id("documentation-panel")
            .role(Role::TabPanel)
            .aria_label("Documentation")
            .size_full()
            .flex()
            .child(
                div()
                    .id("documentation-navigation")
                    .role(Role::Navigation)
                    .aria_label("Documentation symbols")
                    .w(px(200.0))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .border_r_1()
                    .border_color(rgb(0x0048_4848))
                    .bg(rgb(0x0035_3535))
                    .child(
                        div()
                            .px_3()
                            .py_3()
                            .border_b_1()
                            .border_color(rgb(0x0048_4848))
                            .child(
                                div()
                                    .id("documentation-heading")
                                    .role(Role::Heading)
                                    .aria_level(2)
                                    .text_size(px(12.0))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgb(0x00ee_eeee))
                                    .child(self.package_name()),
                            )
                            .child(
                                div()
                                    .id("documentation-item-count")
                                    .role(Role::Status)
                                    .aria_label("Indexed documentation item count")
                                    .mt_1()
                                    .text_size(px(11.0))
                                    .text_color(rgb(0x00bd_bdbd))
                                    .child(format!(
                                        "{} documented items",
                                        self.catalog.symbol_count()
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .id("documentation-symbol-list")
                            .role(Role::List)
                            .aria_label("Indexed declarations")
                            .flex_1()
                            .min_h_0()
                            .flex()
                            .overflow_hidden()
                            .child(self.docs_sidebar(cx)),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .bg(rgb(0x0030_3030))
                    .child(self.doc_reader(selected.as_ref(), cx)),
            )
            .into_any_element()
    }

    #[allow(clippy::too_many_lines)]
    fn doc_reader(
        &mut self,
        selected: Option<&DocRow>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let Some(row) = selected.cloned() else {
            return self.crate_reader(cx);
        };
        let package = self.package_name();
        let source = row.source.clone();
        let header_action = source.clone().map(|source| {
            let line = source.line;
            div()
                .id("view-source")
                .role(Role::Button)
                .aria_label(format!("View source for {} at line {}", row.title, line))
                .px_3()
                .py_2()
                .rounded_sm()
                .border_1()
                .border_color(rgb(0x0076_511d))
                .bg(rgb(0x002a_2116))
                .text_size(px(13.0))
                .text_color(rgb(0x00ff_d18a))
                .cursor_pointer()
                .hover(|button| button.bg(rgb(0x003a_2c1b)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.open_source_preview(&source, cx);
                }))
                .child(format!("View source · line {line}"))
        });
        let members = self.child_rows(&row);
        let related = self.related_rows(&row);
        let mut page = div()
            .id("documentation-reader")
            .role(Role::Document)
            .aria_label(format!("Documentation for {}", row.title))
            .size_full()
            .overflow_y_scroll()
            .child(
                div()
                    .max_w(px(900.0))
                    .w_full()
                    .mx_auto()
                    .px_6()
                    .py_7()
                    .flex()
                    .flex_col()
                    .gap_5()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_size(px(13.0))
                            .text_color(rgb(0x00ba_baba))
                            .child(package)
                            .child("/")
                            .child("docs")
                            .child("/")
                            .child(row.kind.clone()),
                    )
                    .child(
                        div()
                            .flex()
                            .items_start()
                            .justify_between()
                            .gap_5()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_3()
                                            .child(badge(&row.kind, 0x002b_2116, 0x00ff_c66d))
                                            .child(
                                                div()
                                                    .text_size(px(34.0))
                                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                                    .child(row.title.clone()),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(13.0))
                                            .text_color(rgb(0x00b7_b7b7))
                                            .child(short_coordinate(&row.coordinate)),
                                    ),
                            )
                            .children(header_action),
                    )
                    .when(!row.signature.is_empty(), |page| {
                        page.child(code_block(
                            "declaration",
                            row.signature.clone(),
                            Hsla::from(rgb(0x00bc_d8ff)),
                        ))
                    })
                    .child(section_heading("Documentation", "#documentation"))
                    .children(row.blocks.iter().map(doc_block))
                    .when(!members.is_empty(), |content| {
                        content
                            .child(
                                div()
                                    .mt_3()
                                    .pt_5()
                                    .border_t_1()
                                    .border_color(rgb(0x002a_3341))
                                    .child(section_heading(
                                        "Associated items",
                                        "#associated-items",
                                    )),
                            )
                            .children(
                                members
                                    .into_iter()
                                    .map(|member| doc_relation_link(member, "Associated", cx)),
                            )
                    })
                    .child(
                        div()
                            .mt_3()
                            .pt_5()
                            .border_t_1()
                            .border_color(rgb(0x002a_3341))
                            .child(section_heading("Source", "#source")),
                    ),
            );
        if let Some(source) = source {
            let line = source.line;
            page = page.child(
                div()
                    .mx_10()
                    .mb_5()
                    .p_4()
                    .rounded_lg()
                    .bg(rgb(0x0012_1922))
                    .border_1()
                    .border_color(rgb(0x0029_3647))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(px(13.0))
                                    .text_color(rgb(0x00d6_dfed))
                                    .child(source.display_path.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .text_color(rgb(0x0083_92a7))
                                    .child(format!("indexed location · line {line}")),
                            ),
                    )
                    .child(
                        div()
                            .px_3()
                            .py_2()
                            .rounded_lg()
                            .bg(rgb(0x001c_2f49))
                            .text_size(px(12.0))
                            .text_color(rgb(0x00b9_d8ff))
                            .child("Exact indexed source"),
                    ),
            );
        }
        if !related.is_empty() {
            page = page
                .child(
                    div()
                        .mx_10()
                        .mt_2()
                        .child(section_heading("Related declarations", "#related")),
                )
                .children(related.into_iter().map(|row| {
                    let key = row.key.clone();
                    div()
                        .id(format!("related-{}", super::a11y_key(&key)))
                        .role(Role::Link)
                        .aria_label(format!("Related declaration {}, {}", row.title, row.kind))
                        .mx_10()
                        .mb_2()
                        .p_3()
                        .rounded_lg()
                        .bg(rgb(0x0012_1821))
                        .border_1()
                        .border_color(rgb(0x0027_3342))
                        .cursor_pointer()
                        .hover(|item| item.bg(rgb(0x001c_2634)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.choose(key.clone(), cx);
                        }))
                        .child(
                            div()
                                .text_size(px(14.0))
                                .text_color(rgb(0x00d8_e4f4))
                                .child(row.title),
                        )
                        .child(
                            div()
                                .mt_1()
                                .text_size(px(12.0))
                                .text_color(rgb(0x0084_91a5))
                                .child(format!("{} · {}", row.kind, row.language)),
                        )
                }));
        }
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(page)
            .into_any_element()
    }

    fn crate_reader(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let package = self.package_name();
        let version = self.catalog.package.version.clone();
        let description = if self.catalog.package.description.is_empty() {
            "Local package documentation".to_owned()
        } else {
            self.catalog.package.description.clone()
        };
        let readme = self
            .catalog
            .package
            .readme
            .iter()
            .skip_while(|block| matches!(block, ReadmeBlock::Heading { level: 1, .. }))
            .cloned()
            .collect::<Vec<_>>();
        let mut groups = std::collections::BTreeMap::<SharedString, Vec<DocRow>>::new();
        for row in self
            .catalog
            .order
            .iter()
            .filter_map(|index| self.catalog.rows.get(*index))
            .filter(|row| {
                self.docs_filter
                    .as_ref()
                    .is_none_or(|kind| &row.kind == kind)
            })
        {
            groups
                .entry(row.kind.clone())
                .or_default()
                .push(row.clone());
        }
        let mut groups = groups.into_iter().collect::<Vec<_>>();
        groups.sort_by_key(|(kind, _)| docs_kind_rank(kind));
        let filter_title = self
            .docs_filter
            .as_ref()
            .map(|kind| plural_kind(kind))
            .unwrap_or_else(|| "Crate items".to_owned());
        let source = self
            .catalog
            .order
            .iter()
            .filter_map(|index| self.catalog.rows.get(*index))
            .filter_map(|row| row.source.clone())
            .min_by_key(|source| {
                let path = source.display_path.as_ref();
                (
                    !path.ends_with("src/lib.rs"),
                    !path.ends_with("lib.rs"),
                    path.len(),
                )
            });
        let mut reader = div()
            .max_w(px(980.0))
            .w_full()
            .mx_auto()
            .px_8()
            .py_7()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .items_end()
                            .gap_2()
                            .child(
                                div()
                                    .text_size(px(27.0))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(format!("Crate {package}")),
                            )
                            .child(
                                div()
                                    .text_size(px(13.0))
                                    .text_color(rgb(0x00b7_b7b7))
                                    .child(version),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .text_size(px(12.0))
                            .text_color(rgb(0x00dd_a252))
                            .child(
                                div()
                                    .id("docs-search-link")
                                    .role(Role::Link)
                                    .aria_label("Search documentation")
                                    .px_2()
                                    .py_1()
                                    .rounded_sm()
                                    .cursor_pointer()
                                    .hover(|item| item.bg(rgb(0x003e_3e3e)))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open_search(window, cx);
                                    }))
                                    .child("⌕ Search"),
                            )
                            .child(
                                div()
                                    .id("docs-settings-link")
                                    .role(Role::Button)
                                    .aria_label("Documentation display settings")
                                    .px_2()
                                    .py_1()
                                    .rounded_sm()
                                    .cursor_pointer()
                                    .hover(|item| item.bg(rgb(0x003e_3e3e)))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.status = "Documentation follows system text and reduced-motion settings".into();
                                        cx.notify();
                                    }))
                                    .child("⚙ Settings"),
                            )
                            .child(
                                div()
                                    .id("docs-help-link")
                                    .role(Role::Button)
                                    .aria_label("Documentation navigation help")
                                    .px_2()
                                    .py_1()
                                    .rounded_sm()
                                    .cursor_pointer()
                                    .hover(|item| item.bg(rgb(0x003e_3e3e)))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.status = "Browse by item kind, follow declarations, or use Command-K to search".into();
                                        cx.notify();
                                    }))
                                    .child("? Help"),
                            )
                            .child(
                                div()
                                    .id("docs-summary-link")
                                    .role(Role::Button)
                                    .aria_label("Show all crate items")
                                    .px_2()
                                    .py_1()
                                    .rounded_sm()
                                    .cursor_pointer()
                                    .hover(|item| item.bg(rgb(0x003e_3e3e)))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.selected = None;
                                        this.docs_filter = None;
                                        cx.notify();
                                    }))
                                    .child("⌄ Summary"),
                            ),
                    ),
            )
            .children(source.map(|source| {
                div()
                    .id("crate-source-link")
                    .role(Role::Link)
                    .aria_label(format!("View crate source {}", source.display_path))
                    .px_1()
                    .py_1()
                    .rounded_sm()
                    .cursor_pointer()
                    .text_size(px(13.0))
                    .text_color(rgb(0x00dd_a252))
                    .hover(|item| item.bg(rgb(0x003e_3e3e)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_source_preview(&source, cx);
                    }))
                    .child("Source")
            }))
            .child(div().h(px(1.0)).w_full().bg(rgb(0x0080_8080)))
            .child(
                div()
                    .text_size(px(24.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(package.clone()),
            )
            .child(
                div()
                    .font_family("Georgia")
                    .text_size(px(17.0))
                    .line_height(px(26.0))
                    .text_color(rgb(0x00ee_eeee))
                    .child(description),
            )
            .children(readme.into_iter().map(crate_readme_block))
            .child(
                div()
                    .id("crate-items-heading")
                    .role(Role::Heading)
                    .aria_level(2)
                    .mt_4()
                    .pb_2()
                    .border_b_1()
                    .border_color(rgb(0x0080_8080))
                    .text_size(px(23.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(filter_title),
            );
        for (kind, rows) in groups {
            reader = reader.child(declaration_group(kind, rows, cx));
        }
        div()
            .id("crate-documentation-reader")
            .role(Role::Document)
            .aria_label(format!("Crate {package}"))
            .size_full()
            .overflow_y_scroll()
            .child(reader)
            .into_any_element()
    }

    fn related_rows(&self, row: &DocRow) -> Vec<DocRow> {
        self.catalog
            .order
            .iter()
            .filter_map(|index| self.catalog.rows.get(*index))
            .filter(|candidate| candidate.id != row.id && candidate.parent == row.parent)
            .take(5)
            .cloned()
            .collect()
    }

    fn child_rows(&self, row: &DocRow) -> Vec<DocRow> {
        let RowId::Symbol(symbol) = row.id else {
            return Vec::new();
        };
        let parent = backend_library::encode_id(symbol.as_bytes());
        self.catalog
            .order
            .iter()
            .filter_map(|index| self.catalog.rows.get(*index))
            .filter(|candidate| candidate.parent.as_deref() == Some(parent.as_str()))
            .cloned()
            .collect()
    }
}

fn sidebar_heading(label: &'static str) -> gpui::AnyElement {
    div()
        .mt_4()
        .mb_1()
        .text_size(px(14.0))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(rgb(0x00f1_f1f1))
        .child(label)
        .into_any_element()
}

fn plural_kind(kind: &str) -> String {
    if kind.ends_with('y') {
        format!("{}ies", kind.trim_end_matches('y'))
    } else if kind.ends_with('s') || kind.ends_with('x') || kind.ends_with("ch") {
        format!("{kind}es")
    } else {
        format!("{kind}s")
    }
}

fn docs_kind_rank(kind: &str) -> usize {
    match kind {
        "Module" => 0,
        "Macro" => 1,
        "Struct" => 2,
        "Enum" => 3,
        "Trait" | "Interface" => 4,
        "Class" => 5,
        "Function" => 6,
        "Method" | "Constructor" => 7,
        "Type" => 8,
        "Constant" | "Variable" => 9,
        "Field" | "Property" => 10,
        "Import" => 11,
        _ => 12,
    }
}

fn declaration_group(
    kind: SharedString,
    rows: Vec<DocRow>,
    cx: &mut Context<NativeApp>,
) -> gpui::AnyElement {
    let count = rows.len();
    div()
        .id(format!("docs-group-{}", super::a11y_key(&kind)))
        .role(Role::Region)
        .aria_label(format!("{} declarations", plural_kind(&kind)))
        .mt_3()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .id(format!("docs-group-heading-{}", super::a11y_key(&kind)))
                .role(Role::Heading)
                .aria_level(2)
                .pb_1()
                .border_b_1()
                .border_color(rgb(0x0062_6262))
                .text_size(px(19.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(format!("{} ({count})", plural_kind(&kind))),
        )
        .children(rows.into_iter().map(|row| {
            let key = row.key.clone();
            let summary = row
                .blocks
                .iter()
                .find_map(|block| match block {
                    super::DocBlock::Prose(text) => Some(text.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| row.signature.clone());
            div()
                .id(format!("crate-item-{}", super::a11y_key(&key)))
                .role(Role::Link)
                .aria_label(format!("{} {}, {}", row.kind, row.title, summary))
                .px_2()
                .py_2()
                .rounded_sm()
                .cursor_pointer()
                .hover(|item| item.bg(rgb(0x003b_3b3b)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.choose(key.clone(), cx);
                }))
                .child(
                    div()
                        .flex()
                        .items_baseline()
                        .gap_3()
                        .child(
                            div()
                                .font_family("monospace")
                                .text_size(px(14.0))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(rgb(0x00df_a84e))
                                .child(row.title),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .text_size(px(13.0))
                                .text_color(rgb(0x00c5_c5c5))
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .child(summary),
                        ),
                )
        }))
        .into_any_element()
}

fn doc_relation_link(
    row: DocRow,
    relation: &'static str,
    cx: &mut Context<NativeApp>,
) -> gpui::AnyElement {
    let key = row.key.clone();
    div()
        .id(format!(
            "docs-relation-{}-{}",
            relation,
            super::a11y_key(&key)
        ))
        .role(Role::Link)
        .aria_label(format!("{relation} {} {}", row.kind, row.title))
        .p_3()
        .rounded_sm()
        .border_1()
        .border_color(rgb(0x0049_4949))
        .cursor_pointer()
        .hover(|item| item.bg(rgb(0x003b_3b3b)))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.choose(key.clone(), cx);
        }))
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .font_family("monospace")
                        .text_size(px(14.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(rgb(0x00df_a84e))
                        .child(row.title),
                )
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(rgb(0x00ae_aeae))
                        .child(row.signature),
                ),
        )
        .into_any_element()
}

fn crate_readme_block(block: ReadmeBlock) -> gpui::AnyElement {
    match block {
        ReadmeBlock::Heading { level, text } => div()
            .mt_3()
            .pb_2()
            .border_b_1()
            .border_color(rgb(0x0078_7878))
            .text_size(px(if level <= 2 { 22.0 } else { 18.0 }))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .child(text)
            .into_any_element(),
        ReadmeBlock::Paragraph(text) => div()
            .font_family("Georgia")
            .text_size(px(17.0))
            .line_height(px(26.0))
            .text_color(rgb(0x00ee_eeee))
            .child(text)
            .into_any_element(),
        ReadmeBlock::Bullet(text) => div()
            .pl_4()
            .font_family("Georgia")
            .text_size(px(16.0))
            .text_color(rgb(0x00ee_eeee))
            .child(format!("• {text}"))
            .into_any_element(),
        ReadmeBlock::Code { text, .. } => {
            code_block("example", text.into(), Hsla::from(rgb(0x00d8_c0f0)))
        }
    }
}

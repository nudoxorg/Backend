//! crates.io-style package views over the immutable desktop catalog.

use super::package_metadata::{DependencyKind, ReadmeBlock};
use super::{
    Context, FluentBuilder, Hsla, InteractiveElement, IntoElement, NativeApp, PackageTab,
    ParentElement, Role, StatefulInteractiveElement, Styled, a11y_key, badge, code_block, div, px,
    revision_short, rgb, section_heading, side_card, source_lines, source_text_color, version_card,
};

impl NativeApp {
    pub(super) fn package_page(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let content = match self.package_tab {
            PackageTab::Readme => self.package_readme(cx),
            PackageTab::Code => self.package_code(cx),
            PackageTab::Versions => self.package_versions(),
            PackageTab::Dependencies => self.package_dependencies(cx),
            PackageTab::Dependents => Self::package_dependents(),
            PackageTab::Security => self.package_security(),
        };
        div()
            .id("package-page")
            .role(Role::TabPanel)
            .aria_label(format!("{} package overview", self.package_name()))
            .size_full()
            .overflow_y_scroll()
            .bg(rgb(0x0030_302f))
            .child(
                div()
                    .max_w(px(1180.0))
                    .w_full()
                    .mx_auto()
                    .px_4()
                    .py_4()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(self.package_hero())
                    .child(self.package_tabs(cx))
                    .child(div().w_full().child(content)),
            )
            .into_any_element()
    }

    fn package_hero(&self) -> gpui::AnyElement {
        let metadata = &self.catalog.package;
        let version = if metadata.version == "local" {
            "local".to_owned()
        } else {
            format!("v{}", metadata.version)
        };
        div()
            .id("package-summary")
            .role(Role::Region)
            .aria_label("Package summary")
            .rounded_lg()
            .overflow_hidden()
            .bg(rgb(0x0014_1413))
            .child(
                div()
                    .max_w(px(1180.0))
                    .w_full()
                    .mx_auto()
                    .px_8()
                    .py_8()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .text_size(px(38.0))
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .child(self.package_name()),
                            )
                            .child(
                                div()
                                    .text_size(px(25.0))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgb(0x00b2_b2ae))
                                    .child(version),
                            ),
                    )
                    .child(
                        div()
                            .max_w(px(820.0))
                            .text_size(px(17.0))
                            .text_color(rgb(0x00f0_eee6))
                            .child(if metadata.description.is_empty() {
                                "Local package documentation".to_owned()
                            } else {
                                metadata.description.clone()
                            }),
                    )
                    .child(
                        div().flex().items_center().gap_2().children(
                            metadata.keywords.iter().take(6).map(|keyword| {
                                badge(&format!("#{keyword}"), 0x0031_312f, 0x00cf_cfca)
                            }),
                        ),
                    ),
            )
            .into_any_element()
    }

    fn package_readme(&self, _cx: &mut Context<Self>) -> gpui::AnyElement {
        let metadata = &self.catalog.package;
        let mut readme = metadata.readme.clone();
        let package_name = self.package_name().to_string();
        if readme.is_empty() {
            readme = vec![
                ReadmeBlock::Paragraph(if metadata.description.is_empty() {
                    format!("{package_name} is indexed locally and ready to browse.")
                } else {
                    metadata.description.clone()
                }),
                ReadmeBlock::Heading {
                    level: 2,
                    text: "Documentation".to_owned(),
                },
                ReadmeBlock::Paragraph(format!(
                    "{} documented declarations across {} source languages. Open Documentation to browse the complete typed index, or Code to inspect line-numbered source.",
                    self.catalog.symbol_count(),
                    self.catalog.languages.len()
                )),
                ReadmeBlock::Heading {
                    level: 2,
                    text: "Indexed languages".to_owned(),
                },
            ];
            readme.extend(self.catalog.languages.iter().map(|(language, count)| {
                ReadmeBlock::Bullet(format!("{language}: {count} declarations"))
            }));
        }
        let install_command = if metadata.members > 1 {
            "cargo build --workspace".to_owned()
        } else {
            format!("cargo add {package_name}")
        };
        let mut metadata_lines = Vec::new();
        if metadata.members > 0 {
            metadata_lines.push(format!("{} workspace packages", metadata.members));
        }
        if metadata.rust_version != "Unspecified" {
            metadata_lines.push(format!("Rust {}", metadata.rust_version));
        }
        if metadata.license != "Unspecified" {
            metadata_lines.push(format!("License: {}", metadata.license));
        }
        if !metadata.features.is_empty() {
            metadata_lines.push(format!("{} Cargo features", metadata.features.len()));
        }
        metadata_lines.extend([
            format!("{} documented declarations", self.catalog.symbol_count()),
            format!("{} source lines", self.catalog.source_lines),
            format!("{} source", byte_size(self.catalog.source_bytes)),
            format!("{} languages", self.catalog.languages.len()),
        ]);
        div()
            .id("package-readme")
            .role(Role::Article)
            .aria_label(format!("{} readme", self.package_name()))
            .flex()
            .gap_8()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .p_6()
                    .rounded_lg()
                    .bg(rgb(0x0014_1413))
                    .children(
                        readme
                            .into_iter()
                            .enumerate()
                            .map(|(index, block)| readme_block(index, block)),
                    ),
            )
            .child(
                div()
                    .w(px(260.0))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(side_card("Metadata", metadata_lines))
                    .child(side_card(
                        "Install",
                        vec![
                            install_command,
                            format!("{} source files", self.catalog.files.len()),
                        ],
                    ))
                    .when(!metadata.documentation.is_empty(), |sidebar| {
                        sidebar.child(side_card(
                            "Documentation",
                            vec![metadata.documentation.clone()],
                        ))
                    })
                    .when(!metadata.repository.is_empty(), |sidebar| {
                        sidebar.child(side_card("Repository", vec![metadata.repository.clone()]))
                    })
                    .when(!metadata.homepage.is_empty(), |sidebar| {
                        sidebar.child(side_card("Homepage", vec![metadata.homepage.clone()]))
                    })
                    .when(!metadata.categories.is_empty(), |sidebar| {
                        sidebar.child(side_card("Categories", metadata.categories.clone()))
                    })
                    .when(!metadata.features.is_empty(), |sidebar| {
                        sidebar.child(side_card(
                            "Features",
                            metadata
                                .features
                                .iter()
                                .map(|feature| {
                                    let members = if feature.members.is_empty() {
                                        feature.name.clone()
                                    } else {
                                        format!("{}: {}", feature.name, feature.members.join(", "))
                                    };
                                    if feature.users > 1 {
                                        format!("{members} · {} packages", feature.users)
                                    } else {
                                        members
                                    }
                                })
                                .collect(),
                        ))
                    })
                    .child(side_card(
                        "Local index",
                        vec![
                            revision_short(self.root.version().as_bytes()),
                            self.status.to_string(),
                        ],
                    )),
            )
            .into_any_element()
    }

    #[allow(clippy::too_many_lines)]
    fn package_code(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let files = self
            .catalog
            .files
            .iter()
            .take(80)
            .cloned()
            .collect::<Vec<_>>();
        let file_list = div()
            .id("package-source-list")
            .role(Role::Navigation)
            .aria_label("Package source files")
            .w(px(272.0))
            .flex_none()
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .border_r_1()
            .border_color(rgb(0x0034_3432))
            .child(
                div()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(rgb(0x0034_3432))
                    .text_size(px(13.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Files"),
            )
            .children(files.into_iter().map(|path| {
                let target = self
                    .catalog
                    .order
                    .iter()
                    .filter_map(|item| self.catalog.rows.get(*item))
                    .find(|row| {
                        row.source
                            .as_ref()
                            .is_some_and(|source| source.display_path.as_ref() == path.as_str())
                    })
                    .and_then(|row| row.source.clone());
                let label = path.rsplit('/').next().unwrap_or(&path).to_owned();
                let line = target.as_ref().map_or(1, |source| source.line);
                let selected = self
                    .source_preview
                    .as_ref()
                    .is_some_and(|preview| preview.path.as_ref() == path.as_str());
                div()
                    .id(format!("source-file-{}", a11y_key(&path)))
                    .role(Role::Link)
                    .aria_label(format!("Source file {path}"))
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(rgb(0x002d_2d2b))
                    .when(selected, |item| item.bg(rgb(0x0024_2c38)))
                    .cursor_pointer()
                    .hover(|item| item.bg(rgb(0x0020_252d)))
                    .when(target.is_some(), |item| {
                        item.on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(source) = target.as_ref() {
                                this.open_code_preview(source, cx);
                            }
                        }))
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .font_family("SF Mono")
                                    .text_size(px(13.0))
                                    .text_color(rgb(0x00d5_e1f2))
                                    .child(label),
                            )
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .text_color(rgb(0x0084_94aa))
                                    .child(format!("line {line}")),
                            ),
                    )
                    .child(
                        div()
                            .mt_1()
                            .text_size(px(11.0))
                            .text_color(rgb(0x0077_879d))
                            .child(path),
                    )
            }));
        let preview = if self.source_loading {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(rgb(0x0098_a2b3))
                .child("Loading source…")
                .into_any_element()
        } else if let Some(error) = &self.source_error {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(rgb(0x00e8_a3a3))
                .child(error.clone())
                .into_any_element()
        } else if let Some(preview) = &self.source_preview {
            let lines = source_lines(&preview.lines, preview.line);
            div()
                .size_full()
                .flex()
                .flex_col()
                .child(
                    div()
                        .h(px(46.0))
                        .flex_none()
                        .px_4()
                        .flex()
                        .items_center()
                        .justify_between()
                        .border_b_1()
                        .border_color(rgb(0x0034_3432))
                        .child(
                            div()
                                .font_family("SF Mono")
                                .text_size(px(13.0))
                                .child(preview.path.clone()),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_3()
                                .child(
                                    div()
                                        .text_size(px(11.0))
                                        .text_color(rgb(0x0085_8b94))
                                        .child(format!("{} lines shown", lines.len())),
                                )
                                .child(
                                    div()
                                        .id("open-full-source")
                                        .role(Role::Button)
                                        .aria_label("Open full line-numbered source")
                                        .px_3()
                                        .py_1()
                                        .rounded_sm()
                                        .bg(rgb(0x002b_2116))
                                        .text_size(px(11.0))
                                        .text_color(rgb(0x00ff_c66d))
                                        .cursor_pointer()
                                        .hover(|item| item.bg(rgb(0x003a_2c1b)))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.route = super::Route::Source;
                                            cx.notify();
                                        }))
                                        .child("View source"),
                                ),
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .overflow_hidden()
                        .bg(rgb(0x0012_1416))
                        .children(lines.into_iter().map(|(line, text)| {
                            let focused = line == preview.line;
                            div()
                                .id(("package-code-line", line))
                                .role(Role::ListItem)
                                .aria_label(format!("Line {line}: {text}"))
                                .min_w_0()
                                .px_3()
                                .py_1()
                                .flex()
                                .gap_3()
                                .when(focused, |item| item.bg(rgb(0x0029_261d)))
                                .child(
                                    div()
                                        .w(px(42.0))
                                        .flex_none()
                                        .font_family("SF Mono")
                                        .text_size(px(12.0))
                                        .text_color(rgb(0x0064_738a))
                                        .child(line.to_string()),
                                )
                                .child(
                                    div()
                                        .font_family("SF Mono")
                                        .text_size(px(13.0))
                                        .text_color(source_text_color(&text))
                                        .child(text),
                                )
                        })),
                )
                .into_any_element()
        } else {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(rgb(0x0085_8b94))
                .child("Choose a source file")
                .into_any_element()
        };
        div()
            .id("package-code-browser")
            .role(Role::Region)
            .aria_label("Package code browser")
            .h(px(560.0))
            .flex()
            .rounded_lg()
            .overflow_hidden()
            .border_1()
            .border_color(rgb(0x0034_3432))
            .bg(rgb(0x0017_191c))
            .child(file_list)
            .child(div().flex_1().min_w_0().child(preview))
            .into_any_element()
    }

    fn package_versions(&self) -> gpui::AnyElement {
        div()
            .id("package-versions")
            .role(Role::Region)
            .aria_label("Package versions")
            .flex()
            .flex_col()
            .gap_4()
            .child(section_heading("Versions", "#versions"))
            .child(version_card(
                &format!("v{}", self.catalog.package.version),
                revision_short(self.root.version().as_bytes()),
                "current immutable local revision",
                true,
            ))
            .child(
                div()
                    .p_5()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(0x002b_3849))
                    .bg(rgb(0x0013_1b25))
                    .text_size(px(14.0))
                    .text_color(rgb(0x009b_a9bc))
                    .child("Each update produces a new immutable revision. The local surface keeps the newest admitted view and reuses unchanged objects across the transition."),
            )
            .into_any_element()
    }

    fn package_dependencies(&self, _cx: &mut Context<Self>) -> gpui::AnyElement {
        let dependencies = self.catalog.package.dependencies.clone();
        div()
            .id("package-dependencies")
            .role(Role::Region)
            .aria_label("Package dependencies")
            .flex()
            .flex_col()
            .gap_4()
            .child(section_heading("Dependencies", "#dependencies"))
            .child(
                div()
                    .text_size(px(14.0))
                    .text_color(rgb(0x00a0_adbf))
                    .child(format!(
                        "{} unique dependency requirements across {} workspace packages.",
                        dependencies.len(),
                        self.catalog.package.members
                    )),
            )
            .children(dependencies.into_iter().map(|dependency| {
                let color = match dependency.kind {
                    DependencyKind::Runtime => 0x009b_e0bb,
                    DependencyKind::Development => 0x00bc_d8ff,
                    DependencyKind::Build => 0x00ff_d18a,
                };
                div()
                    .id(format!(
                        "dependency-{}-{}",
                        dependency.kind.label(),
                        a11y_key(&dependency.name)
                    ))
                    .role(Role::ListItem)
                    .aria_label(format!(
                        "Dependency {} {}",
                        dependency.name, dependency.requirement
                    ))
                    .p_4()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(0x002b_394b))
                    .bg(rgb(0x0013_1b25))
                    .hover(|item| item.bg(rgb(0x001d_2836)))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_size(px(15.0))
                                    .text_color(rgb(0x00db_e5f3))
                                    .child(dependency.name),
                            )
                            .child(badge(dependency.kind.label(), 0x0020_2b3d, color)),
                    )
                    .child(
                        div()
                            .mt_1()
                            .text_size(px(12.0))
                            .text_color(rgb(0x0086_94aa))
                            .child(format!(
                                "{} · used by {} package{}",
                                dependency.requirement,
                                dependency.users,
                                if dependency.users == 1 { "" } else { "s" }
                            )),
                    )
            }))
            .into_any_element()
    }

    fn package_dependents() -> gpui::AnyElement {
        div()
            .id("package-dependents")
            .role(Role::Region)
            .aria_label("Package dependents")
            .flex()
            .flex_col()
            .gap_4()
            .child(section_heading("Dependents", "#dependents"))
            .child(
                div()
                    .p_6()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(0x002b_3849))
                    .bg(rgb(0x0013_1b25))
                    .text_size(px(15.0))
                    .text_color(rgb(0x00a0_adbf))
                    .child("No remote dependents are attached to this local package yet. Connect a remote index to populate this view without moving local ownership."),
            )
            .into_any_element()
    }

    fn package_security(&self) -> gpui::AnyElement {
        div()
            .id("package-security")
            .role(Role::Region)
            .aria_label("Package security")
            .flex()
            .flex_col()
            .gap_4()
            .child(section_heading("Security", "#security"))
            .child(
                div()
                    .p_6()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(0x002e_4a3c))
                    .bg(rgb(0x0014_231d))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(16.0))
                            .text_color(rgb(0x009b_e0bb))
                            .child("No local advisory findings"),
                    )
                    .child(
                        div()
                            .text_size(px(14.0))
                            .text_color(rgb(0x009e_afa6))
                            .child(format!(
                                "License: {} · source and documentation pinned to revision {}.",
                                self.catalog.package.license,
                                revision_short(self.root.version().as_bytes())
                            )),
                    ),
            )
            .into_any_element()
    }
}

fn readme_block(index: usize, block: ReadmeBlock) -> gpui::AnyElement {
    match block {
        ReadmeBlock::Heading { level, text } => div()
            .id(("readme-heading", index))
            .role(Role::Heading)
            .aria_level(usize::from(level))
            .mt(if level == 1 { px(0.0) } else { px(12.0) })
            .pb_2()
            .when(level <= 2, |heading| {
                heading.border_b_1().border_color(rgb(0x0050_504b))
            })
            .text_size(if level == 1 {
                px(24.0)
            } else if level == 2 {
                px(20.0)
            } else {
                px(17.0)
            })
            .font_weight(gpui::FontWeight::BOLD)
            .text_color(rgb(0x00f0_eee6))
            .child(text)
            .into_any_element(),
        ReadmeBlock::Paragraph(text) => div()
            .text_size(px(15.0))
            .line_height(px(24.0))
            .text_color(rgb(0x00e2_e0da))
            .child(text)
            .into_any_element(),
        ReadmeBlock::Bullet(text) => div()
            .pl_5()
            .text_size(px(15.0))
            .line_height(px(22.0))
            .text_color(rgb(0x009b_e0bb))
            .child(format!("•  {text}"))
            .into_any_element(),
        ReadmeBlock::Code { language, text } => code_block(
            if language.is_empty() {
                "code"
            } else {
                &language
            },
            text.into(),
            Hsla::from(rgb(0x00dc_c7ff)),
        ),
    }
}

fn byte_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    }
}

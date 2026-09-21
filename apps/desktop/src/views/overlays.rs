//! Floating surfaces: the settings sheet, hover cards, and the notice bar.
//! Each of them is temporary, elevated, and dismissible with one key.
//! Nothing that reports a failure lives here; failures stay in the page.
//!
//! Settings is a sheet with a sidebar of pages rather than one long form,
//! because the pages are for different readers: appearance for everyone,
//! editor for people who follow source, agents for people wiring Claude or
//! another tool to the shelf, diagnostics for the one time something is
//! wrong. Each page scrolls on its own; the sheet never grows past the window.
//!
//! The hover card is the reason this module exists. A reader scanning a member
//! list wants the signature and the first sentence without losing their place,
//! and the honest way to give them that is a small elevated card with reserved
//! geometry — the frame appears immediately, the content fills in — rather
//! than a card that pops into existence a third of a second late.
//!
//! The sheet occludes the scrim behind it. GPUI hit-tests every hitbox under
//! the pointer, not only the topmost, so without `occlude()` a click on a
//! sidebar row also reached the scrim's click handler and closed the sheet the
//! reader had just asked to navigate — the "settings vanish when I pick a
//! page" defect.

use super::keys;
use super::workspace::Workspace;
use crate::host::lease::HostMode;
use crate::store::document::HoverCard;
use crate::store::prefs::EditorScheme;
use crate::store::shell::SettingsPage;
use crate::theme::Theme;
use crate::theme::palette::{Appearance, Paint};
use crate::theme::tokens::{InterfaceSize, Space, TypeScale, space, type_size};
use crate::ui::icon::{self, Icon};
use crate::ui::{button, chip, components, glyph, surface, text};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, Div, ElementId, FontWeight, InteractiveElement, IntoElement,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, div, px,
};
use gpui_component::setting::{SelectIndex, SettingGroup, SettingItem, SettingPage};

/// Width of the hover card.
const CARD: f32 = 380.0;

/// Reserved height of the hover card while its content is arriving.
const CARD_RESERVED: f32 = 64.0;

/// Width of the settings sheet.
const SHEET_WIDTH: f32 = 720.0;

/// Width of the settings sidebar.
const SIDEBAR: f32 = 168.0;

impl Workspace {
    /// Returns the anchored hover card, when one is showing.
    pub(super) fn hover_card(&self, theme: &Theme, cx: &Context<Self>) -> Option<AnyElement> {
        let hover = self.document.read(cx).hover()?;
        let anchor = hover.anchor();
        let card = hover.card().cloned();
        let content_theme = theme.clone();
        let trigger = button::icon_button(theme, "hover-card-trigger", Icon::Link);
        let popover = components::popover_with_action(
            theme,
            "hover-card",
            "Details",
            trigger,
            move |_, _, _| {
                let body = card.as_ref().map_or_else(
                    || reserved(&content_theme).into_any_element(),
                    |card| filled(&content_theme, card).into_any_element(),
                );
                surface::raised(&content_theme)
                    .w(px(CARD))
                    .px(space(Space::Room))
                    .py(space(Space::Base))
                    .child(body)
            },
        )
        .default_open(true)
        .overlay_closable(false);
        Some(
            gpui::anchored()
                .position(gpui::point(anchor.x + px(12.0), anchor.y + px(16.0)))
                .snap_to_window_with_margin(px(8.0))
                .child(popover)
                .into_any_element(),
        )
    }

    /// Returns the short confirmation bar, when one is showing.
    pub(super) fn notice_bar(&self, theme: &Theme, cx: &Context<Self>) -> Option<AnyElement> {
        let notice = self.shell.read(cx).notice()?;
        Some(
            div()
                .absolute()
                .bottom(px(36.0))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    surface::raised(theme)
                        .flex()
                        .items_center()
                        .gap(space(Space::Snug))
                        .px(space(Space::Room))
                        .py(space(Space::Snug))
                        .child(text::label(theme).child(notice.text().to_owned()))
                        .when_some(notice.detail().map(ToOwned::to_owned), |bar, detail| {
                            bar.child(
                                text::single_line(text::faint(theme))
                                    .max_w(px(360.0))
                                    .font_family(theme.specimen())
                                    .child(detail),
                            )
                        }),
                )
                .into_any_element(),
        )
    }
}

/// The settings sheet.
impl Workspace {
    /// Returns the settings sheet.
    pub(super) fn settings_sheet(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let current_page = self.shell.read(cx).settings_page();
        let workspace = cx.entity().downgrade();
        let pages = SettingsPage::ALL.into_iter().map(|page| {
            let workspace = workspace.clone();
            let theme = theme.clone();
            SettingPage::new(page.label()).group(SettingGroup::new().item(SettingItem::render(
                move |_, _, app| {
                    workspace
                        .update(app, |workspace, cx| {
                            workspace.settings_page(&theme, page, cx)
                        })
                        .map(IntoElement::into_any_element)
                        .unwrap_or_else(|_| div().into_any_element())
                },
            )))
        });
        let settings = components::settings_with_action(theme, "settings-shell", "Settings", pages)
            .sidebar_width(px(SIDEBAR))
            .default_selected_index(SelectIndex {
                page_ix: SettingsPage::ALL
                    .iter()
                    .position(|page| *page == current_page)
                    .unwrap_or_default(),
                group_ix: None,
            });
        div()
            .absolute()
            .inset_0()
            .child(
                surface::scrim(theme)
                    .id("settings-scrim")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.shell
                            .update(cx, super::super::store::shell::ShellStore::close_settings);
                        this.remove_transient(crate::store::shell::Transient::Settings, cx);
                        this.restore_focus(window, cx);
                    })),
            )
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .p(space(Space::Gutter))
                    .child(
                        surface::raised(theme)
                            .id("settings-sheet")
                            .role(gpui::Role::Dialog)
                            .aria_label("Settings")
                            .key_context("NudoxSettings")
                            .tab_group()
                            .track_focus(&self.settings_focus)
                            .focus_visible(|style| style.border_color(theme.paint(Paint::Focus)))
                            .tab_stop(false)
                            .capture_key_down(cx.listener(
                                |this, event: &gpui::KeyDownEvent, window, cx| {
                                    if event.keystroke.key != "tab"
                                        || event.keystroke.modifiers.control
                                        || event.keystroke.modifiers.alt
                                        || event.keystroke.modifiers.platform
                                    {
                                        return;
                                    }
                                    cx.stop_propagation();
                                    let backwards = event.keystroke.modifiers.shift;
                                    let before_focus = window.focused(cx);
                                    if backwards {
                                        window.focus_prev(cx);
                                    } else {
                                        window.focus_next(cx);
                                    }
                                    // GPUI's tab map wraps the whole window. If the
                                    // next stop escaped this modal group, continue
                                    // in the same direction until the focus is back
                                    // inside the sheet. The attempt cap protects a
                                    // malformed render from turning Tab into a loop.
                                    if !this.settings_focus.contains_focused(window, cx) {
                                        let mut attempts = 0;
                                        while !this.settings_focus.contains_focused(window, cx)
                                            && attempts < 100
                                        {
                                            if backwards {
                                                window.focus_prev(cx);
                                            } else {
                                                window.focus_next(cx);
                                            }
                                            attempts += 1;
                                            if window.focused(cx) == before_focus {
                                                break;
                                            }
                                        }
                                    }
                                },
                            ))
                            .occlude()
                            .w(px(SHEET_WIDTH))
                            .max_w(gpui::relative(1.0))
                            .h(gpui::relative(0.82))
                            .max_h(px(640.0))
                            .flex()
                            .overflow_hidden()
                            .child(settings),
                    ),
            )
    }

    fn settings_page(
        &mut self,
        theme: &Theme,
        page: SettingsPage,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<Div> {
        let body: Vec<AnyElement> = match page {
            SettingsPage::Appearance => vec![
                self.appearance_setting(theme, cx).into_any_element(),
                self.size_setting(theme, cx).into_any_element(),
                self.motion_setting(theme, cx).into_any_element(),
            ],
            SettingsPage::Editor => vec![self.editor_setting(theme, cx).into_any_element()],
            SettingsPage::Agents => vec![self.agents_setting(theme, cx).into_any_element()],
            SettingsPage::Diagnostics => vec![
                self.service_setting(theme, cx).into_any_element(),
                self.index_registry_setting(theme, cx).into_any_element(),
                self.capability_setting(theme, cx).into_any_element(),
            ],
            SettingsPage::Index => vec![self.index_registry_setting(theme, cx).into_any_element()],
            SettingsPage::Registry => {
                vec![self.index_registry_setting(theme, cx).into_any_element()]
            }
            SettingsPage::Legend => vec![legend_setting(theme).into_any_element()],
        };
        div()
            .id("settings-body")
            .flex_1()
            .min_w(px(0.0))
            .h_full()
            .overflow_y_scroll()
            .p(space(Space::Gutter))
            .flex()
            .flex_col()
            .gap(space(Space::Loose))
            .child(text::heading(theme, TypeScale::Section).child(page.label()))
            .children(body)
    }

    fn appearance_setting(&mut self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.shell.read(cx).prefs().appearance();
        setting(theme, "Appearance", "The palette this window is lit with.").child(
            div().flex().gap(space(Space::Tight)).children(
                [Appearance::Ink, Appearance::Vellum].map(|appearance| {
                    let selected = appearance == current;
                    button::button(
                        theme,
                        format!("appearance-{}", appearance.name()),
                        if appearance.is_dark() {
                            "Ink"
                        } else {
                            "Vellum"
                        },
                        if selected {
                            button::Weight::Primary
                        } else {
                            button::Weight::Regular
                        },
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.shell
                            .update(cx, |shell, cx| shell.set_appearance(appearance, cx));
                    }))
                }),
            ),
        )
    }

    fn size_setting(&mut self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.shell.read(cx).prefs().interface();
        setting(
            theme,
            "Interface size",
            "Scales every measurement in the window.",
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(space(Space::Snug))
                .child(
                    button::button(theme, "size-down", "−", button::Weight::Regular).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.shell.update(cx, |shell, cx| {
                                let next = shell.prefs().interface().stepped(false);
                                shell.set_interface(next, cx);
                            });
                        }),
                    ),
                )
                .child(
                    text::label(theme)
                        .font_family(theme.specimen())
                        .child(format!("{}%", current.get())),
                )
                .child(
                    button::button(theme, "size-up", "+", button::Weight::Regular).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.shell.update(cx, |shell, cx| {
                                let next = shell.prefs().interface().stepped(true);
                                shell.set_interface(next, cx);
                            });
                        }),
                    ),
                )
                .child(
                    button::button(theme, "size-reset", "Reset", button::Weight::Quiet).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.shell.update(cx, |shell, cx| {
                                shell.set_interface(InterfaceSize::DEFAULT, cx);
                            });
                        }),
                    ),
                ),
        )
    }

    fn motion_setting(&mut self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let reduced = self.shell.read(cx).reduced_motion();
        setting(
            theme,
            "Motion",
            "Panels and sheets spring open, or snap into place.",
        )
        .child(
            div()
                .flex()
                .gap(space(Space::Tight))
                .child(
                    button::button(
                        theme,
                        "motion-spring",
                        "Spring",
                        if reduced {
                            button::Weight::Regular
                        } else {
                            button::Weight::Primary
                        },
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.shell
                            .update(cx, |shell, cx| shell.set_reduced_motion(false, cx));
                    })),
                )
                .child(
                    button::button(
                        theme,
                        "motion-snap",
                        "Snap",
                        if reduced {
                            button::Weight::Primary
                        } else {
                            button::Weight::Regular
                        },
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.shell
                            .update(cx, |shell, cx| shell.set_reduced_motion(true, cx));
                    })),
                ),
        )
    }

    fn editor_setting(&mut self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.shell.read(cx).editor();
        setting(
            theme,
            "Open in editor",
            "Where a source site opens when it is followed.",
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap(space(Space::Tight))
                .children(EditorScheme::ALL.map(|scheme| {
                    let selected = scheme == current;
                    button::button(
                        theme,
                        format!("editor-{}", scheme.name()),
                        scheme.label(),
                        if selected {
                            button::Weight::Primary
                        } else {
                            button::Weight::Regular
                        },
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.shell
                            .update(cx, |shell, cx| shell.set_editor(scheme, cx));
                    }))
                })),
        )
        .child(text::faint(theme).child(format!(
            "{} opens the source of the page being read.",
            keys::OPEN_SOURCE.label()
        )))
    }

    /// Returns the page that connects an agent to this shelf over MCP.
    ///
    /// The command is assembled from this process's own paths — the MCP
    /// binary beside this one, the workspace this window owns, the project it
    /// discovered — so what a reader copies is the exact invocation that works
    /// on this machine, not a template with angle brackets.
    fn agents_setting(&mut self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let connect = self.connect_command(cx);
        let claude = connect.claude();
        let json = connect.json();
        let copy_claude = claude.clone();
        div()
            .flex()
            .flex_col()
            .gap(space(Space::Loose))
            .child(
                setting(
                    theme,
                    "Connect an agent",
                    "Every package on this shelf becomes a tool an agent can read. Add the MCP server once; it attaches to the same service this window is using.",
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(space(Space::Snug))
                        .child(icon::sized(theme, Icon::Spark, 13.0, Paint::Gilt))
                        .child(text::label(theme).font_weight(FontWeight::MEDIUM).child("Claude Code")),
                )
                .child(command_block(theme, "agents-claude", &claude))
                .child(
                    div()
                        .flex()
                        .gap(space(Space::Snug))
                        .child(
                            button::button(theme, "copy-claude", "Copy command", button::Weight::Primary)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.copy("Command copied", copy_claude.clone(), cx);
                                })),
                        )
                        .child(text::faint(theme).pt(px(5.0)).child(
                            "Run it in a terminal, then ask the agent about anything on the shelf.",
                        )),
                ),
            )
            .child(Self::json_setting(theme, &json, cx))
            .when(!connect.binary_present, |page| {
                page.child(
                    text::dim(theme)
                        .text_color(theme.paint(Paint::Caution))
                        .child(format!(
                            "The MCP binary was not found beside this one at {}. Build backend-mcp and place it there, or edit the path.",
                            connect.binary
                        )),
                )
            })
    }

    fn json_setting(theme: &Theme, json: &str, cx: &mut Context<Self>) -> impl IntoElement {
        let copy_json = json.to_owned();
        setting(
            theme,
            "Any MCP client",
            "Cursor, Codex, Zed, and others take the same server as JSON.",
        )
        .child(command_block(theme, "agents-json", json))
        .child(
            button::button(theme, "copy-json", "Copy JSON", button::Weight::Regular).on_click(
                cx.listener(move |this, _, _, cx| {
                    this.copy("JSON copied", copy_json.clone(), cx);
                }),
            ),
        )
    }

    fn service_setting(&mut self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let engine = self.engine.read(cx);
        let mode = match engine.mode() {
            HostMode::Embedded => "this window owns the service",
            HostMode::Attached => "attached to a service another process owns",
        };
        let endpoint = engine.endpoint().spelling();
        let revision = engine.revision().to_owned();
        let rows = engine.row_count();
        let project = engine.project().to_string_lossy().into_owned();
        let data = self.shell.read(cx).data().to_string_lossy().into_owned();
        let lanes: Vec<_> = engine.coverage().to_vec();
        setting(theme, "Service", mode)
            .child(fact_row(theme, "endpoint", &endpoint))
            .child(fact_row(theme, "workspace", &data))
            .child(fact_row(theme, "project", &project))
            .child(fact_row(
                theme,
                "revision",
                &format!("{revision} · {rows} rows"),
            ))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(space(Space::Tight))
                    .pt(space(Space::Tight))
                    .children(
                        lanes
                            .iter()
                            .map(|lane| chip::coverage_chip(theme, lane).into_any_element()),
                    ),
            )
    }

    fn capability_setting(&mut self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let chips = self.engine.read(cx).capabilities().to_vec();
        let (ready, probing, unavailable) = self.engine.read(cx).capability_totals();
        setting(
            theme,
            "Capabilities",
            "What this build can actually compile and search.",
        )
        .child(text::faint(theme).child(format!(
            "{ready} ready · {probing} probing · {unavailable} unavailable"
        )))
        .child(
            div().flex().flex_wrap().gap(space(Space::Tight)).children(
                chips
                    .iter()
                    .map(|chip| chip::capability_chip(theme, chip).into_any_element()),
            ),
        )
    }

    fn index_registry_setting(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let index = self.index.read(cx);
        let index_status = format!(
            "{} projects · {} declarations",
            index.project_count(),
            index.declaration_count()
        );
        let registry = self.registry.read(cx);
        let registry_status = match registry.page() {
            crate::store::registry::Loadable::Idle => "idle · no request".to_owned(),
            crate::store::registry::Loadable::Loading => "loading".to_owned(),
            crate::store::registry::Loadable::Ready(rows) => {
                format!("ready · {} packages", rows.len())
            }
            crate::store::registry::Loadable::Faulted(fault) => {
                format!("fault · {}", fault.cause().sentence())
            }
        };
        let query = registry.query().text();
        setting(
            theme,
            "Index and registry",
            "The local declaration index and the live package registry answer independently.",
        )
        .child(fact_row(theme, "index", &index_status))
        .child(fact_row(
            theme,
            "registry",
            &format!("{registry_status} · query {query:?}"),
        ))
    }

    /// Assembles the MCP invocation for this machine.
    fn connect_command(&self, cx: &Context<Self>) -> Connect {
        let binary = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf))
            .map_or_else(
                || std::path::PathBuf::from(MCP_BINARY),
                |dir| dir.join(MCP_BINARY),
            );
        Connect {
            binary_present: binary.is_file(),
            binary: binary.to_string_lossy().into_owned(),
            workspace: self.shell.read(cx).data().to_string_lossy().into_owned(),
            project: self
                .engine
                .read(cx)
                .project()
                .to_string_lossy()
                .into_owned(),
        }
    }
}

/// The MCP binary's file name on this platform.
const MCP_BINARY: &str = if cfg!(target_os = "windows") {
    "backend-mcp.exe"
} else {
    "backend-mcp"
};

/// The three paths an MCP client needs, and the two spellings clients take.
struct Connect {
    binary: String,
    binary_present: bool,
    workspace: String,
    project: String,
}

impl Connect {
    fn claude(&self) -> String {
        format!(
            "claude mcp add nudox --scope user -- {} --workspace {} --project {}",
            shell_quote(&self.binary),
            shell_quote(&self.workspace),
            shell_quote(&self.project)
        )
    }

    fn json(&self) -> String {
        format!(
            "{{\n  \"mcpServers\": {{\n    \"nudox\": {{\n      \"command\": {},\n      \"args\": [\"--workspace\", {}, \"--project\", {}]\n    }}\n  }}\n}}",
            json_quote(&self.binary),
            json_quote(&self.workspace),
            json_quote(&self.project)
        )
    }
}

fn shell_quote(text: &str) -> String {
    if text
        .chars()
        .all(|letter| letter.is_ascii_alphanumeric() || "/._-:@+=".contains(letter))
    {
        return text.to_owned();
    }
    if cfg!(target_os = "windows") {
        format!("\"{}\"", text.replace('"', "\\\""))
    } else {
        format!("'{}'", text.replace('\'', "'\\''"))
    }
}

fn json_quote(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| format!("\"{}\"", text.replace('"', "\\\"")))
}

fn command_block(theme: &Theme, id: &'static str, command: &str) -> gpui::Stateful<Div> {
    surface::sunken(theme)
        .id(ElementId::Name(SharedString::new_static(id)))
        .w_full()
        .px(space(Space::Base))
        .py(space(Space::Snug))
        .overflow_x_scroll()
        .font_family(theme.specimen())
        .text_size(type_size(TypeScale::Small))
        .text_color(theme.paint(Paint::Text))
        .child(command.to_owned())
}

fn fact_row(theme: &Theme, name: &str, value: &str) -> Div {
    div()
        .flex()
        .items_start()
        .gap(space(Space::Snug))
        .child(
            text::faint(theme)
                .flex_none()
                .w(px(76.0))
                .pt(px(1.0))
                .child(name.to_owned()),
        )
        .child(
            text::dim(theme)
                .flex_1()
                .min_w(px(0.0))
                .font_family(theme.specimen())
                .child(value.to_owned()),
        )
}

/// Returns the legend: every mark this window draws, and what it means.
///
/// The interface leans on nineteen drawn kind marks and eight language
/// logos, all on one luminance plane. That is only readable if a reader can
/// find out, once, what each shape stands for — so the vocabulary is written
/// down inside the product rather than assumed.
fn legend_setting(theme: &Theme) -> Div {
    setting(
        theme,
        "Legend",
        "Every mark this window draws, and what it stands for.",
    )
    .child(div().flex().flex_wrap().gap(space(Space::Snug)).children(
        crate::theme::kind::ALL_KINDS.map(|kind| {
            div()
                .flex()
                .items_center()
                .gap(space(Space::Tight))
                .child(glyph::kind_tile(theme, Some(kind), false))
                .child(text::faint(theme).child(glyph::kind_label(Some(kind))))
        }),
    ))
    .child(div().flex().flex_wrap().gap(space(Space::Snug)).children(
        backend_present::Language::ALL.map(|language| {
            let ink = theme.on_plane(crate::theme::language::hue(language));
            div()
                .flex()
                .items_center()
                .gap(space(Space::Tight))
                .child(glyph::language_tag(theme, language))
                .when_some(icon::Logo::of(language), |row, logo| {
                    row.child(icon::logo(logo, 12.0, ink))
                })
                .child(text::faint(theme).child(crate::theme::language::label(language)))
        }),
    ))
    .child(
        div().flex().flex_wrap().gap(space(Space::Base)).children(
            [
                ("◐", "indexing or partial"),
                ("✗", "failed or unavailable"),
                ("○", "requested or empty"),
            ]
            .map(|(mark, means)| {
                div()
                    .flex()
                    .items_center()
                    .gap(space(Space::Tight))
                    .child(text::dim(theme).child(mark))
                    .child(text::faint(theme).child(means))
            }),
        ),
    )
}

fn setting(theme: &Theme, title: &str, detail: &str) -> Div {
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(space(Space::Snug))
        .child(
            text::label(theme)
                .font_weight(FontWeight::SEMIBOLD)
                .child(title.to_owned()),
        )
        .child(text::dim(theme).child(detail.to_owned()))
}

fn reserved(theme: &Theme) -> Div {
    div()
        .h(px(CARD_RESERVED))
        .flex()
        .flex_col()
        .justify_center()
        .gap(space(Space::Snug))
        .child(
            div()
                .w(px(220.0))
                .h(px(12.0))
                .rounded_full()
                .bg(theme.paint(Paint::Hover)),
        )
        .child(
            div()
                .w(px(300.0))
                .h(px(10.0))
                .rounded_full()
                .bg(theme.paint(Paint::Hover)),
        )
}

fn filled(theme: &Theme, card: &HoverCard) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(space(Space::Snug))
        .child(
            div()
                .flex()
                .items_center()
                .gap(space(Space::Snug))
                .child(glyph::kind_tile(theme, card.kind(), false))
                .child(
                    text::single_line(text::identity_text(theme, TypeScale::Interface))
                        .child(card.identity().name().to_owned()),
                )
                .child(text::faint(theme).child(glyph::kind_label(card.kind()))),
        )
        .when(!card.signature().is_empty(), |body| {
            body.child(
                div()
                    .font_family(theme.specimen())
                    .text_size(type_size(TypeScale::Small))
                    .text_color(theme.paint(Paint::TextDim))
                    .child(card.signature().to_owned()),
            )
        })
        .when_some(card.summary().map(ToOwned::to_owned), |body, summary| {
            body.child(text::dim(theme).child(summary))
        })
}

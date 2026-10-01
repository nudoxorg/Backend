//! Settings, plain: Appearance is one calm list — theme, contrast, density,
//! text size, motion — one control per row; the window itself is the
//! preview (every change re-resolves the whole shell live). The other pages
//! say what the index holds, which keys do what, and which build this is.

use super::{Ctx, Leaf};
use crate::core::{Activity, Resource, ResourceTerminal, UnavailableReason};
use crate::model::{
    AppSnapshot, AppearancePreference, ConnectionStatus, ContrastPreference, DensityPreference,
    MotionPreference, ZoomPreference,
};
use crate::navigation::{Intent, SettingsPage};
use crate::shell::kit::{quiet, text};
use crate::shell::reader::Reader;
use facet::controls::Swatch;
use facet::icons::{Kind, KindSize, kind_mark};
use facet::tokens::ty;
use facet::{Measure, Palette, Space};
use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, Window, div, px};
use std::path::{Path, PathBuf};

pub(super) fn body(
    page: SettingsPage,
    snapshot: &AppSnapshot,
    store: &super::Pages,
    ctx: &mut Ctx<'_>,
    window: &mut Window,
    cx: &mut Context<Reader>,
) -> Vec<Leaf> {
    match page {
        SettingsPage::Index => index(store, ctx),
        SettingsPage::Registry => registry(ctx),
        SettingsPage::Help => keys(ctx),
        SettingsPage::Legend => legend(ctx),
        SettingsPage::Diagnostics => diagnostics(snapshot, ctx),
        SettingsPage::Connections => connections(snapshot, ctx),
        SettingsPage::Agents => agents(snapshot, ctx),
        SettingsPage::Privacy => privacy(ctx),
        SettingsPage::Editor => editor(ctx),
        SettingsPage::Appearance => appearance(snapshot, ctx, window, cx),
    }
}

fn title(words: &str, ctx: &mut Ctx<'_>) -> Leaf {
    let said = ctx.say(words.to_owned());
    Leaf::new(
        text(ty::DISPLAY, &ctx.measure, ctx.palette.ink0)
            .pb(ctx.measure.space(Space::Base))
            .child(said),
    )
}

fn appearance(
    snapshot: &AppSnapshot,
    ctx: &mut Ctx<'_>,
    window: &mut Window,
    cx: &mut Context<Reader>,
) -> Vec<Leaf> {
    // One calm list, one control per row; the window is the preview.
    let settings = snapshot.settings();
    let measure = ctx.measure;
    let mut leaves = vec![title("Appearance", ctx)];
    let links = ctx.links.clone();
    let theme = facet::controls::theme_toggle(
        "set-theme",
        match settings.appearance {
            AppearancePreference::System => Swatch::System,
            AppearancePreference::Abyss => Swatch::Abyss,
            AppearancePreference::Glacier => Swatch::Glacier,
        },
        &measure,
    )
    .on_select(move |index, _, cx| {
        let appearance = match index {
            0 => AppearancePreference::System,
            1 => AppearancePreference::Abyss,
            _ => AppearancePreference::Glacier,
        };
        links.dispatch(Intent::SetAppearance(appearance), cx);
    });
    let words = ["System", "Abyss", "Glacier"];
    ctx.say(
        words[match settings.appearance {
            AppearancePreference::System => 0,
            AppearancePreference::Abyss => 1,
            AppearancePreference::Glacier => 2,
        }],
    );
    leaves.push(setting("Theme", theme.into_any_element(), ctx));
    let links = ctx.links.clone();
    let contrast = facet::controls::seg("set-contrast", &measure)
        .label("Normal")
        .label("High")
        .selected(usize::from(settings.contrast == ContrastPreference::High))
        .on_select(move |index, _, cx| {
            let contrast = if index == 1 {
                ContrastPreference::High
            } else {
                ContrastPreference::Normal
            };
            links.dispatch(Intent::SetContrast(contrast), cx);
        });
    leaves.push(setting("Contrast", contrast.into_any_element(), ctx));
    let links = ctx.links.clone();
    let density = facet::controls::density_toggle(
        "set-density",
        match settings.density {
            DensityPreference::Comfortable => facet::Density::Comfortable,
            DensityPreference::Compact => facet::Density::Compact,
            DensityPreference::Dense => facet::Density::Dense,
        },
        &measure,
    )
    .on_select(move |index, _, cx| {
        let density = match index {
            0 => DensityPreference::Comfortable,
            1 => DensityPreference::Compact,
            _ => DensityPreference::Dense,
        };
        links.dispatch(Intent::SetDensity(density), cx);
    });
    leaves.push(setting("Density", density.into_any_element(), ctx));
    let display = crate::shell::system::display_key(window, cx);
    let current_percent = settings.zoom.percent(&display);
    let selected = ZoomPreference::LADDER
        .iter()
        .position(|percent| *percent == current_percent)
        .unwrap_or(2);
    let links = ctx.links.clone();
    let mut text_size = facet::controls::seg("set-text-size", &measure);
    for percent in ZoomPreference::LADDER {
        text_size = text_size.label(format!("{percent}%"));
    }
    let text_size = text_size.selected(selected).on_select(move |index, window, cx| {
        let Some(percent) = ZoomPreference::LADDER.get(index).copied() else {
            return;
        };
        links.dispatch(
            Intent::ZoomTo {
                display: crate::shell::system::display_key(window, cx),
                percent,
            },
            cx,
        );
    });
    leaves.push(setting("Text size", text_size.into_any_element(), ctx));
    let palette = ctx.palette;
    let text_note = ctx.say("Relative to the operating system’s text scale; remembered per display.");
    leaves.push(Leaf::new(quiet(text_note, &measure, palette)));
    let links = ctx.links.clone();
    let motion = facet::controls::seg("set-motion", &measure)
        .label("System")
        .label("Full")
        .label("Reduced")
        .selected(match settings.motion {
            MotionPreference::System => 0,
            MotionPreference::Full => 1,
            MotionPreference::Reduced => 2,
        })
        .on_select(move |index, _, cx| {
            let motion = match index {
                0 => MotionPreference::System,
                1 => MotionPreference::Full,
                _ => MotionPreference::Reduced,
            };
            links.dispatch(Intent::SetMotion(motion), cx);
        });
    leaves.push(setting("Motion", motion.into_any_element(), ctx));
    leaves
}

/// One row: the setting's name, its one control at the far end.
fn setting(name: &str, control: AnyElement, ctx: &mut Ctx<'_>) -> Leaf {
    let measure = ctx.measure;
    let palette = ctx.palette;
    let name = ctx.say(name.to_owned());
    Leaf::new(
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap(measure.space(Space::Roomy))
            .py(measure.space(Space::Roomy))
            .border_b_1()
            .border_color(palette.line1.hsla())
            .child(text(ty::ROW, &measure, palette.ink1).child(name))
            .child(control),
    )
}

fn index(store: &super::Pages, ctx: &mut Ctx<'_>) -> Vec<Leaf> {
    let mut leaves = vec![title("Index & registries", ctx)];
    let measure = ctx.measure;
    let palette = ctx.palette;
    // What compiles your code: the Rust the app found (or where it looked),
    // in the words the first run says it in.
    if let Some(rust) = crate::host::toolchain::report() {
        let missing = matches!(rust, crate::host::toolchain::Rust::Missing { .. });
        let words = ctx.say(rust.words());
        let ink = if missing {
            palette.coral.base
        } else {
            palette.ink1
        };
        leaves.push(setting(
            "Compiler",
            text(ty::SMALL, &measure, ink)
                .min_w(px(0.0))
                .child(words)
                .into_any_element(),
            ctx,
        ));
    }
    let health = store.health();
    match health.loaded_value() {
        Some(health) => {
            let facts = [
                ("Declarations", health.rows.to_string()),
                (
                    "Files indexed",
                    format!(
                        "{} of {}",
                        health.ingest.files_indexed, health.ingest.files_discovered
                    ),
                ),
                (
                    "Files without declarations",
                    health.ingest.files_unavailable.to_string(),
                ),
                ("Capabilities ready", health.ready_capabilities.join(", ")),
            ];
            for (name, value) in facts {
                let value = ctx.say(value);
                leaves.push(setting(
                    name,
                    text(ty::MONO_ROW, &measure, palette.ink1)
                        .child(value)
                        .into_any_element(),
                    ctx,
                ));
            }
            for language in health.ingest.languages.iter() {
                let value = ctx.say(format!(
                    "{} declarations in {} files",
                    language.declarations, language.files
                ));
                leaves.push(setting(
                    &language.language,
                    text(ty::MONO_SMALL, &measure, palette.ink2)
                        .child(value)
                        .into_any_element(),
                    ctx,
                ));
            }
        }
        None => {}
    }
    if let Some((message, problem)) = health_notice(&health) {
        let words = ctx.say(message);
        let ink = if problem { palette.coral.base } else { palette.ink3 };
        leaves.push(Leaf::new(text(ty::SMALL, &measure, ink).child(words)));
    }
    leaves
}

/// What the health resource actually says when there is no complete current
/// value (or when a previous value is being retained through a failed refresh).
fn health_notice(resource: &Resource<crate::model::pages::HealthModel>) -> Option<(String, bool)> {
    let present = resource.loaded_value().is_some();
    match (present, resource.terminal(), resource.activity()) {
        (true, ResourceTerminal::Complete, Activity::Working | Activity::Waiting) => Some((
            "Updating the index report; the last complete report is shown.".to_owned(),
            false,
        )),
        (true, ResourceTerminal::Complete, Activity::Stopped) => Some((
            "The index refresh stopped; the last complete report is shown.".to_owned(),
            false,
        )),
        (true, ResourceTerminal::Complete, Activity::Rest | Activity::NotYet) => None,
        (true, ResourceTerminal::Partial, _) => Some((
            "This index report is incomplete while the service is still building it.".to_owned(),
            false,
        )),
        (true, ResourceTerminal::Unavailable(reason), _) => {
            Some((unavailable_health_words(reason).to_owned(), true))
        }
        (true, ResourceTerminal::Fault(error), _) => Some((
            format!("The index refresh failed; the last report is shown: {}", error.message()),
            true,
        )),
        (false, ResourceTerminal::Complete, Activity::NotYet) => Some((
            "The index health report has not been requested yet.".to_owned(),
            false,
        )),
        (false, ResourceTerminal::Complete, Activity::Waiting) => Some((
            "Waiting for the local index health report.".to_owned(),
            false,
        )),
        (false, ResourceTerminal::Complete, Activity::Working) => Some((
            "Loading the local index health report.".to_owned(),
            false,
        )),
        (false, ResourceTerminal::Complete, Activity::Rest | Activity::Stopped) => {
            Some(("No index health report is available.".to_owned(), false))
        }
        (false, ResourceTerminal::Partial, _) => Some((
            "The local service is still building the index health report.".to_owned(),
            false,
        )),
        (false, ResourceTerminal::Unavailable(reason), _) => {
            Some((unavailable_health_words(reason).to_owned(), true))
        }
        (false, ResourceTerminal::Fault(error), _) => Some((
            format!("The index health check failed: {}", error.message()),
            true,
        )),
    }
}

fn unavailable_health_words(reason: &UnavailableReason) -> &'static str {
    match reason {
        UnavailableReason::Unsupported => "The connected service does not provide index health.",
        UnavailableReason::OutOfScope => "Index health is unavailable outside the selected project scope.",
    }
}

fn registry(ctx: &mut Ctx<'_>) -> Vec<Leaf> {
    let mut leaves = vec![title("Registry sources", ctx)];
    let measure = ctx.measure;
    let palette = ctx.palette;
    let home = std::env::var_os("NUDOX_CARGO_HOME")
        .or_else(|| std::env::var_os("CARGO_HOME"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")));
    let source_root = std::env::var_os("NUDOX_CARGO_ROOT").map(PathBuf::from);
    let home_words = ctx.say(home.as_ref().map_or_else(
        || "No Cargo home could be resolved from NUDOX_CARGO_HOME, CARGO_HOME, or HOME.".to_owned(),
        |path| path.display().to_string(),
    ));
    leaves.push(setting(
        "Cargo home",
        text(ty::MONO_SMALL, &measure, palette.ink1)
            .min_w(px(0.0))
            .child(home_words)
            .into_any_element(),
        ctx,
    ));
    let root_words = ctx.say(source_root.as_ref().map_or_else(
        || {
            "Not set; the local reader uses the effective crates.io source under Cargo home."
                .to_owned()
        },
        |path| path.display().to_string(),
    ));
    leaves.push(setting(
        "Selected source root",
        text(ty::MONO_SMALL, &measure, palette.ink1)
            .min_w(px(0.0))
            .child(root_words)
            .into_any_element(),
        ctx,
    ));
    let detail = ctx.say("If the same release appears under conflicting cached indexes, set NUDOX_CARGO_ROOT to the intended registry source root and restart the desktop. The exact index path is shown in the release notice. Local archives are unpacked only when their checksum is known for the selected source.");
    leaves.push(Leaf::new(quiet(detail, &measure, palette)));
    let offline = ctx.say("Explicit Add actions use the connected local service’s RegistryGateway. The desktop does not know its configured online/offline policy; start backend-locald with --registry-offline to disable network acquisition.");
    leaves.push(Leaf::new(quiet(offline, &measure, palette)));
    if source_root.is_none() {
        let example =
            "NUDOX_CARGO_ROOT=/path/to/CARGO_HOME/registry/src/index.crates.io-…".to_owned();
        let template_text = ctx.say(example.clone());
        leaves.push(Leaf::new(
            text(ty::MONO_SMALL, &measure, palette.ink2).child(template_text),
        ));
        let copy = facet::controls::button(
            "settings-copy-registry-root",
            "Copy setup template",
            &measure,
        )
        .ghost()
        .on_click(move |_, cx| {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(example.clone()))
        });
        leaves.push(setting(
            "Authority resolution",
            copy.into_any_element(),
            ctx,
        ));
    }
    leaves
}

fn keys(ctx: &mut Ctx<'_>) -> Vec<Leaf> {
    let mut leaves = vec![title("Keys", ctx)];
    let mut shown = Vec::new();
    for key in crate::shell::keys::TABLE {
        if shown.contains(&key.command) {
            continue;
        }
        shown.push(key.command);
        let caps = crate::shell::keys::TABLE
            .iter()
            .filter(|other| other.command == key.command)
            .map(|other| other.cap)
            .collect::<Vec<_>>()
            .join("  ");
        let measure = ctx.measure;
        let palette = ctx.palette;
        let caps = ctx.say(caps);
        leaves.push(setting(
            key.says,
            text(ty::MONO_ROW, &measure, palette.ink1)
                .child(caps)
                .into_any_element(),
            ctx,
        ));
    }
    // The sidebar's and the graph's own keys, while each has the keyboard.
    let groups: [(&str, Vec<(&str, &str)>); 2] = [
        (
            "In the sidebar",
            crate::shell::side::KEYS
                .iter()
                .map(|key| (key.cap, key.says))
                .collect(),
        ),
        (
            "In the graph",
            facet::graph::keys::KEYS
                .iter()
                .map(|key| (key.cap, key.says))
                .collect(),
        ),
    ];
    for (group, keys) in groups {
        let measure = ctx.measure;
        let palette = ctx.palette;
        let heading = ctx.say(group);
        leaves.push(Leaf::new(
            text(ty::HEAD, &measure, palette.ink0)
                .pt(measure.space(Space::Roomy))
                .pb(measure.space(Space::Base))
                .child(heading),
        ));
        for (cap, says) in keys {
            let caps = ctx.say(cap);
            leaves.push(setting(
                says,
                text(ty::MONO_ROW, &measure, palette.ink1)
                    .child(caps)
                    .into_any_element(),
                ctx,
            ));
        }
    }
    leaves
}

fn legend(ctx: &mut Ctx<'_>) -> Vec<Leaf> {
    let mut leaves = vec![title("Legend", ctx)];
    let measure = ctx.measure;
    let palette = ctx.palette;
    let intro = ctx.say("Declaration mark shape names its kind; its hue groups the kind by family.");
    leaves.push(Leaf::new(quiet(intro, &measure, palette)));
    let heading = ctx.say("Declaration kinds");
    leaves.push(Leaf::new(
        text(ty::HEAD, &measure, palette.ink0)
            .pt(measure.space(Space::Roomy))
            .pb(measure.space(Space::Base))
            .child(heading),
    ));
    for kind in Kind::ALL {
        let mark = kind_mark(kind, KindSize::Sm, palette);
        let name = ctx.say(kind.name());
        let family = ctx.say(family_name(kind.family()));
        let row = div()
            .flex()
            .items_center()
            .justify_between()
            .gap(measure.space(Space::Roomy))
            .py(measure.space(Space::Base))
            .border_b_1()
            .border_color(palette.line1.hsla())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(measure.space(Space::Base))
                    .child(mark)
                    .child(text(ty::ROW, &measure, palette.ink1).child(name)),
            )
            .child(text(ty::MONO_SMALL, &measure, palette.ink2).child(family));
        leaves.push(Leaf::new(row));
    }

    let heading = ctx.say("Sidebar state marks");
    leaves.push(Leaf::new(
        text(ty::HEAD, &measure, palette.ink0)
            .pt(measure.space(Space::Roomy))
            .pb(measure.space(Space::Base))
            .child(heading),
    ));
    for (label, meaning, color) in [
        ("Your uses", "Mint notch and use count", palette.mint.base),
        ("Changed", "Amber diamond and change count", palette.amber.base),
        ("Gone", "Coral status words", palette.coral.base),
        ("Members", "Quiet count when no other state mark is shown", palette.ink3),
    ] {
        let meaning = ctx.say(meaning);
        leaves.push(setting(
            label,
            text(ty::ROW, &measure, color)
                .child(meaning)
                .into_any_element(),
            ctx,
        ));
    }

    let heading = ctx.say("Focus and motion colors");
    leaves.push(Leaf::new(
        text(ty::HEAD, &measure, palette.ink0)
            .pt(measure.space(Space::Roomy))
            .pb(measure.space(Space::Base))
            .child(heading),
    ));
    for (label, meaning, color) in [
        ("Mint", "Actions, yours, or done", palette.mint.base),
        ("Periwinkle", "Keyboard focus", palette.peri.base),
        ("Amber", "Waiting", palette.amber.base),
        ("Coral", "Stopped or unavailable", palette.coral.base),
    ] {
        let meaning = ctx.say(meaning);
        leaves.push(setting(
            label,
            text(ty::ROW, &measure, color)
                .child(meaning)
                .into_any_element(),
            ctx,
        ));
    }
    leaves
}

const fn family_name(family: facet::tokens::Family) -> &'static str {
    match family {
        facet::tokens::Family::Namespace => "Namespace",
        facet::tokens::Family::Type => "Type",
        facet::tokens::Family::Contract => "Contract",
        facet::tokens::Family::Callable => "Callable",
        facet::tokens::Family::Value => "Value",
    }
}

fn diagnostics(snapshot: &AppSnapshot, ctx: &mut Ctx<'_>) -> Vec<Leaf> {
    let mut leaves = vec![title("Diagnostics", ctx)];
    let measure = ctx.measure;
    let palette = ctx.palette;
    let version = ctx.say(env!("CARGO_PKG_VERSION"));
    leaves.push(setting(
        "Version",
        text(ty::MONO_ROW, &measure, palette.ink1)
            .child(version)
            .into_any_element(),
        ctx,
    ));
    let mode = ctx.say(match snapshot.settings().service_mode {
        crate::model::ServiceMode::Embedded => "embedded local service",
        crate::model::ServiceMode::Attached => "attached to a running local service",
    });
    leaves.push(setting(
        "Service",
        text(ty::ROW, &measure, palette.ink1)
            .child(mode)
            .into_any_element(),
        ctx,
    ));
    let connection = ctx.say(connection_words(snapshot.settings().connection));
    leaves.push(setting(
        "Connection check",
        text(ty::ROW, &measure, palette.ink1)
            .child(connection)
            .into_any_element(),
        ctx,
    ));
    let (project, data, endpoint) = owner_paths(snapshot);
    for (label, path) in [
        ("Project", project),
        ("Workspace data", data),
        ("Local endpoint", endpoint),
    ] {
        if let Some(path) = path {
            let words = ctx.say(path.to_string_lossy().into_owned());
            leaves.push(setting(
                label,
                text(ty::MONO_SMALL, &measure, palette.ink1)
                    .child(words)
                    .into_any_element(),
                ctx,
            ));
        }
    }
    leaves
}

fn connections(snapshot: &AppSnapshot, ctx: &mut Ctx<'_>) -> Vec<Leaf> {
    let mut leaves = vec![title("Connections", ctx)];
    let measure = ctx.measure;
    let palette = ctx.palette;
    let words = ctx.say(connection_words(snapshot.settings().connection));
    leaves.push(setting(
        "Desktop to local index",
        text(ty::ROW, &measure, palette.ink1)
            .child(words)
            .into_any_element(),
        ctx,
    ));
    let mode = ctx.say(match snapshot.settings().service_mode {
        crate::model::ServiceMode::Embedded => "This desktop starts and owns its local service.",
        crate::model::ServiceMode::Attached => {
            "This desktop shares a local service started elsewhere."
        }
    });
    leaves.push(Leaf::new(quiet(mode, &measure, palette)));
    let links = ctx.links.clone();
    let testing = snapshot.settings().connection == ConnectionStatus::Testing;
    let probe = facet::controls::button("settings-test-connection", "Test connection", &measure)
        .primary()
        .busy(testing)
        .on_click(move |_, cx| links.dispatch(Intent::TestConnection, cx));
    leaves.push(setting("Local service", probe.into_any_element(), ctx));
    let external = ctx.say("MCP client connections are configured in the client that launches the server; this check reports only the desktop’s local service link.");
    leaves.push(Leaf::new(quiet(external, &measure, palette)));
    leaves
}

fn agents(snapshot: &AppSnapshot, ctx: &mut Ctx<'_>) -> Vec<Leaf> {
    let mut leaves = vec![title("Agents & MCP", ctx)];
    let measure = ctx.measure;
    let palette = ctx.palette;
    let connection = ctx.say(format!(
        "Desktop local index: {}",
        connection_words(snapshot.settings().connection)
    ));
    leaves.push(setting(
        "Local service",
        text(ty::ROW, &measure, palette.ink1)
            .child(connection)
            .into_any_element(),
        ctx,
    ));
    let installed = mcp_binary();
    let binary_status = ctx.say(installed.as_ref().map_or_else(
        || "backend-mcp is not on PATH or beside this app. Build or install the backend-mcp executable, then use the configuration below.".to_owned(),
        |path| format!("MCP server found at {}", path.display()),
    ));
    let status_color = if installed.is_some() {
        palette.ink1
    } else {
        palette.coral.base
    };
    leaves.push(setting(
        "Server executable",
        text(ty::SMALL, &measure, status_color)
            .min_w(px(0.0))
            .child(binary_status)
            .into_any_element(),
        ctx,
    ));
    let locald = installed.as_deref().and_then(locald_companion);
    let locald_status = ctx.say(locald.as_ref().map_or_else(
        || "backend-locald must be installed beside backend-mcp so this setup can start the local service when Nudox is closed.".to_owned(),
        |path| format!("Local service executable found at {}", path.display()),
    ));
    leaves.push(setting(
        "Service executable",
        text(
            ty::SMALL,
            &measure,
            if locald.is_some() {
                palette.ink1
            } else {
                palette.coral.base
            },
        )
        .min_w(px(0.0))
        .child(locald_status)
        .into_any_element(),
        ctx,
    ));
    let recheck = facet::controls::button("settings-recheck-mcp", "Check again", &measure)
        .ghost()
        .on_click(|_, cx| cx.refresh_windows());
    leaves.push(setting("Discovery", recheck.into_any_element(), ctx));
    let (project, data, endpoint) = owner_paths(snapshot);
    let (Some(project), Some(data), Some(endpoint)) = (project, data, endpoint) else {
        let detail = ctx.say("Add a project and start or reconnect to the local service before configuring an MCP client. Use Settings › Connections to test the desktop's local service link.");
        leaves.push(Leaf::new(quiet(detail, &measure, palette)));
        return leaves;
    };
    let Some(binary) = installed.as_deref() else {
        let detail = ctx.say("Install backend-mcp beside Nudox, in ~/.cargo/bin, ~/.local/bin, a Homebrew or Nix bin directory, or on PATH. Then choose Check again to refresh discovery.");
        leaves.push(Leaf::new(quiet(detail, &measure, palette)));
        return leaves;
    };
    let Some(locald) = locald.as_deref() else {
        let detail = ctx.say("Install backend-locald beside backend-mcp. The MCP process uses that exact companion when it needs to start the local index; when Nudox is already open it attaches to the live service.");
        leaves.push(Leaf::new(quiet(detail, &measure, palette)));
        return leaves;
    };
    let Some(config) = mcp_config(&project, &data, &endpoint, binary, locald) else {
        let detail = ctx.say("The MCP setup paths could not be represented in the client configuration. Check the project and workspace paths in Settings › Diagnostics.");
        leaves.push(Leaf::new(quiet(detail, &measure, palette)));
        return leaves;
    };
    let explanation = ctx.say("Paste this server entry into your MCP client configuration. It launches a local stdio server against this project and the same local index. The desktop cannot see whether your external client has connected.");
    leaves.push(Leaf::new(quiet(explanation, &measure, palette)));
    let shown = ctx.say(config.clone());
    leaves.push(Leaf::new(
        text(ty::MONO_SMALL, &measure, palette.ink1).child(shown),
    ));
    let copy = facet::controls::button("settings-copy-mcp-config", "Copy MCP setup", &measure)
        .ghost()
        .on_click(move |_, cx| {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(config.clone()))
        });
    leaves.push(setting("Client setup", copy.into_any_element(), ctx));
    leaves
}

fn editor(ctx: &mut Ctx<'_>) -> Vec<Leaf> {
    let mut leaves = vec![title("Editor", ctx)];
    let measure = ctx.measure;
    let palette = ctx.palette;
    let command = ctx.say("code -g path:line → zed path:line → the operating system’s registered file opener");
    leaves.push(setting(
        "Open in editor",
        text(ty::MONO_SMALL, &measure, palette.ink1)
            .min_w(px(0.0))
            .child(command)
            .into_any_element(),
        ctx,
    ));
    let words = ctx.say("This desktop build has no editor preference field. The first available command wins; the operating-system fallback opens the file without a line target.");
    leaves.push(Leaf::new(quiet(words, &measure, palette)));
    leaves
}

fn privacy(ctx: &mut Ctx<'_>) -> Vec<Leaf> {
    let mut leaves = vec![title("Privacy & local data", ctx)];
    let measure = ctx.measure;
    let palette = ctx.palette;
    let detail = ctx.say("Project source and the local index stay on this machine. Registry metadata can be fetched by the local service unless it was started with --registry-offline.");
    leaves.push(Leaf::new(quiet(detail, &measure, palette)));
    for (label, message) in [
        (
            "Network policy",
            "This desktop build cannot change the local service’s network policy here. See Registry sources for the service-level offline option.",
        ),
        (
            "Advisories",
            "The saved desktop advisory toggle is not connected to the local service; it has no effect in this build.",
        ),
        (
            "Metadata cache",
            "Cache enablement and retention are not controlled by this desktop build.",
        ),
    ] {
        let message = ctx.say(message);
        leaves.push(setting(
            label,
            text(ty::SMALL, &measure, palette.ink3)
                .min_w(px(0.0))
                .child(message)
                .into_any_element(),
            ctx,
        ));
    }
    leaves
}

fn connection_words(status: ConnectionStatus) -> &'static str {
    match status {
        ConnectionStatus::Unknown => "Not checked yet",
        ConnectionStatus::Testing => "Checking the local service…",
        ConnectionStatus::Connected => "Connected",
        ConnectionStatus::Disconnected => "Unavailable",
    }
}

fn owner_paths(snapshot: &AppSnapshot) -> (Option<PathBuf>, Option<PathBuf>, Option<PathBuf>) {
    let project = snapshot
        .workspace()
        .active
        .as_ref()
        .or(snapshot.workspace().host.as_ref())
        .map(|project| PathBuf::from(project.as_str()));
    let composition = crate::host::registry::composed();
    let endpoint = composition
        .as_ref()
        .map(|composition| composition.endpoint.clone());
    let data = composition
        .as_ref()
        .and_then(|composition| composition.refusals.as_ref())
        .and_then(|refusals| refusals.parent())
        .and_then(Path::parent)
        .map(Path::to_path_buf);
    (project, data, endpoint)
}

fn mcp_config(
    project: &Path,
    data: &Path,
    endpoint: &Path,
    binary: &Path,
    locald: &Path,
) -> Option<String> {
    let project = project.to_str()?;
    let data = data.to_str()?;
    let endpoint = endpoint.to_str()?;
    let binary = binary.to_str()?;
    let locald = locald.to_str()?;
    let config = serde_json::json!({
        "mcpServers": {
            "nudox": {
                "command": binary,
                "env": {
                    "BACKEND_LOCALD_BIN": locald
                },
                "args": [
                    "--project", project,
                    "--workspace", data,
                    "--endpoint", endpoint
                ]
            }
        }
    });
    serde_json::to_string_pretty(&config).ok()
}

fn mcp_binary() -> Option<PathBuf> {
    let executable = if cfg!(windows) {
        "backend-mcp.exe"
    } else {
        "backend-mcp"
    };
    let sibling = std::env::current_exe()
        .ok()
        .and_then(|current| current.parent().map(|parent| parent.join(executable)));
    find_mcp_binary(
        executable,
        sibling,
        std::env::var_os("PATH"),
        std::env::var_os("NUDOX_CARGO_HOME")
            .or_else(|| std::env::var_os("CARGO_HOME"))
            .map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    )
}

fn find_mcp_binary(
    executable: &str,
    sibling: Option<PathBuf>,
    path: Option<std::ffi::OsString>,
    cargo_home: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    let mut candidates = sibling.into_iter().collect::<Vec<_>>();
    if let Some(path) = path {
        candidates.extend(
            std::env::split_paths(&path)
                .filter(|directory| directory.is_absolute())
                .map(|directory| directory.join(executable)),
        );
    }
    if let Some(cargo_home) = cargo_home.filter(|path| path.is_absolute()) {
        candidates.push(cargo_home.join("bin").join(executable));
    }
    if let Some(home) = home.filter(|path| path.is_absolute()) {
        candidates.extend(
            [".cargo/bin", ".local/bin", ".nix-profile/bin"]
                .into_iter()
                .map(|directory| home.join(directory).join(executable)),
        );
    }
    candidates.extend(
        [
            "/opt/homebrew/bin",
            "/usr/local/bin",
            "/run/current-system/sw/bin",
            "/nix/var/nix/profiles/default/bin",
        ]
        .into_iter()
        .map(|directory| PathBuf::from(directory).join(executable)),
    );
    let mut seen = std::collections::HashSet::new();
    candidates
        .into_iter()
        .filter(|candidate| seen.insert(candidate.clone()))
        .find(|candidate| is_executable_file(candidate))
}

fn locald_companion(mcp: &Path) -> Option<PathBuf> {
    let locald = mcp.with_file_name(format!("backend-locald{}", std::env::consts::EXE_SUFFIX));
    is_executable_file(&locald).then_some(locald)
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[allow(dead_code)]
fn _palette(_: &Palette, _: &Measure) {}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::{find_mcp_binary, health_notice, locald_companion, mcp_config};
    use crate::core::{FaultCode, Resource, ResourceTerminal, UnavailableReason};
    use crate::model::pages::HealthModel;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn index_health_notice_reflects_waiting_and_terminal_failures() {
        let not_requested: Resource<HealthModel> = Resource::not_yet();
        assert_eq!(
            health_notice(&not_requested).as_ref().map(|(message, _)| message.as_str()),
            Some("The index health report has not been requested yet.")
        );

        let waiting = not_requested.clone().waiting();
        assert_eq!(
            health_notice(&waiting).as_ref().map(|(message, _)| message.as_str()),
            Some("Waiting for the local index health report.")
        );

        let unsupported = Resource::unavailable(UnavailableReason::Unsupported);
        assert_eq!(
            health_notice(&unsupported),
            Some(("The connected service does not provide index health.".to_owned(), true))
        );

        let failed = Resource::error(FaultCode::Transport, "service offline");
        assert!(matches!(failed.terminal(), ResourceTerminal::Fault(_)));
        assert_eq!(
            health_notice(&failed),
            Some(("The index health check failed: service offline".to_owned(), true))
        );
    }

    #[test]
    fn mcp_discovery_finds_a_user_install_outside_a_finder_path() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let home = std::env::temp_dir().join(format!("nudox-mcp-discovery-{nonce}"));
        let binary = home.join(".cargo/bin/backend-mcp");
        fs::create_dir_all(binary.parent().expect("binary directory")).expect("create bin");
        fs::write(&binary, b"test executable").expect("write executable marker");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o755))
                .expect("set executable marker permissions");
        }

        let found = find_mcp_binary(
            "backend-mcp",
            None,
            Some("/usr/bin:/bin".into()),
            None,
            Some(home.clone()),
        );

        assert_eq!(found, Some(binary));
        fs::remove_dir_all(home).expect("temporary directory cleanup");
    }

    #[test]
    fn locald_companion_requires_an_executable_sibling() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("nudox-mcp-companion-{nonce}"));
        fs::create_dir_all(&directory).expect("create app bin directory");
        let mcp = directory.join("backend-mcp");
        let locald = directory.join("backend-locald");
        fs::write(&mcp, b"mcp").expect("write mcp marker");
        fs::write(&locald, b"locald").expect("write locald marker");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&locald, fs::Permissions::from_mode(0o755))
                .expect("set locald executable permissions");
        }

        assert_eq!(locald_companion(&mcp), Some(locald.clone()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&locald, fs::Permissions::from_mode(0o644))
                .expect("remove locald executable permissions");
            assert_eq!(locald_companion(&mcp), None);
        }
        fs::remove_dir_all(directory).expect("temporary directory cleanup");
    }

    #[test]
    fn mcp_setup_config_pins_the_service_and_workspace_paths() {
        let config = mcp_config(
            Path::new("/project"),
            Path::new("/workspace/data"),
            Path::new("/workspace/locald.sock"),
            Path::new("/app/Contents/MacOS/backend-mcp"),
            Path::new("/app/Contents/MacOS/backend-locald"),
        )
        .expect("absolute UTF-8 setup paths");
        let config: serde_json::Value =
            serde_json::from_str(&config).expect("valid MCP client config");
        let server = &config["mcpServers"]["nudox"];
        assert_eq!(server["command"], "/app/Contents/MacOS/backend-mcp");
        assert_eq!(
            server["env"]["BACKEND_LOCALD_BIN"],
            "/app/Contents/MacOS/backend-locald"
        );
        assert_eq!(
            server["args"],
            serde_json::json!([
                "--project",
                "/project",
                "--workspace",
                "/workspace/data",
                "--endpoint",
                "/workspace/locald.sock"
            ])
        );
    }

    #[test]
    fn mcp_discovery_does_not_use_a_relative_path_entry() {
        let found = find_mcp_binary(
            "nudox-test-no-mcp-binary",
            None,
            Some(PathBuf::from("relative/bin").into_os_string()),
            None,
            None,
        );
        assert!(found.is_none());
    }
}

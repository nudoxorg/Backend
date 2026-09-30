//! Settings, plain: Appearance is one calm list — theme, contrast, density,
//! text size, motion — one control per row; the window itself is the
//! preview (every change re-resolves the whole shell live). The other pages
//! say what the index holds, which keys do what, and which build this is.

use super::{Ctx, Leaf};
use crate::model::{
    AppSnapshot, AppearancePreference, ConnectionStatus, ContrastPreference, DensityPreference,
    MotionPreference, PrivacyPreference,
};
use crate::navigation::{Intent, SettingsPage};
use crate::shell::kit::{quiet, text};
use crate::shell::reader::Reader;
use facet::controls::Swatch;
use facet::tokens::ty;
use facet::{Measure, Palette, Space};
use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, div, px};
use std::path::{Path, PathBuf};

pub(super) fn body(
    page: SettingsPage,
    snapshot: &AppSnapshot,
    store: &super::Pages,
    ctx: &mut Ctx<'_>,
    _cx: &mut Context<Reader>,
) -> Vec<Leaf> {
    match page {
        SettingsPage::Index => index(store, ctx),
        SettingsPage::Registry => registry(ctx),
        SettingsPage::Help | SettingsPage::Legend => keys(ctx),
        SettingsPage::Diagnostics => diagnostics(snapshot, ctx),
        SettingsPage::Connections => connections(snapshot, ctx),
        SettingsPage::Agents => agents(snapshot, ctx),
        SettingsPage::Privacy => privacy(snapshot, ctx),
        SettingsPage::Editor => editor(ctx),
        SettingsPage::Appearance => appearance(snapshot, ctx),
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

fn appearance(snapshot: &AppSnapshot, ctx: &mut Ctx<'_>) -> Vec<Leaf> {
    // One calm list, one control per row; the window is the preview. Text
    // size is not a setting: the system sets it, ⌘+ / ⌘− / ⌘0 adjust it.
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
    match store.health().loaded_value() {
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
        None => {
            let words = ctx.say("The index report is on its way.");
            leaves.push(Leaf::new(quiet(words, &measure, palette)));
        }
    }
    leaves
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
    let Some(config) = mcp_config(&project, &data, &endpoint, binary) else {
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
    let words = ctx.say("Open code uses the operating system’s registered editor or the editor configured for this desktop process. Source paths and lines come from the current index.");
    leaves.push(Leaf::new(quiet(words, &measure, palette)));
    leaves
}

fn privacy(snapshot: &AppSnapshot, ctx: &mut Ctx<'_>) -> Vec<Leaf> {
    let mut leaves = vec![title("Privacy & local data", ctx)];
    let measure = ctx.measure;
    let palette = ctx.palette;
    let settings = snapshot.settings();
    let links = ctx.links.clone();
    let privacy = facet::controls::button(
        "settings-registry-metadata",
        match settings.privacy {
            PrivacyPreference::LocalOnly => "Local only",
            PrivacyPreference::RegistryMetadata => "Registry metadata allowed",
        },
        &measure,
    )
    .ghost()
    .on_click(move |_, cx| links.dispatch(Intent::TogglePrivacy, cx));
    leaves.push(setting("Network policy", privacy.into_any_element(), ctx));
    let links = ctx.links.clone();
    let advisories = facet::controls::button(
        "settings-advisories",
        if settings.advisories { "On" } else { "Off" },
        &measure,
    )
    .ghost()
    .on_click(move |_, cx| links.dispatch(Intent::ToggleAdvisories, cx));
    leaves.push(setting(
        "Registry advisories",
        advisories.into_any_element(),
        ctx,
    ));
    let links = ctx.links.clone();
    let cache = facet::controls::button(
        "settings-cache",
        if settings.cache_enabled { "On" } else { "Off" },
        &measure,
    )
    .ghost()
    .on_click(move |_, cx| links.dispatch(Intent::ToggleCache, cx));
    leaves.push(setting(
        "Reuse local metadata cache",
        cache.into_any_element(),
        ctx,
    ));
    let links = ctx.links.clone();
    let days = div()
        .flex()
        .items_center()
        .gap(measure.space(Space::Base))
        .child(
            facet::controls::button("settings-cache-shorter", "−", &measure)
                .ghost()
                .on_click({
                    let links = links.clone();
                    move |_, cx| links.dispatch(Intent::SetCacheDays { up: false }, cx)
                }),
        )
        .child(
            text(ty::MONO_ROW, &measure, palette.ink1)
                .child(ctx.say(format!("{} days", settings.cache_days))),
        )
        .child(
            facet::controls::button("settings-cache-longer", "+", &measure)
                .ghost()
                .on_click(move |_, cx| links.dispatch(Intent::SetCacheDays { up: true }, cx)),
        );
    leaves.push(setting("Cache retention", days.into_any_element(), ctx));
    let detail = ctx.say("Project source and index data stay on this machine. Registry metadata requests follow the network policy above.");
    leaves.push(Leaf::new(quiet(detail, &measure, palette)));
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

fn mcp_config(project: &Path, data: &Path, endpoint: &Path, binary: &Path) -> Option<String> {
    let project = project.to_str()?;
    let data = data.to_str()?;
    let endpoint = endpoint.to_str()?;
    let binary = binary.to_str()?;
    let config = serde_json::json!({
        "mcpServers": {
            "nudox": {
                "command": binary,
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
        std::env::var_os("CARGO_HOME").map(PathBuf::from),
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
        .find(|candidate| candidate.is_file())
}

#[allow(dead_code)]
fn _palette(_: &Palette, _: &Measure) {}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::find_mcp_binary;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

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

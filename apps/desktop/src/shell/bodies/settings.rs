//! Settings, plain: Appearance is one calm list — theme, contrast, density,
//! text size, motion — one control per row; the window itself is the
//! preview (every change re-resolves the whole shell live). The other pages
//! say what the index holds, which keys do what, and which build this is.

use super::{Ctx, Leaf};
use crate::core::{Activity, Resource, ResourceTerminal, UnavailableReason};
use crate::model::{
    AppSnapshot, AppearancePreference, ConnectionStatus, ContrastPreference, DensityPreference,
    MotionPreference, PrivacyPreference, ZoomPreference,
};
use crate::navigation::{Intent, SettingsPage};
use crate::shell::kit::{quiet, text};
use crate::shell::reader::Reader;
use facet::controls::Swatch;
use facet::folio;
use facet::icons::{Kind, KindSize, kind_mark};
use facet::tokens::{TypeRole, ty};
use facet::{Measure, Palette, Space};
use gpui::{
    AnyElement, Context, Hsla, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div,
};
use std::path::{Path, PathBuf};

pub(super) fn body(
    page: SettingsPage,
    snapshot: &AppSnapshot,
    store: &super::Pages,
    ctx: &mut Ctx<'_>,
    window: &mut Window,
    cx: &mut Context<Reader>,
) -> Vec<Leaf> {
    // Departing Settings does not own a second native control/fact tree.
    // Reader supplies its transition chrome; the settled destination mounts
    // these controls once it owns the route.
    if !ctx.active {
        return Vec::new();
    }
    match page {
        SettingsPage::About => about(ctx),
        SettingsPage::Index => index(store, ctx),
        SettingsPage::Registry => registry(snapshot, ctx),
        SettingsPage::Help => keys(ctx),
        SettingsPage::Legend => legend(ctx),
        SettingsPage::Diagnostics => diagnostics(snapshot, ctx),
        SettingsPage::Connections => connections(snapshot, ctx),
        SettingsPage::Agents => agents(snapshot, ctx),
        SettingsPage::Privacy => privacy(snapshot, ctx, cx),
        SettingsPage::Editor => editor(ctx),
        SettingsPage::Appearance => appearance(snapshot, ctx, window, cx),
    }
}

fn title(words: &str, ctx: &mut Ctx<'_>) -> Leaf {
    let said = ctx.say(words.to_owned());
    Leaf::new(
        div()
            .id(format!("settings-heading-{words}"))
            .role(gpui::Role::Heading)
            .aria_label(words.to_owned())
            .aria_level(1)
            .pb(ctx.measure.space(Space::Base))
            .child(text(ty::DISPLAY, &ctx.measure, ctx.palette.ink0).child(said)),
    )
}

fn about(ctx: &mut Ctx<'_>) -> Vec<Leaf> {
    let measure = ctx.measure;
    let palette = ctx.palette;
    let mut leaves = vec![title("About Nudox", ctx)];
    let purpose = ctx.say("Read the code you depend on.");
    leaves.push(Leaf::new(text(ty::ROW, &measure, palette.ink1).child(purpose)));
    let version = ctx.say(env!("CARGO_PKG_VERSION"));
    leaves.push(setting("Version", text(ty::MONO_ROW, &measure, palette.ink1)
        .child(version).into_any_element(), ctx));
    leaves
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
    .aria_label("Theme").disabled(!ctx.active).admit(ctx.native_local_guard(cx)).local_activation(ctx.native_local_activation_scope(cx))
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
    let contrast = facet::controls::seg("set-contrast", &measure).aria_label("Contrast").disabled(!ctx.active).admit(ctx.native_local_guard(cx)).local_activation(ctx.native_local_activation_scope(cx))
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
    .aria_label("Density").disabled(!ctx.active).admit(ctx.native_local_guard(cx)).local_activation(ctx.native_local_activation_scope(cx))
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
    let mut text_size = facet::controls::seg("set-text-size", &measure).aria_label("Text size").disabled(!ctx.active).admit(ctx.native_local_guard(cx)).local_activation(ctx.native_local_activation_scope(cx));
    for percent in ZoomPreference::LADDER {
        text_size = text_size.label(format!("{percent}%"));
    }
    let text_size = text_size
        .selected(selected)
        .on_select(move |index, window, cx| {
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
    let text_note =
        ctx.say("Relative to the operating system’s text scale; remembered per display.");
    leaves.push(Leaf::new(quiet(text_note, &measure, palette)));
    let links = ctx.links.clone();
    let motion = facet::controls::seg("set-motion", &measure).aria_label("Motion").disabled(!ctx.active).admit(ctx.native_local_guard(cx)).local_activation(ctx.native_local_activation_scope(cx))
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
    let label = name.to_owned();
    let name = ctx.say(label.clone());
    Leaf::new(
        div()
            .id(format!("settings-row-{label}"))
            .role(gpui::Role::Group)
            .aria_label(label)
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

/// A read-only fact is one complete native reading unit. Stacking the value
/// below its name gives the whole folio width to long paths and status text,
/// including at double text size; unlike a control row it never claims focus.
fn value_row(
    name: &str,
    value: impl Into<String>,
    role: TypeRole,
    ink: impl Into<Hsla>,
    ctx: &mut Ctx<'_>,
) -> Leaf {
    let value = value.into();
    let measure = ctx.measure;
    let palette = ctx.palette;
    let name_text = ctx.say(name.to_owned());
    let value_text = ctx.say(value.clone());
    let id = format!("settings-value-{name}-{value}");
    Leaf::new(
        div()
            .id(id.clone())
            .role(gpui::Role::Group)
            .aria_label(format!("{name}: {value}"))
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(measure.space(Space::Base))
            .py(measure.space(Space::Roomy))
            .border_b_1()
            .border_color(palette.line1.hsla())
            .child(folio::text::wrap(
                format!("{id}-name"),
                name_text,
                ty::ROW,
                palette.ink1,
                &measure,
                None,
            ))
            .child(folio::text::wrap(
                format!("{id}-value"),
                value_text,
                role,
                ink,
                &measure,
                None,
            )),
    )
}

fn section(words: &str, ctx: &mut Ctx<'_>) -> Leaf {
    let said = ctx.say(words.to_owned());
    Leaf::new(
        div()
            .id(format!("settings-section-{words}"))
            .role(gpui::Role::Heading)
            .aria_label(words.to_owned())
            .aria_level(2)
            .pt(ctx.measure.space(Space::Roomy))
            .pb(ctx.measure.space(Space::Base))
            .child(text(ty::HEAD, &ctx.measure, ctx.palette.ink0).child(said)),
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
        let ink = if missing {
            palette.coral.base
        } else {
            palette.ink1
        };
        leaves.push(value_row("Compiler", rust.words(), ty::SMALL, ink, ctx));
    }
    let health = store.health();
    match health.loaded_value() {
        Some(health) => {
            leaves.push(value_row(
                "Declarations",
                health.rows.to_string(),
                ty::MONO_ROW,
                palette.ink1,
                ctx,
            ));
            if health.ingest.files_discovered == 0 {
                leaves.push(value_row(
                    "File progress",
                    "No indexed files reported for this revision",
                    ty::ROW,
                    palette.ink2,
                    ctx,
                ));
            } else {
                leaves.push(value_row(
                    "Files indexed",
                    format!(
                        "{} of {}",
                        health.ingest.files_indexed, health.ingest.files_discovered
                    ),
                    ty::MONO_ROW,
                    palette.ink1,
                    ctx,
                ));
                leaves.push(value_row(
                    "Files without declarations",
                    health.ingest.files_unavailable.to_string(),
                    ty::MONO_ROW,
                    palette.ink1,
                    ctx,
                ));
            }
            for language in health.ingest.languages.iter() {
                leaves.push(value_row(
                    &language.language,
                    format!(
                        "{} declarations in {} files",
                        language.declarations, language.files
                    ),
                    ty::MONO_SMALL,
                    palette.ink2,
                    ctx,
                ));
            }
            leaves.push(section("Ready capabilities", ctx));
            if health.ready_capabilities.is_empty() {
                leaves.push(value_row(
                    "Ready capabilities",
                    "No fresh readiness proof reported",
                    ty::ROW,
                    palette.ink2,
                    ctx,
                ));
            }
            for family in health.ready_capabilities.iter() {
                let (name, profile) = capability_words(*family);
                leaves.push(value_row(
                    name,
                    format!("{profile} · Ready"),
                    ty::ROW,
                    palette.ink1,
                    ctx,
                ));
            }
            if !health.not_ready_capabilities.is_empty() {
                leaves.push(section("Other declared capabilities", ctx));
                for missing in health.not_ready_capabilities.iter() {
                    let (name, profile) = capability_words(missing.family);
                    let state = capability_state_words(missing.state);
                    leaves.push(value_row(
                        name,
                        format!("{profile} · {state}"),
                        ty::ROW,
                        palette.ink2,
                        ctx,
                    ));
                }
            }
        }
        None => {}
    }
    if let Some((message, problem)) = health_notice(&health) {
        let words = ctx.say(message);
        let ink = if problem {
            palette.coral.base
        } else {
            palette.ink3
        };
        leaves.push(Leaf::new(text(ty::SMALL, &measure, ink).child(words)));
    }
    leaves
}

/// Names only facts carried by the admitted closed capability family.
fn capability_words(family: backend_library::CapabilityFamily) -> (&'static str, String) {
    match family {
        backend_library::CapabilityFamily::StructuralFrontend { profile } => {
            ("Structural parser", profile_words(profile).to_owned())
        }
        backend_library::CapabilityFamily::LanguageOracle { profile, task } => {
            let task = match task {
                backend_library::LanguageOracleTask::Parse => "parse",
                backend_library::LanguageOracleTask::TypeCheck => "type check",
                backend_library::LanguageOracleTask::SemanticIndex => "semantic index",
            };
            (
                "Compiler semantics",
                format!("{} · {task}", profile_words(profile)),
            )
        }
        backend_library::CapabilityFamily::Embedding { recipe: None } => {
            ("Embedding", "No model recipe configured".to_owned())
        }
        backend_library::CapabilityFamily::Embedding { recipe: Some(_) } => {
            ("Embedding", "Model recipe declared".to_owned())
        }
    }
}

fn profile_words(profile: impl Into<[u8; 2]>) -> &'static str {
    match profile.into() {
        [0, 0] => "Rust 2015",
        [0, 1] => "Rust 2018",
        [0, 2] => "Rust 2021",
        [0, 3] => "Rust 2024",
        [1, 0] => "TypeScript",
        [1, 1] => "TypeScript with JSX",
        [2, 0] => "Python 3.10",
        [2, 1] => "Python 3.11",
        [2, 2] => "Python 3.12",
        [2, 3] => "Python 3.13",
        [2, 4] => "Python 3.14",
        [3, 0] => "Go 1.22",
        [3, 1] => "Go 1.23",
        [3, 2] => "Go 1.24",
        [3, 3] => "Go 1.25",
        [4, 0] => "Java 8",
        [4, 1] => "Java 11",
        [4, 2] => "Java 17",
        [4, 3] => "Java 21",
        [4, 4] => "Java 25",
        [5, 0] => "C# 10",
        [5, 1] => "C# 11",
        [5, 2] => "C# 12",
        [5, 3] => "C# 13",
        [5, 4] => "C# 14",
        [6, 0] => "C11",
        [6, 1] => "C17",
        [6, 2] => "C23",
        [6, 128] => "C++17",
        [6, 129] => "C++20",
        [6, 130] => "C++23",
        [6, 131] => "C++26",
        _ => "Unrecognized language profile",
    }
}

fn capability_state_words(state: backend_library::CapabilityLifecycle) -> &'static str {
    use backend_library::{CapabilityLifecycle as State, CapabilityUnavailable as Why};
    match state {
        State::Unavailable(Why::NoManifest) => "Not configured",
        State::Unavailable(Why::NotInstalled) => "Not installed",
        State::Unavailable(Why::UnsupportedTarget) => "Unsupported on this device",
        State::Unavailable(Why::UnsupportedAbi) => "Unsupported protocol",
        State::Unavailable(Why::MissingDependency) => "Required dependency missing",
        State::Unavailable(Why::ProbeFailed) => "Readiness check failed",
        State::Probing => "Checking readiness",
        State::Installed => "Installed; readiness not checked",
        State::Resident => "Resident; readiness not checked",
        State::Active => "Active; readiness not checked",
        State::Ready => "Ready",
        State::Revoked => "Access revoked",
    }
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
            format!(
                "The index refresh failed; the last report is shown: {}",
                error.message()
            ),
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
        (false, ResourceTerminal::Complete, Activity::Working) => {
            Some(("Loading the local index health report.".to_owned(), false))
        }
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
        UnavailableReason::OutOfScope => {
            "Index health is unavailable outside the selected project scope."
        }
    }
}

fn registry(snapshot: &AppSnapshot, ctx: &mut Ctx<'_>) -> Vec<Leaf> {
    let mut leaves = vec![title("Registry sources", ctx)];
    let measure = ctx.measure;
    let palette = ctx.palette;
    let home = std::env::var_os("NUDOX_CARGO_HOME")
        .or_else(|| std::env::var_os("CARGO_HOME"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")));
    let source_root = std::env::var_os("NUDOX_CARGO_ROOT").map(PathBuf::from);
    let home_words = home.as_ref().map_or_else(
        || "No Cargo home could be resolved from NUDOX_CARGO_HOME, CARGO_HOME, or HOME.".to_owned(),
        |path| path.display().to_string(),
    );
    leaves.push(value_row(
        "Cargo home",
        home_words,
        ty::MONO_SMALL,
        palette.ink1,
        ctx,
    ));
    let root_words = source_root.as_ref().map_or_else(
        || {
            "Not set; the local reader uses the effective crates.io source under Cargo home."
                .to_owned()
        },
        |path| path.display().to_string(),
    );
    leaves.push(value_row(
        "Selected source root",
        root_words,
        ty::MONO_SMALL,
        palette.ink1,
        ctx,
    ));
    let detail = ctx.say("If the same release appears under conflicting cached indexes, set NUDOX_CARGO_ROOT to the intended registry source root and restart the desktop. The exact index path is shown in the release notice. Local archives are unpacked only when their checksum is known for the selected source.");
    leaves.push(Leaf::new(quiet(detail, &measure, palette)));
    let policy = match snapshot.settings().privacy {
        PrivacyPreference::LocalOnly => {
            "Local only is saved for this desktop's next embedded-service start."
        }
        PrivacyPreference::RegistryMetadata => {
            "Registry metadata is saved for this desktop's next embedded-service start."
        }
    };
    let policy = ctx.say(policy);
    leaves.push(Leaf::new(quiet(policy, &measure, palette)));
    let running = ctx.say(running_service_policy_note(snapshot));
    leaves.push(Leaf::new(quiet(running, &measure, palette)));
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
        .disabled(!ctx.active)
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
        let palette = ctx.palette;
        leaves.push(value_row(key.says, caps, ty::MONO_ROW, palette.ink1, ctx));
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
            leaves.push(value_row(says, cap, ty::MONO_ROW, palette.ink1, ctx));
        }
    }
    leaves
}

fn legend(ctx: &mut Ctx<'_>) -> Vec<Leaf> {
    let mut leaves = vec![title("Legend", ctx)];
    let measure = ctx.measure;
    let palette = ctx.palette;
    let intro =
        ctx.say("Declaration mark shape names its kind; its hue groups the kind by family.");
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
        let name = kind.name();
        let family = family_name(kind.family());
        let name_text = ctx.say(name);
        let family_text = ctx.say(family);
        let row = div()
            .id(format!("settings-kind-{name}"))
            .role(gpui::Role::Group)
            .aria_label(format!("{name}: {family} family"))
            .w_full()
            .min_w_0()
            .flex()
            .flex_wrap()
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
                    .child(text(ty::ROW, &measure, palette.ink1).child(name_text)),
            )
            .child(folio::text::wrap(
                format!("settings-kind-{name}-family"),
                family_text,
                ty::MONO_SMALL,
                palette.ink2,
                &measure,
                None,
            ));
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
        (
            "Changed",
            "Amber diamond and change count",
            palette.amber.base,
        ),
        ("Gone", "Coral status words", palette.coral.base),
        (
            "Members",
            "Quiet count when no other state mark is shown",
            palette.ink3,
        ),
    ] {
        leaves.push(value_row(label, meaning, ty::ROW, color, ctx));
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
        leaves.push(value_row(label, meaning, ty::ROW, color, ctx));
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
    let palette = ctx.palette;
    leaves.push(value_row(
        "Version",
        env!("CARGO_PKG_VERSION"),
        ty::MONO_ROW,
        palette.ink1,
        ctx,
    ));
    let mode =
        confirmed_host_mode(snapshot).map_or("service mode not confirmed yet", |mode| match mode {
            crate::host::lease::HostMode::Embedded => "embedded local service",
            crate::host::lease::HostMode::Attached => "attached to a running local service",
        });
    leaves.push(value_row("Service", mode, ty::ROW, palette.ink1, ctx));
    leaves.push(value_row(
        "Connection check",
        connection_words(snapshot.settings().connection),
        ty::ROW,
        palette.ink1,
        ctx,
    ));
    leaves.push(value_row(
        "Check details",
        connection_explanation(snapshot.settings().connection),
        ty::SMALL,
        palette.ink2,
        ctx,
    ));
    let (project, data, endpoint) = owner_paths(snapshot);
    for (label, path) in [
        ("Project", project),
        ("Workspace data", data),
        ("Local endpoint", endpoint),
    ] {
        if let Some(path) = path {
            leaves.push(value_row(
                label,
                path.to_string_lossy().into_owned(),
                ty::MONO_SMALL,
                palette.ink1,
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
    leaves.push(value_row(
        "Desktop to local index",
        connection_words(snapshot.settings().connection),
        ty::ROW,
        palette.ink1,
        ctx,
    ));
    let mode = match confirmed_host_mode(snapshot) {
        Some(crate::host::lease::HostMode::Embedded) => {
            "The owner watcher last confirmed this desktop's embedded local service."
        }
        Some(crate::host::lease::HostMode::Attached) => {
            "The owner watcher last confirmed an attached local service owned elsewhere."
        }
        None => "The local service owner has not confirmed a serving attachment yet.",
    };
    leaves.push(value_row("Owner confirmation", mode, ty::SMALL, palette.ink2, ctx));
    leaves.push(value_row(
        "Check details", connection_explanation(snapshot.settings().connection),
        ty::SMALL, palette.ink2, ctx,
    ));
    let links = ctx.links.clone();
    let testing = snapshot.settings().connection == ConnectionStatus::Testing;
    let probe = facet::controls::button("settings-test-connection", "Test connection", &measure)
        .disabled(!ctx.active || snapshot.settings().confirmed_service_mode.is_none())
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
    let connection = format!(
        "Desktop local index: {}",
        connection_words(snapshot.settings().connection)
    );
    leaves.push(value_row(
        "Local service",
        connection,
        ty::ROW,
        palette.ink1,
        ctx,
    ));
    let installed = mcp_binary();
    let binary_status = installed.as_ref().map_or_else(
        || "backend-mcp is not on PATH or beside this app. Build or install the backend-mcp executable, then use the configuration below.".to_owned(),
        |path| format!("MCP server found at {}", path.display()),
    );
    let status_color = if installed.is_some() {
        palette.ink1
    } else {
        palette.coral.base
    };
    leaves.push(value_row(
        "Server executable",
        binary_status,
        ty::SMALL,
        status_color,
        ctx,
    ));
    let locald = installed.as_deref().and_then(locald_companion);
    let locald_status = locald.as_ref().map_or_else(
        || "backend-locald must be installed beside backend-mcp so this setup can start the local service when Nudox is closed.".to_owned(),
        |path| format!("Local service executable found at {}", path.display()),
    );
    leaves.push(value_row(
        "Service executable",
        locald_status,
        ty::SMALL,
        if locald.is_some() {
            palette.ink1
        } else {
            palette.coral.base
        },
        ctx,
    ));
    let recheck = facet::controls::button("settings-recheck-mcp", "Check again", &measure)
        .disabled(!ctx.active)
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
        .disabled(!ctx.active)
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
    leaves.push(value_row(
        "Open in editor",
        "code -g path:line → zed path:line → the operating system’s registered file opener",
        ty::MONO_SMALL,
        palette.ink1,
        ctx,
    ));
    let words = ctx.say("This desktop build has no editor preference field. The first available command wins; the operating-system fallback opens the file without a line target.");
    leaves.push(Leaf::new(quiet(words, &measure, palette)));
    leaves
}

fn privacy(snapshot: &AppSnapshot, ctx: &mut Ctx<'_>, cx: &mut Context<Reader>) -> Vec<Leaf> {
    let mut leaves = vec![title("Privacy & local data", ctx)];
    let measure = ctx.measure;
    let palette = ctx.palette;
    let settings = snapshot.settings();
    let mode_note = running_service_policy_note(snapshot);
    let mode_note = ctx.say(mode_note);
    leaves.push(Leaf::new(quiet(mode_note, &measure, palette)));

    let links = ctx.links.clone();
    let network = facet::controls::seg("set-privacy", &measure).aria_label("Network policy").disabled(!ctx.active).admit(ctx.native_local_guard(cx)).local_activation(ctx.native_local_activation_scope(cx))
        .label("Local only")
        .label("Registry metadata")
        .selected(usize::from(
            settings.privacy == PrivacyPreference::RegistryMetadata,
        ))
        .on_select(move |index, _, cx| {
            links.dispatch(
                Intent::SetPrivacy(if index == 1 {
                    PrivacyPreference::RegistryMetadata
                } else {
                    PrivacyPreference::LocalOnly
                }),
                cx,
            );
        });
    leaves.push(setting("Network policy", network.into_any_element(), ctx));

    let links = ctx.links.clone();
    let advisories = facet::controls::seg("set-advisories", &measure).aria_label("Advisory refresh").disabled(!ctx.active).admit(ctx.native_local_guard(cx)).local_activation(ctx.native_local_activation_scope(cx))
        .label("Refresh feeds")
        .label("Pause feeds")
        .selected(usize::from(!settings.advisories))
        .on_select(move |index, _, cx| {
            links.dispatch(Intent::SetAdvisoriesEnabled(index == 0), cx);
        });
    leaves.push(setting(
        "Advisory refresh",
        advisories.into_any_element(),
        ctx,
    ));

    let links = ctx.links.clone();
    let cache = facet::controls::seg("set-registry-cache", &measure).aria_label("Registry result cache").disabled(!ctx.active).admit(ctx.native_local_guard(cx)).local_activation(ctx.native_local_activation_scope(cx))
        .label("Reuse results")
        .label("Always refresh")
        .selected(usize::from(!settings.cache_enabled))
        .on_select(move |index, _, cx| {
            links.dispatch(Intent::SetCacheEnabled(index == 0), cx);
        });
    leaves.push(setting(
        "Registry result cache",
        cache.into_any_element(),
        ctx,
    ));

    let links = ctx.links.clone();
    let decrement = facet::controls::button("cache-age-down", "−", &measure)
        .aria_label("Decrease maximum reusable age")
        .disabled(!ctx.active || settings.cache_days <= 1)
        .ghost()
        .on_click(move |_, cx| links.dispatch(Intent::SetCacheDays { up: false }, cx));
    let links = ctx.links.clone();
    let increment = facet::controls::button("cache-age-up", "+", &measure)
        .aria_label("Increase maximum reusable age")
        .disabled(!ctx.active || settings.cache_days >= 90)
        .ghost()
        .on_click(move |_, cx| links.dispatch(Intent::SetCacheDays { up: true }, cx));
    let age = ctx.say(format!("{} days", settings.cache_days));
    let age_down = ctx.links.clone();
    let age_up = ctx.links.clone();
    let age_keys = ctx.links.clone();
    let active = ctx.active;
    let days = settings.cache_days;
    let mut age_control = div()
        .id("cache-age")
        .role(gpui::Role::SpinButton)
        .aria_label("Maximum reusable age")
        .aria_value(format!("{days} days"))
        .aria_numeric_value(f64::from(days))
        .aria_min_numeric_value(1.0)
        .aria_max_numeric_value(90.0)
        .aria_description("Available ages: 1, 7, 14, 30, 90 days. Use Up and Down to change.")
        .aria_disabled(!active);
    if active {
        age_control = age_control
            .focusable()
            .tab_index(0)
            .on_key_down(move |event, _, cx| {
                let up = match event.keystroke.key.as_str() {
                    "up" => true,
                    "down" => false,
                    _ => return,
                };
                if (up && days < 90) || (!up && days > 1) {
                    age_keys.dispatch(Intent::SetCacheDays { up }, cx);
                }
                cx.stop_propagation();
            });
        if days > 1 {
            age_control = age_control
                .on_a11y_action(gpui::AccessibleAction::Decrement, move |_, _, cx| {
                    age_down.dispatch(Intent::SetCacheDays { up: false }, cx)
                });
        }
        if days < 90 {
            age_control = age_control
                .on_a11y_action(gpui::AccessibleAction::Increment, move |_, _, cx| {
                    age_up.dispatch(Intent::SetCacheDays { up: true }, cx)
                });
        }
    }
    let age_control = age_control
        .flex()
        .items_center()
        .gap(measure.space(Space::Base))
        .child(decrement)
        .child(text(ty::MONO_ROW, &measure, palette.ink1).child(age))
        .child(increment);
    leaves.push(setting(
        "Maximum reusable age",
        age_control.into_any_element(),
        ctx,
    ));

    for note in [
        "Local only blocks registry, registry-discovery, and remote advisory-feed requests. Project files and the local index remain on this machine.",
        "Pausing advisory refresh keeps already admitted findings available; it suppresses explicit feed refreshes.",
        "Disabling result reuse does not delete downloaded package data. When reuse is enabled, a result needs a current authenticated receipt within the selected age; offline adds stop when that receipt is expired.",
    ] {
        let words = ctx.say(note);
        leaves.push(Leaf::new(quiet(words, &measure, palette)));
    }
    leaves
}

/// The persisted service mode is history, not a current serving attachment.
/// Only the owner watcher sets this ephemeral value; an ancillary check cannot
/// confirm or withdraw it.
fn confirmed_host_mode(snapshot: &AppSnapshot) -> Option<crate::host::lease::HostMode> {
    Some(match snapshot.settings().confirmed_service_mode? {
        crate::model::ServiceMode::Embedded => crate::host::lease::HostMode::Embedded,
        crate::model::ServiceMode::Attached => crate::host::lease::HostMode::Attached,
    })
}

fn running_service_policy_note(snapshot: &AppSnapshot) -> &'static str {
    match confirmed_host_mode(snapshot) {
        Some(crate::host::lease::HostMode::Embedded) => {
            "The last confirmed owner is this desktop's embedded service. Saved choices do not change its current policy; restart the desktop to apply them on its next embedded start."
        }
        Some(crate::host::lease::HostMode::Attached) => {
            "The last confirmed owner is an attached local service and controls current policy. Saved choices apply only if this desktop later starts an embedded service."
        }
        None => {
            "The active service mode is not confirmed. Saved choices apply on the next embedded-service start and do not change an already-running owner."
        }
    }
}

fn connection_words(status: ConnectionStatus) -> &'static str {
    match status {
        ConnectionStatus::Unknown => "Not checked yet",
        ConnectionStatus::Testing => "Checking the local service…",
        ConnectionStatus::Connected => "Connected",
        ConnectionStatus::TransportFailed => "Revision check failed over local transport",
        ConnectionStatus::CheckFailed => "Revision check failed",
        ConnectionStatus::CheckUnavailable => "Revision check unavailable",
        ConnectionStatus::OwnerUnavailable => "Local owner unavailable",
    }
}

fn connection_explanation(status: ConnectionStatus) -> &'static str {
    match status {
        ConnectionStatus::Unknown => "No revision-check result is current for this owner generation yet.",
        ConnectionStatus::Testing => "Checking an authenticated revision; this does not read the catalog or inspect product capabilities.",
        ConnectionStatus::Connected => "The local service answered an authenticated revision check. This does not establish catalog or feature availability.",
        ConnectionStatus::TransportFailed => "The last revision request failed over local transport. This alone does not prove the owner stopped; test again. If the owner watcher reports failure, use Try again in the service notice.",
        ConnectionStatus::CheckFailed => "The last revision check ended without an admitted reply. Test again; this result does not determine the availability of other pages or features.",
        ConnectionStatus::CheckUnavailable => "This service does not expose the revision check. Other service features have not been tested by this result.",
        ConnectionStatus::OwnerUnavailable => "The owner watcher withdrew the serving attachment. Use Try again in the service notice to restart it.",
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

#[cfg(test)]
mod policy_status_tests {
    use super::*;

    #[test]
    fn persisted_service_mode_is_not_presented_as_the_live_host_before_owner_ready() {
        let mut settings = crate::model::SettingsState::default();
        settings.service_mode = crate::model::ServiceMode::Attached;
        let restored = AppSnapshot::empty(crate::core::VersionedRoot::unserved())
            .with_settings(settings.clone());
        assert_eq!(confirmed_host_mode(&restored), None);

        settings.connection = ConnectionStatus::Connected;
        let status_only = AppSnapshot::empty(crate::core::VersionedRoot::unserved())
            .with_settings(settings.clone());
        assert_eq!(confirmed_host_mode(&status_only), None);
        settings.confirmed_service_mode = Some(crate::model::ServiceMode::Attached);
        let ready =
            AppSnapshot::empty(crate::core::VersionedRoot::unserved()).with_settings(settings.clone());
        assert_eq!(
            confirmed_host_mode(&ready),
            Some(crate::host::lease::HostMode::Attached)
        );
        settings.connection = ConnectionStatus::TransportFailed;
        assert_eq!(confirmed_host_mode(&ready.with_settings(settings)), Some(crate::host::lease::HostMode::Attached));
    }
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
    use super::{
        capability_state_words, capability_words, connection_explanation, connection_words,
        find_mcp_binary, health_notice, locald_companion, mcp_config,
    };
    use crate::core::{FaultCode, Resource, ResourceTerminal, UnavailableReason};
    use crate::model::pages::{HealthModel, PageValue, ReadFailure};
    use crate::navigation::{Intent, SettingsPage};
    use crate::runtime::page_mapping::health_model;
    use crate::runtime::reads::{PageReader, ReadContext, ReadPool, ReadRequest};
    use crate::shell::fit_tests::{painted, resize};
    use crate::shell::tests::{Fixture, rig_with_reads};
    use backend_library::{
        Basis, CapabilityAuthority, CapabilityFamily, CapabilityId, CapabilityInventory,
        CapabilityLifecycle, CapabilityStatus, CapabilityTarget, CapabilityUnavailable, Cursor,
        HealthReport, IngestProgress, LanguageRows, RevisionReceipt, SemanticLanguageProfile,
        SourceLanguage, object_version, view_state_root,
    };
    use facet::probe::TextOverflow;
    use gpui::TestAppContext;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn product_health() -> HealthModel {
        let rust = SemanticLanguageProfile::from_name("rust")
            .expect("closed Rust profile")
            .profile()
            .expect("admitted profile");
        let csharp = SemanticLanguageProfile::from_name("csharp")
            .expect("closed C# profile")
            .profile()
            .expect("admitted profile");
        let python = SemanticLanguageProfile::from_name("python")
            .expect("closed Python profile")
            .profile()
            .expect("admitted profile");
        let inventory = CapabilityInventory::try_new(vec![
            CapabilityStatus::observed(
                CapabilityId::new([1; 32]),
                CapabilityFamily::StructuralFrontend { profile: rust },
                [7; 32],
                CapabilityTarget::Native {
                    os: 1,
                    architecture: 1,
                },
                1,
                CapabilityAuthority::Structural { producer: [7; 32] },
                CapabilityLifecycle::Ready,
            ),
            CapabilityStatus::unavailable(
                CapabilityId::new([2; 32]),
                CapabilityFamily::StructuralFrontend { profile: csharp },
                CapabilityUnavailable::UnsupportedTarget,
            ),
            CapabilityStatus::observed(
                CapabilityId::new([3; 32]),
                CapabilityFamily::StructuralFrontend { profile: python },
                [8; 32],
                CapabilityTarget::Native {
                    os: 1,
                    architecture: 1,
                },
                1,
                CapabilityAuthority::Structural { producer: [8; 32] },
                CapabilityLifecycle::Installed,
            ),
        ])
        .expect("admitted owner inventory");
        let root = view_state_root(&[]);
        let object = object_version(b"settings-health-fixture");
        let report = HealthReport::from_admitted_parts(
            RevisionReceipt::new(root, Cursor::at(root, 1), object),
            Basis::new(root, object),
            Box::new([]),
            42,
            inventory,
        )
        .with_progress(
            IngestProgress::new(
                2,
                1,
                1,
                42,
                vec![LanguageRows::new(SourceLanguage::Rust, 1, 42)],
                Vec::new(),
            )
            .expect("admitted ingest progress"),
        );
        health_model(&report)
    }

    struct ProductHealthReader(HealthModel);

    impl PageReader for ProductHealthReader {
        fn read(
            &mut self,
            request: &ReadRequest,
            context: &ReadContext<'_>,
        ) -> Result<PageValue, ReadFailure> {
            if matches!(request, ReadRequest::Health) {
                Ok(PageValue::Health(self.0.clone()))
            } else {
                Fixture.read(request, context)
            }
        }
    }

    #[gpui::test]
    fn admitted_capability_values_and_settings_facts_are_native_reading_units_at_double_text_size(
        cx: &mut TestAppContext,
    ) {
        let health = product_health();
        assert_eq!(health.ready_capabilities.len(), 1);
        assert_eq!(health.not_ready_capabilities.len(), 2);
        assert_eq!(
            capability_words(health.ready_capabilities[0]),
            ("Structural parser", "Rust 2024".to_owned())
        );
        assert_eq!(
            capability_words(health.not_ready_capabilities[0].family),
            ("Structural parser", "C# 14".to_owned())
        );
        assert_eq!(
            capability_state_words(health.not_ready_capabilities[0].state),
            "Unsupported on this device"
        );
        assert_eq!(
            capability_state_words(health.not_ready_capabilities[1].state),
            "Installed; readiness not checked"
        );

        let pool = ReadPool::start(2, move |_| ProductHealthReader(health.clone()))
            .expect("fixture read pool");
        let mut rig = rig_with_reads(cx, None, 360.0, 900.0, pool);
        rig.cx.update(|window, _| window.set_a11y_forced(true));
        let display = rig.shell.read_with(rig.cx, |shell, _| shell.display_key());
        for percent in [100_u16, 150, 200] {
            rig.go(Intent::ZoomTo {
                display: display.clone(),
                percent,
            });
            resize(&mut rig, 360.0, 900.0);
            rig.go(Intent::OpenSettings(SettingsPage::Index));
            rig.settle();
            let ledger = painted(&mut rig);
            for value in [
                "Rust 2024 · Ready",
                "C# 14 · Unsupported on this device",
                "Python 3.14 · Installed; readiness not checked",
            ] {
                let row = ledger
                    .texts
                    .iter()
                    .find(|text| text.content == value)
                    .expect("actual product DTO value painted");
                assert_eq!(row.overflow, TextOverflow::Wrap, "{percent}%: {row:?}");
                assert!(!row.clipped_without_ellipsis(), "{percent}%: {row:?}");
                assert!(
                    row.bounds.x >= 0.0 && row.bounds.x + row.bounds.width <= 360.5,
                    "{percent}%: {row:?}"
                );
            }
            let native: serde_json::Value = serde_json::from_str(
                &rig.cx
                    .update(|window, _| window.debug_a11y_tree_json())
                    .expect("native AccessKit tree"),
            )
            .expect("native tree JSON");
            let labels = native["nodes"]
                .as_object()
                .expect("native nodes")
                .values()
                .filter_map(|node| node["aria"]["label"].as_str())
                .collect::<Vec<_>>();
            assert!(
                labels.contains(&"Structural parser: Rust 2024 · Ready"),
                "{percent}% native AX: {labels:?}"
            );
            assert!(
                labels.contains(&"Structural parser: C# 14 · Unsupported on this device"),
                "{percent}% native AX: {labels:?}"
            );
            assert!(
                labels
                    .contains(&"Structural parser: Python 3.14 · Installed; readiness not checked"),
                "{percent}% native AX: {labels:?}"
            );

            for (page, label) in [
                (
                    SettingsPage::Connections,
                    format!(
                        "Desktop to local index: {}",
                        connection_words(rig.graph.store.read_with(rig.cx, |store, _| {
                            store.snapshot().settings().connection
                        }))
                    ),
                ),
                (
                    SettingsPage::Diagnostics,
                    format!(
                        "Connection check: {}",
                        connection_words(rig.graph.store.read_with(rig.cx, |store, _| {
                            store.snapshot().settings().connection
                        }))
                    ),
                ),
                (
                    SettingsPage::Legend,
                    "Your uses: Mint notch and use count".to_owned(),
                ),
            ] {
                rig.go(Intent::OpenSettings(page));
                rig.settle();
                rig.repaint();
                let native: serde_json::Value = serde_json::from_str(
                    &rig.cx
                        .update(|window, _| window.debug_a11y_tree_json())
                        .expect("native AccessKit tree"),
                )
                .expect("native tree JSON");
                let labels = native["nodes"]
                    .as_object()
                    .expect("native nodes")
                    .values()
                    .filter_map(|node| node["aria"]["label"].as_str())
                    .collect::<Vec<_>>();
                assert!(
                    labels.iter().any(|actual| *actual == label.as_str()),
                    "{percent}% {page:?} native AX lacks {label}: {labels:?}"
                );
            }
        }
        let owner_key = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().key());
        for (intent, value) in [
            (Intent::OwnerUnavailable, "Local owner unavailable"),
            (Intent::OwnerReady { key: owner_key, mode: crate::model::ServiceMode::Embedded }, "Connected"),
        ] {
            rig.go(intent);
            for (page, label) in [
                (
                    SettingsPage::Connections,
                    format!("Desktop to local index: {value}"),
                ),
                (
                    SettingsPage::Diagnostics,
                    format!("Connection check: {value}"),
                ),
            ] {
                rig.go(Intent::OpenSettings(page));
                rig.settle();
                rig.repaint();
                let native: serde_json::Value = serde_json::from_str(
                    &rig.cx
                        .update(|window, _| window.debug_a11y_tree_json())
                        .expect("native AccessKit tree"),
                )
                .expect("native tree JSON");
                let labels = native["nodes"]
                    .as_object()
                    .expect("native nodes")
                    .values()
                    .filter_map(|node| node["aria"]["label"].as_str())
                    .collect::<Vec<_>>();
                assert!(
                    labels.contains(&label.as_str()),
                    "{page:?} native AX lacks {label}: {labels:?}"
                );
                let status = rig.graph.store.read_with(rig.cx, |store, _| {
                    store.snapshot().settings().connection
                });
                let detail = format!("Check details: {}", connection_explanation(status));
                assert!(
                    labels.contains(&detail.as_str()),
                    "{page:?} native AX lacks {detail}: {labels:?}"
                );
            }
        }
    }

    #[test]
    fn index_health_notice_reflects_waiting_and_terminal_failures() {
        let not_requested: Resource<HealthModel> = Resource::not_yet();
        assert_eq!(
            health_notice(&not_requested)
                .as_ref()
                .map(|(message, _)| message.as_str()),
            Some("The index health report has not been requested yet.")
        );

        let waiting = not_requested.clone().waiting();
        assert_eq!(
            health_notice(&waiting)
                .as_ref()
                .map(|(message, _)| message.as_str()),
            Some("Waiting for the local index health report.")
        );

        let unsupported = Resource::unavailable(UnavailableReason::Unsupported);
        assert_eq!(
            health_notice(&unsupported),
            Some((
                "The connected service does not provide index health.".to_owned(),
                true
            ))
        );

        let failed = Resource::error(FaultCode::Transport, "service offline");
        assert!(matches!(failed.terminal(), ResourceTerminal::Fault(_)));
        assert_eq!(
            health_notice(&failed),
            Some((
                "The index health check failed: service offline".to_owned(),
                true
            ))
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
        let locald = directory.join(format!("backend-locald{}", std::env::consts::EXE_SUFFIX));
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

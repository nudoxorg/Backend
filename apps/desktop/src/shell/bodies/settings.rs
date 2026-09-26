//! Settings, plain: Appearance is one calm list — theme, contrast, density,
//! text size, motion — one control per row; the window itself is the
//! preview (every change re-resolves the whole shell live). The other pages
//! say what the index holds, which keys do what, and which build this is.

use super::{Ctx, Leaf};
use crate::model::{
    AppSnapshot, AppearancePreference, ContrastPreference, DensityPreference, MotionPreference,
};
use facet::controls::Swatch;
use crate::navigation::{Intent, SettingsPage};
use crate::shell::kit::{quiet, text};
use crate::shell::reader::Reader;
use facet::tokens::ty;
use facet::{Measure, Palette, Space};
use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, div};

pub(super) fn body(
    page: SettingsPage,
    snapshot: &AppSnapshot,
    store: &super::Pages,
    ctx: &mut Ctx<'_>,
    _cx: &mut Context<Reader>,
) -> Vec<Leaf> {
    match page {
        SettingsPage::Index | SettingsPage::Registry => index(store, ctx),
        SettingsPage::Help | SettingsPage::Legend => keys(ctx),
        SettingsPage::Diagnostics | SettingsPage::Connections => about(snapshot, ctx),
        SettingsPage::Appearance | SettingsPage::Editor | SettingsPage::Agents | SettingsPage::Privacy => {
            appearance(snapshot, ctx)
        }
    }
}

fn title(words: &str, ctx: &mut Ctx<'_>) -> Leaf {
    let said = ctx.say(words.to_owned());
    Leaf::new(text(ty::DISPLAY, &ctx.measure, ctx.palette.ink0).pb(ctx.measure.space(Space::Base)).child(said))
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
    ctx.say(words[match settings.appearance {
        AppearancePreference::System => 0,
        AppearancePreference::Abyss => 1,
        AppearancePreference::Glacier => 2,
    }]);
    leaves.push(setting("Theme", theme.into_any_element(), ctx));
    let links = ctx.links.clone();
    let contrast = facet::controls::seg("set-contrast", &measure)
        .label("Normal")
        .label("High")
        .selected(usize::from(settings.contrast == ContrastPreference::High))
        .on_select(move |index, _, cx| {
            let contrast = if index == 1 { ContrastPreference::High } else { ContrastPreference::Normal };
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
    match store.health().loaded_value() {
        Some(health) => {
            let facts = [
                ("Declarations", health.rows.to_string()),
                ("Files indexed", format!("{} of {}", health.ingest.files_indexed, health.ingest.files_discovered)),
                ("Files without declarations", health.ingest.files_unavailable.to_string()),
                ("Capabilities ready", health.ready_capabilities.join(", ")),
            ];
            for (name, value) in facts {
                let value = ctx.say(value);
                leaves.push(setting(name, text(ty::MONO_ROW, &measure, palette.ink1).child(value).into_any_element(), ctx));
            }
            for language in health.ingest.languages.iter() {
                let value = ctx.say(format!("{} declarations in {} files", language.declarations, language.files));
                leaves.push(setting(&language.language, text(ty::MONO_SMALL, &measure, palette.ink2).child(value).into_any_element(), ctx));
            }
        }
        None => {
            let words = ctx.say("The index report is on its way.");
            leaves.push(Leaf::new(quiet(words, &measure, palette)));
        }
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
            text(ty::MONO_ROW, &measure, palette.ink1).child(caps).into_any_element(),
            ctx,
        ));
    }
    leaves
}

fn about(snapshot: &AppSnapshot, ctx: &mut Ctx<'_>) -> Vec<Leaf> {
    let mut leaves = vec![title("About", ctx)];
    let measure = ctx.measure;
    let palette = ctx.palette;
    let version = ctx.say(env!("CARGO_PKG_VERSION"));
    leaves.push(setting("Version", text(ty::MONO_ROW, &measure, palette.ink1).child(version).into_any_element(), ctx));
    let mode = ctx.say(match snapshot.settings().service_mode {
        crate::model::ServiceMode::Embedded => "embedded local service",
        crate::model::ServiceMode::Attached => "attached to a running local service",
    });
    leaves.push(setting("Service", text(ty::ROW, &measure, palette.ink1).child(mode).into_any_element(), ctx));
    leaves
}

#[allow(dead_code)]
fn _palette(_: &Palette, _: &Measure) {}

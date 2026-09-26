//! The icon set: UI icons, kind marks, modifier marks, capability marks,
//! language marks, the chevron and the logo, embedded as SVG assets behind
//! closed enums. (Lane F2.)
//!
//! "Icons are cut, not drawn": mitred joins, square caps, and every closed
//! shape carries one lit facet — a filled sub-path at a third of the
//! stroke's strength. The SVGs are monochrome masks (black on transparent,
//! no `currentColor`), so GPUI tints them with the element's text colour.
//!
//! **Assets.** [`Assets`] embeds every file (`icons/{ui,kind,mod,cap,lang,
//! core}/<name>.svg`, `brand/{logo,mark}.svg`) and also serves *variants*
//! of the monochrome marks: `icons/ui/search~w1800.svg` is `search` with a
//! 1.8-unit stroke (the board scales stroke width inversely with size),
//! `~f50` sets the lit facet to 50 %, `~d` dashes every stroke (a capability
//! that arrives through a blanket impl). Tokens combine with `-`:
//! `icons/kind/package~w2400-f50.svg` is the glyph inside a gem. Compose
//! with the app's own assets through [`Assets::with`].
//!
//! **Builders.** [`ui`], [`kind_mark`], [`mod_mark`], [`cap_mark`],
//! [`lang_mark`], [`chevron`] and [`logo`] render at the board sizes with the
//! board colours; each returns an ordinary element you can keep styling.
//!
//! Regenerate the files and the tables with
//! `python3 apps/facet/assets/icons/gen_icons.py`.

mod set;
mod table;

#[cfg(feature = "gallery")]
pub(crate) mod gallery;

pub use set::{Cap, Icon, Kind, Lang, Mod};

use crate::tokens::{Palette, Voice, hex};
use gpui::{
    AnyElement, App, AssetSource, Global, Hsla, ImageSource, IntoElement, ParentElement,
    RenderImage, Result, SharedString, Styled, Svg, Window, div, img, px, svg,
};
use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;

/// Every embedded asset path, sorted.
pub fn paths() -> impl Iterator<Item = &'static str> {
    table::FILES.iter().map(|(path, _)| *path)
}

fn embedded(path: &str) -> Option<&'static [u8]> {
    table::FILES
        .binary_search_by(|(p, _)| (*p).cmp(path))
        .ok()
        .map(|i| table::FILES[i].1)
}

/// FACET's embedded assets (icons and the logo), with stroke/facet variants.
#[derive(Clone, Copy, Debug, Default)]
pub struct Assets;

impl Assets {
    /// These assets first, then `fallback` (the app's own asset source).
    #[must_use]
    pub fn with<A: AssetSource>(self, fallback: A) -> Layered<Self, A> {
        Layered(self, fallback)
    }
}

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = embedded(path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        Ok(variant(path).map(Cow::Owned))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(paths()
            .filter(|p| p.starts_with(path))
            .map(SharedString::new_static)
            .collect())
    }
}

/// Two asset sources, the first shadowing the second.
#[derive(Clone, Copy, Debug)]
pub struct Layered<A, B>(pub A, pub B);

impl<A: AssetSource, B: AssetSource> AssetSource for Layered<A, B> {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match self.0.load(path)? {
            Some(bytes) => Ok(Some(bytes)),
            None => self.1.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut all = self.0.list(path)?;
        for p in self.1.list(path)? {
            if !all.contains(&p) {
                all.push(p);
            }
        }
        Ok(all)
    }
}

/// How a monochrome mark is drawn: stroke width in viewBox units, the lit
/// facet's opacity, dashed or not.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stroke {
    /// Stroke width in 24-unit viewBox units.
    pub width: f32,
    /// Opacity of the lit facet (`class="f"` sub-paths); `None` keeps the
    /// group's own (.3 for icons, .34 for kinds).
    pub facet: Option<f32>,
    /// Dashes every stroke `2.2 2.2` (capabilities that arrive via blanket).
    pub dashed: bool,
}

impl Stroke {
    /// A plain stroke of `width` units.
    #[must_use]
    pub const fn width(width: f32) -> Self {
        Self {
            width,
            facet: None,
            dashed: false,
        }
    }
}

/// The asset path of `base` (`icons/<group>/<name>.svg`) drawn with `stroke`.
#[must_use]
pub fn variant_path(base: &str, stroke: Stroke) -> SharedString {
    let stem = base.strip_suffix(".svg").unwrap_or(base);
    #[allow(clippy::cast_possible_truncation)]
    let mut spec = format!("w{}", (stroke.width * 1000.0).round() as i32);
    if let Some(facet) = stroke.facet {
        #[allow(clippy::cast_possible_truncation)]
        let _ = write!(spec, "-f{}", (facet * 100.0).round() as i32);
    }
    if stroke.dashed {
        spec.push_str("-d");
    }
    SharedString::from(format!("{stem}~{spec}.svg"))
}

/// Builds a variant's bytes from its base file (`None` if the path is not a
/// variant of an embedded mark).
fn variant(path: &str) -> Option<Vec<u8>> {
    let stem = path.strip_suffix(".svg")?;
    let (base, spec) = stem.split_once('~')?;
    let source = std::str::from_utf8(embedded(&format!("{base}.svg"))?).ok()?;
    let mut svg = source.to_owned();
    let head_end = svg.find('>')?;
    let mut extra = String::new();
    for token in spec.split('-') {
        let (key, value) = token.split_at(1);
        match key {
            "w" => {
                let width = value.parse::<f32>().ok()? / 1000.0;
                let at = svg[..head_end].find("stroke-width=\"")? + "stroke-width=\"".len();
                let end = at + svg[at..].find('"')?;
                svg.replace_range(at..end, &format!("{width}"));
            }
            "f" => {
                let facet = value.parse::<f32>().ok()? / 100.0;
                svg = replace_attr(&svg, "opacity", &format!("{facet}"));
            }
            "d" => extra.push_str(" stroke-dasharray=\"2.2 2.2\""),
            _ => return None,
        }
    }
    if !extra.is_empty() {
        let head_end = svg.find('>')?;
        svg.insert_str(head_end, &extra);
    }
    Some(svg.into_bytes())
}

/// Replaces the value of every ` name="…"` attribute.
fn replace_attr(svg: &str, name: &str, value: &str) -> String {
    let needle = format!(" {name}=\"");
    let mut out = String::with_capacity(svg.len());
    let mut rest = svg;
    while let Some(at) = rest.find(&needle) {
        let start = at + needle.len();
        out.push_str(&rest[..start]);
        out.push_str(value);
        let tail = &rest[start..];
        let end = tail.find('"').unwrap_or(tail.len());
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

/// The `.ico` size ladder: the stroke thins as the icon grows.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum IconSize {
    /// 12 px, stroke 2.0.
    S12,
    /// 14 px, stroke 1.8.
    S14,
    /// 16 px, stroke 1.6 (the default).
    #[default]
    S16,
    /// 18 px, stroke 1.6.
    S18,
    /// 20 px, stroke 1.6.
    S20,
    /// 24 px, stroke 1.5.
    S24,
    /// 32 px, stroke 1.4.
    S32,
}

impl IconSize {
    /// Every size.
    pub const ALL: [Self; 7] = [
        Self::S12,
        Self::S14,
        Self::S16,
        Self::S18,
        Self::S20,
        Self::S24,
        Self::S32,
    ];

    /// Edge length in px.
    #[must_use]
    pub const fn px(self) -> f32 {
        match self {
            Self::S12 => 12.0,
            Self::S14 => 14.0,
            Self::S16 => 16.0,
            Self::S18 => 18.0,
            Self::S20 => 20.0,
            Self::S24 => 24.0,
            Self::S32 => 32.0,
        }
    }

    /// Stroke width in viewBox units.
    #[must_use]
    pub const fn stroke(self) -> f32 {
        match self {
            Self::S12 => 2.0,
            Self::S14 => 1.8,
            Self::S16 | Self::S18 | Self::S20 => 1.6,
            Self::S24 => 1.5,
            Self::S32 => 1.4,
        }
    }
}

/// A UI icon at a board size, tinted `color`.
#[must_use]
pub fn ui(icon: Icon, size: IconSize, color: impl Into<Hsla>) -> Svg {
    svg()
        .path(variant_path(icon.path(), Stroke::width(size.stroke())))
        .size(px(size.px()))
        .flex_none()
        .text_color(color.into())
}

/// The chevron: breadcrumb separator, disclosure twisty, "forward".
/// Rotate it with `.with_transformation(Transformation::rotate(..))`.
#[must_use]
pub fn chevron(size: IconSize, color: impl Into<Hsla>) -> Svg {
    svg()
        .path(variant_path(
            "icons/core/chevron.svg",
            Stroke::width(size.stroke()),
        ))
        .size(px(size.px()))
        .flex_none()
        .text_color(color.into())
}

/// Kind mark sizes (`.k.sm`, `.k`, `.k.lg`): the box, the glyph, its stroke.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum KindSize {
    /// A 15 px box, 14 px glyph, stroke 1.8.
    Sm,
    /// An 18 px box, 16 px glyph, stroke 1.7.
    #[default]
    Md,
    /// A 26 px box, 24 px glyph, stroke 1.5.
    Lg,
}

impl KindSize {
    /// `(box, glyph, stroke)`.
    #[must_use]
    pub const fn metrics(self) -> (f32, f32, f32) {
        match self {
            Self::Sm => (15.0, 14.0, 1.8),
            Self::Md => (18.0, 16.0, 1.7),
            Self::Lg => (26.0, 24.0, 1.5),
        }
    }
}

impl Kind {
    /// The kind's hue in `palette`.
    #[must_use]
    pub fn hue(self, palette: &Palette) -> Hsla {
        palette.family(self.family()).hue.into()
    }
}

/// A kind mark: the kind's shape in its family's hue, no tile.
#[must_use]
pub fn kind_mark(kind: Kind, size: KindSize, palette: &Palette) -> AnyElement {
    let (boxed, glyph, stroke) = size.metrics();
    div()
        .size(px(boxed))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .child(
            svg()
                .path(variant_path(kind.path(), Stroke::width(stroke)))
                .size(px(glyph))
                .text_color(kind.hue(palette)),
        )
        .into_any_element()
}

/// A modifier mark: 15 px (18 px in a legend cell), ink3 unless it warns.
/// Pair it with a tooltip carrying [`Mod::tooltip`].
#[must_use]
pub fn mod_mark(modifier: Mod, size: f32, palette: &Palette) -> Svg {
    let color: Hsla = match modifier.voice() {
        Some(voice) => palette.voice(voice).base.into(),
        None => palette.ink3.into(),
    };
    svg()
        .path(variant_path(modifier.path(), Stroke::width(1.6)))
        .size(px(size))
        .flex_none()
        .text_color(color)
}

/// How a capability is present.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum CapState {
    /// Not implemented: a closed door (ink4).
    #[default]
    Off,
    /// Written or derived (the contracts hue).
    On,
    /// Arrives through a blanket or auto impl (contracts hue, dashed).
    Via,
}

/// A capability mark: a 30 px hit target with an 18 px icon.
#[must_use]
pub fn cap_mark(cap: Cap, state: CapState, palette: &Palette) -> AnyElement {
    let color: Hsla = match state {
        CapState::Off => palette.ink4.into(),
        CapState::On | CapState::Via => palette.f_con.hue.into(),
    };
    let stroke = Stroke {
        width: 1.5,
        facet: None,
        dashed: state == CapState::Via,
    };
    div()
        .size(px(30.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .child(
            svg()
                .path(variant_path(cap.path(), stroke))
                .size(px(18.0))
                .text_color(color),
        )
        .into_any_element()
}

impl Lang {
    /// The mark colour (the boards use one set of brand-tinted literals in
    /// both appearances).
    #[must_use]
    pub fn color(self) -> Hsla {
        hex(self.rgb()).into()
    }
}

/// A language mark (14 px on the boards) in its brand tint.
#[must_use]
pub fn lang_mark(lang: Lang, size: f32) -> Svg {
    svg()
        .path(lang.path())
        .size(px(size))
        .flex_none()
        .text_color(lang.color())
}

impl Mod {
    /// The mark's colour in `palette` (ink3 unless it warns).
    #[must_use]
    pub fn color(self, palette: &Palette) -> Hsla {
        match self.voice() {
            Some(Voice::Mint) => palette.mint.base.into(),
            Some(Voice::Peri) => palette.peri.base.into(),
            Some(Voice::Amber) => palette.amber.base.into(),
            Some(Voice::Coral) => palette.coral.base.into(),
            None => palette.ink3.into(),
        }
    }
}

/// Where the full lockup gives way to the chevron diamond: "full lockup
/// from 48 px, the chevron diamond below that".
pub const LOGO_FULL_FROM: f32 = 48.0;

/// Rasterized logos, keyed by `(full, device px)`.
#[derive(Default)]
struct LogoCache(HashMap<(bool, u32), Arc<RenderImage>>);

impl Global for LogoCache {}

/// The Nudox logo at `size` px: the owner's original artwork from 48 px up,
/// the chevron diamond below. Both are full-colour SVGs rasterized by resvg
/// at exactly the window's device size (no resampling), cached per size.
#[must_use]
pub fn logo(size: f32) -> AnyElement {
    let full = size >= LOGO_FULL_FROM;
    let source = ImageSource::Custom(Arc::new(move |window: &mut Window, cx: &mut App| {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let device = (size * window.scale_factor()).round().max(1.0) as u32;
        if let Some(image) = cx
            .try_global::<LogoCache>()
            .and_then(|cache| cache.0.get(&(full, device)))
        {
            return Some(Ok(image.clone()));
        }
        let path = if full {
            "brand/logo.svg"
        } else {
            "brand/mark.svg"
        };
        let bytes = embedded(path)?;
        let intrinsic = if full { 48.0 } else { 240.0 };
        // `render_single_frame` rasterizes at `scale * 2` times the SVG's own
        // size; pick the scale that lands exactly on the device size.
        #[allow(clippy::cast_precision_loss)]
        let scale = device as f32 / (intrinsic * gpui::SMOOTH_SVG_SCALE_FACTOR);
        let image = match cx.svg_renderer().render_single_frame(bytes, scale) {
            Ok(image) => image,
            Err(error) => return Some(Err(error.into())),
        };
        cx.default_global::<LogoCache>()
            .0
            .insert((full, device), image.clone());
        Some(Ok(image))
    }));
    img(source).size(px(size)).flex_none().into_any_element()
}

#[cfg(test)]
#[allow(clippy::too_many_lines)]
mod tests {
    use super::*;
    use crate::tokens::Family;
    use std::collections::BTreeSet;
    use std::path::Path;

    fn files_on_disk(dir: &str) -> BTreeSet<String> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join(dir);
        let mut out = BTreeSet::new();
        for entry in std::fs::read_dir(&root).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(stem) = name.strip_suffix(".svg") {
                out.insert(format!("{dir}/{stem}.svg"));
            }
        }
        out
    }

    #[test]
    fn every_variant_has_a_file_and_every_file_a_variant() {
        let groups: [(&str, BTreeSet<String>); 5] = [
            (
                "icons/ui",
                Icon::ALL.iter().map(|i| i.path().to_owned()).collect(),
            ),
            (
                "icons/kind",
                Kind::ALL.iter().map(|k| k.path().to_owned()).collect(),
            ),
            (
                "icons/mod",
                Mod::ALL.iter().map(|m| m.path().to_owned()).collect(),
            ),
            (
                "icons/cap",
                Cap::ALL.iter().map(|c| c.path().to_owned()).collect(),
            ),
            (
                "icons/lang",
                Lang::ALL.iter().map(|l| l.path().to_owned()).collect(),
            ),
        ];
        for (dir, enum_paths) in groups {
            assert_eq!(
                files_on_disk(dir),
                enum_paths,
                "{dir}: enum and files disagree"
            );
            for path in &enum_paths {
                assert!(embedded(path).is_some(), "{path} is not embedded");
            }
        }
        assert_eq!(Icon::ALL.len(), 50);
        assert_eq!(Kind::ALL.len(), 20);
        assert_eq!(Mod::ALL.len(), 17);
        assert_eq!(Cap::ALL.len(), 14);
        assert_eq!(Lang::ALL.len(), 7);
        // Everything on disk is embedded, and nothing else is.
        let mut on_disk = BTreeSet::new();
        for dir in [
            "icons/ui",
            "icons/kind",
            "icons/mod",
            "icons/cap",
            "icons/lang",
            "icons/core",
            "brand",
        ] {
            let root = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("assets")
                .join(dir);
            for entry in std::fs::read_dir(&root).into_iter().flatten().flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if Path::new(&name)
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("svg"))
                {
                    on_disk.insert(format!("{dir}/{name}"));
                }
            }
        }
        let embedded_set: BTreeSet<String> = paths().map(str::to_owned).collect();
        assert_eq!(on_disk, embedded_set);
    }

    #[test]
    fn embedded_table_is_sorted_for_lookup() {
        let all: Vec<&str> = paths().collect();
        let mut sorted = all.clone();
        sorted.sort_unstable();
        assert_eq!(all, sorted);
    }

    #[test]
    fn variants_rewrite_stroke_facet_and_dash() {
        let path = variant_path(
            Kind::Package.path(),
            Stroke {
                width: 2.4,
                facet: Some(0.5),
                dashed: true,
            },
        );
        assert_eq!(path.as_ref(), "icons/kind/package~w2400-f50-d.svg");
        let bytes = Assets
            .load(&path)
            .ok()
            .flatten()
            .map(Cow::into_owned)
            .unwrap_or_default();
        let text = String::from_utf8(bytes).unwrap_or_default();
        assert!(text.contains("stroke-width=\"2.4\""), "{text}");
        assert!(text.contains("opacity=\"0.5\""), "{text}");
        assert!(text.contains("stroke-dasharray=\"2.2 2.2\""), "{text}");
        assert!(!text.contains("opacity=\".34\""), "{text}");
        // Unknown bases and tokens are refused, not guessed.
        assert!(
            Assets
                .load("icons/ui/nope~w1000.svg")
                .ok()
                .flatten()
                .is_none()
        );
        assert!(
            Assets
                .load("icons/ui/search~q1.svg")
                .ok()
                .flatten()
                .is_none()
        );
    }

    #[test]
    fn monochrome_marks_never_use_current_color() {
        for path in paths().filter(|p| p.starts_with("icons/")) {
            let text = std::str::from_utf8(embedded(path).unwrap_or_default()).unwrap_or_default();
            assert!(!text.contains("currentColor"), "{path}");
            assert!(text.starts_with("<svg "), "{path}");
        }
    }

    #[test]
    fn kinds_carry_their_family() {
        assert_eq!(Kind::Module.family(), Family::Namespace);
        assert_eq!(Kind::Struct.family(), Family::Type);
        assert_eq!(Kind::Trait.family(), Family::Contract);
        assert_eq!(Kind::Macro.family(), Family::Callable);
        assert_eq!(Kind::Variant.family(), Family::Value);
        assert_eq!(Mod::Unsafe.voice(), Some(Voice::Amber));
        assert_eq!(Mod::Async.tooltip(), "async: returns a future");
    }
}

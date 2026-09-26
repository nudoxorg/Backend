//! Measure: container-relative, density-aware, fluid sizing, and the ladder
//! of detail.
//!
//! Components never size themselves from the window. A region (shelf,
//! reader, margin, a card in a grid) hands its children a [`Measure`] for
//! the width they actually get; spacing, type and control sizes are
//! resolved from it. Three rules make the whole app flow across screens and
//! text sizes:
//!
//! 1. **Effective width.** Decisions use `width / text_scale`, so 200 % text
//!    on a 1440 px window behaves like 720 px: margins fold, the shelf becomes
//!    a spine, exactly as if the window were smaller. Nothing ever overlaps
//!    because the text got bigger.
//! 2. **Continuous, not stepped.** Spacing and display type interpolate
//!    smoothly with effective width ([`Measure::fluid`]); only structural
//!    changes (a margin folding, a shelf collapsing) are discrete, and those
//!    are animated by `motion::flow`.
//! 3. **The ladder.** Every datum can be drawn at four rungs of detail —
//!    [`Rung::Mark`] (a glyph), [`Rung::Tag`] (glyph + name),
//!    [`Rung::Row`] (a line with its key facts), [`Rung::Card`] (everything
//!    that fits in a popup). A container picks the highest rung its width
//!    affords; **hovering or focusing anything shows it one rung up**, and
//!    activating it descends to its page. Holding ⌥ (x-ray) raises every
//!    visible datum one rung in place where room allows.

use crate::fonts::Typeset;
use crate::theme::Facet;
use crate::tokens::{Face, TypeRole};
use gpui::{Pixels, Styled, px};

/// How much the interface packs into a pixel.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum Density {
    /// The boards' spacing.
    #[default]
    Comfortable,
    /// Tighter rows and gaps, same legibility.
    Compact,
    /// For big screens full of data: the tightest rows that stay readable.
    Dense,
}

impl Density {
    /// Multiplier for gaps and padding.
    #[must_use]
    pub const fn space(self) -> f32 {
        match self {
            Self::Comfortable => 1.0,
            Self::Compact => 0.8,
            Self::Dense => 0.64,
        }
    }

    /// Multiplier for UI and mono text (display type follows the fluid curve).
    #[must_use]
    pub const fn text(self) -> f32 {
        match self {
            Self::Comfortable => 1.0,
            Self::Compact => 0.95,
            Self::Dense => 0.9,
        }
    }

    /// Multiplier for row and control heights.
    #[must_use]
    pub const fn row(self) -> f32 {
        match self {
            Self::Comfortable => 1.0,
            Self::Compact => 0.86,
            Self::Dense => 0.74,
        }
    }

    /// The next denser setting (wraps to comfortable).
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Comfortable => Self::Compact,
            Self::Compact => Self::Dense,
            Self::Dense => Self::Comfortable,
        }
    }
}

/// A container's width class, decided on effective width (width ÷ text scale).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum Room {
    /// Under 480: a phone-width column. Lists, not diagrams.
    Narrow,
    /// 480–760: one column, compact marks.
    Slim,
    /// 760–1100: the reading column without a margin.
    Regular,
    /// 1100–1600: reading column plus margin.
    Wide,
    /// 1600 and up: room for a third column (pinned peeks).
    Vast,
}

impl Room {
    /// Classifies an effective width in px.
    #[must_use]
    pub fn of(effective: f32) -> Self {
        if effective < 480.0 {
            Self::Narrow
        } else if effective < 760.0 {
            Self::Slim
        } else if effective < 1100.0 {
            Self::Regular
        } else if effective < 1600.0 {
            Self::Wide
        } else {
            Self::Vast
        }
    }
}

/// The spacing scale, in px at comfortable density and 100 % text.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Space {
    /// 2 px.
    Hair,
    /// 4 px.
    Tight,
    /// 6 px.
    Snug,
    /// 8 px.
    Base,
    /// 12 px.
    Roomy,
    /// 16 px.
    Gutter,
    /// 24 px.
    Wide,
    /// 34 px (the folio/margin gutter).
    Section,
    /// 48 px.
    Chapter,
}

impl Space {
    /// The base value in px.
    #[must_use]
    pub const fn base(self) -> f32 {
        match self {
            Self::Hair => 2.0,
            Self::Tight => 4.0,
            Self::Snug => 6.0,
            Self::Base => 8.0,
            Self::Roomy => 12.0,
            Self::Gutter => 16.0,
            Self::Wide => 24.0,
            Self::Section => 34.0,
            Self::Chapter => 48.0,
        }
    }
}

/// Control heights.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Control {
    /// 24 px.
    Small,
    /// 30 px.
    Medium,
    /// 38 px.
    Large,
    /// 46 px.
    Huge,
}

impl Control {
    const fn base(self) -> f32 {
        match self {
            Self::Small => 24.0,
            Self::Medium => 30.0,
            Self::Large => 38.0,
            Self::Huge => 46.0,
        }
    }
}

/// The four rungs of detail every datum can be drawn at.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum Rung {
    /// A glyph: a gem, a tick, a stone.
    Mark,
    /// Glyph and name.
    Tag,
    /// A line with its key facts.
    Row,
    /// Everything that fits in a popup.
    Card,
}

impl Rung {
    /// One rung up (hover, focus, x-ray). A card stays a card.
    #[must_use]
    pub const fn up(self) -> Self {
        match self {
            Self::Mark => Self::Tag,
            Self::Tag => Self::Row,
            Self::Row | Self::Card => Self::Card,
        }
    }
}

/// Minimum effective widths (px) a datum needs to be drawn at each rung above
/// a mark. A container picks the highest rung that fits.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Needs {
    /// Width for a tag.
    pub tag: f32,
    /// Width for a row.
    pub row: f32,
    /// Width for a card.
    pub card: f32,
}

/// Transient reveal modes, driven by held modifiers.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub struct Reveal {
    /// ⌘ held: key caps appear on everything that has a key.
    pub keys: bool,
    /// ⌥ held: x-ray — every visible datum rises one rung in place.
    pub xray: bool,
}

/// Where fluid values sit between their small and large ends.
const FLUID_FROM: f32 = 480.0;
const FLUID_TO: f32 = 1600.0;

/// The width a container actually gets, and everything sized from it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Measure {
    width: Pixels,
    scale: f32,
    density: Density,
    reveal: Reveal,
}

impl Measure {
    /// A measure for a container `width` wide under the active theme.
    #[must_use]
    pub fn new(width: Pixels, facet: &Facet) -> Self {
        Self {
            width,
            scale: facet.text_scale,
            density: facet.density,
            reveal: facet.reveal,
        }
    }

    /// The container width in px.
    #[must_use]
    pub const fn width(&self) -> Pixels {
        self.width
    }

    /// Width ÷ text scale: what layout decisions are made on.
    #[must_use]
    pub fn effective(&self) -> f32 {
        f32::from(self.width) / self.scale
    }

    /// The width class.
    #[must_use]
    pub fn room(&self) -> Room {
        Room::of(self.effective())
    }

    /// Text scale (1.0 = 100 %).
    #[must_use]
    pub const fn scale(&self) -> f32 {
        self.scale
    }

    /// Density.
    #[must_use]
    pub const fn density(&self) -> Density {
        self.density
    }

    /// Held reveal modes.
    #[must_use]
    pub const fn reveal(&self) -> Reveal {
        self.reveal
    }

    /// A measure for a child container of another width.
    #[must_use]
    pub fn within(&self, width: Pixels) -> Self {
        Self {
            width: width.max(px(0.0)),
            ..*self
        }
    }

    /// A measure for a child inset by `by` on both sides.
    #[must_use]
    pub fn inset(&self, by: Pixels) -> Self {
        self.within(self.width - by * 2.0)
    }

    /// How many columns of at least `min` effective px fit (≤ `max`), and the
    /// measure of one column, with `gap` between them.
    #[must_use]
    pub fn columns(&self, min: f32, gap: Space, max: usize) -> (usize, Self) {
        let gap = f32::from(self.space(gap));
        let width = f32::from(self.width);
        let min = min * self.scale;
        let mut count = 1;
        while count < max.max(1) {
            let next = count + 1;
            #[allow(clippy::cast_precision_loss)] // column counts are tiny
            let column = (width - gap * (next - 1) as f32) / next as f32;
            if column < min {
                break;
            }
            count = next;
        }
        #[allow(clippy::cast_precision_loss)]
        let column = (width - gap * (count - 1) as f32) / count as f32;
        (count, self.within(px(column)))
    }

    /// Where this container sits on the fluid curve, 0.0 (480 effective px or
    /// less) to 1.0 (1600 or more), smoothstepped so there is no kink.
    #[must_use]
    pub fn t(&self) -> f32 {
        let raw = ((self.effective() - FLUID_FROM) / (FLUID_TO - FLUID_FROM)).clamp(0.0, 1.0);
        raw * raw * (3.0 - 2.0 * raw)
    }

    /// A value that grows smoothly from `small` to `large` px with the
    /// container, times the text scale.
    #[must_use]
    pub fn fluid(&self, small: f32, large: f32) -> Pixels {
        px((small + (large - small) * self.t()) * self.scale)
    }

    /// A spacing token, at this density, text scale and container width
    /// (gaps breathe a little more in big containers and tighten in small ones).
    #[must_use]
    pub fn space(&self, space: Space) -> Pixels {
        let breathe = 0.78 + 0.34 * self.t();
        px(space.base() * self.density.space() * breathe * self.scale)
    }

    /// A type role resolved for this container: display type grows fluidly
    /// (78 % of its size at 480 effective px, full size at 1600), UI and mono
    /// type follow density, with floors so nothing becomes illegible.
    #[must_use]
    pub fn role(&self, role: TypeRole) -> TypeRole {
        let factor = match role.face {
            Face::Display => 0.78 + 0.22 * self.t(),
            Face::Ui | Face::Mono | Face::Serif => self.density.text(),
        };
        let floor: f32 = match role.face {
            Face::Display => 16.0,
            Face::Ui => 10.5,
            Face::Mono => 10.5,
            Face::Serif => 12.0,
        };
        let size = (role.size * factor).max(floor.min(role.size));
        let ratio = size / role.size;
        TypeRole {
            size: size * self.scale,
            line: role.line * ratio * self.scale,
            ..role
        }
    }

    /// The standard list row height.
    #[must_use]
    pub fn row(&self) -> Pixels {
        px(27.0 * self.density.row() * self.scale)
    }

    /// A control height.
    #[must_use]
    pub fn control(&self, control: Control) -> Pixels {
        px((control.base() * self.density.row()).max(22.0) * self.scale)
    }

    /// An icon size (icons follow text, not density, below 14 px).
    #[must_use]
    pub fn icon(&self, base: f32) -> Pixels {
        let size = if base > 14.0 {
            base * self.density.text()
        } else {
            base
        };
        px(size * self.scale)
    }

    /// The highest rung whose need fits this container, raised one rung while
    /// x-ray is held.
    #[must_use]
    pub fn rung(&self, needs: Needs) -> Rung {
        let width = self.effective();
        let fitted = if width >= needs.card {
            Rung::Card
        } else if width >= needs.row {
            Rung::Row
        } else if width >= needs.tag {
            Rung::Tag
        } else {
            Rung::Mark
        };
        if self.reveal.xray { fitted.up() } else { fitted }
    }
}

/// Typesetting against a [`Measure`] (density, fluid display type, text
/// scale) instead of the bare theme.
pub trait Set: Typeset {
    /// Sets `role` as resolved for the container.
    #[must_use]
    fn set(self, role: TypeRole, measure: &Measure) -> Self {
        self.typeset_at(measure.role(role), 1.0)
    }
}

impl<E: Styled> Set for E {}

#[cfg(test)]
mod tests {
    use super::{Density, Measure, Needs, Reveal, Room, Rung, Space};
    use crate::theme::Facet;
    use crate::tokens::ty;
    use gpui::px;

    fn at(width: f32, scale: f32, density: Density) -> Measure {
        Measure::new(
            px(width),
            &Facet {
                text_scale: scale,
                density,
                ..Facet::default()
            },
        )
    }

    #[test]
    fn big_text_behaves_like_a_small_window() {
        assert_eq!(at(1440.0, 2.0, Density::Comfortable).room(), Room::Slim);
        assert_eq!(at(720.0, 1.0, Density::Comfortable).room(), Room::Slim);
        assert_eq!(at(1440.0, 1.0, Density::Comfortable).room(), Room::Wide);
    }

    #[test]
    fn fluid_values_are_continuous_across_room_boundaries() {
        let mut last = f32::from(at(300.0, 1.0, Density::Comfortable).space(Space::Gutter));
        for w in 301..2200 {
            #[allow(clippy::cast_precision_loss)]
            let now = f32::from(at(w as f32, 1.0, Density::Comfortable).space(Space::Gutter));
            assert!((now - last).abs() < 0.02, "jump at {w}: {last} -> {now}");
            assert!(now >= last, "spacing shrank as the container grew at {w}");
            last = now;
        }
    }

    #[test]
    fn display_type_never_drops_below_its_floor() {
        let narrow = at(320.0, 1.0, Density::Dense);
        assert!(narrow.role(ty::TITLE).size >= 16.0);
        assert!(narrow.role(ty::BODY).size >= 10.5);
        let wide = at(1600.0, 1.0, Density::Comfortable);
        assert!((wide.role(ty::HERO).size - ty::HERO.size).abs() < 0.001);
    }

    #[test]
    fn columns_fit_and_never_underflow() {
        let (count, column) = at(1000.0, 1.0, Density::Comfortable).columns(300.0, Space::Gutter, 6);
        assert_eq!(count, 3);
        assert!(f32::from(column.width()) >= 300.0);
        let (one, _) = at(200.0, 1.0, Density::Comfortable).columns(300.0, Space::Gutter, 6);
        assert_eq!(one, 1);
    }

    #[test]
    fn xray_raises_one_rung_and_cards_stay_cards() {
        let needs = Needs {
            tag: 200.0,
            row: 400.0,
            card: 800.0,
        };
        let plain = at(450.0, 1.0, Density::Comfortable);
        assert_eq!(plain.rung(needs), Rung::Row);
        let facet = Facet {
            reveal: Reveal {
                xray: true,
                keys: false,
            },
            ..Facet::default()
        };
        assert_eq!(Measure::new(px(450.0), &facet).rung(needs), Rung::Card);
        assert_eq!(Rung::Card.up(), Rung::Card);
    }
}

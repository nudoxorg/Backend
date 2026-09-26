//! The lens card: an aggregate one rung down, short at rest.
//!
//! Anatomy (the calm `v4/shots/Lenses.png`): a headline (mono name and a
//! quiet "when"), then the one line about **your** code — first, before
//! anything else, with the numbers that matter to you in mint — then at most
//! three items, then "and five more". No stacked bars, no legends, no key
//! foot: the float layer draws the plate, the connector to the exact tick or
//! region, and the crumb when it chains.
//!
//! Every line is one [`Spell`](crate::data::Spell) element; each item is a
//! door (resting on it chains into its peek). The card is as wide as its
//! longest line, within the width the float layer grants, so it grows with
//! the text scale.
//!
//! Constructors take plain data: [`Lens::release`], [`Lens::module`],
//! [`Lens::language`], [`Lens::direction`] (a rose direction or a compass
//! arm).

use crate::data::{Dir, Door, Seg, Side, spell};
use crate::icons::{Kind, Lang, Stroke, variant_path};
use crate::measure::Measure;
use crate::theme::ActiveFacet;
use crate::tokens::{Palette, TypeRole, ty};
use gpui::{
    AnyElement, App, ElementId, Hsla, IntoElement, ParentElement, SharedString, Styled, Window,
    div, px,
};

/// What an item did in a release.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Sigil {
    /// Added (`+`, mint).
    Added,
    /// Changed (`~`, periwinkle).
    Changed,
    /// Removed (`−`, coral).
    Removed,
}

impl Sigil {
    const fn glyph(self) -> &'static str {
        match self {
            Self::Added => "+",
            Self::Changed => "~",
            Self::Removed => "−",
        }
    }

    fn ink(self, palette: &Palette) -> Hsla {
        match self {
            Self::Added => palette.mint.base.into(),
            Self::Changed => palette.peri.base.into(),
            Self::Removed => palette.coral.base.into(),
        }
    }
}

/// The small mark before a lens's title.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum LensMark {
    /// A language mark (the language comb's lens).
    Lang(Lang),
    /// A kind glyph (a module region's lens).
    Kind(Kind),
}

/// One item row.
#[derive(Clone, Debug, PartialEq)]
pub struct LensItem {
    /// What it did (release lenses).
    pub sigil: Option<Sigil>,
    /// Its kind glyph.
    pub kind: Option<Kind>,
    /// Its name (mono).
    pub name: SharedString,
    /// A share `0..=1` drawn as a quiet bar, and the count at its end.
    pub share: Option<(f32, SharedString)>,
}

impl LensItem {
    /// An item named `name`.
    #[must_use]
    pub fn new(name: impl Into<SharedString>) -> Self {
        Self {
            sigil: None,
            kind: None,
            name: name.into(),
            share: None,
        }
    }

    /// Its kind glyph.
    #[must_use]
    pub const fn kind(mut self, kind: Kind) -> Self {
        self.kind = Some(kind);
        self
    }

    /// What it did.
    #[must_use]
    pub const fn sigil(mut self, sigil: Sigil) -> Self {
        self.sigil = Some(sigil);
        self
    }

    /// A share and its count.
    #[must_use]
    pub fn share(mut self, share: f32, count: impl Into<SharedString>) -> Self {
        self.share = Some((share.clamp(0.0, 1.0), count.into()));
        self
    }
}

/// A lens: plain data in, a card out ([`lens_card`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Lens {
    /// The mark before the title.
    pub mark: Option<LensMark>,
    /// The title (mono): a version, a module, a language, a direction.
    pub title: SharedString,
    /// The quiet word after it: "3 weeks ago", "214 public items".
    pub when: Option<SharedString>,
    /// The line about your code, first: runs, `true` = the number or name
    /// that matters to you (mint).
    pub yours: Vec<(SharedString, bool)>,
    /// At most three are shown.
    pub items: Vec<LensItem>,
    /// How many more there are beyond the shown items.
    pub more: usize,
}

/// Items a lens shows before "and N more".
pub const SHOWN: usize = 3;

impl Lens {
    /// A release: `1.0.200 · 3 weeks ago`, what touches your code, what changed.
    #[must_use]
    pub fn release(
        version: impl Into<SharedString>,
        when: impl Into<SharedString>,
        yours: Vec<(SharedString, bool)>,
        changes: Vec<LensItem>,
    ) -> Self {
        Self::of(None, version.into(), Some(when.into()), yours, changes)
    }

    /// A module region: `de · 214 public items`, how many you use, the ones
    /// you use most.
    #[must_use]
    pub fn module(name: impl Into<SharedString>, items: usize, used: usize, most: Vec<LensItem>) -> Self {
        let yours = if used > 0 {
            vec![
                ("your code uses ".into(), false),
                (used.to_string().into(), true),
                (" of them, most often".into(), false),
            ]
        } else {
            vec![("your code uses none of them".into(), false)]
        };
        let mut lens = Self::of(
            None,
            name.into(),
            Some(format!("{items} public items").into()),
            yours,
            most,
        );
        lens.more = used.saturating_sub(lens.items.len().min(SHOWN)).max(lens.more);
        lens
    }

    /// A language on Orbit's comb: `python · 7 packages`, the most opened.
    #[must_use]
    pub fn language(lang: Lang, name: impl Into<SharedString>, packages: usize, opened: &[(&str, usize)]) -> Self {
        let top = opened.iter().map(|(_, n)| *n).max().unwrap_or(1).max(1);
        #[allow(clippy::cast_precision_loss)]
        let items = opened
            .iter()
            .map(|(name, n)| LensItem::new((*name).to_owned()).share(*n as f32 / top as f32, n.to_string()))
            .collect();
        Self::of(
            Some(LensMark::Lang(lang)),
            name.into(),
            Some(format!("{packages} packages").into()),
            Vec::new(),
            items,
        )
    }

    /// A rose direction or a compass arm: `to · 3 calls`, its members.
    #[must_use]
    pub fn direction(dir: Dir, unit: &str, members: Vec<LensItem>, yours: Vec<(SharedString, bool)>) -> Self {
        let count = members.len();
        Self::of(None, dir.word().into(), Some(format!("{count} {unit}").into()), yours, members)
    }

    fn of(
        mark: Option<LensMark>,
        title: SharedString,
        when: Option<SharedString>,
        yours: Vec<(SharedString, bool)>,
        items: Vec<LensItem>,
    ) -> Self {
        let more = items.len().saturating_sub(SHOWN);
        Self {
            mark,
            title,
            when,
            yours,
            items,
            more,
        }
    }
}

/// "five", "fourteen", "213": counts a sentence can say.
#[must_use]
pub fn count_words(n: usize) -> SharedString {
    const WORDS: [&str; 21] = [
        "no", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven",
        "twelve", "thirteen", "fourteen", "fifteen", "sixteen", "seventeen", "eighteen", "nineteen",
        "twenty",
    ];
    WORDS
        .get(n)
        .map_or_else(|| n.to_string().into(), |w| SharedString::new_static(w))
}

const TITLE: TypeRole = TypeRole {
    weight: 600.0,
    size: 14.0,
    line: 18.0,
    ..ty::MONO_ROW
};
const WHEN: TypeRole = TypeRole {
    size: 12.0,
    line: 16.0,
    ..ty::SMALL
};
const LINE: TypeRole = TypeRole {
    size: 12.5,
    line: 17.0,
    ..ty::SMALL
};
const LINE_B: TypeRole = TypeRole {
    weight: 600.0,
    ..LINE
};
const ITEM: TypeRole = TypeRole {
    weight: 500.0,
    size: 12.0,
    line: 17.0,
    ..ty::MONO_ROW
};
const SIGIL: TypeRole = TypeRole {
    weight: 600.0,
    ..ITEM
};
const MORE: TypeRole = TypeRole {
    size: 12.5,
    line: 16.0,
    ..ty::CAPTION
};

/// The card's content for `measure` (the float layer's card width). Items
/// open through `item_door` (part 0 of each row), when given.
pub fn lens_card(
    lens: &Lens,
    measure: &Measure,
    item_door: Option<(ElementId, Door)>,
    cx: &App,
) -> AnyElement {
    let palette = cx.palette();
    let s = measure.scale();
    let d = measure.density().space().max(0.8);
    let inner = measure.inset(px(16.0 * s * d));

    let mut head = spell(&inner).nowrap();
    match lens.mark {
        Some(LensMark::Lang(lang)) => {
            head = head
                .seg(Seg::Icon {
                    path: lang.path().into(),
                    size: 14.0,
                    color: lang.color(),
                })
                .gap(8.0);
        }
        Some(LensMark::Kind(kind)) => {
            head = head
                .seg(Seg::Icon {
                    path: variant_path(kind.path(), Stroke::width(1.8)),
                    size: 14.0,
                    color: kind.hue(palette),
                })
                .gap(8.0);
        }
        None => {}
    }
    head = head.text(lens.title.clone(), TITLE, palette.ink0);
    if let Some(when) = &lens.when {
        head = head.gap(10.0).text(when.clone(), WHEN, palette.ink3);
    }

    let mut card = div()
        .flex()
        .flex_col()
        .gap(px(9.0 * s * d))
        .px(px(16.0 * s * d))
        .py(px(14.0 * s * d))
        .min_w(px(220.0 * s))
        .child(head);

    if !lens.yours.is_empty() {
        let mut line = spell(&inner);
        for (words, strong) in &lens.yours {
            line = if *strong {
                line.text(words.clone(), LINE_B, palette.mint.base)
            } else {
                line.text(words.clone(), LINE, palette.ink3)
            };
        }
        card = card.child(line);
    }

    let shown = lens.items.iter().take(SHOWN);
    let mut list = div().flex().flex_col().gap(px(5.0 * s * d));
    for (i, item) in shown.enumerate() {
        let mut row = spell(&inner).nowrap();
        if let Some(sigil) = item.sigil {
            row = row
                .part(
                    0,
                    Seg::Cell {
                        text: sigil.glyph().into(),
                        role: SIGIL,
                        color: sigil.ink(palette),
                        width: 10.0,
                        right: false,
                    },
                )
                .part(0, Seg::Gap(8.0));
        }
        if let Some(kind) = item.kind {
            row = row
                .part(
                    0,
                    Seg::Icon {
                        path: variant_path(kind.path(), Stroke::width(1.8)),
                        size: 14.0,
                        color: kind.hue(palette),
                    },
                )
                .part(0, Seg::Gap(8.0));
        }
        match &item.share {
            Some((share, count)) => {
                row = row
                    .part(
                        0,
                        Seg::Cell {
                            text: item.name.clone(),
                            role: ITEM,
                            color: palette.ink1.into(),
                            width: 78.0,
                            right: false,
                        },
                    )
                    .part(0, Seg::Gap(8.0))
                    .part(
                        0,
                        Seg::Line {
                            parts: vec![
                                (*share, palette.ink3.into(), false),
                                ((1.0 - share).max(0.0), palette.line1.into(), false),
                            ],
                            width: 160.0,
                            height: 3.0,
                        },
                    )
                    .part(0, Seg::Gap(10.0))
                    .part(
                        0,
                        Seg::Cell {
                            text: count.clone(),
                            role: WHEN,
                            color: palette.ink3.into(),
                            width: 22.0,
                            right: true,
                        },
                    );
            }
            None => {
                row = row.part_text(0, item.name.clone(), ITEM, palette.ink1);
            }
        }
        if let Some((mark, door)) = &item_door {
            row = row
                .id(ElementId::NamedChild(
                    std::sync::Arc::new(mark.clone()),
                    format!("item-{i}").into(),
                ))
                .door(door.clone())
                .side(Side::Right);
        }
        list = list.child(row);
    }
    if lens.more > 0 {
        list = list.child(
            spell(&inner).text(format!("and {} more", count_words(lens.more)), MORE, palette.ink3),
        );
    }
    if !lens.items.is_empty() {
        card = card.child(list);
    }
    card.into_any_element()
}

/// A [`Door`] whose parts open `lens(part)` as a lens card.
pub fn door(lens: impl Fn(usize) -> Option<Lens> + 'static) -> Door {
    Door::lens(move |part, measure, _window: &mut Window, cx| match lens(part) {
        Some(data) => lens_card(&data, measure, None, cx),
        None => div().into_any_element(),
    })
}

#[cfg(test)]
mod tests {
    use super::{Lens, LensItem, SHOWN, Sigil, count_words};
    use crate::icons::Kind;

    #[test]
    fn a_release_lens_shows_three_and_counts_the_rest() {
        let items: Vec<LensItem> = (0..8)
            .map(|i| LensItem::new(format!("item{i}")).kind(Kind::Function).sigil(Sigil::Changed))
            .collect();
        let lens = Lens::release("1.0.200", "3 weeks ago", Vec::new(), items);
        assert_eq!(lens.more, 8 - SHOWN);
        assert_eq!(count_words(lens.more), "five");
    }

    #[test]
    fn a_module_lens_counts_what_you_use_beyond_the_shown() {
        let most = vec![
            LensItem::new("Deserialize").kind(Kind::Trait),
            LensItem::new("Visitor").kind(Kind::Trait),
            LensItem::new("from_str").kind(Kind::Function),
        ];
        let lens = Lens::module("de", 214, 17, most);
        assert_eq!(lens.more, 14);
        assert_eq!(count_words(lens.more), "fourteen");
        assert_eq!(lens.yours[1].0, "17");
        assert!(lens.yours[1].1, "the count you care about is the mint one");
        let none = Lens::module("de::impls", 44, 0, Vec::new());
        assert_eq!(none.more, 0);
        assert_eq!(none.yours[0].0, "your code uses none of them");
    }

    #[test]
    fn language_shares_are_relative_to_the_most_opened() {
        let lens = Lens::language(crate::icons::Lang::Python, "python", 7, &[("numpy", 22), ("requests", 11)]);
        let shares: Vec<f32> = lens.items.iter().filter_map(|i| i.share.as_ref().map(|s| s.0)).collect();
        assert!((shares[0] - 1.0).abs() < 1e-6 && (shares[1] - 0.5).abs() < 1e-6);
        assert_eq!(count_words(213), "213");
    }
}

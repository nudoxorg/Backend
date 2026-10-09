//! The peek card: a symbol, package, location or version one rung up.
//!
//! Short at rest, deeper when asked (`gui-plan.md` §6.2):
//!
//! | state | a symbol peek shows |
//! |---|---|
//! | rest | mark, name, where, a one-line signature, one sentence, your uses |
//! | ⌘ held | + the pin mark and the key foot |
//! | ⌥ held, or rested on again | + the pin mark, compass bar, "in your code" and the file comb |
//! | a chained child | the crumb, mark, name, where, one sentence (+ the same reveals) |
//! | pinned | mark, name, path |
//!
//! Every section is optional: data that is absent is simply not drawn, and
//! the reveal state (from the [`Measure`] and the float layer's
//! [`float::Build`]) decides the rest. Plain data in; nothing here knows the
//! engine. Content is built for the card's own measure, so it scales with
//! text and re-flows in the narrow sheet.

use super::float::{self, Build, FloatKind, FloatRequest, Surface};
use super::text::{Sig, prose};
use crate::data::{Directions, FileUses, compass_bar, fcomb};
use crate::icons::{self, IconSize, Kind, Lang};
use crate::measure::{Measure, Set};
use crate::paint::gem;
use crate::reading::{self, Intent, ReadingRole};
use crate::theme::ActiveFacet;
use crate::tokens::{Face, Palette, TypeRole};
use gpui::{
    AnyElement, App, Bounds, ElementId, IntoElement, ParentElement, Pixels, SharedString, Styled,
    Window, div, px,
};
use std::rc::Rc;
use std::sync::Arc;

// ------------------------------------------------------------------ data

/// A symbol.
#[derive(Clone, Default)]
pub struct SymbolPeek {
    /// The kind (the gem).
    pub kind: Option<Kind>,
    /// The name, as it is spelled.
    pub name: SharedString,
    /// Where it lives, as prose markup: ``enum in `present::relation` ``.
    pub place: SharedString,
    /// The bare path (the pinned row's second line).
    pub path: SharedString,
    /// One line of signature (ellipsised to the card).
    pub signature: Option<Sig>,
    /// One sentence, prose markup.
    pub sentence: Option<SharedString>,
    /// Number of reported use sites; this is not complete usage coverage.
    pub uses: Option<usize>,
    /// In how many of your files.
    pub files: Option<usize>,
    /// Its relational shape (the ⌥ compass bar).
    pub compass: Option<Directions>,
    /// Uses grouped by file (the ⌥ file comb).
    pub file_uses: Vec<FileUses>,
}

/// A package.
#[derive(Clone, Default)]
pub struct PackagePeek {
    /// The name.
    pub name: SharedString,
    /// Its ecosystem.
    pub lang: Option<Lang>,
    /// The version you read.
    pub version: SharedString,
    /// The registry (`crates.io`).
    pub registry: SharedString,
    /// One sentence, prose markup.
    pub sentence: Option<SharedString>,
    /// How many of its items your code reaches.
    pub reach: Option<usize>,
}

/// One line of a location excerpt.
#[derive(Clone)]
pub struct ExcerptLine {
    /// The line number.
    pub number: usize,
    /// The code, as a signature-style run.
    pub code: Sig,
    /// The lines the location points at.
    pub lit: bool,
}

/// A location in a file.
#[derive(Clone, Default)]
pub struct LocationPeek {
    /// The file name.
    pub name: SharedString,
    /// The path.
    pub path: SharedString,
    /// The line it points at.
    pub line: usize,
    /// A few lines around it.
    pub excerpt: Vec<ExcerptLine>,
}

/// A release.
#[derive(Clone, Default)]
pub struct VersionPeek {
    /// The version.
    pub version: SharedString,
    /// When (`3 weeks ago`).
    pub when: SharedString,
    /// The one line about your code: (the item, how many places you call it).
    pub yours: Option<(SharedString, usize)>,
    /// Items added.
    pub added: usize,
    /// Items changed.
    pub changed: usize,
}

/// Anything a peek can show.
#[derive(Clone)]
pub enum Peek {
    /// A symbol.
    Symbol(SymbolPeek),
    /// A package.
    Package(PackagePeek),
    /// A location.
    Location(LocationPeek),
    /// A release.
    Version(VersionPeek),
}

impl Peek {
    /// The card's name (its crumb and pinned row).
    #[must_use]
    pub fn title(&self) -> SharedString {
        match self {
            Self::Symbol(symbol) => symbol.name.clone(),
            Self::Package(package) => package.name.clone(),
            Self::Location(location) => location.name.clone(),
            Self::Version(version) => version.version.clone(),
        }
    }

    /// The card's width at 100 % text.
    #[must_use]
    pub const fn width(&self, child: bool) -> f32 {
        match self {
            Self::Symbol(_) if child => 320.0,
            Self::Symbol(_) => 360.0,
            Self::Package(_) => 340.0,
            Self::Location(_) => 380.0,
            Self::Version(_) => 320.0,
        }
    }
}

/// The float request for a peek of `peek` on the trigger `key` at `anchor`.
pub fn request(key: impl Into<ElementId>, anchor: Bounds<Pixels>, peek: Peek) -> FloatRequest {
    FloatRequest::new(key, anchor, FloatKind::Peek, content(peek))
}

/// The content builder for the float layer.
pub fn content(peek: Peek) -> impl Fn(&Measure, &mut Window, &mut App) -> AnyElement + 'static {
    let peek = Rc::new(peek);
    move |measure: &Measure, window: &mut Window, cx: &mut App| card(&peek, measure, window, cx)
}

// ------------------------------------------------------------------ type

const fn role(face: Face, weight: f32, size: f32, line: f32, tracking: f32) -> TypeRole {
    TypeRole {
        face,
        weight,
        size,
        line,
        tracking,
        italic: matches!(face, Face::Serif),
    }
}

const NAME: TypeRole = role(Face::Mono, 600.0, 14.5, 18.0, -0.01);
const CHILD_NAME: TypeRole = role(Face::Mono, 600.0, 14.0, 18.0, -0.01);
const WHERE: TypeRole = role(Face::Ui, 400.0, 12.0, 16.0, 0.0);
const SIG: TypeRole = role(Face::Mono, 400.0, 12.0, 18.0, 0.0);
const SAY: TypeRole = role(Face::Serif, 400.0, 14.5, 21.0, 0.0);
const FACT: TypeRole = role(Face::Ui, 400.0, 12.5, 17.0, 0.0);
const FACT_BOLD: TypeRole = role(Face::Ui, 600.0, 12.5, 17.0, 0.0);
const FOOT: TypeRole = role(Face::Ui, 400.0, 11.5, 14.0, 0.0);
const CRUMB: TypeRole = role(Face::Mono, 400.0, 11.0, 14.0, 0.0);
const CAPTION: TypeRole = role(Face::Serif, 400.0, 12.0, 14.0, 0.0);
const COUNT: TypeRole = role(Face::Mono, 400.0, 11.0, 14.0, 0.0);
const PIN_NAME: TypeRole = role(Face::Mono, 500.0, 12.5, 16.0, 0.0);
const PIN_PATH: TypeRole = role(Face::Mono, 400.0, 11.0, 14.0, 0.0);
const EXCERPT: TypeRole = role(Face::Mono, 400.0, 11.5, 19.0, 0.0);
const DELTA: TypeRole = role(Face::Mono, 600.0, 12.0, 16.0, 0.0);

/// A card-internal length: `value` px at 100 % text and comfortable density.
fn k(measure: &Measure, value: f32) -> Pixels {
    px(value * measure.scale() * measure.density().space())
}

// ------------------------------------------------------------------ the card

/// What a card shows, from the reveal state and the layer's build context.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Show {
    keys: bool,
    deep: bool,
    child: bool,
    pinned: bool,
}

impl Show {
    fn of(measure: &Measure, build: &Build) -> Self {
        let reveal = measure.reveal();
        Self {
            keys: reveal.keys,
            deep: reveal.xray || build.deep,
            child: build.level.is_some_and(|level| level > 0),
            pinned: build.surface == Surface::Pinned,
        }
    }
}

/// Renders `peek` for `measure` (inside the float layer the build context
/// decides child / pinned / sheet; outside it renders as a root card).
pub fn card(peek: &Peek, measure: &Measure, window: &mut Window, cx: &mut App) -> AnyElement {
    let build = float::build(window, cx);
    float::title(peek.title(), window, cx);
    let show = Show::of(measure, &build);
    let palette = cx.facet().palette();
    if show.pinned {
        return pinned_row(peek, measure, palette);
    }
    let crumbs = if show.child {
        float::take_crumbs(window, cx)
    } else {
        Vec::new()
    };
    let width = if build.surface == Surface::Sheet {
        measure.width()
    } else {
        k(measure, peek.width(show.child)).min(measure.width())
    };
    let inner = measure.within(width - k(measure, 32.0));
    let mut body = div()
        .w(width)
        .flex()
        .flex_col()
        .gap(k(measure, 10.0))
        .pt(k(measure, 14.0))
        .px(k(measure, 16.0))
        .pb(k(measure, 14.0));
    if !crumbs.is_empty() {
        body = body.child(
            float::crumb_line(crumbs.iter(), &inner, palette)
                .set(CRUMB, &inner)
                .mb(-k(measure, 2.0)),
        );
    }
    let id = |part: &str| peek_key(peek, part);
    let sections: Vec<AnyElement> = match peek {
        Peek::Symbol(symbol) => symbol_sections(symbol, show, &inner, palette, &id),
        Peek::Package(package) => package_sections(package, show, &inner, palette, &id),
        Peek::Location(location) => location_sections(location, show, &inner, palette, &id),
        Peek::Version(version) => version_sections(version, show, &inner, palette, &id),
    };
    body = body.children(sections);
    if show.keys {
        body = body.child(foot(peek, &inner, palette));
    }
    body.into_any_element()
}

fn head(
    id: ElementId,
    mark: AnyElement,
    name: &SharedString,
    place: Option<AnyElement>,
    pin: bool,
    name_role: TypeRole,
    measure: &Measure,
    palette: &Palette,
) -> AnyElement {
    let mut text = div().flex().flex_col().gap(k(measure, 2.0)).min_w_0().flex_1().child(
        div()
            .set(name_role, measure)
            .text_color(palette.ink0.hsla())
            .truncate()
            .child(reading::text(id, name.clone(), Intent::Reading(ReadingRole::Heading))),
    );
    if let Some(place) = place {
        text = text.child(place);
    }
    let mut row = div()
        .flex()
        .items_center()
        .gap(k(measure, 11.0))
        .child(mark)
        .child(text);
    if pin {
        row = row.child(icons::ui(icons::Icon::Pin, IconSize::S12, palette.ink3).size(measure.icon(12.0)));
    }
    row.into_any_element()
}

fn place_line(id: impl Into<ElementId>, markup: &SharedString, measure: &Measure, palette: &Palette) -> AnyElement {
    div()
        .truncate()
        .child(
            prose(id, markup.clone(), WHERE, measure)
                .color(palette.ink3)
                .code_color(palette.ink2),
        )
        .into_any_element()
}

fn say(id: impl Into<ElementId>, markup: &SharedString, measure: &Measure, palette: &Palette) -> AnyElement {
    prose(id, markup.clone(), SAY, measure)
        .color(palette.ink1)
        .code_color(palette.ink2)
        .into_any_element()
}

/// "**14** uses in your code": the one fact, its number in mint.
fn fact(id: ElementId, parts: &[(&str, bool)], measure: &Measure, palette: &Palette) -> AnyElement {
    let mut row = div().flex().flex_wrap().items_baseline();
    for (index, (text, bold)) in parts.iter().enumerate() {
        let key = ElementId::NamedChild(Arc::new(id.clone()), format!("piece-{index}").into());
        let words = reading::text(key, SharedString::from((*text).to_owned()), Intent::Reading(ReadingRole::Fact));
        row = row.child(if *bold {
            div()
                .set(FACT_BOLD, measure)
                .text_color(palette.mint.base.hsla())
                .child(words)
        } else {
            div()
                .set(FACT, measure)
                .text_color(palette.ink3.hsla())
                .child(words)
        });
    }
    row.into_any_element()
}

fn gem_mark(kind: Kind, size: f32, measure: &Measure) -> AnyElement {
    gem(kind).size(size * measure.scale()).into_any_element()
}

fn symbol_sections(symbol: &SymbolPeek, show: Show, measure: &Measure, palette: &Palette, id: &impl Fn(&str) -> ElementId) -> Vec<AnyElement> {
    let mut out = Vec::new();
    let mark = gem_mark(symbol.kind.unwrap_or(Kind::Unknown), if show.child { 24.0 } else { 28.0 }, measure);
    let place = (!symbol.place.is_empty()).then(|| place_line(id("where"), &symbol.place, measure, palette));
    out.push(head(
        id("name"),
        mark,
        &symbol.name,
        place,
        show.keys || show.deep,
        if show.child { CHILD_NAME } else { NAME },
        measure,
        palette,
    ));
    if !show.child
        && let Some(signature) = &symbol.signature
    {
        out.push(
            div()
                .px(k(measure, 10.0))
                .py(k(measure, 7.0))
                .bg(palette.inset.hsla())
                .truncate()
                .child(signature.render(id("sig"), SIG, measure, palette))
                .into_any_element(),
        );
    }
    if let Some(sentence) = &symbol.sentence {
        out.push(say(id("say"), sentence, measure, palette));
    }
    if !show.child
        && let Some(uses) = symbol.uses
    {
        let count = uses.to_string();
        out.push(fact(id("uses"), &[(count.as_str(), true), (" reported use sites", false)], measure, palette));
    }
    if show.deep {
        let mut more = div().flex().flex_col().gap(k(measure, 10.0)).pt(k(measure, 4.0));
        let mut any = false;
        if let Some(dirs) = symbol.compass {
            more = more.child(compass_bar(dirs, measure).id(id("compass")));
            any = true;
        }
        if !symbol.file_uses.is_empty() {
            let mut caption = div()
                .flex()
                .justify_between()
                .items_baseline()
                .child(div().set(CAPTION, measure).text_color(palette.ink3.hsla()).child(reading::text(id("files-caption"), "in your code", Intent::Reading(ReadingRole::Fact))));
            if let (Some(uses), Some(files)) = (symbol.uses, symbol.files) {
                caption = caption.child(
                    div()
                        .set(COUNT, measure)
                        .text_color(palette.ink2.hsla())
                        .child(reading::text(id("files-count"), format!("{uses} reported use sites · {files} files"), Intent::Reading(ReadingRole::Fact))),
                );
            }
            more = more
                .child(caption)
                .child(fcomb(symbol.file_uses.clone(), measure).id(id("files")));
            any = true;
        }
        if any {
            out.push(more.into_any_element());
        }
    }
    out
}

fn package_sections(package: &PackagePeek, show: Show, measure: &Measure, palette: &Palette, id: &impl Fn(&str) -> ElementId) -> Vec<AnyElement> {
    let mut place = div()
        .flex()
        .items_center()
        .gap(k(measure, 4.0))
        .set(WHERE, measure)
        .text_color(palette.ink3.hsla());
    if let Some(lang) = package.lang {
        place = place.child(icons::lang_mark(lang, 11.0 * measure.scale()));
    }
    place = place
        .child(div().set(PIN_PATH, measure).text_color(palette.ink2.hsla()).child(reading::text(id("version"), package.version.clone(), Intent::Reading(ReadingRole::Fact))))
        .child(reading::text(id("registry"), format!("· {}", package.registry), Intent::Reading(ReadingRole::Fact)));
    let mut out = vec![head(
        id("name"),
        gem_mark(Kind::Package, 28.0, measure),
        &package.name,
        Some(place.into_any_element()),
        show.keys || show.deep,
        NAME,
        measure,
        palette,
    )];
    if let Some(sentence) = &package.sentence {
        out.push(say(id("say"), sentence, measure, palette));
    }
    if let Some(reach) = package.reach {
        let count = reach.to_string();
        out.push(fact(
            id("reach"),
            &[("your code reaches ", false), (count.as_str(), true), (" of its items", false)],
            measure,
            palette,
        ));
    }
    out
}

fn location_sections(location: &LocationPeek, show: Show, measure: &Measure, palette: &Palette, id: &impl Fn(&str) -> ElementId) -> Vec<AnyElement> {
    let place = div()
        .set(WHERE, measure)
        .text_color(palette.ink3.hsla())
        .truncate()
        .child(reading::text(id("where"), format!("{} · line {}", location.path, location.line), Intent::Reading(ReadingRole::Fact)))
        .into_any_element();
    let mut out = vec![head(
        id("name"),
        icons::ui(icons::Icon::File, IconSize::S16, palette.ink2)
            .size(measure.icon(16.0))
            .into_any_element(),
        &location.name,
        Some(place),
        show.keys || show.deep,
        NAME,
        measure,
        palette,
    )];
    if !location.excerpt.is_empty() {
        let mut block = div()
            .flex()
            .flex_col()
            .py(k(measure, 6.0))
            .bg(palette.inset.hsla());
        for line in &location.excerpt {
            let mut row = div()
                .flex()
                .gap(k(measure, 12.0))
                .px(k(measure, 10.0))
                .set(EXCERPT, measure)
                .whitespace_nowrap()
                .overflow_hidden()
                .child(
                    div()
                        .w(k(measure, 26.0))
                        .flex_none()
                        .flex()
                        .justify_end()
                        .text_color(palette.ink4.hsla())
                        .child(reading::text(id(&format!("line-number-{}", line.number)), line.number.to_string(), Intent::Reading(ReadingRole::Fact))),
                )
                .child(line.code.render(
                    ElementId::Name(format!("peek:loc:{}:{}", location.path, line.number).into()),
                    EXCERPT,
                    measure,
                    palette,
                ));
            if line.lit {
                row = row.bg(palette.peri.base.alpha(0.07).hsla());
            }
            block = block.child(row);
        }
        out.push(block.into_any_element());
    }
    out
}

fn version_sections(version: &VersionPeek, show: Show, measure: &Measure, palette: &Palette, id: &impl Fn(&str) -> ElementId) -> Vec<AnyElement> {
    let tick = div()
        .w(k(measure, 2.0))
        .h(k(measure, 24.0))
        .mx(k(measure, 6.0))
        .bg(palette.ink1.hsla())
        .into_any_element();
    let place = div()
        .set(WHERE, measure)
        .text_color(palette.ink3.hsla())
        .child(reading::text(id("when"), version.when.clone(), Intent::Reading(ReadingRole::Fact)))
        .into_any_element();
    let mut out = vec![head(id("name"), tick, &version.version, Some(place), show.keys || show.deep, NAME, measure, palette)];
    if let Some((item, places)) = &version.yours {
        let count = places.to_string();
        out.push(fact(
            id("yours"),
            &[
                (item.as_ref(), true),
                (" changed, and your code calls it in ", false),
                (count.as_str(), true),
                (" places", false),
            ],
            measure,
            palette,
        ));
    }
    if version.added + version.changed > 0 {
        let delta = |key: ElementId, sign: &str, n: usize, color: gpui::Hsla| {
            div()
                .set(DELTA, measure)
                .text_color(color)
                .child(reading::text(key, format!("{sign}{n}"), Intent::Reading(ReadingRole::Fact)))
        };
        out.push(
            div()
                .flex()
                .items_baseline()
                .gap(k(measure, 4.0))
                .set(FACT, measure)
                .text_color(palette.ink3.hsla())
                .child(delta(id("added-count"), "+", version.added, palette.mint.base.hsla()))
                .child(reading::text(id("added"), "added", Intent::Reading(ReadingRole::Fact)))
                .child(delta(id("changed-count"), "~", version.changed, palette.peri.base.hsla()))
                .child(reading::text(id("changed"), "changed", Intent::Reading(ReadingRole::Fact)))
                .into_any_element(),
        );
    }
    out
}

fn foot(peek: &Peek, measure: &Measure, palette: &Palette) -> AnyElement {
    use crate::controls::{KbdVoice, keys};
    let items: &[(&[&str], &str)] = match peek {
        Peek::Symbol(_) | Peek::Package(_) => {
            &[(&["Space"], "pin"), (&["↵"], "open"), (&["⌥", "→"], "follow")]
        }
        Peek::Location(_) => &[(&["S"], "peel to source"), (&["↵"], "open file")],
        Peek::Version(_) => &[(&["↵"], "diff"), (&["Space"], "pin")],
    };
    let mut row = div()
        .flex()
        .flex_wrap()
        .gap(k(measure, 14.0))
        .pt(k(measure, 10.0))
        .border_t_1()
        .border_color(palette.line1.hsla())
        .set(FOOT, measure)
        .text_color(palette.ink3.hsla());
    for (index, (chord, word)) in items.iter().enumerate() {
        row = row.child(
            div()
                .flex()
                .items_center()
                .gap(k(measure, 6.0))
                .child(keys(chord, KbdVoice::Plain, measure))
                .child(reading::text(peek_key(peek, &format!("key-hint-{index}")), *word, Intent::Reading(ReadingRole::Fact))),
        );
    }
    row.into_any_element()
}

fn pinned_row(peek: &Peek, measure: &Measure, palette: &Palette) -> AnyElement {
    let (mark, name, path): (AnyElement, SharedString, SharedString) = match peek {
        Peek::Symbol(symbol) => (
            gem_mark(symbol.kind.unwrap_or(Kind::Unknown), 20.0, measure),
            symbol.name.clone(),
            symbol.path.clone(),
        ),
        Peek::Package(package) => (gem_mark(Kind::Package, 20.0, measure), package.name.clone(), package.registry.clone()),
        Peek::Location(location) => (
            icons::ui(icons::Icon::File, IconSize::S14, palette.ink3)
                .size(measure.icon(14.0))
                .into_any_element(),
            location.name.clone(),
            location.path.clone(),
        ),
        Peek::Version(version) => (
            div().w(px(2.0)).h(k(measure, 18.0)).mx(k(measure, 6.0)).bg(palette.ink2.hsla()).into_any_element(),
            version.version.clone(),
            version.when.clone(),
        ),
    };
    div()
        .flex()
        .items_center()
        .gap(k(measure, 10.0))
        .child(mark)
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(1.0))
                .min_w_0()
                .child(div().set(PIN_NAME, measure).text_color(palette.ink1.hsla()).truncate().child(reading::text(peek_key(peek, "pin-name"), name, Intent::Reading(ReadingRole::Heading))))
                .child(div().set(PIN_PATH, measure).text_color(palette.ink4.hsla()).truncate().child(reading::text(peek_key(peek, "pin-path"), path, Intent::Reading(ReadingRole::Fact)))),
        )
        .into_any_element()
}

/// Section keys remain beneath the float card/pin's existing global owner
/// scope. These reading identities never establish producer authority.
fn peek_key(peek: &Peek, part: &str) -> ElementId {
    match peek {
        Peek::Symbol(symbol) => ElementId::Name(format!("peek:{}:{part}", symbol.name).into()),
        Peek::Package(package) => ElementId::Name(format!("peek:pkg:{}:{part}", package.name).into()),
        Peek::Location(location) => ElementId::Name(format!("peek:loc:{}:{}:{part}", location.path, location.line).into()),
        Peek::Version(version) => ElementId::Name(format!("peek:version:{}:{part}", version.version).into()),
    }
}

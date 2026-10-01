//! The ecosystem mark: the registry's own stone and, in the hero, its word.
//!
//! - **Rest.** One quiet glyph (`crates.io` is a stone cut as an octagon,
//!   not the word "cargo"); the word follows in the hero. A workspace
//!   package's stone has a mint table and its word is "local".
//! - **Card** (rest, or Enter): what kind of package this is and where it
//!   lives, one sentence when the registry needs explaining, and the
//!   install line in a well you press to copy.
//! - **Copy.** The line goes where it now lives: a copy drops through the
//!   well's bottom edge into the clipboard, visible only below that edge so
//!   it never lies over the line it came from; the edge flashes mint as it
//!   passes; the line itself dims and comes back; the stone turns one
//!   symmetry step; "copy" rolls to "copied" and back.
//! - **Quiet** ([`EcosystemMark::quiet`]): the stone alone, no word, no
//!   card, for rows whose ecosystem the list already implies.

use super::card::{self, Content, k, text};
use super::glyph;
use crate::measure::Measure;
use crate::motion::{self, Motion, Spring};
use crate::paint::mix;
use crate::theme::ActiveFacet;
use crate::tokens::motion::DROP;
use gpui::{
    AnyElement, App, ClickEvent, ClipboardItem, ElementId, InteractiveElement, IntoElement, ParentElement, RenderOnce,
    SharedString, StatefulInteractiveElement, Styled, Window, canvas, div, px,
};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

/// A package ecosystem, by its registry.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Eco {
    /// crates.io.
    Crates,
    /// npm.
    Npm,
    /// PyPI.
    Pypi,
    /// Go modules.
    Go,
    /// Maven Central.
    Maven,
    /// NuGet.
    Nuget,
    /// C and C++: no registry.
    Cpp,
}

impl Eco {
    /// Reads an ecosystem name or alias (`crates`, `cargo`, `rust`, `npm`,
    /// `typescript`, `python`, `csharp`, `c++`…).
    #[must_use]
    pub fn of(name: &str) -> Option<Self> {
        Some(match name.to_ascii_lowercase().as_str() {
            "crates" | "crates.io" | "cargo" | "rust" => Self::Crates,
            "npm" | "typescript" | "javascript" => Self::Npm,
            "pypi" | "python" => Self::Pypi,
            "go" | "golang" => Self::Go,
            "maven" | "java" => Self::Maven,
            "nuget" | "csharp" | "c#" | "dotnet" => Self::Nuget,
            "cpp" | "c" | "c++" | "clang" => Self::Cpp,
            _ => return None,
        })
    }

    /// The word beside the stone.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Crates => "crates.io",
            Self::Npm => "npm",
            Self::Pypi => "PyPI",
            Self::Go => "Go",
            Self::Maven => "Maven",
            Self::Nuget => "NuGet",
            Self::Cpp => "source",
        }
    }

    /// What a package here is called.
    #[must_use]
    pub const fn kind(self) -> &'static str {
        match self {
            Self::Crates => "Rust crate",
            Self::Npm => "npm package",
            Self::Pypi => "Python package",
            Self::Go => "Go module",
            Self::Maven => "Java library",
            Self::Nuget => ".NET package",
            Self::Cpp => "C/C++ library",
        }
    }

    /// The registry, as its users say it.
    #[must_use]
    pub const fn registry(self) -> &'static str {
        match self {
            Self::Crates => "crates.io",
            Self::Npm => "npmjs.com",
            Self::Pypi => "PyPI",
            Self::Go => "proxy.golang.org",
            Self::Maven => "Maven Central",
            Self::Nuget => "nuget.org",
            Self::Cpp => "no registry",
        }
    }

    /// The registry's install line for `package` (at `version` where the
    /// line names one).
    #[must_use]
    pub fn install(self, package: &str, version: Option<&str>) -> String {
        match (self, version) {
            (Self::Crates, _) => format!("cargo add {package}"),
            (Self::Npm, _) => format!("npm i {package}"),
            (Self::Pypi, _) => format!("pip install {package}"),
            (Self::Go, Some(v)) => format!("go get {package}@{v}"),
            (Self::Go, None) => format!("go get {package}"),
            (Self::Maven, Some(v)) => format!("implementation(\"{package}:{v}\")"),
            (Self::Maven, None) => format!("implementation(\"{package}\")"),
            (Self::Nuget, Some(v)) => format!("dotnet add package {package} --version {v}"),
            (Self::Nuget, None) => format!("dotnet add package {package}"),
            (Self::Cpp, _) => format!("git clone {package}"),
        }
    }
}

/// What the ecosystem mark knows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EcoFacts {
    /// The ecosystem.
    pub eco: Eco,
    /// The exact install line (the registry's own, or the path dependency
    /// of a workspace package). `None`: the card shows the registry only.
    pub install: Option<SharedString>,
    /// A workspace package that was never published: its path.
    pub local: Option<SharedString>,
    /// One sentence when the registry needs explaining.
    pub say: Option<SharedString>,
}

impl EcoFacts {
    /// A package in `eco` installed with `install`.
    #[must_use]
    pub fn new(eco: Eco, install: Option<&str>) -> Self {
        Self {
            eco,
            install: install.map(|line| SharedString::from(line.to_owned())),
            local: None,
            say: None,
        }
    }

    /// The card's kind line (`Rust crate`, a local `Rust package`).
    #[must_use]
    pub fn kind(&self) -> String {
        if self.local.is_some() {
            format!("{} package", self.eco.kind().split(' ').next().unwrap_or_default())
        } else {
            self.eco.kind().to_owned()
        }
    }

    /// The card's where line.
    #[must_use]
    pub fn place(&self) -> String {
        match &self.local {
            Some(path) if !path.is_empty() => format!("in this workspace · {path}"),
            Some(_) => "in this workspace".to_owned(),
            None => self.eco.registry().to_owned(),
        }
    }

    /// The card's sentence, if it has one.
    #[must_use]
    pub fn sentence(&self) -> Option<String> {
        self.say.as_ref().map(ToString::to_string).or_else(|| {
            if self.local.is_some() {
                Some("Never published: other packages reach it by path.".to_owned())
            } else if self.eco == Eco::Cpp {
                Some("No registry to install from.".to_owned())
            } else {
                None
            }
        })
    }
}

/// The copy tick and the drop run this long (ms).
const DROP_MS: f32 = 420.0;
/// The "copied" reel rolls back after this (ms).
const TICK_MS: f32 = 1100.0;
/// Reels and odometer wheels (MOTION.md's REEL).
pub(crate) const REEL: Spring = Spring {
    response: 0.24,
    damping: 0.92,
};

struct Copy {
    at: Rc<Cell<Option<Instant>>>,
    /// A sheet's press was scheduled (once).
    scheduled: bool,
}

/// The ecosystem mark (see the module docs).
#[derive(IntoElement)]
pub struct EcosystemMark {
    id: ElementId,
    facts: Rc<EcoFacts>,
    measure: Measure,
    word: bool,
    quiet: bool,
    sheet: Option<u64>,
    pressed: Option<u64>,
}

/// The ecosystem mark for `facts`, sized for `measure`.
#[must_use]
pub fn ecosystem_mark(id: impl Into<ElementId>, facts: EcoFacts, measure: &Measure) -> EcosystemMark {
    EcosystemMark {
        id: id.into(),
        facts: Rc::new(facts),
        measure: *measure,
        word: true,
        quiet: false,
        sheet: None,
        pressed: None,
    }
}

impl EcosystemMark {
    /// The stone alone (list rows).
    #[must_use]
    pub const fn glyph_only(mut self) -> Self {
        self.word = false;
        self
    }

    /// The stone alone, with no card: a row whose ecosystem the list
    /// already implies.
    #[must_use]
    pub const fn quiet(mut self) -> Self {
        self.quiet = true;
        self.word = false;
        self
    }

    /// Opens the card on the first paint (state sheets, stills).
    #[must_use]
    pub const fn open_look(mut self) -> Self {
        self.sheet = Some(0);
        self
    }

    /// Opens the card `after` ms into the scene (films).
    #[must_use]
    pub const fn open_at(mut self, after: u64) -> Self {
        self.sheet = Some(after);
        self
    }

    /// Opens the card and presses copy `after` ms (films, stills).
    #[must_use]
    pub const fn press_look(mut self, after: u64) -> Self {
        self.sheet = Some(0);
        self.pressed = Some(after);
        self
    }

    fn key(&self) -> ElementId {
        ElementId::NamedChild(Arc::new(self.id.clone()), "card".into())
    }
}

/// The stone as an element, `size` px square.
fn stone_element(eco: Eco, size: f32, ink: gpui::Hsla, table: Option<gpui::Hsla>, turn: f32, lift: f32) -> AnyElement {
    canvas(|_, _, _| {}, move |bounds, (), window, _| {
        let bounds = gpui::Bounds::new(gpui::point(bounds.origin.x, bounds.origin.y - px(lift)), bounds.size);
        glyph::stone(eco, bounds, ink, table, turn, window);
    })
    .size(px(size))
    .flex_none()
    .into_any_element()
}

impl RenderOnce for EcosystemMark {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.facet().palette();
        let measure = self.measure;
        let s = measure.scale();
        let key = self.key();
        let live = card::live(&self.id, &key, window, cx);
        let copy = window.use_keyed_state(ElementId::NamedChild(Arc::new(self.id.clone()), "copy".into()), cx, |_, _| Copy {
            at: Rc::new(Cell::new(None)),
            scheduled: false,
        });
        let copied = copy.read(cx).at.clone();
        let motion = Motion::scoped(ElementId::View(copy.entity_id()), cx);
        let turn_key = ElementId::NamedChild(Arc::new(self.id.clone()), "turn".into());
        let turn = motion.animate(turn_key.clone(), 0.0, crate::motion::spec::FOLLOW, window, cx);
        let ink = mix(palette.ink2.into(), palette.ink0.into(), live.lit);
        let table = self.facts.local.as_ref().map(|_| gpui::Hsla::from(palette.mint.base));
        let stone = stone_element(self.facts.eco, 16.0 * s, ink, table, turn, live.lit);
        let word = self.facts.local.as_ref().map_or(self.facts.eco.word(), |_| "local");
        let mut mark = div().flex().items_center().gap(px(7.0 * s)).child(stone);
        if self.word {
            mark = mark.child(text(
                ElementId::NamedChild(Arc::new(self.id.clone()), "word".into()),
                word,
                card::WORD,
                &measure,
                mix(palette.ink2.into(), palette.ink0.into(), live.lit),
            ));
        }
        if self.quiet {
            return card::door(&self.id, &key, &live, None, None, None, mark);
        }
        // Pressing the stone copies too (the card need not be open).
        let facts = self.facts.clone();
        let press_at = copied.clone();
        let press_motion = motion.clone();
        let press_turn = turn_key.clone();
        let mark = div().id("eco-mark").child(mark).on_click(move |_: &ClickEvent, _window, cx| {
            if let Some(line) = &facts.install {
                press(line, &press_at, &press_motion, &press_turn, cx);
            }
        });
        if let Some(after) = self.pressed
            && !copy.read(cx).scheduled
        {
            // A sheet's press: the copy starts `after` ms into the scene.
            copy.update(cx, |copy, _| copy.scheduled = true);
            let at = copied.clone();
            let line = self.facts.install.clone();
            let press_motion = motion.clone();
            let press_turn = turn_key.clone();
            {
                window
                    .spawn(cx, async move |cx| {
                        cx.background_executor().timer(std::time::Duration::from_millis(after)).await;
                        let _ = cx.update(|window, cx| {
                            if let Some(line) = &line
                                && at.get().is_none()
                            {
                                press(line, &at, &press_motion, &press_turn, cx);
                                window.refresh();
                            }
                        });
                    })
                    .detach();
            }
        }
        let content = eco_card(self.facts.clone(), copied, motion, turn_key);
        card::door(&self.id, &key, &live, Some(content), self.sheet, None, mark)
    }
}

fn press(line: &SharedString, at: &Rc<Cell<Option<Instant>>>, motion: &Motion, turn: &ElementId, cx: &mut App) {
    cx.write_to_clipboard(ClipboardItem::new_string(line.to_string()));
    at.set(Some(motion::now(cx)));
    // The stone turns one step back to where it started.
    motion.set(turn.clone(), 1.0);
}

fn eco_card(facts: Rc<EcoFacts>, copied: Rc<Cell<Option<Instant>>>, motion: Motion, turn_key: ElementId) -> Content {
    let field = Rc::new(Cell::new((0.0_f32, 0.0_f32)));
    Rc::new(move |measure: &Measure, window: &mut Window, cx: &mut App| {
        let palette = cx.facet().palette();
        let s = measure.scale();
        let now = motion::now(cx);
        let reduced = motion::reduced(cx);
        let t = copied.get().map(|at| now.saturating_duration_since(at).as_secs_f32() * 1000.0);
        if t.is_some_and(|t| t < TICK_MS + 600.0) {
            motion::request_frame(window, cx);
        }
        let turn = motion.animate(turn_key.clone(), 0.0, crate::motion::spec::FOLLOW, window, cx);
        let table = facts.local.as_ref().map(|_| gpui::Hsla::from(palette.mint.base));
        let head = div()
            .flex()
            .items_center()
            .gap(k(measure, 11.0))
            .child(stone_element(facts.eco, 30.0 * s, palette.ink1.into(), table, turn, 0.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(1.0 * s))
                    .child(text("mk-eco-kind", facts.kind(), card::TITLE, measure, palette.ink0))
                    .child(text("mk-eco-where", facts.place(), card::PLACE, measure, palette.ink3)),
            );
        let mut body = card::body(356.0, measure).child(head);
        if let Some(say) = facts.sentence() {
            body = body.child(div().mt(k(measure, 8.0)).child(text("mk-eco-say", say, card::SAY, measure, palette.ink1)));
        }
        if let Some(line) = &facts.install {
            body = body.child(well(line, t, reduced, measure, &copied, &motion, &turn_key, &field, window, cx));
        }
        body.into_any_element()
    })
}

/// The install line's well, and the slot under it the copy drops through.
#[allow(clippy::too_many_arguments)]
fn well(
    line: &SharedString,
    t: Option<f32>,
    reduced: bool,
    measure: &Measure,
    copied: &Rc<Cell<Option<Instant>>>,
    motion: &Motion,
    turn_key: &ElementId,
    sizes: &Rc<Cell<(f32, f32)>>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let palette = cx.facet().palette();
    let s = measure.scale();
    let band = |from: f32, span: f32| t.map_or(0.0, |t| ((t - from) / span).clamp(0.0, 1.0));
    let live = t.is_some_and(|t| t < DROP_MS);
    // The line dims while its copy leaves and comes back.
    let dim = if reduced || !live { 0.0 } else { band(0.0, 60.0) * (1.0 - band(340.0, 80.0)) };
    let ink = mix(palette.ink0.into(), palette.ink3.into(), dim);
    // The slot's edge flashes mint as the copy passes it; under reduced
    // motion it holds mint for 1.2 s instead, then settles.
    let flash = if reduced {
        t.map_or(0.0, |t| if t < 1200.0 { 1.0 } else { 1.0 - ((t - 1200.0) / 160.0).clamp(0.0, 1.0) })
    } else {
        (std::f32::consts::PI * band(200.0, 200.0)).sin().max(0.0)
    };
    let edge = mix(palette.line2.into(), palette.mint.base.into(), flash);
    // The reel: "copy" rolls up to "copied" and back.
    let ticked = t.is_some_and(|t| t < TICK_MS);
    let reel_key = ElementId::NamedChild(Arc::new(turn_key.clone()), "reel".into());
    let reel = motion.animate(reel_key, if ticked { 1.0 } else { 0.0 }, crate::motion::Spec::Spring(REEL), window, cx);
    let line_h = k(measure, 16.0);
    let pad_y = k(measure, 8.0);
    // The field is as tall as its wrapped line; its height is read back from
    // the last paint, so the copy's slot sits exactly at its bottom edge.
    let field_h = px(sizes.get().0.max(f32::from(line_h + pad_y * 2.0)));
    let press_line = line.clone();
    let press_at = copied.clone();
    let press_motion = motion.clone();
    let press_turn = turn_key.clone();
    let mut hint = div().h(line_h).overflow_hidden().flex_none();
    {
        // Two rows in a one-row window; REEL rolls between them, so the two
        // words are never both whole in it.
        hint = hint.child(
            div()
                .flex()
                .flex_col()
                .mt(-line_h * reel.clamp(0.0, 1.0))
                .child(text("mk-eco-copy", "copy", card::FOOT, measure, palette.ink4).into_any_element())
                .child(text("mk-eco-copied", "copied", card::FOOT, measure, palette.mint.base)),
        );
    }
    let field = div()
        .id("mk-eco-line")
        .relative()
        .w_full()
        .flex()
        .items_start()
        .gap(k(measure, 10.0))
        .px(k(measure, 10.0))
        .py(pad_y)
        .bg(palette.table.hsla())
        .border_b_1()
        .border_color(edge)
        .cursor_pointer()
        .child(div().relative().flex_1().min_w_0().child(tokens("mk-eco-install", line, measure, ink)).child({
            let sizes = sizes.clone();
            canvas(move |bounds, _, _| sizes.set((sizes.get().0, f32::from(bounds.size.width))), |_, (), _, _| {})
                .absolute()
                .top_0()
                .left_0()
                .size_full()
        }))
        .child(hint)
        .child({
            let sizes = sizes.clone();
            canvas(move |bounds, _, _| sizes.set((f32::from(bounds.size.height), sizes.get().1)), |_, (), _, _| {})
                .absolute()
                .top_0()
                .left_0()
                .size_full()
        })
        .on_click(move |_: &ClickEvent, _window, cx| {
            press(&press_line, &press_at, &press_motion, &press_turn, cx);
        });
    // The copy, falling through the slot: drawn only below the field's
    // bottom edge, so it never lies over the line it came from.
    let tray_h = k(measure, 22.0);
    // It falls until it has left the slot entirely (field bottom, then the
    // tray to the card's foot), so it is never cut while still in view.
    let through = f32::from(field_h - pad_y + tray_h) + 2.0 * s;
    let fall = if reduced || !live { None } else { Some(through * DROP.ease(band(60.0, 300.0))) };
    let tray = fall.map(|dy| {
        div()
            .absolute()
            .left_0()
            .right_0()
            .top(field_h)
            .h(tray_h)
            .overflow_hidden()
            .child(
                div()
                    .absolute()
                    .left(k(measure, 10.0))
                    .top(pad_y - field_h + px(dy))
                    .w(px(sizes.get().1.max(1.0)))
                    .child(tokens("mk-eco-ghost", line, measure, palette.ink0.into())),
            )
    });
    div()
        .relative()
        .w_full()
        .mt(k(measure, 11.0))
        .pb(k(measure, 9.0))
        .child(field)
        .children(tray)
        .into_any_element()
}


/// The card's content on its own (boards show it in place).
#[cfg(feature = "gallery")]
pub(crate) fn board_card(facts: &EcoFacts) -> Content {
    eco_card(
        Rc::new(facts.clone()),
        Rc::new(Cell::new(None)),
        Motion::new(),
        ElementId::Name("mk-eco-board".into()),
    )
}

/// An install line that wraps only between its tokens: `--version`, a
/// module path or a quoted path never break inside (each token is one
/// unbreakable run; the row wraps between them).
fn tokens(key: &str, line: &SharedString, measure: &Measure, ink: gpui::Hsla) -> AnyElement {
    let role = measure.role(card::LINE);
    let space = role.size * 0.6;
    let row = div()
        .flex()
        .flex_wrap()
        .gap_x(px(space))
        .set_line()
        .children(line.split_whitespace().map(|token| {
            div().whitespace_nowrap().set_role(measure).text_color(ink).child(token.to_owned()).into_any_element()
        }));
    crate::probe::text(key.to_owned(), line.clone(), role, 1.0, crate::probe::TextOverflow::Wrap, row).into_any_element()
}

trait LineRole {
    fn set_line(self) -> Self;
    fn set_role(self, measure: &Measure) -> Self;
}

impl LineRole for gpui::Div {
    fn set_line(self) -> Self {
        self
    }

    fn set_role(self, measure: &Measure) -> Self {
        use crate::measure::Set;
        self.set(card::LINE, measure)
    }
}

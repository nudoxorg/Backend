//! The license mark: a ring per family and, in the hero, the license's
//! short word (`MIT/Apache-2.0`).
//!
//! - **Rest.** The ring's shape is the family ([`glyph::ring`]); an `OR` is
//!   a row of rings (your choice), an `AND` a chain of linked rings (all
//!   apply). Hovering turns an open ring a quarter, and a choice's rings
//!   part a little.
//! - **Card** (GitHub's, in plain words): the license's name and family;
//!   for a choice, one tab per license (the one assumed starts selected;
//!   resting on a tab switches the columns); Permissions, Conditions and
//!   Limitations; whether it fits your project; for a choice, which option
//!   the card assumed ("assuming you take it under MIT"); one line about
//!   your tree when it matters; and, at its foot, [`spdx::HEDGE`].
//! - **Your own project** ([`LicenseFacts::own`]): the card reads the
//!   whole lockfile instead ("1,189 packages: permissive throughout, plus
//!   MPL-2.0 in dwrote and option-ext…"), with what it could not read
//!   counted as unread, never as unlicensed.

use super::card::{self, Content, k, text};
use super::glyph;
use super::semver::word;
use super::spdx::{self, Expr, Family, TreeLicenses};
use crate::measure::Measure;
use crate::motion::{Motion, SNAPPY, Spec};
use crate::paint::mix;
use crate::theme::ActiveFacet;
use crate::tokens::Palette;
use gpui::{
    AnyElement, App, ElementId, Hsla, InteractiveElement, IntoElement, ParentElement, RenderOnce, SharedString,
    StatefulInteractiveElement, Styled, Window, canvas, div, px,
};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

/// What the license mark knows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LicenseFacts {
    /// The package's SPDX expression; `None` when the manifest has no
    /// license field.
    pub spdx: Option<SharedString>,
    /// The license file's name when there is no expression (`LICENSE`).
    pub file: Option<SharedString>,
    /// Your project's own license (what fit is measured against).
    pub yours: SharedString,
    /// Your project's name, for consequence words ("would make present GPL").
    pub project: SharedString,
    /// The package's name, to find it in your tree.
    pub package: Option<SharedString>,
    /// What the lockfile says about every license in your tree, when the
    /// reader has a lockfile.
    pub tree: Option<Rc<TreeLicenses>>,
    /// This is your project's own hero: the card reads the whole tree.
    pub own: bool,
}

impl LicenseFacts {
    /// A package licensed `spdx`, read by a project licensed `yours`.
    #[must_use]
    pub fn new(spdx: Option<&str>, yours: &str, project: &str) -> Self {
        Self {
            spdx: spdx.map(|s| SharedString::from(s.to_owned())),
            file: None,
            yours: yours.to_owned().into(),
            project: project.to_owned().into(),
            package: None,
            tree: None,
            own: false,
        }
    }

    /// The expression the card reads (a license file reads as its own id).
    #[must_use]
    pub fn expr(&self) -> Option<Expr> {
        match (&self.spdx, &self.file) {
            (Some(spdx), _) => spdx::parse(spdx),
            (None, Some(_)) => Some(Expr::Id(spdx::LICENSE_FILE.to_owned())),
            (None, None) => None,
        }
    }

    /// The mark's one word.
    #[must_use]
    pub fn word(&self) -> String {
        match (&self.spdx, &self.file, self.expr()) {
            (Some(_), _, Some(expr)) => spdx::rest_word(&expr),
            (None, Some(_), _) => "custom".to_owned(),
            _ => "none".to_owned(),
        }
    }

    /// Everything the card says, in order, as plain lines (tests read these
    /// through the rendered card; the hedge is always last).
    #[must_use]
    pub fn reading(&self) -> Reading {
        let expr = self.expr();
        let ids = expr.as_ref().map(spdx::ids).unwrap_or_default();
        let (title, place) = match (&self.spdx, &self.file) {
            (None, Some(file)) => ("Custom terms".to_owned(), format!("license-file = \"{file}\"")),
            (None, None) => ("No license".to_owned(), "no license field".to_owned()),
            (Some(spdx), _) => {
                if ids.len() == 1 {
                    let terms = spdx::terms(&ids[0]);
                    (
                        terms.map_or_else(|| ids[0].clone(), |t| t.name.to_owned()),
                        spdx::family(&ids[0]).words().to_owned(),
                    )
                } else {
                    (
                        spdx.to_string(),
                        match &expr {
                            Some(Expr::Or(_)) => format!("your choice of {}", word(ids.len())),
                            _ => "all of them apply".to_owned(),
                        },
                    )
                }
            }
        };
        let fit = if self.own {
            spdx::Fit {
                level: 0,
                line: "Your project's own license: every package in your tree is checked against it.".to_owned(),
                choice: ids.first().cloned().into_iter().collect(),
                assumed: false,
                chose: Vec::new(),
            }
        } else {
            spdx::fit(expr.as_ref(), &self.yours, &self.project)
        };
        let assumed = if self.own { None } else { spdx::assumption(&fit) };
        let tree = match (&self.tree, &expr) {
            (Some(tree), _) if self.own => Some(TreeLine::Summary(spdx::tree_summary(tree))),
            (Some(tree), Some(expr)) => {
                spdx::tree_line(expr, tree, self.package.as_deref()).map(|(line, new)| TreeLine::Line(line, new))
            }
            _ => None,
        };
        Reading {
            title,
            place,
            ids,
            fit,
            assumed,
            tree,
        }
    }
}

/// What a license card says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reading {
    /// The name, or the expression.
    pub title: String,
    /// The family, or "your choice of two".
    pub place: String,
    /// Every license id, one tab each when there are several.
    pub ids: Vec<String>,
    /// Whether it fits.
    pub fit: spdx::Fit,
    /// Which option was assumed, when there was a choice.
    pub assumed: Option<String>,
    /// What it means for your tree.
    pub tree: Option<TreeLine>,
}

/// The card's line about your tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeLine {
    /// The project hero's whole-tree sentence, and what was not read.
    Summary((String, Option<String>)),
    /// A package's one line; `true` when it brings new terms.
    Line(String, bool),
}

/// The license mark (see the module docs).
#[derive(IntoElement)]
pub struct LicenseMark {
    id: ElementId,
    facts: Rc<LicenseFacts>,
    measure: Measure,
    word: bool,
    sheet: Option<u64>,
}

/// The license mark for `facts`, sized for `measure`.
#[must_use]
pub fn license_mark(id: impl Into<ElementId>, facts: LicenseFacts, measure: &Measure) -> LicenseMark {
    LicenseMark {
        id: id.into(),
        facts: Rc::new(facts),
        measure: *measure,
        word: true,
        sheet: None,
    }
}

impl LicenseMark {
    /// The rings alone (rows).
    #[must_use]
    pub const fn glyph_only(mut self) -> Self {
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
}

/// One ring, `size` px, turned `turn` degrees, painted `nudge` px along
/// (a choice's rings part by paint, so nothing beside them moves).
fn ring_element(family: Family, size: f32, ink: Hsla, turn: f32) -> AnyElement {
    nudged_ring(family, size, ink, turn, 0.0)
}

fn nudged_ring(family: Family, size: f32, ink: Hsla, turn: f32, nudge: f32) -> AnyElement {
    canvas(|_, _, _| {}, move |bounds, (), window, _| {
        let bounds = gpui::Bounds::new(gpui::point(bounds.origin.x + px(nudge), bounds.origin.y), bounds.size);
        glyph::ring(family, bounds, ink, turn, window);
    })
    .size(px(size))
    .flex_none()
    .into_any_element()
}

/// The rings of an expression: a choice is a row of rings, an `AND` links
/// them (they overlap). A choice's rings sit `part` px apart at most; at
/// rest (`parted` 0) each is drawn towards the row's middle, so they part
/// on hover without moving anything around them. `fit` rings are drawn in
/// `fit_ink`.
fn rings(expr: Option<&Expr>, size: f32, ink: Hsla, part: f32, turn: f32, parted: f32, fit: Option<(&[String], Hsla)>) -> AnyElement {
    let one = |id: &str, nudge: f32| {
        let family = spdx::family(id);
        let chosen = fit.filter(|(ids, _)| ids.iter().any(|i| i == id)).map(|(_, c)| c);
        let spin = if family == Family::Permissive { turn } else { 0.0 };
        nudged_ring(family, size, chosen.unwrap_or(ink), spin, nudge)
    };
    fn walk(e: &Expr, one: &dyn Fn(&str, f32) -> AnyElement, size: f32, part: f32, parted: f32, nudge: f32) -> AnyElement {
        match e {
            Expr::Id(id) => one(id, nudge),
            Expr::Or(any) => {
                #[allow(clippy::cast_precision_loss)]
                let mid = (any.len() as f32 - 1.0) * 0.5;
                div()
                    .flex()
                    .items_center()
                    .gap(px(part))
                    .children(any.iter().enumerate().map(|(i, e)| {
                        #[allow(clippy::cast_precision_loss)]
                        let towards = (mid - i as f32) * (part * 0.75) * (1.0 - parted);
                        walk(e, one, size, part, parted, nudge + towards)
                    }))
                    .into_any_element()
            }
            Expr::And(all) => div()
                .flex()
                .items_center()
                .children(all.iter().enumerate().map(|(i, e)| {
                    let inner = walk(e, one, size, part, parted, nudge);
                    if i == 0 { inner } else { div().ml(px(-size * 0.25)).child(inner).into_any_element() }
                }))
                .into_any_element(),
        }
    }
    match expr {
        Some(e) => walk(e, &one, size, part, parted, 0.0),
        None => ring_element(Family::Unknown, size, ink, 0.0),
    }
}

impl RenderOnce for LicenseMark {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.facet().palette();
        let measure = self.measure;
        let s = measure.scale();
        let key = ElementId::NamedChild(Arc::new(self.id.clone()), "card".into());
        let live = card::live(&self.id, &key, window, cx);
        let ink = mix(palette.ink2.into(), palette.ink0.into(), live.lit);
        let motion = Motion::scoped(ElementId::NamedChild(Arc::new(self.id.clone()), "motion".into()), cx);
        // Hover turns an open ring a quarter (under the fixed light), and a
        // choice's rings part.
        let turn = motion.animate(
            ElementId::NamedChild(Arc::new(self.id.clone()), "turn".into()),
            if live.lit > 0.5 { -45.0 } else { 0.0 },
            Spec::Spring(SNAPPY),
            window,
            cx,
        );
        let expr = self.facts.expr();
        let mut mark = div()
            .flex()
            .items_center()
            .gap(px(7.0 * s))
            .child(rings(expr.as_ref(), 16.0 * s, ink, 4.0 * s, turn, live.lit, None));
        if self.word {
            mark = mark.child(text(
                ElementId::NamedChild(Arc::new(self.id.clone()), "word".into()),
                self.facts.word(),
                card::WORD,
                &measure,
                ink,
            ));
        }
        let selected = window
            .use_keyed_state(ElementId::NamedChild(Arc::new(self.id.clone()), "tab".into()), cx, |_, _| {
                Rc::new(RefCell::new(None::<String>))
            })
            .read(cx)
            .clone();
        let content = license_card(self.facts.clone(), selected);
        card::door(&self.id, &key, &live, Some(content), self.sheet, None, mark)
    }
}

fn license_card(facts: Rc<LicenseFacts>, selected: Rc<RefCell<Option<String>>>) -> Content {
    Rc::new(move |measure: &Measure, window: &mut Window, cx: &mut App| {
        let palette = cx.facet().palette();
        let reading = facts.reading();
        let expr = facts.expr();
        let fit_ids = reading.fit.choice.clone();
        let chosen = selected.borrow().clone().filter(|id| reading.ids.contains(id)).or_else(|| fit_ids.first().cloned());
        let fit_ink: Hsla = palette.mint.base.into();
        let head = div()
            .flex()
            .items_center()
            .gap(k(measure, 11.0))
            .child(rings(
                expr.as_ref(),
                26.0 * measure.scale(),
                palette.ink1.into(),
                4.0 * measure.scale(),
                0.0,
                1.0,
                (reading.fit.level <= 1).then_some((fit_ids.as_slice(), fit_ink)),
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(1.0 * measure.scale()))
                    .min_w_0()
                    .child(text("mk-lic-title", reading.title.clone(), card::TITLE, measure, palette.ink0))
                    .child(text("mk-lic-place", reading.place.clone(), card::PLACE, measure, palette.ink3)),
            );
        let mut body = card::body(384.0, measure).child(head);
        let listed: Vec<&String> = reading.ids.iter().filter(|id| id.as_str() != spdx::LICENSE_FILE).collect();
        if listed.len() > 1 {
            body = body.child(tabs(&listed, chosen.as_deref(), &selected, measure, palette, window));
        }
        if let Some(id) = chosen.as_ref().filter(|id| id.as_str() != spdx::LICENSE_FILE) {
            body = body.child(columns(id, measure, palette));
        }
        body = body.child(fit(&reading, measure, palette));
        if let Some(assumed) = &reading.assumed {
            body = body.child(div().mt(k(measure, 4.0)).pl(k(measure, 21.0)).child(text(
                "mk-lic-assumed",
                assumed.clone(),
                card::FACT,
                measure,
                palette.ink3,
            )));
        }
        match &reading.tree {
            Some(TreeLine::Summary((line, unread))) => {
                let mut tree = div()
                    .mt(k(measure, 9.0))
                    .flex()
                    .flex_col()
                    .gap(k(measure, 2.0))
                    .child(text("mk-lic-tree-head", "Your tree", card::FACT, measure, palette.ink3))
                    .child(text("mk-lic-tree", line.clone(), card::FACT, measure, palette.ink2));
                if let Some(unread) = unread {
                    tree = tree.child(text("mk-lic-unread", unread.clone(), card::FACT, measure, palette.ink3));
                }
                body = body.child(tree);
            }
            Some(TreeLine::Line(line, new)) => {
                body = body.child(div().mt(k(measure, 9.0)).child(text(
                    "mk-lic-tree",
                    line.clone(),
                    card::FACT,
                    measure,
                    if *new { palette.ink1 } else { palette.ink2 },
                )));
            }
            None => {}
        }
        body = body.child(div().mt(k(measure, 10.0)).child(text("mk-lic-hedge", spdx::HEDGE, card::FOOT, measure, palette.ink4)));
        body.into_any_element()
    })
}

fn tabs(
    ids: &[&String],
    chosen: Option<&str>,
    selected: &Rc<RefCell<Option<String>>>,
    measure: &Measure,
    palette: &'static Palette,
    _window: &mut Window,
) -> AnyElement {
    let mut row = div()
        .mt(k(measure, 11.0))
        .flex()
        .gap(k(measure, 16.0))
        .border_b_1()
        .border_color(palette.line1.hsla());
    for (i, id) in ids.iter().enumerate() {
        let on = chosen == Some(id.as_str());
        let ink: Hsla = if on { palette.ink0.into() } else { palette.ink3.into() };
        let pick = selected.clone();
        let this = (*id).clone();
        let hover_pick = selected.clone();
        let hover_this = (*id).clone();
        let mut tab = div()
            .id(("mk-lic-tab", i))
            .relative()
            .flex()
            .items_center()
            .gap(k(measure, 6.0))
            .pb(k(measure, 7.0))
            .cursor_pointer()
            .child(ring_element(spdx::family(id), 13.0 * measure.scale(), ink, 0.0))
            .child(text(("mk-lic-tab-word", i), spdx::short_id(id), card::HEAD, measure, ink))
            .on_click(move |_, window, _| {
                *pick.borrow_mut() = Some(this.clone());
                window.refresh();
            })
            .on_hover(move |hovered, window, _| {
                if *hovered {
                    *hover_pick.borrow_mut() = Some(hover_this.clone());
                    window.refresh();
                }
            });
        if on {
            tab = tab.child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom(px(-1.0))
                    .h(px(1.5 * measure.scale()))
                    .bg(palette.peri.base.hsla()),
            );
        }
        row = row.child(tab);
    }
    row.into_any_element()
}

fn columns(id: &str, measure: &Measure, palette: &'static Palette) -> AnyElement {
    let terms = spdx::terms(id);
    let (p, c, l): (&[&str], &[&str], &[&str]) = terms.map_or((&[], &[], &[]), |t| (t.permissions, t.conditions, t.limitations));
    let col = |i: usize, head: &'static str, items: &[&str]| {
        let mut col = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .gap(k(measure, 3.0))
            .child(div().mb(k(measure, 3.0)).child(text(("mk-lic-col", i), head, card::HEAD, measure, palette.ink3)));
        if items.is_empty() {
            col = col.child(text(("mk-lic-none", i), "none", card::FACT, measure, palette.ink4));
        }
        for (j, item) in items.iter().enumerate() {
            col = col.child(text(("mk-lic-item", i * 16 + j), *item, card::FACT, measure, palette.ink1));
        }
        col
    };
    div()
        .mt(k(measure, 11.0))
        .flex()
        .gap(k(measure, 14.0))
        .child(col(0, "Permissions", p))
        .child(col(1, "Conditions", c))
        .child(col(2, "Limitations", l))
        .into_any_element()
}

fn fit(reading: &Reading, measure: &Measure, palette: &'static Palette) -> AnyElement {
    let ink: Hsla = match reading.fit.level {
        0 | 1 => palette.mint.base.into(),
        2 => palette.ink2.into(),
        _ => palette.ink3.into(),
    };
    div()
        .mt(k(measure, 12.0))
        .pt(k(measure, 10.0))
        .border_t_1()
        .border_color(palette.line1.hsla())
        .flex()
        .items_start()
        .gap(k(measure, 9.0))
        .child(div().mt(k(measure, 4.0)).child(ring_element(Family::Permissive, 12.0 * measure.scale(), ink, 0.0)))
        .child(div().flex_1().min_w_0().child(text("mk-lic-fit", reading.fit.line.clone(), card::SAY, measure, palette.ink1)))
        .into_any_element()
}

/// The card's content on its own (boards show it in place).
#[cfg(feature = "gallery")]
pub(crate) fn board_card(facts: &LicenseFacts) -> Content {
    license_card(Rc::new(facts.clone()), Rc::new(RefCell::new(None)))
}

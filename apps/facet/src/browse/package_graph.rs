//! Package detail and direct dependency graph, grounded in exact source facts.

use super::view::{KeyboardReveal, child, words, words_ellipsis};
use crate::controls::button::button;
use crate::icons::Icon;
use crate::measure::{Control, Measure, Space};
use crate::theme::ActiveFacet;
use crate::tokens::ty;
#[cfg(test)]
use gpui::Entity;
use gpui::{
    App, ElementId, FocusHandle, InteractiveElement, IntoElement, ParentElement, RenderOnce,
    ScrollHandle, SharedString, Styled, Window, div,
};
use std::rc::Rc;

/// Direction around the exact package coordinate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    /// Packages declared by this release.
    Dependencies,
    /// Packages that declare a dependency on this coordinate.
    Dependents,
}

/// A release tick with its exact package URL and standing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Release {
    pub version: SharedString,
    pub coordinate: SharedString,
    pub standing: SharedString,
    pub current: bool,
}

/// One exact authority choice for an ambiguous coordinate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorityChoice {
    pub authority: SharedString,
    pub coordinate: SharedString,
    pub display: SharedString,
}

/// Fact state kept distinct through the view.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Knowledge {
    Known,
    Unknown(Option<SharedString>),
    Unavailable(SharedString),
    Partial {
        reason: SharedString,
        unavailable: bool,
    },
    Ambiguous(Vec<AuthorityChoice>),
}

/// One dependency or dependent row with its recorded evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Edge {
    pub identity: SharedString,
    pub label: SharedString,
    pub exact_package: Option<SharedString>,
    pub requirement: SharedString,
    pub scope: SharedString,
    pub optional: bool,
    pub source_authority: SharedString,
    pub authority_kind: SharedString,
    pub frontier: SharedString,
    pub provenance: SharedString,
}

/// The immutable package page prepared off the render path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Model {
    pub coordinate: SharedString,
    pub name: SharedString,
    pub version: Option<SharedString>,
    pub ecosystem: Option<SharedString>,
    pub description: Option<SharedString>,
    pub registry_facts: Vec<(SharedString, SharedString)>,
    pub releases: Vec<Release>,
    pub direction: Direction,
    pub view_root: SharedString,
    pub facts_witness: SharedString,
    pub catalog_snapshot: Option<SharedString>,
    pub selected_authority: Option<SharedString>,
    pub knowledge: Knowledge,
    pub edges: Vec<Edge>,
    pub next: Option<SharedString>,
}

/// Navigation and package-opening actions owned by the desktop shell.
#[derive(Clone)]
pub struct Actions {
    pub active: bool,
    pub scroll: ScrollHandle,
    pub dependencies: Rc<dyn Fn(&mut Window, &mut App)>,
    pub dependents: Rc<dyn Fn(&mut Window, &mut App)>,
    pub choose_authority: Rc<dyn Fn(SharedString, &mut Window, &mut App)>,
    pub next_page: Rc<dyn Fn(SharedString, &mut Window, &mut App)>,
    pub open_package: Rc<dyn Fn(SharedString, &mut Window, &mut App)>,
}

struct State {
    selected: Option<usize>,
    focus: FocusHandle,
    focus_claimed: bool,
    reveal: KeyboardReveal,
}

impl State {
    fn new(scroll: ScrollHandle, cx: &mut gpui::Context<Self>) -> Self {
        Self {
            selected: None,
            focus: cx.focus_handle(),
            focus_claimed: false,
            reveal: KeyboardReveal::new(scroll),
        }
    }
}

/// Build the native package detail and graph view.
#[must_use]
pub fn package_graph(
    id: impl Into<ElementId>,
    model: Model,
    actions: Actions,
    measure: &Measure,
) -> PackageGraph {
    PackageGraph {
        id: id.into(),
        model,
        actions,
        measure: *measure,
        #[cfg(test)]
        test_state: None,
    }
}

#[derive(IntoElement)]
pub struct PackageGraph {
    id: ElementId,
    model: Model,
    actions: Actions,
    measure: Measure,
    #[cfg(test)]
    test_state: Option<Entity<State>>,
}

impl RenderOnce for PackageGraph {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let m = self.measure;
        let p = cx.palette();
        #[cfg(test)]
        let state = self.test_state.unwrap_or_else(|| {
            window.use_keyed_state(child(&self.id, "state"), cx, |_, cx| {
                State::new(self.actions.scroll.clone(), cx)
            })
        });
        #[cfg(not(test))]
        let state = window.use_keyed_state(child(&self.id, "state"), cx, |_, cx| {
            State::new(self.actions.scroll.clone(), cx)
        });
        let claim = state.update(cx, |state, _| {
            if !self.actions.active {
                state.focus_claimed = false;
                None
            } else if state.focus_claimed {
                None
            } else {
                state.focus_claimed = true;
                Some(state.focus.clone())
            }
        });
        if let Some(focus) = claim {
            window.focus(&focus, cx);
        }

        let mut page = div()
            .id(self.id.clone())
            .key_context("PackageGraph")
            .track_focus(&state.read(cx).focus)
            .flex()
            .flex_col()
            .w(m.width())
            .gap(m.space(Space::Gutter))
            .child(words(
                child(&self.id, "eyebrow"),
                "PACKAGE · EXACT RELEASE",
                ty::CAPTION,
                p.ink1,
                &m,
            ))
            .child(words_ellipsis(
                child(&self.id, "title"),
                self.model.name.clone(),
                ty::TITLE,
                p.ink0,
                &m,
            ))
            .child(words(
                child(&self.id, "coordinate"),
                self.model.coordinate.clone(),
                ty::MONO_ROW,
                p.ink2,
                &m,
            ));

        if let Some(description) = &self.model.description {
            page = page.child(words(
                child(&self.id, "description"),
                description.clone(),
                ty::LEDE,
                p.ink2,
                &m,
            ));
        }

        let mut facts = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(m.space(Space::Roomy));
        if let Some(ecosystem) = &self.model.ecosystem {
            facts = facts.child(words(
                child(&self.id, "ecosystem"),
                format!("Ecosystem · {ecosystem}"),
                ty::SMALL,
                p.ink1,
                &m,
            ));
        }
        if let Some(version) = &self.model.version {
            facts = facts.child(words(
                child(&self.id, "version"),
                format!("Release · {version}"),
                ty::SMALL,
                p.ink1,
                &m,
            ));
        }
        for (at, (label, value)) in self.model.registry_facts.iter().enumerate() {
            facts = facts.child(words(
                child(&self.id, format!("fact-{at}")),
                format!("{} · {}", label, value),
                ty::SMALL,
                p.ink1,
                &m,
            ));
        }
        page = page.child(facts);

        if let Some(authority) = self.model.selected_authority {
            page = page.child(words(
                child(&self.id, "selected-authority"),
                format!("Selected exact source authority · {authority}"),
                ty::MONO_SMALL,
                p.ink1,
                &m,
            ));
        }

        if !self.model.releases.is_empty() {
            let mut releases = div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(m.space(Space::Tight));
            for (at, release) in self.model.releases.iter().take(12).enumerate() {
                let text = if release.current {
                    format!("{} · current · {}", release.version, release.standing)
                } else {
                    format!("{} · {}", release.version, release.standing)
                };
                releases = releases.child(words(
                    child(&self.id, format!("release-{at}")),
                    text,
                    ty::CAPTION,
                    if release.current { p.ink0 } else { p.ink2 },
                    &m,
                ));
            }
            page = page.child(releases);
        }
        page = page.child(words(
            child(&self.id, "scope"),
            "Versions stay scoped to this ecosystem and exact package coordinate; no cross-registry identity is inferred.",
            ty::CAPTION,
            p.ink2,
            &m,
        ));

        let dependency = Rc::clone(&self.actions.dependencies);
        let dependent = Rc::clone(&self.actions.dependents);
        let direction = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(m.space(Space::Base))
            .child(
                button(child(&self.id, "dependencies"), "Dependencies", &m)
                    .ghost()
                    .size(Control::Small)
                    .icon(Icon::Trail)
                    .disabled(self.model.direction == Direction::Dependencies)
                    .on_click(move |window, cx| dependency(window, cx)),
            )
            .child(
                button(child(&self.id, "dependents"), "Dependents", &m)
                    .ghost()
                    .size(Control::Small)
                    .icon(Icon::Users)
                    .disabled(self.model.direction == Direction::Dependents)
                    .on_click(move |window, cx| dependent(window, cx)),
            );
        let section_title = match self.model.direction {
            Direction::Dependencies => "Dependencies",
            Direction::Dependents => "Used by",
        };
        page = page.child(direction).child(words(
            child(&self.id, "graph-title"),
            section_title,
            ty::HEAD,
            p.ink0,
            &m,
        ));

        match &self.model.knowledge {
            Knowledge::Known if self.model.edges.is_empty() => {
                let empty = match self.model.direction {
                    Direction::Dependencies => {
                        "This authority records no dependencies for this release."
                    }
                    Direction::Dependents => {
                        "No matching dependents were recorded in this graph snapshot."
                    }
                };
                page = page.child(words(
                    child(&self.id, "known-empty"),
                    empty,
                    ty::LEDE,
                    p.ink2,
                    &m,
                ));
            }
            Knowledge::Known => {
                page = page.child(words(
                    child(&self.id, "graph-count"),
                    format!(
                        "{} recorded edges · known for this snapshot",
                        self.model.edges.len()
                    ),
                    ty::CAPTION,
                    p.ink2,
                    &m,
                ));
            }
            Knowledge::Unknown(reason) => {
                page = page.child(words(
                    child(&self.id, "unknown"),
                    reason.as_ref().map_or_else(
                        || "Dependency facts are unknown for this coordinate.".to_owned(),
                        |reason| format!("Dependency facts are unknown · {reason}"),
                    ),
                    ty::LEDE,
                    p.ink2,
                    &m,
                ));
            }
            Knowledge::Unavailable(reason) => {
                page = page.child(words(
                    child(&self.id, "unavailable"),
                    format!("The source could not provide dependency facts · {reason}"),
                    ty::LEDE,
                    p.ink2,
                    &m,
                ));
            }
            Knowledge::Partial {
                reason,
                unavailable,
            } => {
                page = page.child(words(
                    child(&self.id, "partial"),
                    format!(
                        "{} · {}",
                        if *unavailable {
                            "Some sources could not provide dependency facts; showing recorded matches"
                        } else {
                            "Some source facts are unknown; showing recorded matches"
                        },
                        reason
                    ),
                    ty::LEDE,
                    p.ink2,
                    &m,
                ));
            }
            Knowledge::Ambiguous(choices) => {
                page = page.child(words(
                    child(&self.id, "ambiguous"),
                    "Several independent authorities publish this coordinate. Choose one before its edges are shown.",
                    ty::LEDE,
                    p.ink2,
                    &m,
                ));
                for (at, choice) in choices.iter().enumerate() {
                    let choose = Rc::clone(&self.actions.choose_authority);
                    let authority = choice.authority.clone();
                    page = page.child(
                        button(
                            child(&self.id, format!("authority-{at}")),
                            choice.display.clone(),
                            &m,
                        )
                        .ghost()
                        .size(Control::Small)
                        .icon(Icon::Seal)
                        .on_click(move |window, cx| choose(authority.clone(), window, cx)),
                    );
                }
            }
        }

        let selected = state.read(cx).selected;
        for (at, edge) in self.model.edges.iter().enumerate() {
            let row_id = child(&self.id, format!("edge-{at}"));
            let highlighted = selected == Some(at);
            let mut row = div()
                .id(row_id.clone())
                .flex()
                .flex_col()
                .gap(m.space(Space::Tight))
                .py(m.space(Space::Snug));
            let target = edge.exact_package.clone();
            if let Some(target) = target {
                let open = Rc::clone(&self.actions.open_package);
                row = row.child(
                    button(child(&row_id, "target"), edge.label.clone(), &m)
                        .ghost()
                        .size(Control::Small)
                        .icon(Icon::Package)
                        .on_click(move |window, cx| open(target.clone(), window, cx)),
                );
            } else {
                row = row.child(words(
                    child(&row_id, "target"),
                    edge.label.clone(),
                    ty::MONO_ROW,
                    if highlighted { p.ink0 } else { p.ink1 },
                    &m,
                ));
            }
            row = row
                .child(words(
                    child(&row_id, "requirement"),
                    format!(
                        "{} · {}{}",
                        edge.requirement,
                        edge.scope,
                        if edge.optional { " · optional" } else { "" }
                    ),
                    ty::SMALL,
                    p.ink2,
                    &m,
                ))
                .child(words(
                    child(&row_id, "authority"),
                    format!("Exact source authority · {}", edge.source_authority),
                    ty::MONO_SMALL,
                    p.ink1,
                    &m,
                ))
                .child(words(
                    child(&row_id, "evidence"),
                    format!(
                        "Evidence · {} · frontier {} · fact {}",
                        edge.authority_kind, edge.frontier, edge.provenance
                    ),
                    ty::CAPTION,
                    p.ink2,
                    &m,
                ));
            let reveal = state.read(cx).reveal.clone();
            let painted = if highlighted {
                reveal.selected(row)
            } else {
                row.into_any_element()
            };
            page = page.child(painted);
        }

        if let Some(cursor) = &self.model.next {
            let cursor = cursor.clone();
            let next = Rc::clone(&self.actions.next_page);
            page = page.child(
                button(child(&self.id, "more"), "More graph rows", &m)
                    .ghost()
                    .size(Control::Small)
                    .icon(Icon::More)
                    .on_click(move |window, cx| next(cursor.clone(), window, cx)),
            );
        }
        page = page.child(words(
            child(&self.id, "snapshot"),
            format!(
                "Graph snapshot · root {} · dependency facts {}{}",
                self.model.view_root,
                self.model.facts_witness,
                self.model
                    .catalog_snapshot
                    .as_ref()
                    .map_or_else(String::new, |snapshot| format!(" · catalog {}", snapshot)),
            ),
            ty::CAPTION,
            p.ink2,
            &m,
        ));
        let count = self.model.edges.len();
        let key_state = state.clone();
        let dependencies = Rc::clone(&self.actions.dependencies);
        let dependents = Rc::clone(&self.actions.dependents);
        let open_package = Rc::clone(&self.actions.open_package);
        let keys = self
            .model
            .edges
            .iter()
            .map(|edge| edge.exact_package.clone())
            .collect::<Vec<_>>();
        page = page.on_key_down(move |event, window, cx| {
            match event.keystroke.key.as_str() {
                "left" => dependencies(window, cx),
                "right" => dependents(window, cx),
                "down" | "j" => {
                    key_state.update(cx, |state, cx| {
                        let next = state
                            .selected
                            .map_or(0, |at| at.saturating_add(1).min(count.saturating_sub(1)));
                        if count > 0 && state.selected != Some(next) {
                            state.selected = Some(next);
                            state.reveal.request();
                        }
                        cx.notify();
                    });
                }
                "up" | "k" => {
                    key_state.update(cx, |state, cx| {
                        let next = state
                            .selected
                            .map_or(count.saturating_sub(1), |at| at.saturating_sub(1));
                        if count > 0 && state.selected != Some(next) {
                            state.selected = Some(next);
                            state.reveal.request();
                        }
                        cx.notify();
                    });
                }
                "enter" => {
                    if let Some(Some(package)) = key_state
                        .read(cx)
                        .selected
                        .and_then(|at| keys.get(at).cloned())
                    {
                        open_package(package, window, cx);
                    }
                }
                _ => return,
            }
            cx.stop_propagation();
        });
        page.into_any_element()
    }
}

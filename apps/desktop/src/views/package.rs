//! One registry package, as a document: what it is, how it is used, who uses it.
//! Seven sections, each stating whether it is a recorded fact or a labelled sample.
//! Everything that names a package is a door; everything else is prose.
//!
//! The design decision this page embodies is that a package page is a piece of
//! writing with a shape, not a form with fields. A reader arrives with one of
//! four questions — what is this, how do I use it, what does it drag in, is it
//! alive — and the page answers them in that order: the header states the
//! identity and the one action worth taking, the README teaches, dependencies
//! and dependents say what it is wired to, and usage says whether anyone is
//! still there. A section that has not been read yet reserves its geometry so
//! the page never reflows under the eye, and a section whose fact nobody
//! publishes yet carries a sample wearing the word `sample` — because an empty
//! box teaches a reader nothing, and an unlabelled plausible number teaches
//! them something false.
//!
//! Nothing here is scraped and nothing is guessed. The versions, the reverse
//! dependencies, and the release count come from the surface commands the CLI
//! runs, in the same order and with the same absences; the rest comes from
//! [`crate::store::dossier`], which is honest about being a stand-in.

use super::home::{head_and_body, skeleton};
use super::workspace::Workspace;
use crate::motion::{Beat, entering_opacity, once};
use crate::store::document::Target;
use crate::store::dossier::{
    self, Dependency, Dependent, Dossier, Downloads, History, Link, Owner, Precis, Provenance, Rank,
    ReadmeBlock, Release, Reverse, Section, Stamp, Standing as ReleaseStanding,
};
use crate::store::registry::{self, Loadable, Standing};
use crate::theme::Theme;
use crate::theme::language::hue as language_hue;
use crate::theme::ramp::Hue;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Radius, Space, TypeScale, hairline, radius, space, type_size};
use crate::ui::icon::Icon;
use crate::ui::{button, chart, chip, fault as fault_ui, icon, surface, text};
use backend_library::RegistryEcosystem;
use backend_present::Language;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnimationExt as _, AnyElement, Context, Div, ElementId, FontWeight, InteractiveElement,
    IntoElement, ParentElement, SharedString, StatefulInteractiveElement, Styled, div, px,
};

/// How many rows a long section lists before it offers the rest.
const BUDGET: usize = 12;

/// Height of the usage chart, in pixels.
const CHART: u16 = 84;

/// How many releases the adoption breakdown names.
const ADOPTED: usize = 6;

/// The decay an adoption breakdown assumes, newest release first.
///
/// This is a stated assumption rather than a measurement: no surface command
/// publishes per-version downloads, so the breakdown spreads the sampled
/// series over the newest releases along this curve. It is drawn under the
/// same `sample` tag as the series it divides.
const DECAY: [u64; ADOPTED] = [40, 24, 14, 9, 7, 6];

impl Workspace {
    /// Returns the registry page for one pinned package coordinate.
    pub(super) fn package_page(
        &mut self,
        theme: &Theme,
        coordinate: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.registry
            .update(cx, |registry, cx| registry.read_package(coordinate, cx));
        let state = self.registry.read(cx).package_for(coordinate).cloned();
        if let Some(Loadable::Faulted(fault)) = &state {
            let actions = Self::affordances(theme, "package", fault, coordinate, cx);
            return fault_ui::block(theme, fault, actions).into_any_element();
        }
        let dossier = self
            .registry
            .read(cx)
            .dossier_for(coordinate)
            .cloned()
            .unwrap_or_else(|| dossier::sample(coordinate));
        let reduced = theme.reduced_motion();
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(space(Space::Gutter))
            .child(self.package_header(theme, &dossier, cx))
            .child(self.readme_section(theme, &dossier, cx))
            .child(self.dependencies_section(theme, &dossier, cx))
            .child(self.dependents_section(theme, &dossier, cx))
            .child(self.versions_section(theme, &dossier, cx))
            .child(self.usage_section(theme, &dossier, cx))
            .child(self.owners_section(theme, &dossier, cx))
            .with_animation(
                ElementId::Name(SharedString::from(format!("package-{coordinate}"))),
                once(Beat::Reveal, reduced),
                |page, delta| page.opacity(entering_opacity(delta)),
            )
            .into_any_element()
    }
}

/// The header.
impl Workspace {
    fn package_header(
        &mut self,
        theme: &Theme,
        dossier: &Dossier,
        cx: &mut Context<Self>,
    ) -> Div {
        let coordinate = dossier.coordinate().to_owned();
        let held = self.jobs.read(cx).merge(self.engine.read(cx).shelf());
        let standing = registry::add_standing(&held, &coordinate);
        let precis = dossier.precis().ready();
        let unfurled = self.is_unfurled(&picker_key(&coordinate));
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(space(Space::Snug))
            .child(
                text::single_line(text::identity_text(theme, TypeScale::Small))
                    .child(coordinate.clone()),
            )
            .child(Self::title_row(theme, dossier, unfurled, cx))
            .when(unfurled, |header| {
                header.child(Self::version_menu(theme, dossier, cx))
            })
            .when_some(precis, |header, precis| {
                header.child(Self::precis_block(theme, dossier.precis(), precis))
            })
            .child(Self::meta_row(theme, dossier))
            .when_some(precis, |header, precis| {
                header.child(Self::link_row(theme, precis, dossier.precis().provenance(), cx))
            })
            .child(Self::action_row(
                theme,
                &coordinate,
                &dossier.spelling().label(),
                standing,
                cx,
            ))
    }

    /// Returns the title line: logo, name, version picker, and yanked mark.
    fn title_row(
        theme: &Theme,
        dossier: &Dossier,
        unfurled: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let spelling = dossier.spelling();
        let yanked = dossier
            .pinned()
            .is_some_and(|release| release.standing() == ReleaseStanding::Yanked);
        div()
            .flex()
            .items_center()
            .flex_wrap()
            .gap(space(Space::Snug))
            .when_some(dossier.ecosystem(), |row, ecosystem| {
                row.children(super::browse::ecosystem_logo(theme, ecosystem, 22.0))
            })
            .child(text::heading(theme, TypeScale::Title).child(spelling.name().to_owned()))
            .when(!spelling.version().is_empty(), |row| {
                row.child(Self::version_button(theme, dossier, unfurled, cx))
            })
            .when_some(dossier.history().ready(), |row, history| {
                row.child(chip::count_chip(theme, history.versions(), "versions"))
            })
            .when(yanked, |row| {
                row.child(
                    div()
                        .flex_none()
                        .px(px(5.0))
                        .py(px(1.0))
                        .rounded(radius(Radius::Hair))
                        .bg(theme.paint(Paint::Fault))
                        .text_size(type_size(TypeScale::Micro))
                        .text_color(theme.paint(Paint::Ground))
                        .child("yanked"),
                )
            })
    }

    /// Returns the version badge, which opens the picker when pressed.
    fn version_button(
        theme: &Theme,
        dossier: &Dossier,
        unfurled: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = picker_key(dossier.coordinate());
        let count = dossier
            .releases()
            .ready()
            .map_or(0, Vec::len);
        div()
            .id(ElementId::Name(SharedString::from("package-version-badge")))
            .flex()
            .flex_none()
            .items_center()
            .gap(space(Space::Tight))
            .px(space(Space::Snug))
            .py(px(2.0))
            .rounded(radius(Radius::Small))
            .bg(theme.paint(Paint::Hover))
            .border(hairline())
            .border_color(theme.paint(if unfurled {
                Paint::GiltDim
            } else {
                Paint::Hairline
            }))
            .cursor_pointer()
            .hover(|style| style.bg(theme.paint(Paint::Selected)))
            .child(
                text::text_at(theme, TypeScale::Small, Paint::Text)
                    .font_family(theme.specimen())
                    .child(dossier.spelling().version().to_owned()),
            )
            .child(
                text::faint(theme).child(format!("of {count}")),
            )
            .child(icon::sized(
                theme,
                if unfurled {
                    Icon::ChevronDown
                } else {
                    Icon::ChevronRight
                },
                11.0,
                Paint::TextFaint,
            ))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_unfurl(&key, cx);
            }))
            .into_any_element()
    }

    /// Returns the version picker: every release, newest first.
    fn version_menu(theme: &Theme, dossier: &Dossier, cx: &mut Context<Self>) -> Div {
        let current = dossier.spelling().version().to_owned();
        let releases = dossier.releases().ready().cloned().unwrap_or_default();
        surface::sunken(theme)
            .w_full()
            .max_h(px(260.0))
            .overflow_hidden()
            .p(space(Space::Snug))
            .flex()
            .flex_col()
            .gap(px(1.0))
            .children(releases.iter().enumerate().map(|(at, release)| {
                version_row(theme, "picker", at, release, release.version() == current, cx)
            }))
    }

    /// Returns the description, the keywords, and the sample tag over both.
    fn precis_block(theme: &Theme, section: &Section<Precis>, precis: &Precis) -> Div {
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(space(Space::Snug))
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(space(Space::Snug))
                    .child(
                        text::text_at(theme, TypeScale::Body, Paint::Text)
                            .flex_1()
                            .child(precis.description().to_owned()),
                    )
                    .when(section.provenance().is_sample(), |row| {
                        row.child(chart::sample_tag(theme))
                    }),
            )
            .when(!precis.keywords().is_empty(), |block| {
                block.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(space(Space::Tight))
                        .children(precis.keywords().iter().map(|keyword| {
                            chip::badge(theme, keyword)
                        })),
                )
            })
    }

    /// Returns the one dense line of facts under the description.
    fn meta_row(theme: &Theme, dossier: &Dossier) -> Div {
        let mut facts: Vec<String> = Vec::new();
        if let Some(license) = dossier.precis().ready().and_then(Precis::license) {
            facts.push(license.to_owned());
        }
        if let Some(release) = dossier.pinned() {
            facts.push(release.size());
            facts.push(
                release
                    .published()
                    .map_or_else(|| "publication date not recorded".to_owned(), |stamp| {
                        format!("published {}", stamp.spelled())
                    }),
            );
        }
        if let Some(downloads) = dossier.downloads().ready() {
            facts.push(format!(
                "{} downloads",
                dossier::tally_label(downloads.total())
            ));
        }
        if let Some(latest) = dossier
            .history()
            .ready()
            .and_then(History::latest)
            .filter(|latest| *latest != dossier.spelling().version())
        {
            facts.push(format!("newest is {latest}"));
        }
        text::faint(theme).child(facts.join(" · "))
    }

    /// Returns the external link buttons a package publishes about itself.
    ///
    /// A sampled link is copied rather than opened. The address is plausible
    /// and unverified, and sending a reader's browser somewhere on the strength
    /// of a stand-in is the one dishonesty a label cannot excuse; when the feed
    /// starts publishing links the same buttons open them.
    fn link_row(
        theme: &Theme,
        precis: &Precis,
        provenance: Provenance,
        cx: &mut Context<Self>,
    ) -> Div {
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(space(Space::Snug))
            .children(
                precis
                    .links()
                    .iter()
                    .enumerate()
                    .map(|(at, link)| Self::link_button(theme, at, link, provenance, cx)),
            )
            .when(provenance.is_sample(), |row| {
                row.child(chart::sample_tag(theme))
            })
    }

    fn link_button(
        theme: &Theme,
        at: usize,
        link: &Link,
        provenance: Provenance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let url = link.url().to_owned();
        let sampled = provenance.is_sample();
        button::button(
            theme,
            format!("package-link-{at}"),
            link.kind().label(),
            button::Weight::Regular,
        )
        .child(icon::sized(theme, Icon::External, 11.0, Paint::TextFaint))
        .on_click(cx.listener(move |this, _, _, cx| {
            if sampled {
                this.copy("Sample link copied", url.clone(), cx);
            } else {
                cx.open_url(&url);
            }
        }))
        .into_any_element()
    }

    fn action_row(
        theme: &Theme,
        coordinate: &str,
        label: &str,
        standing: Standing,
        cx: &mut Context<Self>,
    ) -> Div {
        let indexed = coordinate.to_owned();
        let opened = coordinate.to_owned();
        let copied = coordinate.to_owned();
        let notice = format!("{label} copied");
        div()
            .flex()
            .flex_wrap()
            .gap(space(Space::Snug))
            .when(standing.is_actionable(), |row| {
                row.child(
                    button::button(theme, "package-add", "Add to shelf", button::Weight::Primary)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.index_project(indexed.clone(), cx);
                        })),
                )
            })
            .when(standing == Standing::OnShelf, |row| {
                row.child(
                    button::button(
                        theme,
                        "package-open",
                        "Open on shelf",
                        button::Weight::Primary,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_project(opened.clone(), cx);
                    })),
                )
            })
            .when(standing == Standing::Adding, |row| {
                row.child(
                    text::dim(theme)
                        .text_color(theme.paint(Paint::Caution))
                        .child(standing.label()),
                )
            })
            .child(
                button::button(
                    theme,
                    "package-copy",
                    "Copy coordinate",
                    button::Weight::Quiet,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.copy(&notice, copied.clone(), cx);
                })),
            )
    }
}

/// The README.
impl Workspace {
    fn readme_section(
        &self,
        theme: &Theme,
        dossier: &Dossier,
        cx: &mut Context<Self>,
    ) -> Div {
        let section = dossier.readme();
        let body = match section.state() {
            Loadable::Idle | Loadable::Loading => reserved_lines(theme).into_any_element(),
            Loadable::Faulted(fault) => {
                fault_ui::block(theme, fault, Vec::new()).into_any_element()
            }
            Loadable::Ready(blocks) if blocks.is_empty() => text::dim(theme)
                .child("This package ships no README.")
                .into_any_element(),
            Loadable::Ready(blocks) => readme_blocks(theme, blocks).into_any_element(),
        };
        self.folding(theme, "readme", "Readme", section.provenance(), None, body, cx)
    }
}

/// Dependencies, dependents, versions, usage, owners.
impl Workspace {
    fn dependencies_section(
        &self,
        theme: &Theme,
        dossier: &Dossier,
        cx: &mut Context<Self>,
    ) -> Div {
        let section = dossier.dependencies();
        let note = section
            .ready()
            .map(|rows| format!("{} declared", rows.len()));
        let body = match section.state() {
            Loadable::Idle | Loadable::Loading => reserved_lines(theme).into_any_element(),
            Loadable::Faulted(fault) => {
                fault_ui::block(theme, fault, Vec::new()).into_any_element()
            }
            Loadable::Ready(rows) if rows.is_empty() => text::dim(theme)
                .child("This package declares no dependencies.")
                .into_any_element(),
            Loadable::Ready(rows) => Self::dependency_list(theme, rows, cx),
        };
        self.folding(
            theme,
            "dependencies",
            "Dependencies",
            section.provenance(),
            note,
            body,
            cx,
        )
    }

    fn dependency_list(
        theme: &Theme,
        rows: &[Dependency],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap(px(1.0))
            .children(
                rows.iter()
                    .enumerate()
                    .map(|(at, row)| dependency_row(theme, at, row, cx)),
            )
            .into_any_element()
    }

    fn dependents_section(
        &self,
        theme: &Theme,
        dossier: &Dossier,
        cx: &mut Context<Self>,
    ) -> Div {
        let section = dossier.dependents();
        let unfurled = self.is_unfurled("package-dependents");
        let body = match section.state() {
            Loadable::Idle | Loadable::Loading => reserved_lines(theme).into_any_element(),
            Loadable::Faulted(fault) => {
                fault_ui::block(theme, fault, Vec::new()).into_any_element()
            }
            Loadable::Ready(Reverse::NotRecorded(reason)) => text::dim(theme)
                .child(format!(
                    "The configured feed does not publish reverse dependencies: {reason}."
                ))
                .into_any_element(),
            Loadable::Ready(Reverse::Recorded(rows)) if rows.is_empty() => text::dim(theme)
                .child("The feed publishes reverse dependencies and records none here.")
                .into_any_element(),
            Loadable::Ready(Reverse::Recorded(rows)) => {
                Self::dependent_list(theme, rows, unfurled, cx)
            }
        };
        let note = match section.state() {
            Loadable::Ready(Reverse::Recorded(rows)) => Some(format!("{} recorded", rows.len())),
            _ => None,
        };
        self.folding(
            theme,
            "dependents",
            "Dependents",
            section.provenance(),
            note,
            body,
            cx,
        )
    }

    fn dependent_list(
        theme: &Theme,
        rows: &[Dependent],
        unfurled: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let shown = if unfurled { rows.len() } else { rows.len().min(BUDGET) };
        let rest = rows.len().saturating_sub(shown);
        div()
            .flex()
            .flex_col()
            .gap(px(1.0))
            .children(
                rows.iter()
                    .take(shown)
                    .enumerate()
                    .map(|(at, row)| dependent_row(theme, at, row, cx)),
            )
            .when(rest > 0, |list| {
                list.child(
                    button::button(
                        theme,
                        "package-dependents-more",
                        &format!("Show {rest} more"),
                        button::Weight::Quiet,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.toggle_unfurl("package-dependents", cx);
                    })),
                )
            })
            .into_any_element()
    }

    fn versions_section(
        &self,
        theme: &Theme,
        dossier: &Dossier,
        cx: &mut Context<Self>,
    ) -> Div {
        let section = dossier.releases();
        let current = dossier.spelling().version().to_owned();
        let unfurled = self.is_unfurled("package-versions-list");
        let note = section.ready().map(|rows| format!("{} recorded", rows.len()));
        let body = match section.state() {
            Loadable::Idle | Loadable::Loading => reserved_lines(theme).into_any_element(),
            Loadable::Faulted(fault) => {
                fault_ui::block(theme, fault, Vec::new()).into_any_element()
            }
            Loadable::Ready(rows) if rows.is_empty() => text::dim(theme)
                .child("The local index recorded no version under this package name.")
                .into_any_element(),
            Loadable::Ready(rows) => Self::version_list(theme, rows, &current, unfurled, cx),
        };
        self.folding(
            theme,
            "versions",
            "Versions",
            section.provenance(),
            note,
            body,
            cx,
        )
    }

    fn version_list(
        theme: &Theme,
        rows: &[Release],
        current: &str,
        unfurled: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let shown = if unfurled { rows.len() } else { rows.len().min(BUDGET) };
        let rest = rows.len().saturating_sub(shown);
        div()
            .flex()
            .flex_col()
            .gap(px(1.0))
            .children(rows.iter().take(shown).enumerate().map(|(at, release)| {
                version_row(theme, "list", at, release, release.version() == current, cx)
            }))
            .when(rest > 0, |list| {
                list.child(
                    button::button(
                        theme,
                        "package-versions-more",
                        &format!("Show {rest} more"),
                        button::Weight::Quiet,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.toggle_unfurl("package-versions-list", cx);
                    })),
                )
            })
            .into_any_element()
    }

    fn usage_section(
        &self,
        theme: &Theme,
        dossier: &Dossier,
        cx: &mut Context<Self>,
    ) -> Div {
        let section = dossier.downloads();
        let body = match section.state() {
            Loadable::Idle | Loadable::Loading => reserved_lines(theme).into_any_element(),
            Loadable::Faulted(fault) => {
                fault_ui::block(theme, fault, Vec::new()).into_any_element()
            }
            Loadable::Ready(downloads) => usage_body(theme, dossier, downloads),
        };
        self.folding(theme, "usage", "Usage", section.provenance(), None, body, cx)
    }

    fn owners_section(
        &self,
        theme: &Theme,
        dossier: &Dossier,
        cx: &mut Context<Self>,
    ) -> Div {
        let section = dossier.owners();
        let body = match section.state() {
            Loadable::Idle | Loadable::Loading => reserved_lines(theme).into_any_element(),
            Loadable::Faulted(fault) => {
                fault_ui::block(theme, fault, Vec::new()).into_any_element()
            }
            Loadable::Ready(owners) if owners.is_empty() => text::dim(theme)
                .child("The feed records no publisher for this package.")
                .into_any_element(),
            Loadable::Ready(owners) => owner_row(theme, owners).into_any_element(),
        };
        self.folding(theme, "owners", "Owners", section.provenance(), None, body, cx)
    }

    /// Returns one page section with a fold control and a provenance tag.
    ///
    /// The head is the same shape for all seven sections, which is what lets a
    /// reader learn the page once: a chevron, a title, what the section holds,
    /// and — when it applies — the one word that says the contents are a
    /// stand-in. The explaining sentence sits inside the body rather than in
    /// the head, so a folded page is a clean table of contents.
    #[allow(
        clippy::too_many_arguments,
        reason = "a section head is one row of independent parts, each of which the caller owns"
    )]
    fn folding(
        &self,
        theme: &Theme,
        key: &'static str,
        title: &str,
        provenance: Provenance,
        note: Option<String>,
        body: AnyElement,
        cx: &mut Context<Self>,
    ) -> Div {
        let folded = self.is_folded(key);
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(space(Space::Snug))
            .child(section_head(theme, key, title, provenance, note, folded, cx))
            .when(!folded, |section| {
                section
                    .when_some(provenance.sentence(), |body, sentence| {
                        body.child(text::faint(theme).child(sentence))
                    })
                    .child(body)
            })
    }
}

/// Returns the clickable head of one page section.
fn section_head(
    theme: &Theme,
    key: &'static str,
    title: &str,
    provenance: Provenance,
    note: Option<String>,
    folded: bool,
    cx: &mut Context<Workspace>,
) -> gpui::Stateful<Div> {
    div()
        .id(ElementId::Name(SharedString::from(format!(
            "package-head-{key}"
        ))))
        .flex()
        .items_center()
        .gap(space(Space::Snug))
        .cursor_pointer()
        .child(icon::sized(
            theme,
            if folded {
                Icon::ChevronRight
            } else {
                Icon::ChevronDown
            },
            12.0,
            Paint::TextFaint,
        ))
        .child(
            text::heading(theme, TypeScale::Section)
                .flex_none()
                .child(title.to_owned()),
        )
        .when_some(note, |head, note| {
            head.child(text::faint(theme).flex_none().child(note))
        })
        .when_some(provenance.tag(), |head, _| {
            head.child(chart::sample_tag(theme))
        })
        .child(div().flex_1().h(hairline()).bg(theme.paint(Paint::Hairline)))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.toggle_fold(key, cx);
        }))
}

/// Returns the usage body: the series, the sentence, and the adoption split.
fn usage_body(theme: &Theme, dossier: &Dossier, downloads: &Downloads) -> AnyElement {
    let hue = language_hue(
        dossier
            .ecosystem()
            .map_or(Language::Unknown, super::home::ecosystem_language),
    );
    let ends = downloads.span().map_or_else(
        || (String::new(), String::new()),
        |(first, last)| (first.iso(), last.iso()),
    );
    let peak = downloads
        .weekly()
        .iter()
        .max_by_key(|tally| tally.count())
        .map(|tally| {
            format!(
                "peak {} in the week of {}",
                dossier::tally_label(tally.count()),
                tally.week().iso()
            )
        });
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(space(Space::Base))
        .child(chart::histogram(
            theme,
            hue,
            &downloads.counts(),
            CHART,
            (ends.0.as_str(), ends.1.as_str()),
        ))
        .child(
            text::dim(theme).child(format!(
                "{} downloads in the last ninety days, {} over the package's lifetime.",
                dossier::tally_label(downloads.recent()),
                dossier::tally_label(downloads.total())
            )),
        )
        .when_some(peak, |body, peak| {
            body.child(text::faint(theme).child(peak))
        })
        .children(adoption_block(theme, hue, dossier, downloads))
        .into_any_element()
}

/// Returns the per-version adoption meters, when there are versions to split.
fn adoption_block(
    theme: &Theme,
    hue: Hue,
    dossier: &Dossier,
    downloads: &Downloads,
) -> Option<Div> {
    let releases = dossier.releases().ready()?;
    if releases.len() < 2 {
        return None;
    }
    let shares = adoption(releases, downloads.recent());
    Some(
        head_and_body(
            theme,
            "Adoption by version",
            div()
                .w_full()
                .flex()
                .flex_col()
                .gap(space(Space::Snug))
                .children(shares.into_iter().map(|(version, count, percent)| {
                    div()
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(space(Space::Snug))
                                .child(
                                    text::text_at(theme, TypeScale::Small, Paint::Text)
                                        .flex_1()
                                        .font_family(theme.specimen())
                                        .child(version),
                                )
                                .child(
                                    text::faint(theme)
                                        .flex_none()
                                        .child(format!("{percent}% · {}", dossier::tally_label(count))),
                                ),
                        )
                        .child(chart::meter(theme, hue, count, downloads.recent().max(1)))
                }))
                .into_any_element(),
        )
        .pt(space(Space::Tight)),
    )
}

/// Returns each of the newest releases' share of the recent download window.
fn adoption(releases: &[Release], recent: u64) -> Vec<(String, u64, u16)> {
    let named: Vec<&Release> = releases.iter().take(ADOPTED).collect();
    let weight: u64 = DECAY.iter().take(named.len()).sum();
    named
        .iter()
        .zip(DECAY)
        .map(|(release, share)| {
            let count = recent.saturating_mul(share) / weight.max(1);
            (
                release.version().to_owned(),
                count,
                chart::share(count, recent.max(1)),
            )
        })
        .collect()
}

/// Returns one version row, struck through when the release was withdrawn.
///
/// The `list` prefix is load-bearing rather than decorative: the picker and
/// the versions section can both be open at once, and two elements carrying
/// one identity are two elements sharing one hover and one click.
fn version_row(
    theme: &Theme,
    list: &str,
    at: usize,
    release: &Release,
    current: bool,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let opened = release.coordinate().to_owned();
    let yanked = release.standing() == ReleaseStanding::Yanked;
    div()
        .id(ElementId::Name(SharedString::from(format!(
            "package-{list}-release-{at}"
        ))))
        .flex()
        .items_center()
        .gap(space(Space::Snug))
        .px(space(Space::Snug))
        .py(px(2.0))
        .rounded(radius(Radius::Hair))
        .when(current, |row| row.bg(theme.paint(Paint::GiltWash)))
        .hover(|style| style.bg(theme.paint(Paint::Hover)))
        .cursor_pointer()
        .on_click(cx.listener(move |this, _, _, cx| {
            this.open_package(opened.clone(), Target::Here, cx);
        }))
        .child(
            text::single_line(
                text::text_at(
                    theme,
                    TypeScale::Small,
                    if yanked { Paint::TextFaint } else { Paint::Text },
                )
                .flex_none()
                .w(px(96.0))
                .font_family(theme.specimen())
                .font_weight(if current {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::NORMAL
                }),
            )
            .when(yanked, Styled::line_through)
            .child(release.version().to_owned()),
        )
        .child(
            text::faint(theme).flex_1().child(
                release
                    .published()
                    .map_or_else(|| "date not recorded".to_owned(), Stamp::iso),
            ),
        )
        .child(text::faint(theme).flex_none().child(release.size()))
        .when(yanked, |row| row.child(mark(theme, "yanked", Paint::Fault)))
        .when(current, |row| row.child(mark(theme, "current", Paint::Gilt)))
        .into_any_element()
}

/// Returns the one-word mark a release row ends with.
fn mark(theme: &Theme, word: &'static str, role: Paint) -> Div {
    div()
        .flex_none()
        .text_size(type_size(TypeScale::Micro))
        .text_color(theme.paint(role))
        .child(word)
}

/// Returns one dependency row, which opens the package it names.
fn dependency_row(
    theme: &Theme,
    at: usize,
    row: &Dependency,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let opened = row.resolved().to_owned();
    package_row(theme, format!("package-dependency-{at}"), row.ecosystem(), opened, cx)
        .child(
            text::single_line(text::label(theme))
                .flex_1()
                .min_w(px(0.0))
                .child(row.name().to_owned()),
        )
        .child(
            text::faint(theme)
                .flex_none()
                .font_family(theme.specimen())
                .child(row.requirement().to_owned()),
        )
        .when_some(row.role().tag(), |line, tag| {
            line.child(chip::badge(theme, tag))
        })
        .into_any_element()
}

/// Returns one dependent row, with its download weight when one is published.
fn dependent_row(
    theme: &Theme,
    at: usize,
    row: &Dependent,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let opened = row.coordinate().to_owned();
    package_row(theme, format!("package-dependent-{at}"), row.ecosystem(), opened, cx)
        .child(
            text::single_line(text::label(theme))
                .flex_1()
                .min_w(px(0.0))
                .child(row.name().to_owned()),
        )
        .child(
            text::faint(theme)
                .flex_none()
                .font_family(theme.specimen())
                .child(row.version().to_owned()),
        )
        .when_some(row.downloads(), |line, count| {
            line.child(
                text::faint(theme)
                    .flex_none()
                    .child(dossier::tally_label(count)),
            )
        })
        .into_any_element()
}

/// Returns the shell every row that names another package shares.
fn package_row(
    theme: &Theme,
    id: String,
    ecosystem: RegistryEcosystem,
    opened: String,
    cx: &mut Context<Workspace>,
) -> gpui::Stateful<Div> {
    div()
        .id(ElementId::Name(SharedString::from(id)))
        .flex()
        .items_center()
        .gap(space(Space::Snug))
        .px(space(Space::Snug))
        .py(px(2.0))
        .rounded(radius(Radius::Hair))
        .hover(|style| style.bg(theme.paint(Paint::Hover)))
        .cursor_pointer()
        .on_click(cx.listener(move |this, _, _, cx| {
            this.open_package(opened.clone(), Target::Here, cx);
        }))
        .children(super::browse::ecosystem_logo(theme, ecosystem, 13.0))
}

/// Returns the owner chips: a handle, and what that handle does.
fn owner_row(theme: &Theme, owners: &[Owner]) -> Div {
    div()
        .flex()
        .flex_wrap()
        .gap(space(Space::Snug))
        .children(owners.iter().map(|owner| {
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(space(Space::Tight))
                .px(space(Space::Snug))
                .py(px(2.0))
                .rounded(radius(Radius::Capsule))
                .border(hairline())
                .border_color(theme.paint(Paint::Hairline))
                .child(
                    text::text_at(theme, TypeScale::Small, Paint::Text)
                        .child(owner.handle().to_owned()),
                )
                .child(text::faint(theme).child(owner.seat().label()))
        }))
}

/// Returns the README as elements: headings, prose, code wells, and lists.
fn readme_blocks(theme: &Theme, blocks: &[ReadmeBlock]) -> Div {
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(space(Space::Base))
        .children(blocks.iter().map(|block| readme_block(theme, block)))
}

fn readme_block(theme: &Theme, block: &ReadmeBlock) -> AnyElement {
    match block {
        ReadmeBlock::Heading { rank, text: body } => text::heading(theme, rank_scale(*rank))
            .child(body.clone())
            .into_any_element(),
        ReadmeBlock::Paragraph(body) => text::body(theme).child(body.clone()).into_any_element(),
        ReadmeBlock::Code { fence, text: body } => code_well(theme, *fence, body),
        ReadmeBlock::List(items) => div()
            .flex()
            .flex_col()
            .gap(space(Space::Tight))
            .children(items.iter().map(|item| {
                div()
                    .flex()
                    .gap(space(Space::Snug))
                    .child(text::faint(theme).flex_none().child("·"))
                    .child(text::body(theme).flex_1().child(item.clone()))
            }))
            .into_any_element(),
    }
}

/// Returns the type rung one README heading rank is set at.
const fn rank_scale(rank: Rank) -> TypeScale {
    match rank {
        Rank::Title => TypeScale::Section,
        Rank::Section => TypeScale::Interface,
        Rank::Subsection => TypeScale::Small,
    }
}

/// Returns one code well, tagged with the language its fence named.
fn code_well(theme: &Theme, fence: dossier::Fence, body: &str) -> AnyElement {
    let ink = fence.language().map_or_else(
        || theme.paint(Paint::TextFaint),
        |language| theme.on_plane(language_hue(language)),
    );
    surface::sunken(theme)
        .w_full()
        .flex()
        .flex_col()
        .gap(space(Space::Tight))
        .px(space(Space::Room))
        .py(space(Space::Snug))
        .child(
            div()
                .flex_none()
                .text_size(type_size(TypeScale::Micro))
                .text_color(ink)
                .child(fence.tag()),
        )
        .child(
            div()
                .w_full()
                .font_family(theme.specimen())
                .text_size(type_size(TypeScale::Small))
                .text_color(theme.paint(Paint::Text))
                .child(body.to_owned()),
        )
        .into_any_element()
}

/// Returns the unfurl key the version picker on one coordinate toggles.
fn picker_key(coordinate: &str) -> String {
    format!("package-picker-{coordinate}")
}

/// Returns the reserved geometry a section holds while its read is in flight.
fn reserved_lines(theme: &Theme) -> Div {
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(space(Space::Tight))
        .child(skeleton(theme, 420.0, 14.0))
        .child(skeleton(theme, 360.0, 14.0))
        .child(skeleton(theme, 300.0, 14.0))
}

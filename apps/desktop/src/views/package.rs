//! One registry package, as a document: what it is, how it is used, who uses it.
//! Seven sections, each stating whether it is a recorded fact or unavailable
//! in the configured registry feed.
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
//! publishes yet carries a typed absence. Empty or unsupported upstream data
//! remains visible as a fact about coverage rather than becoming a plausible
//! value.
//!
//! Nothing here is scraped and nothing is guessed. The versions, the reverse
//! dependencies, and the release count come from the surface commands the CLI
//! runs, in the same order and with the same absences; the rest comes from
//! [`crate::store::dossier`], which retains the service's typed coverage.

use super::home::skeleton;
use super::workspace::Workspace;
use crate::motion::{Beat, entering_opacity, once};
use crate::store::document::Target;
use crate::store::dossier::{
    self, Dependency, Dependent, Dossier, Downloads, History, Link, Owner, PackageRoute, Precis,
    Provenance, Rank, ReadmeBlock, Release, Reverse, Section, Stamp, Standing as ReleaseStanding,
};
use crate::store::registry::{self, Loadable, Standing};
use crate::theme::Theme;
use crate::theme::language::hue as language_hue;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Radius, Space, TypeScale, hairline, radius, space, type_size};
use crate::ui::icon::Icon;
use crate::ui::{button, chart, chip, fault as fault_ui, icon, surface, text};
use backend_library::{RegistryEcosystem, RegistrySecurityStanding};
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

/// Reserves the package page geometry while the first live read is installed.
fn reserved_package_page(theme: &Theme, coordinate: &str) -> Div {
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(space(Space::Gutter))
        .child(text::identity_text(theme, TypeScale::Small).child(coordinate.to_owned()))
        .child(skeleton(theme, 280.0, 28.0))
        .child(skeleton(theme, 520.0, 16.0))
        .child(reserved_lines(theme))
}

fn package_tab(
    theme: &Theme,
    id: &'static str,
    label: &'static str,
    fold_key: &'static str,
    cx: &mut Context<Workspace>,
) -> gpui::Stateful<Div> {
    button::button(theme, id, label, button::Weight::Quiet)
        .on_click(cx.listener(move |this, _, _, cx| this.toggle_fold(fold_key, cx)))
}

fn package_route_tab(
    theme: &Theme,
    id: &'static str,
    label: &'static str,
    route: &'static str,
    cx: &mut Context<Workspace>,
) -> gpui::Stateful<Div> {
    button::button(theme, id, label, button::Weight::Quiet)
        .on_click(cx.listener(move |this, _, _, cx| this.toggle_unfurl(route, cx)))
}

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
        let Some(dossier) = self.registry.read(cx).dossier_for(coordinate).cloned() else {
            return reserved_package_page(theme, coordinate).into_any_element();
        };
        let reduced = theme.reduced_motion();
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(space(Space::Gutter))
            .child(self.package_header(theme, &dossier, cx))
            .child(Self::package_tabs(theme, cx))
            .child(self.package_route_sections(theme, &dossier, cx))
            .child(self.primary_package_content(theme, &dossier, cx))
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

    /// Returns the internal package information architecture. Each tab names a
    /// typed route in this live dossier; no route is synthesized from a package
    /// name or handed off to an external site.
    fn package_tabs(theme: &Theme, cx: &mut Context<Self>) -> gpui::Stateful<Div> {
        div()
            .id("package-tabs")
            .w_full()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(space(Space::Tight))
            .border_b(hairline())
            .border_color(theme.paint(Paint::Hairline))
            .child(package_tab(
                theme,
                "package-tab-readme",
                "Readme",
                "readme",
                cx,
            ))
            .child(package_route_tab(
                theme,
                "package-tab-docs",
                "Docs",
                PackageRoute::Documentation.key(),
                cx,
            ))
            .child(package_route_tab(
                theme,
                "package-tab-source",
                "Source",
                PackageRoute::Source.key(),
                cx,
            ))
            .child(package_route_tab(
                theme,
                "package-tab-code",
                "Code",
                PackageRoute::Code.key(),
                cx,
            ))
            .child(package_route_tab(
                theme,
                "package-tab-search",
                "Search",
                PackageRoute::Search.key(),
                cx,
            ))
            .child(package_tab(
                theme,
                "package-tab-versions",
                "Versions",
                "versions",
                cx,
            ))
            .child(package_tab(
                theme,
                "package-tab-dependencies",
                "Dependencies",
                "dependencies",
                cx,
            ))
            .child(package_tab(
                theme,
                "package-tab-dependents",
                "Dependents",
                "dependents",
                cx,
            ))
            .child(package_route_tab(
                theme,
                "package-tab-security",
                "Security",
                "package-security",
                cx,
            ))
    }

    /// Returns the internal docs/source/code/search/security routes that a tab
    /// unfolds. Their bodies are typed coverage statements, so an unavailable
    /// capability stays visible instead of turning into an external guess.
    fn package_route_sections(
        &self,
        theme: &Theme,
        dossier: &Dossier,
        cx: &mut Context<Workspace>,
    ) -> Div {
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(space(Space::Snug))
            .when(
                self.is_unfurled(PackageRoute::Documentation.key()),
                |body| {
                    body.child(self.package_coverage_route(
                        theme,
                        dossier,
                        PackageRoute::Documentation,
                        cx,
                    ))
                },
            )
            .when(self.is_unfurled(PackageRoute::Source.key()), |body| {
                body.child(self.package_coverage_route(theme, dossier, PackageRoute::Source, cx))
            })
            .when(self.is_unfurled(PackageRoute::Code.key()), |body| {
                body.child(self.package_coverage_route(theme, dossier, PackageRoute::Code, cx))
            })
            .when(self.is_unfurled(PackageRoute::Search.key()), |body| {
                body.child(self.package_coverage_route(theme, dossier, PackageRoute::Search, cx))
            })
            .when(self.is_unfurled("package-security"), |body| {
                body.child(self.security_route(theme, dossier, cx))
            })
    }

    fn package_coverage_route(
        &self,
        theme: &Theme,
        dossier: &Dossier,
        route: PackageRoute,
        cx: &mut Context<Workspace>,
    ) -> Div {
        let body = dossier.route_request(route).map_or_else(
            || {
                text::dim(theme)
                    .child("The package coordinate is not admitted by the local index, so this route cannot open.")
                    .into_any_element()
            },
            |request| {
                text::dim(theme)
                    .child(format!(
                        "{} is bound to the local semantic index for {}. Content remains unavailable until that provider returns a recorded page.",
                        request.route().title(),
                        request.package().as_str(),
                    ))
                    .into_any_element()
            },
        );
        self.folding(
            theme,
            route.key(),
            route.title(),
            Provenance::NotRecorded,
            None,
            body,
            cx,
        )
    }

    fn security_route(&self, theme: &Theme, dossier: &Dossier, cx: &mut Context<Workspace>) -> Div {
        let (provenance, body) = dossier.pinned().map_or_else(
            || {
                (
                    Provenance::NotRecorded,
                    text::dim(theme)
                        .child("Security status is not recorded by the configured registry feed.")
                        .into_any_element(),
                )
            },
            |release| {
                (
                    Provenance::Recorded,
                    text::text_at(theme, TypeScale::Body, Paint::Text)
                        .child(Self::security_label(release.security()))
                        .into_any_element(),
                )
            },
        );
        self.folding(
            theme,
            "package-security",
            "Security",
            provenance,
            None,
            body,
            cx,
        )
    }

    /// Returns the primary docs.rs-like reading split: README first, with
    /// package facts and internal docs/source handoffs in a compact rail.
    /// The minimum widths are deliberately small enough to wrap at compact
    /// window sizes, keeping every affordance reachable at 640x480 and below.
    fn primary_package_content(
        &self,
        theme: &Theme,
        dossier: &Dossier,
        cx: &mut Context<Workspace>,
    ) -> Div {
        div()
            .w_full()
            .flex()
            .flex_wrap()
            .items_start()
            .gap(space(Space::Gutter))
            .child(
                div()
                    .flex_1()
                    .min_w(px(300.0))
                    .child(self.readme_section(theme, dossier, cx)),
            )
            .child(self.metadata_rail(theme, dossier, cx))
    }

    /// Returns the facts and live links that remain visible while the README
    /// is read. Empty or unavailable registry fields keep their typed absence.
    fn metadata_rail(&self, theme: &Theme, dossier: &Dossier, cx: &mut Context<Workspace>) -> Div {
        let ecosystem = dossier.ecosystem();
        let spelling = dossier.spelling();
        let precis = dossier.precis();
        let links = precis.ready().map(Precis::links).unwrap_or_default();
        let release = dossier.pinned();
        let status = release.map_or_else(
            || "release status not recorded".to_owned(),
            |release| {
                format!(
                    "{} · {}",
                    release.standing().label(),
                    Self::security_label(release.security())
                )
            },
        );
        let install = ecosystem.map_or(
            InstallCapability::Unavailable("ecosystem is not recorded"),
            |ecosystem| install_command(ecosystem, spelling.name(), spelling.version()),
        );
        surface::sunken(theme)
            .flex_none()
            .w(px(288.0))
            .max_w_full()
            .p(space(Space::Snug))
            .flex()
            .flex_col()
            .gap(space(Space::Snug))
            .child(text::heading(theme, TypeScale::Section).child("Package metadata"))
            .child(metadata_fact(theme, "Status", status))
            .when_some(release, |rail, release| {
                rail.child(metadata_fact(theme, "Archive", release.size()))
                    .when_some(release.published(), |rail, stamp| {
                        rail.child(metadata_fact(theme, "Published", stamp.spelled()))
                    })
            })
            .when_some(precis.ready().and_then(Precis::license), |rail, license| {
                rail.child(metadata_fact(theme, "License", license.to_owned()))
            })
            .when_some(
                dossier
                    .downloads()
                    .ready()
                    .filter(|_| !dossier.downloads().provenance().is_not_recorded()),
                |rail, downloads| {
                    rail.child(metadata_fact(
                        theme,
                        "Downloads",
                        dossier::tally_label(downloads.total()),
                    ))
                },
            )
            .when(precis.provenance().is_not_recorded(), |rail| {
                rail.child(
                    text::dim(theme).child(
                        "Additional metadata is not recorded by the configured registry feed.",
                    ),
                )
            })
            .child(install_capability(theme, install, cx))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(space(Space::Tight))
                    .child(
                        button::button(
                            theme,
                            "package-rail-docs",
                            "Open docs route",
                            button::Weight::Quiet,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.toggle_unfurl(PackageRoute::Documentation.key(), cx);
                        })),
                    )
                    .child(
                        button::button(
                            theme,
                            "package-rail-source",
                            "Open source route",
                            button::Weight::Quiet,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.toggle_unfurl(PackageRoute::Source.key(), cx);
                        })),
                    ),
            )
            .when(!links.is_empty(), |rail| {
                rail.child(div().flex().flex_wrap().gap(space(Space::Tight)).children(
                    links.iter().enumerate().map(|(at, link)| {
                        Self::link_button(theme, at, link, precis.provenance(), cx)
                    }),
                ))
            })
    }
}

/// The header.
impl Workspace {
    fn package_header(&mut self, theme: &Theme, dossier: &Dossier, cx: &mut Context<Self>) -> Div {
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
            .when_some(
                precis.filter(|_| !dossier.precis().provenance().is_not_recorded()),
                |header, precis| header.child(Self::precis_block(theme, dossier.precis(), precis)),
            )
            .child(Self::meta_row(theme, dossier))
            .when_some(precis, |header, precis| {
                header.child(Self::link_row(
                    theme,
                    precis,
                    dossier.precis().provenance(),
                    cx,
                ))
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
    fn title_row(theme: &Theme, dossier: &Dossier, unfurled: bool, cx: &mut Context<Self>) -> Div {
        let spelling = dossier.spelling();
        let standing = dossier.pinned().map(Release::standing);
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
            .when_some(
                standing.filter(|standing| *standing != ReleaseStanding::Published),
                |row, standing| {
                    row.child(
                        div()
                            .flex_none()
                            .px(px(5.0))
                            .py(px(1.0))
                            .rounded(radius(Radius::Hair))
                            .bg(theme.paint(Paint::Fault))
                            .text_size(type_size(TypeScale::Micro))
                            .text_color(theme.paint(Paint::Ground))
                            .child(standing.label()),
                    )
                },
            )
    }

    /// Returns the version badge, which opens the picker when pressed.
    fn version_button(
        theme: &Theme,
        dossier: &Dossier,
        unfurled: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = picker_key(dossier.coordinate());
        let count = dossier.releases().ready().map_or(0, Vec::len);
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
            .child(text::faint(theme).child(format!("of {count}")))
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
                version_row(
                    theme,
                    "picker",
                    at,
                    release,
                    release.version() == current,
                    cx,
                )
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
                    div().flex().flex_wrap().gap(space(Space::Tight)).children(
                        precis
                            .keywords()
                            .iter()
                            .map(|keyword| chip::badge(theme, keyword)),
                    ),
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
            facts.push(release.published().map_or_else(
                || "publication date not recorded".to_owned(),
                |stamp| format!("published {}", stamp.spelled()),
            ));
            facts.push(Self::security_label(release.security()).to_owned());
        }
        if let Some(downloads) = dossier.downloads().ready()
            && !dossier.downloads().provenance().is_not_recorded()
        {
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
        if facts.is_empty() {
            return text::faint(theme).child("registry metadata not recorded");
        }
        text::faint(theme).child(facts.join(" · "))
    }

    /// Returns the exact security coverage state published for the release.
    fn security_label(standing: RegistrySecurityStanding) -> &'static str {
        match standing {
            RegistrySecurityStanding::Unassessed => "security unassessed",
            RegistrySecurityStanding::NoKnownAdvisory => "no known advisories",
            RegistrySecurityStanding::Affected { .. } => "security advisory affects this release",
        }
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
                    button::button(
                        theme,
                        "package-add",
                        "Add to shelf",
                        button::Weight::Primary,
                    )
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

/// Returns a compact metadata label/value pair for the rail.
fn metadata_fact(theme: &Theme, label: &str, value: impl Into<String>) -> Div {
    div()
        .flex()
        .items_start()
        .gap(space(Space::Snug))
        .child(text::faint(theme).flex_none().child(label.to_owned()))
        .child(
            text::text_at(theme, TypeScale::Small, Paint::Text)
                .flex_1()
                .min_w(px(0.0))
                .child(value.into()),
        )
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum InstallCapability {
    Available(String),
    Unavailable(&'static str),
}

fn install_capability(
    theme: &Theme,
    capability: InstallCapability,
    cx: &mut Context<Workspace>,
) -> Div {
    match capability {
        InstallCapability::Available(command) => {
            let copied = command.clone();
            div()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(text::faint(theme).child("Install"))
                .child(
                    button::button(
                        theme,
                        "package-rail-install",
                        "Copy install command",
                        button::Weight::Regular,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.copy("Install command copied", copied.clone(), cx);
                    })),
                )
                .child(
                    text::faint(theme)
                        .font_family(theme.specimen())
                        .child(command),
                )
        }
        InstallCapability::Unavailable(reason) => div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .child(text::faint(theme).child("Install"))
            .child(text::dim(theme).child(format!("Install command unavailable: {reason}."))),
    }
}

fn install_command(ecosystem: RegistryEcosystem, name: &str, version: &str) -> InstallCapability {
    if name.is_empty() || version.is_empty() {
        return InstallCapability::Unavailable("package coordinate is incomplete");
    }
    let command = match ecosystem {
        RegistryEcosystem::Cargo => format!("cargo add {name}@{version}"),
        RegistryEcosystem::Npm => format!("npm install {name}@{version}"),
        RegistryEcosystem::Pypi => format!("python -m pip install {name}=={version}"),
        RegistryEcosystem::Maven => {
            let Some((group, artifact)) = name.split_once(':') else {
                return InstallCapability::Unavailable("Maven coordinates require group:artifact");
            };
            format!(
                "<dependency>\n  <groupId>{group}</groupId>\n  <artifactId>{artifact}</artifactId>\n  <version>{version}</version>\n</dependency>"
            )
        }
        RegistryEcosystem::Nuget => format!("dotnet add package {name} --version {version}"),
        RegistryEcosystem::Golang => format!("go get {name}@{version}"),
        RegistryEcosystem::Cpp => {
            format!("conan install --requires={name}/{version} --build=missing")
        }
    };
    InstallCapability::Available(command)
}

#[cfg(test)]
fn documentation_url(ecosystem: RegistryEcosystem, name: &str, version: &str) -> Option<String> {
    if name.is_empty() {
        return None;
    }
    match ecosystem {
        RegistryEcosystem::Cargo => Some(format!("https://docs.rs/{name}/{version}")),
        RegistryEcosystem::Npm => Some(format!(
            "https://www.npmjs.com/package/{}/v/{version}",
            url_path_segment(name)
        )),
        RegistryEcosystem::Pypi => Some(format!("https://pypi.org/project/{name}/{version}/")),
        RegistryEcosystem::Maven => {
            let (group, artifact) = name.split_once(':')?;
            Some(format!(
                "https://javadoc.io/doc/{group}/{artifact}/{version}"
            ))
        }
        RegistryEcosystem::Nuget => {
            Some(format!("https://www.nuget.org/packages/{name}/{version}"))
        }
        RegistryEcosystem::Golang => Some(format!(
            "https://pkg.go.dev/{}@{version}",
            url_module_path(name)
        )),
        RegistryEcosystem::Cpp => None,
    }
}

/// Escapes one package name as a URL path component while preserving the
/// registry's case and version spelling.
#[cfg(test)]
fn url_path_segment(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            escaped.push(char::from(byte));
        } else {
            escaped.push('%');
            escaped.push(hex(byte >> 4));
            escaped.push(hex(byte & 0x0f));
        }
    }
    escaped
}

#[cfg(test)]
fn hex(nibble: u8) -> char {
    char::from(b"0123456789ABCDEF"[usize::from(nibble)])
}

#[cfg(test)]
fn url_module_path(value: &str) -> String {
    value
        .split('/')
        .map(url_path_segment)
        .collect::<Vec<_>>()
        .join("/")
}

/// The README.
impl Workspace {
    fn readme_section(&self, theme: &Theme, dossier: &Dossier, cx: &mut Context<Self>) -> Div {
        let section = dossier.readme();
        let body = match section.state() {
            Loadable::Idle | Loadable::Loading => reserved_lines(theme).into_any_element(),
            Loadable::Faulted(fault) => {
                fault_ui::block(theme, fault, Vec::new()).into_any_element()
            }
            Loadable::Ready(blocks)
                if blocks.is_empty() && section.provenance().is_not_recorded() =>
            {
                text::dim(theme)
                    .child("README content is not recorded by the configured registry feed.")
                    .into_any_element()
            }
            Loadable::Ready(blocks) if blocks.is_empty() => text::dim(theme)
                .child("This package ships no README.")
                .into_any_element(),
            Loadable::Ready(blocks) => readme_blocks(theme, blocks).into_any_element(),
        };
        self.folding(
            theme,
            "readme",
            "Readme",
            section.provenance(),
            None,
            body,
            cx,
        )
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
            Loadable::Ready(rows) if rows.is_empty() && section.provenance().is_not_recorded() => {
                text::dim(theme)
                    .child("Dependency metadata is not recorded by the configured registry feed.")
                    .into_any_element()
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

    fn dependency_list(theme: &Theme, rows: &[Dependency], cx: &mut Context<Self>) -> AnyElement {
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

    fn dependents_section(&self, theme: &Theme, dossier: &Dossier, cx: &mut Context<Self>) -> Div {
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
        let shown = if unfurled {
            rows.len()
        } else {
            rows.len().min(BUDGET)
        };
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

    fn versions_section(&self, theme: &Theme, dossier: &Dossier, cx: &mut Context<Self>) -> Div {
        let section = dossier.releases();
        let current = dossier.spelling().version().to_owned();
        let unfurled = self.is_unfurled("package-versions-list");
        let note = section
            .ready()
            .map(|rows| format!("{} recorded", rows.len()));
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
        let shown = if unfurled {
            rows.len()
        } else {
            rows.len().min(BUDGET)
        };
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

    fn usage_section(&self, theme: &Theme, dossier: &Dossier, cx: &mut Context<Self>) -> Div {
        let section = dossier.downloads();
        let body = match section.state() {
            Loadable::Idle | Loadable::Loading => reserved_lines(theme).into_any_element(),
            Loadable::Faulted(fault) => {
                fault_ui::block(theme, fault, Vec::new()).into_any_element()
            }
            Loadable::Ready(_) if section.provenance().is_not_recorded() => text::dim(theme)
                .child("Download history is not recorded by the configured registry feed.")
                .into_any_element(),
            Loadable::Ready(downloads) => usage_body(theme, dossier, downloads),
        };
        self.folding(
            theme,
            "usage",
            "Usage",
            section.provenance(),
            None,
            body,
            cx,
        )
    }

    fn owners_section(&self, theme: &Theme, dossier: &Dossier, cx: &mut Context<Self>) -> Div {
        let section = dossier.owners();
        let body = match section.state() {
            Loadable::Idle | Loadable::Loading => reserved_lines(theme).into_any_element(),
            Loadable::Faulted(fault) => {
                fault_ui::block(theme, fault, Vec::new()).into_any_element()
            }
            Loadable::Ready(owners)
                if owners.is_empty() && section.provenance().is_not_recorded() =>
            {
                text::dim(theme)
                    .child("Publisher metadata is not recorded by the configured registry feed.")
                    .into_any_element()
            }
            Loadable::Ready(owners) if owners.is_empty() => text::dim(theme)
                .child("The feed records no publisher for this package.")
                .into_any_element(),
            Loadable::Ready(owners) => owner_row(theme, owners).into_any_element(),
        };
        self.folding(
            theme,
            "owners",
            "Owners",
            section.provenance(),
            None,
            body,
            cx,
        )
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
            .child(section_head(
                theme, key, title, provenance, note, folded, cx,
            ))
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
        .child(
            div()
                .flex_1()
                .h(hairline())
                .bg(theme.paint(Paint::Hairline)),
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            this.toggle_fold(key, cx);
        }))
}

/// Returns the usage body from the registry's recorded series.
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
        .child(text::dim(theme).child(format!(
            "{} downloads in the last ninety days, {} over the package's lifetime.",
            dossier::tally_label(downloads.recent()),
            dossier::tally_label(downloads.total())
        )))
        .when_some(peak, |body, peak| {
            body.child(text::faint(theme).child(peak))
        })
        .into_any_element()
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
    let withdrawn = release.standing() != ReleaseStanding::Published;
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
                    if withdrawn {
                        Paint::TextFaint
                    } else {
                        Paint::Text
                    },
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
            .when(withdrawn, Styled::line_through)
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
        .when(withdrawn, |row| {
            row.child(mark(theme, release.standing().label(), Paint::Fault))
        })
        .when(current, |row| {
            row.child(mark(theme, "current", Paint::Gilt))
        })
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
    package_row(
        theme,
        format!("package-dependency-{at}"),
        row.ecosystem(),
        opened,
        cx,
    )
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
    package_row(
        theme,
        format!("package-dependent-{at}"),
        row.ecosystem(),
        opened,
        cx,
    )
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

#[cfg(test)]
mod tests {
    use super::{InstallCapability, documentation_url, install_command};
    use backend_library::RegistryEcosystem;

    #[test]
    fn install_commands_are_pinned_to_the_live_coordinate() {
        assert_eq!(
            install_command(RegistryEcosystem::Cargo, "serde", "1.0.0"),
            InstallCapability::Available("cargo add serde@1.0.0".to_owned())
        );
        assert_eq!(
            install_command(RegistryEcosystem::Pypi, "httpx", "0.27.0"),
            InstallCapability::Available("python -m pip install httpx==0.27.0".to_owned())
        );
        assert_eq!(
            install_command(RegistryEcosystem::Maven, "org.example:widget", "2.1.0"),
            InstallCapability::Available(
                "<dependency>\n  <groupId>org.example</groupId>\n  <artifactId>widget</artifactId>\n  <version>2.1.0</version>\n</dependency>"
                    .to_owned(),
            )
        );
        assert_eq!(
            install_command(RegistryEcosystem::Nuget, "Newtonsoft.Json", "13.0.3"),
            InstallCapability::Available(
                "dotnet add package Newtonsoft.Json --version 13.0.3".to_owned(),
            )
        );
        assert_eq!(
            install_command(RegistryEcosystem::Cargo, "", "1.0.0"),
            InstallCapability::Unavailable("package coordinate is incomplete")
        );
        assert_eq!(
            install_command(RegistryEcosystem::Cpp, "fmt", "11.0.0"),
            InstallCapability::Available(
                "conan install --requires=fmt/11.0.0 --build=missing".to_owned()
            )
        );
    }

    #[test]
    fn documentation_links_preserve_ecosystem_and_version_identity() {
        assert_eq!(
            documentation_url(RegistryEcosystem::Cargo, "serde", "1.0.0").as_deref(),
            Some("https://docs.rs/serde/1.0.0")
        );
        assert_eq!(
            documentation_url(RegistryEcosystem::Npm, "@scope/pkg", "4.2.0").as_deref(),
            Some("https://www.npmjs.com/package/%40scope%2Fpkg/v/4.2.0")
        );
        assert_eq!(
            documentation_url(RegistryEcosystem::Golang, "golang.org/x/tools", "v0.24.0")
                .as_deref(),
            Some("https://pkg.go.dev/golang.org/x/tools@v0.24.0")
        );
        assert_eq!(
            documentation_url(RegistryEcosystem::Golang, "example.org/x/y z", "v1.0.0").as_deref(),
            Some("https://pkg.go.dev/example.org/x/y%20z@v1.0.0")
        );
        assert_eq!(
            documentation_url(RegistryEcosystem::Maven, "org.example:widget", "2.1.0").as_deref(),
            Some("https://javadoc.io/doc/org.example/widget/2.1.0")
        );
        assert_eq!(
            documentation_url(RegistryEcosystem::Cpp, "fmt", "11.0.0"),
            None
        );
    }
}

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
use crate::store::document::ReaderIntent;
use crate::store::document::Target;
use crate::store::dossier::{
    self, Dependency, Dependent, Dossier, Downloads, History, Link, Owner, PackageRoute, Precis,
    Provenance, Rank, ReadmeBlock, Release, Reverse, Section, Stamp, Standing as ReleaseStanding,
};
use crate::store::registry::{self, Loadable, Standing};
use crate::theme::Theme;
use crate::theme::language::hue as language_hue;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Space, TypeScale, hairline, space, type_size};
use crate::ui::icon::Icon;
use crate::ui::{button, chart, chip, components, fault as fault_ui, icon, surface, text};
use backend_library::{
    AcquisitionDecision, AdvisoryPackageDto, AdvisoryStatus, FreshnessState, RegistryEcosystem,
};
use backend_present::Language;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnimationExt as _, AnyElement, Context, Div, ElementId, InteractiveElement, IntoElement,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, div, px, uniform_list,
};
use gpui_component::Selectable as _;
use std::cmp::Ordering;
use std::sync::Arc;

/// How many rows a long section lists before it offers the rest.
const BUDGET: usize = 12;

/// Height of the usage chart, in pixels.
const CHART: u16 = 84;

/// Internal package routes are mutually exclusive, like docs.rs's primary
/// navigation. Keeping the keys together prevents two route bodies from
/// opening under one selected tab.
const PACKAGE_ROUTES: [&str; 5] = [
    PackageRoute::Documentation.key(),
    PackageRoute::Source.key(),
    PackageRoute::Code.key(),
    PackageRoute::Search.key(),
    "package-security",
];

/// Content sections represented by the primary package navigation. Selecting
/// one folds the other long sections so the tab line behaves like a document
/// navigation strip rather than a collection of independent disclosures.
const PACKAGE_SECTIONS: [&str; 6] = [
    "readme",
    "dependencies",
    "dependents",
    "versions",
    "usage",
    "owners",
];

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
    selected: bool,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    button::button(theme, id, label, button::Weight::Quiet)
        .flex_none()
        .selected(selected)
        .toggled(selected)
        .px(px(4.0))
        .when(selected, |tab| {
            tab.border_b(px(2.0))
                .border_color(theme.paint(Paint::Mint))
                .text_color(theme.paint(Paint::Mint))
        })
        .on_click(cx.listener(move |this, _, _, cx| {
            this.select_package_section(fold_key, cx);
        }))
}

fn package_route_tab(
    theme: &Theme,
    id: impl Into<SharedString>,
    label: &'static str,
    route: PackageRoute,
    coordinate: &str,
    authority: RouteAuthority,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let enabled = authority.is_available();
    let reason = authority.reason();
    let coordinate = coordinate.to_owned();
    let mut tab =
        components::button_with_state(theme, id, label, button::Weight::Quiet, !enabled, true)
            .flex_none()
            .tab_index(0)
            .px(px(4.0))
            .when(!enabled, |tab| {
                tab.tooltip(format!("{label} unavailable: {reason}"))
            });
    if enabled {
        tab = tab.on_click(cx.listener(move |this, _, window, cx| {
            let _ = this.open_package_route(coordinate.clone(), route.intent(), window, cx);
        }));
    }
    tab
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RouteAuthority {
    Available,
    Unavailable(&'static str),
}

impl RouteAuthority {
    const fn is_available(self) -> bool {
        matches!(self, Self::Available)
    }

    const fn reason(self) -> &'static str {
        match self {
            Self::Available => "local projection is admitted",
            Self::Unavailable(reason) => reason,
        }
    }
}

fn route_rail_button(
    theme: &Theme,
    id: &'static str,
    label: &'static str,
    route: PackageRoute,
    coordinate: &str,
    authority: RouteAuthority,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let enabled = authority.is_available();
    let coordinate = coordinate.to_owned();
    let mut button =
        components::button_with_state(theme, id, label, components::Weight::Quiet, !enabled, true);
    if enabled {
        button = button.on_click(cx.listener(move |this, _, window, cx| {
            let _ = this.open_package_route(coordinate.clone(), route.intent(), window, cx);
        }));
    } else {
        button = button.tooltip(format!("{label} unavailable: {}", authority.reason()));
    }
    button
}

impl PackageRoute {
    const fn intent(self) -> ReaderIntent {
        match self {
            Self::Documentation => ReaderIntent::Docs,
            Self::Source => ReaderIntent::Source,
            Self::Code => ReaderIntent::Code,
            Self::Search => ReaderIntent::Search,
        }
    }
}

impl Workspace {
    /// Starts each package route at the README and closes stale route bodies.
    /// Package sections are a single navigation strip; the longer metadata
    /// sections remain available through their tabs without competing with
    /// the README hero on first paint.
    pub(super) fn reset_package_navigation(&mut self, cx: &mut Context<Self>) {
        for key in PACKAGE_SECTIONS {
            if key == "readme" {
                if self.is_folded(key) {
                    self.toggle_fold(key, cx);
                }
            } else if !self.is_folded(key) {
                self.toggle_fold(key, cx);
            }
        }
        for key in PACKAGE_ROUTES {
            if self.is_unfurled(key) {
                self.toggle_unfurl(key, cx);
            }
        }
    }

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
            .min_w(px(0.0))
            .flex()
            .flex_col()
            .gap(space(Space::Gutter))
            .child(self.package_header(theme, &dossier, cx))
            .child(self.package_tabs(theme, &dossier, cx))
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
    fn package_tabs(
        &self,
        theme: &Theme,
        dossier: &Dossier,
        cx: &mut Context<Workspace>,
    ) -> impl IntoElement {
        let coordinate = dossier.coordinate().to_owned();
        div()
            .id("package-tabs-scroll")
            .w_full()
            .min_w(px(0.0))
            .overflow_x_scroll()
            .border_b(hairline())
            .border_color(theme.paint(Paint::Rule1))
            .child(
                div()
                    .id("package-tabs")
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(space(Space::Tight))
                    .child(package_tab(
                        theme,
                        "package-tab-readme",
                        "Readme",
                        "readme",
                        !self.is_folded("readme"),
                        cx,
                    ))
                    .child(package_route_tab(
                        theme,
                        "package-tab-docs",
                        "Docs",
                        PackageRoute::Documentation,
                        &coordinate,
                        self.package_route_authority(dossier, PackageRoute::Documentation, cx),
                        cx,
                    ))
                    .child(package_route_tab(
                        theme,
                        "package-tab-source",
                        "Source",
                        PackageRoute::Source,
                        &coordinate,
                        self.package_route_authority(dossier, PackageRoute::Source, cx),
                        cx,
                    ))
                    .child(package_route_tab(
                        theme,
                        "package-tab-code",
                        "Code",
                        PackageRoute::Code,
                        &coordinate,
                        self.package_route_authority(dossier, PackageRoute::Code, cx),
                        cx,
                    ))
                    .child(package_route_tab(
                        theme,
                        "package-tab-search",
                        "Search",
                        PackageRoute::Search,
                        &coordinate,
                        self.package_route_authority(dossier, PackageRoute::Search, cx),
                        cx,
                    ))
                    .child(package_tab(
                        theme,
                        "package-tab-versions",
                        "Versions",
                        "versions",
                        !self.is_folded("versions"),
                        cx,
                    ))
                    .child(package_tab(
                        theme,
                        "package-tab-dependencies",
                        "Dependencies",
                        "dependencies",
                        !self.is_folded("dependencies"),
                        cx,
                    ))
                    .child(package_tab(
                        theme,
                        "package-tab-dependents",
                        "Dependents",
                        "dependents",
                        !self.is_folded("dependents"),
                        cx,
                    ))
                    .child(package_tab(
                        theme,
                        "package-tab-security",
                        "Security",
                        "package-security",
                        self.is_unfurled("package-security"),
                        cx,
                    )),
            )
    }

    /// Checks authority before exposing a route control. Registry metadata is
    /// not semantic authority: a package can be listed by a registry while no
    /// local project/declaration projection exists for the same coordinate.
    /// Keeping this decision here makes a disabled control a precise coverage
    /// statement instead of a speculative navigation attempt.
    fn package_route_authority(
        &self,
        dossier: &Dossier,
        route: PackageRoute,
        cx: &Context<Workspace>,
    ) -> RouteAuthority {
        let coordinate = dossier.coordinate();
        let index = self.index.read(cx);
        let project = index.project(coordinate);
        let symbol = index.symbol_for(coordinate);
        match route {
            PackageRoute::Documentation if symbol.is_some() || project.is_some() => {
                RouteAuthority::Available
            }
            PackageRoute::Documentation => {
                RouteAuthority::Unavailable("the local declaration index has no admitted docs page")
            }
            PackageRoute::Source if symbol.is_some() => RouteAuthority::Available,
            PackageRoute::Source if project.is_some() => RouteAuthority::Unavailable(
                "source requires an admitted declaration, not only a package root",
            ),
            PackageRoute::Source => {
                RouteAuthority::Unavailable("the local declaration index has no source authority")
            }
            PackageRoute::Code | PackageRoute::Search if project.is_some() => {
                RouteAuthority::Available
            }
            PackageRoute::Code => {
                RouteAuthority::Unavailable("the local declaration index has no code outline")
            }
            PackageRoute::Search => {
                RouteAuthority::Unavailable("the local declaration index has no search scope")
            }
        }
    }

    /// Returns the admitted status used by the global Docs disclosure. The
    /// header may be rendered over any reader surface, so it must ask the
    /// same route authority as the package tabs before presenting a handoff.
    pub(super) fn package_docs_status(
        &self,
        dossier: &Dossier,
        cx: &Context<Workspace>,
    ) -> (bool, &'static str) {
        let authority = self.package_route_authority(dossier, PackageRoute::Documentation, cx);
        (authority.is_available(), authority.reason())
    }

    /// Selects one primary package section while keeping its body open.
    fn select_package_section(&mut self, section: &'static str, cx: &mut Context<Self>) {
        for key in PACKAGE_SECTIONS {
            if key == section {
                if self.is_folded(key) {
                    self.toggle_fold(key, cx);
                }
            } else if !self.is_folded(key) {
                self.toggle_fold(key, cx);
            }
        }
        cx.notify();
    }

    /// Returns package-local metadata disclosures. Docs/source/code/search
    /// leave this page through typed reader handoffs; they are never rendered
    /// as a fabricated placeholder inside the registry page.
    fn package_route_sections(
        &self,
        theme: &Theme,
        dossier: &Dossier,
        cx: &mut Context<Workspace>,
    ) -> Div {
        div()
            .w_full()
            .min_w(px(0.0))
            .flex()
            .flex_col()
            .gap(space(Space::Snug))
            .when(self.is_unfurled("package-security"), |body| {
                body.child(self.security_route(theme, dossier, cx))
            })
    }

    fn security_route(&self, theme: &Theme, dossier: &Dossier, cx: &mut Context<Workspace>) -> Div {
        let (provenance, body) = match dossier.releases().state() {
            Loadable::Idle | Loadable::Loading => (
                Provenance::Recorded,
                text::dim(theme)
                    .child("Loading advisory coverage and acquisition decision…")
                    .into_any_element(),
            ),
            Loadable::Faulted(fault) => (
                Provenance::Recorded,
                fault_ui::block(theme, fault, Vec::new()).into_any_element(),
            ),
            Loadable::Ready(_) => dossier.pinned().map_or_else(
                || {
                    (
                        Provenance::NotRecorded,
                        text::dim(theme)
                            .child("Security advisory state is not recorded by the configured registry feed.")
                            .into_any_element(),
                    )
                },
                |release| (Provenance::Recorded, Self::advisory_body(theme, release.advisory())),
            ),
        };
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

    /// Returns the advisory summary and detail owned by the shared product DTO.
    fn advisory_body(theme: &Theme, advisory: &AdvisoryPackageDto) -> AnyElement {
        let decision = match &advisory.decision {
            AcquisitionDecision::Allow => "allow".to_owned(),
            AcquisitionDecision::Warn(reasons) => {
                format!(
                    "warn ({})",
                    reasons
                        .iter()
                        .map(Self::policy_reason_label)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
            AcquisitionDecision::Deny(reasons) => {
                format!(
                    "deny ({})",
                    reasons
                        .iter()
                        .map(Self::policy_reason_label)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        };
        let coverage = format!(
            "coverage: {} · freshness: {}",
            Self::coverage_label(advisory.coverage),
            Self::freshness_label(advisory.freshness)
        );
        let mut details = vec![format!("decision: {decision}"), coverage];
        if advisory.yanked {
            details.push("release: yanked".to_owned());
        }
        if advisory.unlisted {
            details.push("release: unlisted".to_owned());
        }
        if advisory.advisories.is_empty() {
            details.push("No matching advisory object was recorded.".to_owned());
        } else {
            for claim in &advisory.advisories {
                let sources = claim
                    .source_ids
                    .iter()
                    .map(|source| format!("{:?}:{}", source.source, source.id))
                    .collect::<Vec<_>>()
                    .join(", ");
                let aliases = if claim.aliases.is_empty() {
                    "none".to_owned()
                } else {
                    claim.aliases.join(", ")
                };
                let categories = if claim.categories.is_empty() {
                    "unspecified".to_owned()
                } else {
                    claim
                        .categories
                        .iter()
                        .map(|category| format!("{category:?}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                let ranges = claim
                    .affected
                    .iter()
                    .map(|range| format!("{}:{:?}", range.package.name, range.matcher))
                    .collect::<Vec<_>>()
                    .join(", ");
                let fixed = if claim.fixed_ranges.is_empty() {
                    "none".to_owned()
                } else {
                    claim.fixed_ranges.join(", ")
                };
                details.push(format!(
                    "{} · severity {:?} · categories {} · sources {} · aliases {} · affected {} · fixed {}",
                    claim.canonical_id,
                    claim.severity,
                    categories,
                    sources,
                    aliases,
                    if ranges.is_empty() { "unspecified" } else { &ranges },
                    fixed,
                ));
                if claim.statuses.contains(&AdvisoryStatus::Withdrawn) {
                    details.push("advisory status: withdrawn".to_owned());
                }
            }
        }
        div()
            .flex()
            .flex_col()
            .gap(space(Space::Tight))
            .children(
                details.into_iter().map(|detail| {
                    text::text_at(theme, TypeScale::Body, Paint::Silver1).child(detail)
                }),
            )
            .into_any_element()
    }

    fn policy_reason_label(reason: &backend_library::PolicyReason) -> &'static str {
        match reason {
            backend_library::PolicyReason::Advisory(status) => match status {
                AdvisoryStatus::Yanked => "yanked",
                AdvisoryStatus::Unlisted => "unlisted",
                AdvisoryStatus::Vulnerable => "vulnerable",
                AdvisoryStatus::Malicious => "malicious",
                AdvisoryStatus::Unmaintained => "unmaintained",
                AdvisoryStatus::Unsound => "unsound",
                AdvisoryStatus::Withdrawn => "withdrawn",
                AdvisoryStatus::UnknownCoverage => "unknown-coverage",
                AdvisoryStatus::Stale => "stale",
                AdvisoryStatus::Unavailable => "unavailable",
            },
            backend_library::PolicyReason::IncompleteCoverage => "incomplete-coverage",
            backend_library::PolicyReason::UnavailableEvidence => "unavailable-evidence",
            backend_library::PolicyReason::StaleEvidence => "stale-evidence",
            backend_library::PolicyReason::NoCachedEvidence => "no-cached-evidence",
            backend_library::PolicyReason::OverrideAccepted(_) => "override-accepted",
            backend_library::PolicyReason::CachedEvidenceAllowed => "cached-evidence-allowed",
        }
    }

    fn coverage_label(coverage: backend_library::AdvisoryCoverage) -> &'static str {
        match coverage {
            backend_library::AdvisoryCoverage::Complete => "complete",
            backend_library::AdvisoryCoverage::Partial => "partial",
            backend_library::AdvisoryCoverage::Unknown => "unknown",
            backend_library::AdvisoryCoverage::Unavailable => "unavailable",
        }
    }

    fn freshness_label(freshness: FreshnessState) -> &'static str {
        match freshness {
            FreshnessState::Fresh => "fresh",
            FreshnessState::NotModified => "not-modified",
            FreshnessState::Stale => "stale",
            FreshnessState::Unknown => "unknown",
        }
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
            .min_w(px(0.0))
            .flex()
            .flex_wrap()
            .items_start()
            .gap(space(Space::Gutter))
            .child(
                div().flex_1().min_w(px(300.0)).child(
                    surface::sunken(theme)
                        .w_full()
                        .p(space(Space::Room))
                        .child(self.readme_section(theme, dossier, cx)),
                ),
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
        let docs_authority = self.package_route_authority(dossier, PackageRoute::Documentation, cx);
        let source_authority = self.package_route_authority(dossier, PackageRoute::Source, cx);
        let status = release.map_or_else(
            || "release status not recorded".to_owned(),
            |release| {
                format!(
                    "{} · {}",
                    release.standing().label(),
                    Self::advisory_decision_label(release.advisory())
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
            .min_w(px(0.0))
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
                    .child(route_rail_button(
                        theme,
                        "package-rail-docs",
                        "Open docs route",
                        PackageRoute::Documentation,
                        dossier.coordinate(),
                        docs_authority,
                        cx,
                    ))
                    .child(route_rail_button(
                        theme,
                        "package-rail-source",
                        "Open source route",
                        PackageRoute::Source,
                        dossier.coordinate(),
                        source_authority,
                        cx,
                    )),
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
        surface::cut(theme, Paint::Mint)
            .w_full()
            .min_w(px(0.0))
            .p(space(Space::Room))
            .child(
                div()
                    .w_full()
                    .min_w(px(0.0))
                    .flex()
                    .flex_col()
                    .gap(space(Space::Snug))
                    .child(
                        text::single_line(text::identity_text(theme, TypeScale::Small))
                            .child(coordinate.clone()),
                    )
                    .child(Self::title_row(theme, dossier, unfurled, cx))
                    .child(Self::package_header_controls(theme, dossier, cx))
                    .when(unfurled, |header| {
                        header.child(self.version_menu(theme, dossier, cx))
                    })
                    .when_some(
                        precis.filter(|_| !dossier.precis().provenance().is_not_recorded()),
                        |header, precis| {
                            header.child(Self::precis_block(theme, dossier.precis(), precis))
                        },
                    )
                    .child(Self::meta_row(theme, dossier))
                    .child(Self::package_fact_row(theme, dossier))
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
                    )),
            )
    }

    /// Returns a compact registry coverage strip. This keeps the hierarchy
    /// close to a registry package page while making every missing field a
    /// visible, typed state rather than a guessed zero or empty list.
    fn package_fact_row(theme: &Theme, dossier: &Dossier) -> Div {
        let (versions, versions_recorded) =
            section_fact(dossier.history(), |history| history.versions().to_string());
        let (downloads, downloads_recorded) = section_fact(dossier.downloads(), |downloads| {
            dossier::tally_label(downloads.total())
        });
        let (dependencies, dependencies_recorded) =
            section_fact(dossier.dependencies(), |dependencies| {
                dependencies.len().to_string()
            });
        let (dependents, dependents_recorded) =
            section_fact(dossier.dependents(), |reverse| match reverse {
                Reverse::Recorded(dependents) => dependents.len().to_string(),
                Reverse::NotRecorded(_) => "not recorded".to_owned(),
            });
        let (owners, owners_recorded) =
            section_fact(dossier.owners(), |owners| owners.len().to_string());
        let security = dossier
            .pinned()
            .map(|release| Self::advisory_decision_label(release.advisory()))
            .unwrap_or_else(|| "not recorded".to_owned());
        let security_recorded = dossier.pinned().is_some();
        let ecosystem = dossier
            .ecosystem()
            .map(super::library::registry_name)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| "not recorded".to_owned());
        let ecosystem_recorded = dossier.ecosystem().is_some();

        div()
            .flex()
            .flex_wrap()
            .gap(space(Space::Tight))
            .child(chip::fact_chip(
                theme,
                "Versions",
                &versions,
                versions_recorded,
            ))
            .child(chip::fact_chip(
                theme,
                "Downloads",
                &downloads,
                downloads_recorded,
            ))
            .child(chip::fact_chip(
                theme,
                "Dependencies",
                &dependencies,
                dependencies_recorded,
            ))
            .child(chip::fact_chip(
                theme,
                "Dependents",
                &dependents,
                dependents_recorded,
            ))
            .child(chip::fact_chip(theme, "Owners", &owners, owners_recorded))
            .child(chip::fact_chip(
                theme,
                "Security",
                &security,
                security_recorded,
            ))
            .child(chip::fact_chip(
                theme,
                "Registry",
                &ecosystem,
                ecosystem_recorded,
            ))
            .child(chip::fact_chip(theme, "Features", "not recorded", false))
            .child(chip::fact_chip(theme, "Platforms", "not recorded", false))
    }

    /// Small, truthful controls beside the package identity. Registry feeds
    /// often omit platform and feature metadata; keeping those controls
    /// interactive lets a reader ask for the fact and receive an explicit
    /// absence instead of a misleading hard-coded Rust label.
    fn package_header_controls(
        theme: &Theme,
        dossier: &Dossier,
        cx: &mut Context<Workspace>,
    ) -> Div {
        let ecosystem = dossier
            .ecosystem()
            .map(super::library::registry_name)
            .unwrap_or("registry metadata unavailable");
        let language = dossier
            .ecosystem()
            .map(super::home::ecosystem_language)
            .unwrap_or(Language::Unknown);
        let language_label = crate::theme::language::label(language);
        let platform = format!("Platform · {ecosystem}");
        let feature_notice = "Feature flags are not recorded by this registry feed";
        let platform_notice = format!("Platform metadata is not recorded beyond {ecosystem}");
        let language_notice = format!("Source language inferred from ecosystem: {language_label}");
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(space(Space::Tight))
            .child(
                components::button_with_state(
                    theme,
                    "package-platform",
                    &platform,
                    components::Weight::Quiet,
                    dossier.ecosystem().is_none(),
                    true,
                )
                .tooltip(if dossier.ecosystem().is_some() {
                    format!("Target metadata is not recorded beyond {ecosystem}")
                } else {
                    "Platform metadata is not recorded by this registry feed".to_owned()
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.shell
                        .update(cx, |shell, cx| shell.notify(platform_notice.clone(), cx));
                })),
            )
            .child(
                components::button_with_state(
                    theme,
                    "package-features",
                    "Feature flags",
                    components::Weight::Quiet,
                    true,
                    true,
                )
                .tooltip(feature_notice)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.shell
                        .update(cx, |shell, cx| shell.notify(feature_notice, cx));
                })),
            )
            .child(
                components::button_with_state(
                    theme,
                    "package-language",
                    language_label,
                    components::Weight::Quiet,
                    matches!(language, Language::Unknown),
                    true,
                )
                .tooltip("Source language is inferred from recorded ecosystem metadata")
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.shell
                        .update(cx, |shell, cx| shell.notify(language_notice.clone(), cx));
                })),
            )
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
            .when_some(Self::history_chip(theme, dossier), |row, badge| {
                row.child(badge)
            })
            .when_some(
                standing.filter(|standing| *standing != ReleaseStanding::Published),
                |row, standing| {
                    row.child(
                        div()
                            .flex_none()
                            .px(px(5.0))
                            .py(px(1.0))
                            .bg(theme.paint(Paint::Stopped))
                            .text_size(type_size(TypeScale::Micro))
                            .text_color(theme.paint(Paint::Abyss0))
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
        let count = match dossier.history().state() {
            Loadable::Ready(history) if !dossier.history().provenance().is_not_recorded() => {
                Some(history.versions().to_string())
            }
            Loadable::Loading => Some("loading".to_owned()),
            Loadable::Faulted(_) => Some("unavailable".to_owned()),
            Loadable::Idle | Loadable::Ready(_) => None,
        };
        let mut label = dossier.spelling().version().to_owned();
        if let Some(count) = count {
            label.push_str(&format!(" of {count}"));
        }
        components::button_with_state(
            theme,
            "package-version-badge",
            label,
            button::Weight::Quiet,
            false,
            true,
        )
        .flex_none()
        .selected(unfurled)
        .toggled(unfurled)
        .font_family(theme.specimen())
        .tooltip(if unfurled {
            "Hide release history"
        } else {
            "Show release history"
        })
        .child(icon::sized(
            theme,
            if unfurled {
                Icon::ChevronDown
            } else {
                Icon::ChevronRight
            },
            11.0,
            Paint::Silver3,
        ))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.toggle_unfurl(&key, cx);
        }))
        .into_any_element()
    }

    /// Draws only a count whose profile authority actually recorded one. A
    /// missing profile must not collapse into the very different fact "zero
    /// versions"; loading and failure remain visible as their own states.
    fn history_chip(theme: &Theme, dossier: &Dossier) -> Option<Div> {
        match dossier.history().state() {
            Loadable::Ready(history) if !dossier.history().provenance().is_not_recorded() => {
                Some(chip::count_chip(theme, history.versions(), "versions"))
            }
            Loadable::Loading => Some(chip::badge(theme, "versions loading")),
            Loadable::Faulted(_) => Some(chip::badge(theme, "versions unavailable")),
            Loadable::Idle | Loadable::Ready(_) => None,
        }
    }

    /// Returns the version picker: every release, newest first (virtualized).
    fn version_menu(&mut self, theme: &Theme, dossier: &Dossier, cx: &mut Context<Self>) -> Div {
        let current = dossier.spelling().version().to_owned();
        let releases = dossier.releases().ready().cloned().unwrap_or_default();
        let order = release_order(&releases, dossier.ecosystem());
        let rows: Arc<[Release]> = releases.into();
        let entity = cx.entity();
        let theme = theme.clone();
        let row_theme = theme.clone();
        let list = if rows.is_empty() {
            div()
                .w_full()
                .child(text::dim(&theme).child(version_coverage_label(dossier)))
                .into_any_element()
        } else {
            uniform_list(
                "package-version-picker",
                order.len(),
                move |range, _window, _cx| {
                    order[range]
                        .iter()
                        .filter_map(|index| {
                            rows.get(*index).map(|release| {
                                version_row(
                                    &row_theme,
                                    "picker",
                                    release,
                                    release.version() == current,
                                    &entity,
                                )
                            })
                        })
                        .collect()
                },
            )
            .track_scroll(&self.package_version_scroll)
            .h(px(248.0))
            .into_any_element()
        };
        surface::sunken(&theme)
            .w_full()
            .max_h(px(270.0))
            .p(space(Space::Snug))
            .flex()
            .flex_col()
            .gap(px(1.0))
            .child(list)
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
                        text::text_at(theme, TypeScale::Body, Paint::Silver1)
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
            facts.push(Self::advisory_decision_label(release.advisory()));
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

    fn advisory_decision_label(advisory: &AdvisoryPackageDto) -> String {
        match &advisory.decision {
            AcquisitionDecision::Allow => "security allow".to_owned(),
            AcquisitionDecision::Warn(_) => "security warn".to_owned(),
            AcquisitionDecision::Deny(_) => "security deny".to_owned(),
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
        .child(icon::sized(theme, Icon::External, 11.0, Paint::Silver3))
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
                        &theme,
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
                        .text_color(theme.paint(Paint::Waiting))
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
            text::text_at(theme, TypeScale::Small, Paint::Silver1)
                .flex_1()
                .min_w(px(0.0))
                .child(value.into()),
        )
}

/// Summarizes a dossier section without collapsing loading, fault, and feed
/// coverage into the same value. The closure only runs for an admitted ready
/// value, which keeps callers from accidentally reading an unrecorded field.
fn section_fact<T, F>(section: &Section<T>, ready: F) -> (String, bool)
where
    F: FnOnce(&T) -> String,
{
    match section.state() {
        Loadable::Ready(value) if !section.provenance().is_not_recorded() => (ready(value), true),
        Loadable::Ready(_) => ("not recorded".to_owned(), false),
        Loadable::Loading => ("loading".to_owned(), false),
        Loadable::Faulted(_) => ("unavailable".to_owned(), false),
        Loadable::Idle => ("not loaded".to_owned(), false),
    }
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
                    .map(|(_, row)| dependency_row(theme, row, cx)),
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
                    .map(|(_, row)| dependent_row(theme, row, cx)),
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
        &mut self,
        theme: &Theme,
        dossier: &Dossier,
        cx: &mut Context<Self>,
    ) -> Div {
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
            Loadable::Ready(rows) => self.version_list(theme, rows, &current, unfurled, cx),
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
        &mut self,
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
        let order = release_order(rows, None);
        let rows: Arc<[Release]> = rows.to_owned().into();
        let entity = cx.entity();
        let theme = theme.clone();
        let row_theme = theme.clone();
        let current = current.to_owned();
        let list = uniform_list("package-version-list", shown, move |range, _window, _cx| {
            order[..shown]
                .get(range)
                .unwrap_or_default()
                .iter()
                .filter_map(|index| {
                    rows.get(*index).map(|release| {
                        version_row(
                            &row_theme,
                            "list",
                            release,
                            release.version() == current,
                            &entity,
                        )
                    })
                })
                .collect()
        })
        .track_scroll(&self.package_version_scroll)
        .h(px((shown.min(BUDGET).max(1) as f32 * 30.0).min(420.0)));
        div()
            .flex()
            .flex_col()
            .gap(px(1.0))
            .child(list)
            .when(rest > 0, |list| {
                list.child(
                    button::button(
                        &theme,
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
) -> impl IntoElement {
    let label = title.to_owned();
    let mut head = components::button_with_state(
        theme,
        format!("package-head-{key}"),
        label.clone(),
        button::Weight::Quiet,
        false,
        true,
    )
    .w_full()
    .justify_start()
    .selected(!folded)
    .toggled(!folded)
    .child(icon::sized(
        theme,
        if folded {
            Icon::ChevronRight
        } else {
            Icon::ChevronDown
        },
        12.0,
        Paint::Silver3,
    ));
    if let Some(note) = note {
        head = head.child(text::faint(theme).flex_none().child(note));
    }
    if provenance.tag().is_some() {
        head = head.child(chart::sample_tag(theme));
    }
    head = head
        .child(div().flex_1().h(hairline()).bg(theme.paint(Paint::Rule1)))
        .tooltip(if folded {
            format!("Expand {label}")
        } else {
            format!("Collapse {label}")
        })
        .on_click(cx.listener(move |this, _, _, cx| {
            this.toggle_fold(key, cx);
        }));
    head
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
    release: &Release,
    current: bool,
    workspace: &gpui::Entity<Workspace>,
) -> AnyElement {
    let opened = release.coordinate().to_owned();
    let withdrawn = release.standing() != ReleaseStanding::Published;
    let mut label = format!(
        "{} · {} · {}",
        release.version(),
        release
            .published()
            .map_or_else(|| "date not recorded".to_owned(), Stamp::iso),
        release.size()
    );
    if withdrawn {
        label.push_str(" · ");
        label.push_str(release.standing().label());
    }
    if current {
        label.push_str(" · current");
    }
    components::button_with_state(
        theme,
        format!("package-{list}-release-{}", release.coordinate()),
        label.clone(),
        button::Weight::Quiet,
        false,
        true,
    )
    .w_full()
    .justify_start()
    .text_left()
    .selected(current)
    .tooltip(format!("Open package release {}", release.version()))
    .on_click({
        let workspace = workspace.clone();
        move |_, _, cx| {
            let _ = workspace.update(cx, |this, cx| {
                this.open_package(opened.clone(), Target::Here, cx);
            });
        }
    })
    .into_any_element()
}

/// Returns one dependency row, which opens the package it names.
fn dependency_row(theme: &Theme, row: &Dependency, cx: &mut Context<Workspace>) -> AnyElement {
    let opened = row.resolved().to_owned();
    let role = row
        .role()
        .tag()
        .map_or(String::new(), |tag| format!(" · {tag}"));
    let label = format!("{} · {}{role}", row.name(), row.requirement());
    components::button_with_state(
        theme,
        format!("package-dependency-{}", row.resolved()),
        label.clone(),
        button::Weight::Quiet,
        false,
        true,
    )
    .w_full()
    .justify_start()
    .text_left()
    .tooltip(format!("Open dependency {}", row.resolved()))
    .on_click(cx.listener(move |this, _, _, cx| {
        this.open_package(opened.clone(), Target::Here, cx);
    }))
    .into_any_element()
}

/// Returns one dependent row, with its download weight when one is published.
fn dependent_row(theme: &Theme, row: &Dependent, cx: &mut Context<Workspace>) -> AnyElement {
    let opened = row.coordinate().to_owned();
    let downloads = row.downloads().map_or(String::new(), |count| {
        format!(" · {}", dossier::tally_label(count))
    });
    let label = format!("{} · {}{downloads}", row.name(), row.version());
    components::button_with_state(
        theme,
        format!("package-dependent-{}", row.coordinate()),
        label,
        button::Weight::Quiet,
        false,
        true,
    )
    .w_full()
    .justify_start()
    .text_left()
    .tooltip(format!("Open dependent {}", row.coordinate()))
    .on_click(cx.listener(move |this, _, _, cx| {
        this.open_package(opened.clone(), Target::Here, cx);
    }))
    .into_any_element()
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
                .border(hairline())
                .border_color(theme.paint(Paint::Rule1))
                .child(
                    text::text_at(theme, TypeScale::Small, Paint::Silver1)
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
        || theme.paint(Paint::Silver3),
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
                .text_color(theme.paint(Paint::Silver1))
                .child(body.to_owned()),
        )
        .into_any_element()
}

/// Returns the unfurl key the version picker on one coordinate toggles.
fn picker_key(coordinate: &str) -> String {
    format!("package-picker-{coordinate}")
}

/// Returns a stable semantic version order without changing the feed's
/// immutable dossier. Numeric components sort numerically, prereleases sort
/// before their stable release, and opaque registry versions retain a stable
/// lexical fallback. This handles Cargo/npm/PyPI/NuGet/Go-style spellings
/// without pretending Maven/Conan versions are all semver.
fn release_order(rows: &[Release], _ecosystem: Option<RegistryEcosystem>) -> Vec<usize> {
    let mut order: Vec<_> = (0..rows.len()).collect();
    order.sort_by(|left, right| {
        compare_versions(rows[*right].version(), rows[*left].version())
            .then_with(|| rows[*right].coordinate().cmp(rows[*left].coordinate()))
    });
    order
}

fn compare_versions(left: &str, right: &str) -> Ordering {
    let left_key = VersionKey::parse(left);
    let right_key = VersionKey::parse(right);
    compare_numeric_components(&left_key.numbers, &right_key.numbers)
        .then_with(|| match (&left_key.pre, &right_key.pre) {
            (None, None) => Ordering::Equal,
            // A release without a prerelease tag is newer than the same core
            // release with one. `Option`'s derived ordering is the opposite.
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(left), Some(right)) => left.cmp(right),
        })
        .then_with(|| left.cmp(right))
}

fn compare_numeric_components(left: &[u64], right: &[u64]) -> Ordering {
    let width = left.len().max(right.len());
    (0..width)
        .map(|index| {
            left.get(index)
                .copied()
                .unwrap_or_default()
                .cmp(&right.get(index).copied().unwrap_or_default())
        })
        .find(|ordering| *ordering != Ordering::Equal)
        .unwrap_or(Ordering::Equal)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct VersionKey {
    numbers: Vec<u64>,
    pre: Option<Vec<PrePart>>,
    raw: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum PrePart {
    Number(u64),
    Text(String),
}

impl VersionKey {
    fn parse(raw: &str) -> Self {
        let trimmed = raw.trim().trim_start_matches(['v', 'V']);
        let (core, pre) = trimmed
            .split_once('-')
            .map_or((trimmed, None), |(core, pre)| {
                let parts = pre
                    .split('.')
                    .map(|part| {
                        part.parse::<u64>()
                            .map_or_else(|_| PrePart::Text(part.to_owned()), PrePart::Number)
                    })
                    .collect();
                (core, Some(parts))
            });
        let numbers = core
            .split('.')
            .map_while(|part| part.parse::<u64>().ok())
            .collect();
        Self {
            numbers,
            pre,
            raw: raw.to_owned(),
        }
    }
}

fn version_coverage_label(dossier: &Dossier) -> &'static str {
    match dossier.releases().state() {
        Loadable::Idle => "Version history has not been requested.",
        Loadable::Loading => "Loading recorded releases…",
        Loadable::Faulted(_) => "Version history is unavailable from the local service.",
        Loadable::Ready(rows) if rows.is_empty() => {
            if dossier.releases().provenance().is_not_recorded() {
                "Version history is not recorded by the configured registry feed."
            } else {
                "The local index recorded no releases for this package."
            }
        }
        Loadable::Ready(_) => "No version rows are currently visible.",
    }
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
    use super::{
        InstallCapability, PACKAGE_ROUTES, PACKAGE_SECTIONS, compare_versions, install_command,
    };
    use crate::store::dossier::PackageRoute;
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
    fn package_capability_tabs_are_internal_and_stable() {
        assert!(
            PACKAGE_ROUTES
                .iter()
                .all(|route| route.starts_with("package-") && !route.contains("://"))
        );
        assert_eq!(PACKAGE_SECTIONS[0], "readme");
        assert_eq!(PackageRoute::Documentation.key(), "package-docs");
        assert_eq!(PackageRoute::Source.key(), "package-source");
        assert_eq!(PackageRoute::Code.key(), "package-code");
        assert_eq!(PackageRoute::Search.key(), "package-search");
        assert_eq!(PackageRoute::Documentation.title(), "Documentation");
        assert_eq!(PackageRoute::Source.title(), "Source");
        assert_eq!(PackageRoute::Code.title(), "Code");
        assert_eq!(PackageRoute::Search.title(), "Search");
    }

    #[test]
    fn release_order_is_semantic_and_prereleases_precede_stable() {
        assert_eq!(
            compare_versions("1.10.0", "1.9.99"),
            std::cmp::Ordering::Greater
        );
        assert_eq!(
            compare_versions("v2.0.0", "1.99.99"),
            std::cmp::Ordering::Greater
        );
        assert_eq!(
            compare_versions("1.0.0", "1.0.0-alpha.1"),
            std::cmp::Ordering::Greater
        );
        assert_eq!(
            compare_versions("1.0.0-alpha.2", "1.0.0-alpha.10"),
            std::cmp::Ordering::Less
        );
    }
}

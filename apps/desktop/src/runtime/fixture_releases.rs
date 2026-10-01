//! The **fixture** releases: the upgrade lens's data, from facet's release
//! fixture (a slice of the prototype's `releases.json`: **toml and smallvec
//! only**, with this workspace's uses of each).
//!
//! It is a stand-in until the index serves release diffs (W-Browse plans
//! them). Any other package's comb shows no lens: scrubbing still views the
//! release, the lens simply has nothing to compare. The fixture is parsed
//! once, off the UI thread ([`Memo`]), the first time a lens asks; the view
//! that asked redraws when it lands.
//!
//! Callers ask for a package's [`Crate`] through [`release_data_for`] (or
//! [`release_data`], which redraws every window); none of them names the
//! fixture, so index-served diffs replace this module and nothing else.

use crate::model::pages::PackageRef;
use crate::runtime::offload::{Answer, Asker, Memo};
use facet::data::release::Crate;
use gpui::{App, Context, Global};
use std::num::NonZeroUsize;

/// The one thing this module keeps: the parsed fixture.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Fixture {
    Releases,
}

/// The parsed fixture, off the UI thread.
struct Releases(Memo<Fixture, &'static [Crate]>);

impl Global for Releases {}

impl Releases {
    fn parsing() -> Self {
        Self(Memo::new(NonZeroUsize::MIN, |Fixture::Releases| {
            facet::data::release::fixture::crates()
        }))
    }
}

/// The release data for `package` (matched by its registry name), when the
/// fixture has it and has landed. `None` while it loads (the first ask starts
/// it) and for every package outside the fixture. Redraws every window when
/// the fixture lands: a caller with a `Context` uses [`release_data_for`].
pub(crate) fn release_data(package: &PackageRef, cx: &mut App) -> Option<&'static Crate> {
    release_data_asked_by(package, Asker::Everyone, cx)
}

/// [`release_data`] for the view `cx` belongs to: only it redraws when the
/// fixture lands.
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the shell's lens callers move to it (MIGRATE.md, R-Open3); delete this allow with that move"
    )
)]
pub(crate) fn release_data_for<T: 'static>(
    package: &PackageRef,
    cx: &mut Context<T>,
) -> Option<&'static Crate> {
    let asker = Asker::View(cx.entity_id());
    release_data_asked_by(package, asker, cx)
}

fn release_data_asked_by(
    package: &PackageRef,
    asker: Asker,
    cx: &mut App,
) -> Option<&'static Crate> {
    if cx.try_global::<Releases>().is_none() {
        cx.set_global(Releases::parsing());
    }
    let memo = cx.global::<Releases>().0.clone();
    let Answer::Ready(crates) = memo.ask(&Fixture::Releases, asker, cx) else {
        return None;
    };
    if package.is_local() {
        return None;
    }
    let name = package.display_name();
    crates.iter().find(|release| release.name.as_ref() == name)
}

/// `version` as `release` spells it: the registry, the comb and the route
/// say `1.1.6`, the release data may carry build metadata
/// (`1.1.6+spec-1.1.0`). `None` when the data has no such release.
pub(crate) fn spelled(release: &Crate, version: &str) -> Option<gpui::SharedString> {
    release
        .versions
        .iter()
        .find(|known| {
            known.v.as_ref() == version || facet::data::release::short(&known.v) == version
        })
        .map(|known| known.v.clone())
}

/// Installs the parsed fixture (tests; parsing is synchronous there).
#[cfg(test)]
pub(crate) fn install(cx: &mut App) {
    let releases = Releases::parsing();
    releases
        .0
        .seed(Fixture::Releases, facet::data::release::fixture::crates());
    cx.set_global(releases);
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use gpui::{
        AppContext as _, Entity, IntoElement, ParentElement, Render, StyleRefinement,
        TestAppContext, VisualTestContext, Window, div,
    };
    use std::cell::Cell;
    use std::rc::Rc;

    /// A view that shows whether the fixture knows `toml`, and counts renders.
    struct Lens {
        renders: Rc<Cell<u32>>,
        knows: Rc<Cell<bool>>,
    }

    impl Render for Lens {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.renders.set(self.renders.get() + 1);
            let package = PackageRef::parse("pkg:cargo/toml@0.8.23").expect("a registry package");
            self.knows.set(release_data_for(&package, cx).is_some());
            div().child("lens")
        }
    }

    /// The root holds the lens as a cached region, the way the shell does.
    struct Frame {
        child: Entity<Lens>,
    }

    impl Render for Frame {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().child(self.child.clone().cached(StyleRefinement::default()))
        }
    }

    #[gpui::test]
    fn the_fixture_lands_in_the_lens_that_asked_and_the_package_it_knows_has_its_releases(
        cx: &mut TestAppContext,
    ) {
        let (renders, knows) = (Rc::new(Cell::new(0)), Rc::new(Cell::new(false)));
        let (counted, seen) = (Rc::clone(&renders), Rc::clone(&knows));
        let window = cx.update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), move |_, cx| {
                let child = cx.new(|_| Lens {
                    renders: counted,
                    knows: seen,
                });
                cx.new(|_| Frame { child })
            })
            .expect("window")
        });
        let visual = VisualTestContext::from_window(window.into(), cx).into_mut();
        assert!(!knows.get(), "the first frame does not wait for the parse");
        for _ in 0..4 {
            visual.run_until_parked();
            visual.update(|window, cx| window.draw(cx).clear(cx));
        }
        assert!(knows.get(), "the fixture landed, and toml is in it");
        assert_eq!(
            renders.get(),
            2,
            "the lens drew twice: once before the fixture, once with it"
        );
        assert!(
            cx.update(|cx| release_data(
                &PackageRef::parse("pkg:cargo/not-in-the-fixture@1.0.0").expect("a package"),
                cx
            ))
            .is_none(),
            "a package outside the fixture has no releases"
        );
    }
}

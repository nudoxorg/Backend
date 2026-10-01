//! The Library: a project's tree, grouped by the role each dependency plays.
//!
//! One thing speaks: the project's name and its one sentence ("Your 44
//! packages lean on 75 others directly, and 884 in all."). Under it, what
//! needs you (an unmaintained crate, in amber), then the roles in one or two
//! columns, then the packages that are here twice. Rows at rest are a mark, a
//! name and at most one quiet descriptor; why a dependency plays its role
//! waits in a tip, and how a duplicate got here unfolds in place.
//!
//! The model is plain words: the product spells every sentence
//! (`backend-present`), so the page shows what the CLI prints.

use crate::controls::button::button;
use crate::fluid::Modes;
use crate::icons::{Icon, IconSize, Kind, KindSize, kind_mark, ui};
use crate::measure::{Control, Measure, Set, Space};
use crate::motion::Flow;
use crate::overlay::float::{self, FloatKind, FloatRequest};
use crate::overlay::tooltip::Tipped;
use crate::probe::{self, TextOverflow};
use crate::theme::ActiveFacet;
use crate::tokens::fluid::ROLES;
use crate::tokens::{Palette, TypeRole, ty};
use gpui::{
    AnyElement, App, ElementId, FocusHandle, Hsla, InteractiveElement, IntoElement, ListAlignment, ListOffset, ListState,
    ParentElement, Pixels, RenderOnce, SharedString, StatefulInteractiveElement, Styled, Window,
    div, list, px,
};
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

/// One source-backed release that a dependency row can offer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleaseLink {
    /// Stable identity of this exact release within its dependency row.
    pub key: SharedString,
    /// Exact version as the owner recorded it.
    pub version: SharedString,
    /// An admitted package coordinate when this source can be opened.
    pub target: Option<ReleaseHandle>,
    /// Explicit reason when the tree cannot open this source.
    pub unavailable: Option<SharedString>,
    /// Full exact source spelling when equal visible versions need disambiguation.
    pub source_detail: Option<SharedString>,
}

/// An action address into the immutable tree that produced one rendered page.
/// It is never interpreted as a package coordinate by the component.
#[derive(Clone, Debug)]
pub struct ReleaseHandle {
    role: usize,
    row: usize,
    release: usize,
    identity: ReleaseIdentity,
}

impl PartialEq for ReleaseHandle {
    fn eq(&self, other: &Self) -> bool {
        self.identity == other.identity
    }
}

impl Eq for ReleaseHandle {}

impl std::hash::Hash for ReleaseHandle {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::hash::Hash::hash(&self.identity, state);
    }
}

/// A source release's stable identity across row and source order changes.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ReleaseIdentity {
    role: SharedString,
    row: SharedString,
    release: SharedString,
}

impl ReleaseHandle {
    /// Positional test helper; production releases use exact source keys.
    #[cfg(test)]
    #[must_use]
    pub fn new(role: usize, row: usize, release: usize) -> Self {
        Self::identified(role, row, release,
            format!("role-{role}"), format!("row-{row}"), format!("release-{release}"))
    }

    /// Binds current positions to an exact role, row, and source release.
    #[must_use]
    pub fn identified(
        role: usize,
        row: usize,
        release: usize,
        role_key: impl Into<SharedString>,
        row_key: impl Into<SharedString>,
        release_key: impl Into<SharedString>,
    ) -> Self {
        Self {
            role, row, release,
            identity: ReleaseIdentity {
                role: role_key.into(), row: row_key.into(), release: release_key.into(),
            },
        }
    }

    /// The positions to resolve against that immutable tree's typed links.
    #[must_use]
    pub const fn positions(&self) -> (usize, usize, usize) {
        (self.role, self.row, self.release)
    }

    /// The exact identities represented by these positions.
    #[must_use]
    pub fn keys(&self) -> (&str, &str, &str) {
        (self.identity.role.as_ref(), self.identity.row.as_ref(), self.identity.release.as_ref())
    }
}

/// Navigation supplied by the desktop; the shared facet never parses paths.
#[derive(Clone)]
pub struct Actions {
    pub open_package: Rc<dyn Fn(ReleaseHandle, &mut Window, &mut App)>,
}

/// How loudly an alert speaks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Tone {
    /// A state to know about: unmaintained, a notice (amber).
    Warn,
    /// A fault: a vulnerability, malware, unsoundness (coral).
    Fault,
}

/// One advisory that affects the tree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Alert {
    /// "bincode 1.3.3 is unmaintained".
    pub title: SharedString,
    /// The advisory (`RUSTSEC-2025-0141`) and its own title, for the card.
    pub advisory: SharedString,
    /// How it got into the tree, for the card.
    pub path: SharedString,
    /// How loudly it speaks.
    pub tone: Tone,
}

/// One direct dependency.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Row {
    /// Exact stable row identity; never its current ordinal.
    pub key: SharedString,
    /// Package name.
    pub name: SharedString,
    /// The one quiet descriptor at rest ("tests only", "twice · 0.22.1 · 0.23.1").
    pub at_rest: Option<SharedString>,
    /// Why it plays its role and who uses it, for the card rested open.
    pub why: SharedString,
    /// Its own one-line description, the card's one sentence.
    pub about: Option<SharedString>,
    /// Source-backed destinations for every resolved version.
    pub releases: Vec<ReleaseLink>,
}

/// One role and its dependencies.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Role {
    /// Stable role identity; never its current ordinal.
    pub key: SharedString,
    /// Icon chosen from the producer's typed dependency purpose.
    pub icon: Icon,
    /// "speaks formats".
    pub label: SharedString,
    /// "for engine, store and advisory".
    pub serving: Option<SharedString>,
    /// Its direct dependencies.
    pub rows: Vec<Row>,
    /// "and 15 crates that come with them".
    pub brings: Option<SharedString>,
}

/// A package present at more than one version.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Twice {
    /// Package name.
    pub name: SharedString,
    /// Each copy's version, and whether it is yours.
    pub copies: Vec<(SharedString, bool)>,
    /// Each copy's path from your code.
    pub paths: Vec<SharedString>,
    /// What moving would do.
    pub verdict: SharedString,
}

/// Everything the Library shows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Model {
    /// The project's name.
    pub name: SharedString,
    /// Its one sentence.
    pub lede: SharedString,
    /// Rested on lockfile rows inactive for the current target/features resolution.
    pub lede_tip: Option<SharedString>,
    /// How the tree was read, when not by Cargo for this machine.
    pub note: Option<SharedString>,
    /// What affects the tree.
    pub alerts: Vec<Alert>,
    /// Quiet facts after the alerts ("60 crates are here twice", advisory coverage).
    pub facts: Vec<SharedString>,
    /// The roles, in order.
    pub roles: Vec<Role>,
    /// "Here twice", then "60 crates appear at more than one version", when any do.
    pub twice_heading: Option<(SharedString, SharedString)>,
    /// The packages here twice, most actionable first.
    pub twice: Vec<Twice>,
}

/// How many duplicates show before "and N more".
pub const TWICE_AT_REST: usize = 8;
/// Maximum release controls mounted inside one expanded package row.
const RELEASE_WINDOW: usize = 12;

/// Bounded interaction state for one exact project tree and owner revision.
/// The Reader retains this through Back/Forward; the component borrows it.
#[derive(Default)]
pub struct State {
    lists: BTreeMap<SharedString, ListCache>,
    open: VecDeque<SharedString>,
    all_alerts: bool,
    focuses: VecDeque<(ReleaseIdentity, FocusHandle)>,
    release_pages: VecDeque<(SharedString, usize)>,
    return_focus: Option<(ReleaseIdentity, u64)>,
}

struct ListCache {
    state: ListState,
    width: Pixels,
    scale: f32,
}

impl State {
    /// Remembers the exact release that opened the next page. Its button is
    /// focused only if this same tree is painted at a later Reader place.
    pub fn remember_open(&mut self, release: ReleaseHandle, place_key: u64) {
        self.return_focus = Some((release.identity, place_key));
    }

    fn return_target(&self, place_key: u64, active: bool) -> Option<ReleaseIdentity> {
        self.return_focus
            .as_ref()
            .filter(|(_, from)| active && *from != place_key)
            .map(|(release, _)| release.clone())
    }

    fn take_return(&mut self, release: &ReleaseIdentity, place_key: u64, active: bool) -> bool {
        if self.return_target(place_key, active).as_ref() != Some(release) { return false; }
        self.return_focus = None;
        true
    }

    fn focus_for(&mut self, release: &ReleaseIdentity, cx: &mut App) -> FocusHandle {
        if let Some(at) = self.focuses.iter().position(|(key, _)| key == release)
            && let Some((key, focus)) = self.focuses.remove(at)
        {
            self.focuses.push_back((key, focus.clone()));
            return focus;
        }
        let focus = cx.focus_handle().tab_stop(true);
        if self.focuses.len() == 64 {
            if self.focuses.front().is_some_and(|(key, _)| self.return_focus.as_ref().is_some_and(|(returning, _)| key == returning)) {
                self.focuses.rotate_left(1);
            }
            self.focuses.pop_front();
        }
        self.focuses.push_back((release.clone(), focus.clone()));
        focus
    }

    fn release_start(&self, key: &SharedString, count: usize) -> usize {
        let last = count.saturating_sub(1) / RELEASE_WINDOW * RELEASE_WINDOW;
        self.release_pages.iter().find(|(saved, _)| saved == key)
            .map_or(0, |(_, start)| (*start).min(last))
    }

    fn set_release_start(&mut self, key: SharedString, start: usize, count: usize) {
        let last = count.saturating_sub(1) / RELEASE_WINDOW * RELEASE_WINDOW;
        if let Some(at) = self.release_pages.iter().position(|(saved, _)| saved == &key) {
            self.release_pages.remove(at);
        }
        if self.release_pages.len() == 64 { self.release_pages.pop_front(); }
        self.release_pages.push_back((key, start.min(last) / RELEASE_WINDOW * RELEASE_WINDOW));
    }

    fn ensure_open(&mut self, key: SharedString) {
        if !self.is_open(&key) {
            self.toggle(key);
        }
    }

    fn list(&mut self, key: SharedString, count: usize, measure: &Measure) -> ListState {
        let cache = self.lists.entry(key).or_insert_with(|| ListCache {
            state: ListState::new(count, ListAlignment::Top, px(80.))
                .with_uniform_item_height(measure.row()),
            width: measure.width(),
            scale: measure.scale(),
        });
        if cache.state.item_count() != count {
            cache.state.reset_with_uniform_height(count, measure.row());
        } else if cache.width != measure.width() || cache.scale != measure.scale() {
            cache.state.remeasure();
        }
        cache.width = measure.width();
        cache.scale = measure.scale();
        cache.state.clone()
    }

    fn is_open(&self, key: &SharedString) -> bool {
        self.open.contains(key)
    }

    fn toggle(&mut self, key: SharedString) {
        if let Some(at) = self.open.iter().position(|open| open == &key) {
            self.open.remove(at);
        } else {
            self.open.push_back(key);
            if self.open.len() > 64 {
                self.open.pop_front();
            }
        }
    }
}

#[cfg(test)]
mod state_tests {
    use super::{ReleaseHandle, State};

    #[test]
    fn exact_release_handle_identity_ignores_reordered_position_hints() {
        use std::hash::{Hash as _, Hasher as _};
        let before = ReleaseHandle::identified(0, 4, 0, "formats", "shared", "registry-a");
        let after = ReleaseHandle::identified(2, 7, 1, "formats", "shared", "registry-a");
        let sibling = ReleaseHandle::identified(0, 4, 0, "formats", "shared", "registry-b");
        assert_eq!(before, after);
        assert_ne!(before, sibling);
        let digest = |handle: &ReleaseHandle| {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            handle.hash(&mut hasher);
            hasher.finish()
        };
        assert_eq!(digest(&before), digest(&after));
    }

    #[test]
    fn disclosure_follows_exact_row_identity_and_stays_bounded() {
        let mut state = State::default();
        state.toggle("role:source-a@1".into());
        assert!(state.is_open(&"role:source-a@1".into()));
        assert!(!state.is_open(&"role:source-b@1".into()));
        state.toggle("role:source-a@1".into());
        assert!(!state.is_open(&"role:source-a@1".into()));
        for at in 0..128 {
            state.toggle(format!("row-{at}").into());
        }
        assert_eq!(state.open.len(), 64);
        assert!(!state.is_open(&"row-0".into()));
        assert!(state.is_open(&"row-127".into()));
    }

    #[test]
    fn return_focus_waits_for_a_new_active_place_and_is_one_shot() {
        let mut state = State::default();
        let release = ReleaseHandle::new(2, 97, 1);
        state.remember_open(release.clone(), 8);
        assert_eq!(state.return_target(8, true), None);
        assert_eq!(state.return_target(9, false), None);
        assert_eq!(state.return_target(9, true), Some(release.identity.clone()));
        assert!(!state.take_return(&ReleaseHandle::new(2, 97, 0).identity, 9, true));
        assert!(state.take_return(&release.identity, 9, true));
        assert_eq!(state.return_target(9, true), None);
    }
}

#[cfg(test)]
mod mounted_tests {
    use super::*;
    use crate::theme::{Facet, set_facet};
    use gpui::{AppContext as _, Context, Modifiers, Render, TestAppContext, VisualTestContext, point};

    struct Mounted {
        model: Arc<Model>,
        state: Rc<RefCell<State>>,
        opened: Rc<RefCell<Vec<ReleaseHandle>>>,
        width: Pixels,
        place_key: u64,
        visible: bool,
    }

    impl Render for Mounted {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            probe::draw_started(cx);
            if !self.visible {
                return div().w(self.width).h(px(900.0));
            }
            let measure = Measure::new(self.width, &cx.facet());
            let state = Rc::clone(&self.state);
            let opened = Rc::clone(&self.opened);
            let place_key = self.place_key;
            let actions = Actions { open_package: Rc::new(move |release, _, _| {
                state.borrow_mut().remember_open(release.clone(), place_key);
                opened.borrow_mut().push(release);
            }) };
            div().w(self.width).h(px(900.0)).child(library(
                "mounted-library", Arc::clone(&self.model), actions, &measure, Rc::clone(&self.state),
                self.place_key, true,
            ))
        }
    }

    fn draw(cx: &mut VisualTestContext) -> crate::probe::Ledger {
        cx.run_until_parked();
        cx.update(|window, cx| { window.simulate_next_frame(cx); window.draw(cx).clear(cx); });
        cx.update(|_, cx| probe::take(cx))
    }

    fn click(cx: &mut VisualTestContext, ledger: &crate::probe::Ledger, part: &str) {
        let target = ledger.targets.iter().find(|target| target.key.contains(part))
            .unwrap_or_else(|| panic!("no {part} target in native Library frame"));
        let at = point(px(target.bounds.x + target.bounds.width / 2.0),
            px(target.bounds.y + target.bounds.height / 2.0));
        cx.simulate_mouse_move(at, None, Modifiers::none());
        draw(cx);
        cx.simulate_click(at, Modifiers::none());
    }

    #[gpui::test]
    fn back_focuses_the_exact_opened_release_after_its_button_was_unmounted(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            set_facet(Facet { text_scale: 2.0, reduced_motion: true, ..Facet::default() }, cx);
            probe::enable(cx);
        });
        let release = ReleaseHandle::identified(0, 499, 0, "formats", "exact-package", "release-1");
        let rows = (0..500).map(|at| Row {
            key: if at == 499 { "exact-package".into() } else { format!("other-{at:03}").into() },
            name: if at == 499 { "exact-package".into() } else { format!("other-{at:03}").into() },
            at_rest: None, why: "an admitted release".into(), about: None,
            releases: if at == 499 { vec![ReleaseLink { key: "release-1".into(), version: "1".into(),
                target: Some(release.clone()), unavailable: None, source_detail: None }] } else { vec![] },
        }).collect();
        let model = Arc::new(Model {
            name: "project".into(), lede: "Its dependencies".into(), lede_tip: None,
            note: None, alerts: vec![], facts: vec![],
            roles: vec![Role { key: "formats".into(), icon: Icon::Split, label: "speaks formats".into(),
                serving: None, brings: None, rows }],
            twice_heading: None, twice: vec![],
        });
        let state = Rc::new(RefCell::new(State::default()));
        let opened = Rc::new(RefCell::new(Vec::new()));
        let (host, cx) = cx.add_window_view(|_, _| Mounted {
            model, state: Rc::clone(&state), opened: Rc::clone(&opened),
            width: px(260.0), place_key: 1, visible: true,
        });
        draw(cx);
        let list_state = state.borrow().lists.get("role-list-formats").expect("virtual role").state.clone();
        list_state.scroll_to(ListOffset { item_ix: 499, offset_in_item: px(0.) });
        let first = draw(cx);
        click(cx, &first, "exact-package");
        draw(cx);
        list_state.scroll_to(ListOffset { item_ix: 499, offset_in_item: px(160.) });
        let revealed = draw(cx);
        click(cx, &revealed, "release-1");
        assert_eq!(opened.borrow().as_slice(), &[release]);
        host.update(cx, |host, cx| { host.visible = false; cx.notify(); });
        draw(cx);
        list_state.scroll_to(ListOffset { item_ix: 0, offset_in_item: px(0.) });
        host.update(cx, |host, cx| { host.visible = true; host.place_key = 2; cx.notify(); });
        draw(cx);
        cx.simulate_keystrokes("left");
        let returned = draw(cx);
        assert!(returned.targets.iter().any(|target| target.key.contains("release-1") && target.state.focused),
            "Back must restore native focus to the exact clicked release");
        assert!(list_state.logical_scroll_top().item_ix >= 490, "Back must reveal a virtual release after its row was unmounted");
        assert!(state.borrow().return_focus.is_none(), "focus restoration is one shot");
        host.update(cx, |host, cx| { host.width = px(320.0); cx.notify(); });
        let resized = draw(cx);
        assert!(resized.targets.iter().any(|target| target.key.contains("release-1") && target.state.focused),
            "the same release keeps focus through 260→320 reflow at 200% text");
    }

    #[gpui::test]
    fn back_after_same_version_source_reorder_focuses_the_opened_authority(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            set_facet(Facet { text_scale: 2.0, reduced_motion: true, ..Facet::default() }, cx);
            probe::enable(cx);
        });
        let releases = ["registry-a", "registry-b"].into_iter().enumerate().map(|(at, source)| ReleaseLink {
            key: source.into(), version: "1.0.0".into(),
            target: Some(ReleaseHandle::identified(0, 0, at, "formats", "shared-row", source)),
            unavailable: None, source_detail: Some(source.into()),
        }).collect();
        let model = Arc::new(Model {
            name: "project".into(), lede: "Two exact sources".into(), lede_tip: None,
            note: None, alerts: vec![], facts: vec![],
            roles: vec![Role { key: "formats".into(), icon: Icon::Split, label: "speaks formats".into(),
                serving: None, brings: None, rows: vec![Row { key: "shared-row".into(),
                    name: "shared".into(), at_rest: None, why: "two sources".into(),
                    about: None, releases }] }],
            twice_heading: None, twice: vec![],
        });
        let state = Rc::new(RefCell::new(State::default()));
        let opened = Rc::new(RefCell::new(Vec::new()));
        let (host, cx) = cx.add_window_view(|_, _| Mounted {
            model, state: Rc::clone(&state), opened: Rc::clone(&opened),
            width: px(260.0), place_key: 1, visible: true,
        });
        let first = draw(cx);
        click(cx, &first, "shared-row");
        let expanded = draw(cx);
        click(cx, &expanded, "registry-b");
        assert_eq!(opened.borrow().last().map(ReleaseHandle::keys), Some(("formats", "shared-row", "registry-b")));
        host.update(cx, |host, cx| { host.visible = false; cx.notify(); });
        draw(cx);
        host.update(cx, |host, cx| {
            let mut changed = (*host.model).clone();
            changed.roles[0].rows[0].releases.swap(0, 1);
            for (at, release) in changed.roles[0].rows[0].releases.iter_mut().enumerate() {
                release.target = Some(ReleaseHandle::identified(0, 0, at, "formats", "shared-row", release.key.clone()));
            }
            host.model = Arc::new(changed);
            host.visible = true;
            host.place_key = 2;
            host.width = px(320.0);
            cx.notify();
        });
        draw(cx);
        cx.simulate_keystrokes("left");
        let returned = draw(cx);
        assert!(returned.targets.iter().any(|target| target.key.contains("registry-b") && target.state.focused),
            "Back must focus the opened source after source order and width change");
        assert!(!returned.targets.iter().any(|target| target.key.contains("registry-a") && target.state.focused),
            "Back must not transfer focus to the other same-version source");
        assert!(state.borrow().return_focus.is_none(), "exact-source return focus is one shot");
    }

    #[gpui::test]
    fn an_expanded_row_mounts_a_bounded_release_window_with_a_reachable_tail(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            set_facet(Facet { text_scale: 2.0, reduced_motion: true, ..Facet::default() }, cx);
            probe::enable(cx);
        });
        let releases = (0..1_000).map(|at| ReleaseLink {
            key: format!("release-{at}").into(), version: at.to_string().into(),
            target: Some(ReleaseHandle::identified(0, 0, at, "formats", "exact-package", format!("release-{at}"))), unavailable: None, source_detail: None,
        }).collect();
        let model = Arc::new(Model {
            name: "project".into(), lede: "Its dependencies".into(), lede_tip: None,
            note: None, alerts: vec![], facts: vec![],
            roles: vec![Role { key: "formats".into(), icon: Icon::Split, label: "speaks formats".into(),
                serving: None, brings: None, rows: vec![Row { key: "exact-package".into(),
                    name: "exact-package".into(), at_rest: None, why: "an admitted release".into(),
                    about: None, releases }] }],
            twice_heading: None, twice: vec![],
        });
        let state = Rc::new(RefCell::new(State::default()));
        let opened = Rc::new(RefCell::new(Vec::new()));
        let (host, cx) = cx.add_window_view(|_, _| Mounted {
            model, state: Rc::clone(&state), opened: Rc::clone(&opened),
            width: px(260.0), place_key: 1, visible: true,
        });
        let first = draw(cx);
        click(cx, &first, "exact-package");
        let expanded = draw(cx);
        let release_buttons = |ledger: &crate::probe::Ledger| ledger.targets.iter()
            .filter(|target| target.key.contains("release-") && !target.key.contains("releases"))
            .count();
        assert!(release_buttons(&expanded) <= RELEASE_WINDOW);
        click(cx, &expanded, "last-releases");
        let tail = draw(cx);
        assert!(release_buttons(&tail) <= RELEASE_WINDOW);
        assert!(tail.targets.iter().any(|target| target.key.contains("release-996")),
            "Last releases must reach the final bounded window");
        click(cx, &tail, "release-996");
        assert_eq!(opened.borrow().as_slice(), &[ReleaseHandle::identified(0, 0, 996, "formats", "exact-package", "release-996")]);
        host.update(cx, |host, cx| { host.visible = false; cx.notify(); });
        draw(cx);
        host.update(cx, |host, cx| { host.visible = true; host.place_key = 2; cx.notify(); });
        draw(cx);
        cx.simulate_keystrokes("left");
        let returned = draw(cx);
        assert!(returned.targets.iter().any(|target| target.key.contains("release-996") && target.state.focused),
            "Back restores the selected tail release instead of the first page");
    }

    #[gpui::test]
    fn a_500_package_tree_mounts_only_visible_rows_at_200_percent_and_reflows_scroll(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            set_facet(Facet { text_scale: 2.0, reduced_motion: true, ..Facet::default() }, cx);
            probe::enable(cx);
        });
        let rows = (0..500).map(|at| Row {
            key: format!("pkg-{at:03}@1").into(),
            name: format!("pkg-{at:03}").into(),
            at_rest: None,
            why: "In this exact project".into(),
            about: None,
            releases: vec![],
        }).collect();
        let model = Arc::new(Model {
            name: "large project".into(), lede: "Its packages are ready to browse.".into(),
            lede_tip: None, note: None, alerts: vec![], facts: vec![],
            roles: vec![Role { key: "formats".into(), icon: Icon::Split, label: "speaks formats".into(),
                serving: None, rows, brings: None }],
            twice_heading: None, twice: vec![],
        });
        let state = Rc::new(RefCell::new(State::default()));
        let opened = Rc::new(RefCell::new(Vec::new()));
        let (host, cx) = cx.add_window_view(|_, _| Mounted {
            model, state: Rc::clone(&state), opened, width: px(260.0), place_key: 1, visible: true,
        });
        let first = draw(cx);
        let painted = |ledger: &crate::probe::Ledger| ledger.texts.iter().filter(|text| text.content.starts_with("pkg-")).count();
        assert!((1..40).contains(&painted(&first)), "virtual list mounted too many rows: {}", painted(&first));
        assert!(first.texts.iter().any(|text| text.content == "pkg-000"));
        assert!(!first.texts.iter().any(|text| text.content == "pkg-499"));

        let list_state = state.borrow().lists.get("role-list-formats").expect("role list").state.clone();
        let viewport = list_state.viewport_bounds();
        let middle = point(viewport.left() + viewport.size.width / 2.0, viewport.top() + viewport.size.height / 2.0);
        cx.simulate_event(gpui::ScrollWheelEvent {
            position: middle,
            delta: gpui::ScrollDelta::Pixels(point(px(0.0), px(-500.0))),
            ..Default::default()
        });
        let scrolled = draw(cx);
        assert!(list_state.logical_scroll_top().item_ix > 0, "native wheel did not reach the Library rows");
        assert!((1..40).contains(&painted(&scrolled)), "scroll mounted too many rows: {}", painted(&scrolled));
        host.update(cx, |host, cx| { host.width = px(320.0); cx.notify(); });
        let reflowed = draw(cx);
        assert!(list_state.logical_scroll_top().item_ix > 0, "resize lost the virtual list's reading position");
        assert!((1..40).contains(&painted(&reflowed)), "resize mounted too many rows: {}", painted(&reflowed));

        let last = reflowed.targets.iter().find(|target| target.key.contains("last-packages"))
            .expect("Last rows is a native focusable action");
        let last_at = point(px(last.bounds.x + last.bounds.width / 2.0),
            px(last.bounds.y + last.bounds.height / 2.0));
        cx.simulate_mouse_move(last_at, None, Modifiers::none());
        draw(cx);
        cx.simulate_click(last_at, Modifiers::none());
        let last_page = draw(cx);
        assert!(list_state.logical_scroll_top().item_ix >= 490, "Last rows did not reveal the tail");
        assert!(last_page.texts.iter().any(|text| text.content == "pkg-499"));
        assert!((1..40).contains(&painted(&last_page)), "tail mounted too many rows: {}", painted(&last_page));

        cx.simulate_keystrokes("tab");
        let mut focused = last_page.targets.iter().any(|target| target.key.contains("previous-packages") && target.state.focused);
        for _ in 0..20 {
            if focused { break; }
            cx.update(|window, cx| window.focus_next(cx));
            focused = draw(cx).targets.iter().any(|target| target.key.contains("previous-packages") && target.state.focused);
        }
        assert!(focused, "Previous rows must be reachable from the native keyboard focus order at the tail");
        cx.simulate_keystrokes("pageup");
        draw(cx);
        assert!(list_state.logical_scroll_top().item_ix < 490, "focused navigation did not handle PageUp");
    }
}

/// The Library page for `model`, `measure` wide.
#[must_use]
pub fn library(
    id: impl Into<ElementId>,
    model: Arc<Model>,
    actions: Actions,
    measure: &Measure,
    state: Rc<RefCell<State>>,
    place_key: u64,
    active: bool,
) -> Library {
    Library {
        id: id.into(),
        model,
        actions,
        measure: *measure,
        state,
        place_key,
        active,
    }
}

/// See [`library`].
#[derive(IntoElement)]
pub struct Library {
    id: ElementId,
    model: Arc<Model>,
    actions: Actions,
    measure: Measure,
    state: Rc<RefCell<State>>,
    place_key: u64,
    active: bool,
}

/// A package mark at the size its row's text is set at (a 100 % mark beside
/// 200 % text is a speck).
fn package_mark(measure: &Measure, palette: &Palette) -> AnyElement {
    let (boxed, _, _) = KindSize::Sm.metrics();
    let wanted = boxed * measure.scale();
    let size = if wanted < 16.5 {
        KindSize::Sm
    } else if wanted < 22.0 {
        KindSize::Md
    } else {
        KindSize::Lg
    };
    kind_mark(Kind::Package, size, palette)
}

fn child(parent: &ElementId, part: impl Into<SharedString>) -> ElementId {
    ElementId::NamedChild(Arc::new(parent.clone()), part.into())
}

/// Words in one face, published for the layout and contrast lints.
fn words(
    key: ElementId,
    text: SharedString,
    role: TypeRole,
    color: impl Into<Hsla>,
    measure: &Measure,
    overflow: TextOverflow,
) -> AnyElement {
    let color: Hsla = color.into();
    let mut body = div()
        .set(role, measure)
        .text_color(color)
        .child(text.clone());
    if overflow == TextOverflow::Ellipsis {
        body = body.overflow_hidden().whitespace_nowrap().text_ellipsis();
    }
    probe::text(key, text, measure.role(role), 1.0, overflow, body).into_any_element()
}

impl RenderOnce for Library {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.palette();
        let measure = self.measure;
        let model = self.model;
        let id = self.id;
        let mut page = div()
            .id(id.clone())
            .flex()
            .flex_col()
            .w(measure.width())
            .gap(measure.space(Space::Wide))
            .child(hero(&id, &model, &measure, palette, &self.state));
        if self.state.borrow().all_alerts && model.alerts.len() > 8 {
            let list_state =
                self.state
                    .borrow_mut()
                    .list("all-alerts".into(), model.alerts.len() - 8, &measure);
            let alerts_model = Arc::clone(&model);
            let alert_id = child(&id, "all-alerts");
            page = page.child(
                list(list_state, move |at, _window, _cx| {
                    let alert = &alerts_model.alerts[at + 8];
                    alert_view(
                        &child(&alert_id, format!("{}:{}", alert.advisory, alert.path)),
                        alert,
                        &measure,
                        palette,
                    )
                })
                .w(measure.width())
                .h(measure.row() * 6.0),
            );
        }
        if let Some(note) = &model.note {
            page = page.child(words(
                child(&id, "note"),
                note.clone(),
                ty::CAPTION,
                palette.ink2,
                &measure,
                TextOverflow::Wrap,
            ));
        }
        page = page.child(roles(
            &id,
            &model,
            &self.actions,
            &measure,
            palette,
            &self.state,
            self.place_key,
            self.active,
            window,
            cx,
        ));
        page
    }
}

fn hero(
    id: &ElementId,
    model: &Model,
    measure: &Measure,
    palette: &Palette,
    state: &Rc<RefCell<State>>,
) -> AnyElement {
    let gem = measure.fluid(34.0, 44.0);
    let mut lede = div().id(child(id, "lede")).child(words(
        child(id, "lede-words"),
        model.lede.clone(),
        ty::LEDE,
        palette.ink2,
        measure,
        TextOverflow::Wrap,
    ));
    lede = lede.cursor_default();
    let lede: AnyElement = match &model.lede_tip {
        Some(tip) => lede.tip(tip.clone()).into_any_element(),
        None => lede.into_any_element(),
    };
    let mut facts = div()
        .flex()
        .flex_wrap()
        .items_baseline()
        .gap_x(measure.space(Space::Gutter))
        .gap_y(measure.space(Space::Tight));
    for alert in model.alerts.iter().take(8) {
        facts = facts.child(alert_view(
            &child(id, format!("{}:{}", alert.advisory, alert.path)),
            alert,
            measure,
            palette,
        ));
    }
    if model.alerts.len() > 8 {
        let state = Rc::clone(state);
        let all_alerts = state.borrow().all_alerts;
        facts = facts.child(
            button(
                child(id, "all-alerts-toggle"),
                if all_alerts {
                    "Hide alert list"
                } else {
                    "Browse all alerts"
                },
                measure,
            )
            .ghost()
            .size(Control::Small)
            .on_click(move |_, cx| {
                state.borrow_mut().all_alerts = !all_alerts;
                cx.refresh_windows();
            }),
        );
    }
    for (index, fact) in model.facts.iter().enumerate() {
        let color = if index == 0 && model.alerts.is_empty() {
            palette.ink1
        } else {
            palette.ink2
        };
        facts = facts.child(words(
            child(id, format!("fact-{index}")),
            fact.clone(),
            ty::SMALL,
            color,
            measure,
            TextOverflow::Wrap,
        ));
    }
    div()
        .flex()
        .flex_col()
        .gap(measure.space(Space::Roomy))
        .pt(measure.space(Space::Wide))
        .child(
            div()
                .flex()
                .items_center()
                .gap(measure.space(Space::Gutter))
                .child(crate::paint::gem(Kind::Module).size(f32::from(gem)))
                .child(words(
                    child(id, "name"),
                    model.name.clone(),
                    ty::HERO,
                    palette.ink0,
                    measure,
                    TextOverflow::Wrap,
                )),
        )
        .child(lede)
        .child(facts)
        .into_any_element()
}

fn alert_view(id: &ElementId, alert: &Alert, measure: &Measure, palette: &Palette) -> AnyElement {
    let tone = match alert.tone {
        Tone::Warn => palette.amber.base,
        Tone::Fault => palette.coral.base,
    };
    let trigger = div().id(id.clone()).cursor_default().child(words(
        child(id, "words"),
        format!("{} ›", alert.title).into(),
        ty::SMALL,
        tone,
        measure,
        TextOverflow::Wrap,
    ));
    card_on_rest(
        child(id, "card"),
        vec![
            (alert.advisory.clone(), ty::SMALL, Ink::Strong),
            (alert.path.clone(), ty::MONO_SMALL, Ink::Quiet),
        ],
        trigger,
    )
}

/// Roles in one or two columns, balanced by height, then "Here twice".
fn roles(
    id: &ElementId,
    model: &Arc<Model>,
    actions: &Actions,
    measure: &Measure,
    palette: &Palette,
    state: &Rc<RefCell<State>>,
    place_key: u64,
    active: bool,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let modes = Modes::keyed(child(id, "roles-modes"), window, cx);
    let laid = modes.columns(&ROLES, measure.fluid_room(), measure.space(Space::Section));
    let (count, column) = (laid.count, measure.within(laid.column.width()));
    // A change of column count is an epoch: the blocks spring to their new
    // places instead of jumping.
    let flow = Flow::scoped(format!("library-roles-{id:?}"), cx);
    flow.epoch((laid.epoch, count));
    // Each block's height in rows; the duplicates block goes last, in the shorter column.
    let mut blocks: Vec<(usize, AnyElement)> = model
        .roles
        .iter()
        .enumerate()
        .map(|(index, role)| {
            let block = role_block(
                id,
                index,
                Arc::clone(model),
                role,
                actions,
                &column,
                palette,
                state,
                place_key,
                active,
                window,
                cx,
            );
            (
                role.rows.len().min(6) + 2,
                flow.item(child(id, format!("role-flow-{}", role.key)), block)
                    .into_any_element(),
            )
        })
        .collect();
    if !model.twice.is_empty() {
        let block = twice_block(id, Arc::clone(model), &column, palette, state, window, cx);
        blocks.push((
            TWICE_AT_REST + 2,
            flow.item(child(id, "twice-flow"), block).into_any_element(),
        ));
    }
    let mut columns: Vec<(usize, Vec<AnyElement>)> = (0..count).map(|_| (0, Vec::new())).collect();
    let total: usize = blocks.iter().map(|(height, _)| *height).sum();
    let mut filled = 0;
    for (height, block) in blocks {
        // Fill the first column to half, then the rest: keeps reading order down each column.
        let target = if count > 1 && filled >= total.div_ceil(2) {
            1
        } else {
            0
        };
        filled += height;
        let slot = &mut columns[target.min(count - 1)];
        slot.0 += height;
        slot.1.push(block);
    }
    let mut row = div()
        .flex()
        .items_start()
        .gap(measure.space(Space::Section));
    for (_, blocks) in columns {
        row = row.child(
            div()
                .flex()
                .flex_col()
                .w(column.width())
                .gap(measure.space(Space::Wide))
                .children(blocks),
        );
    }
    row.into_any_element()
}

fn role_block(
    id: &ElementId,
    role_index: usize,
    model: Arc<Model>,
    role: &Role,
    actions: &Actions,
    measure: &Measure,
    palette: &Palette,
    state: &Rc<RefCell<State>>,
    place_key: u64,
    active: bool,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let block_id = child(id, format!("role-{}", role.key));
    let icon = role.icon;
    let mut head = div()
        .flex()
        .flex_wrap()
        .items_baseline()
        .gap_x(measure.space(Space::Roomy))
        .child(ui(icon, IconSize::S18, palette.ink2))
        .child(words(
            child(&block_id, "label"),
            role.label.clone(),
            ty::HEAD,
            palette.ink0,
            measure,
            TextOverflow::Wrap,
        ));
    if let Some(serving) = &role.serving {
        head = head.child(words(
            child(&block_id, "serving"),
            serving.clone(),
            ty::SMALL,
            palette.ink2,
            measure,
            TextOverflow::Wrap,
        ));
    }
    let mut block = div()
        .flex()
        .flex_col()
        .gap(measure.space(Space::Tight))
        .child(head);
    let returning_row = state.borrow().return_target(place_key, active)
        .filter(|target| target.role == role.key)
        .and_then(|target| role.rows.iter().position(|row| row.key == target.row));
    if let Some(at) = returning_row {
        let row = &role.rows[at];
        state.borrow_mut().ensure_open(format!("{}:{}", role.key, row.key).into());
    }
    let mut keyboard = None;
    if role.rows.len() <= 8 {
        for (row_index, row) in role.rows.iter().enumerate() {
            let state_key: SharedString = format!("{}:{}", role.key, row.key).into();
            block = block.child(row_view(
                &child(&block_id, row.key.clone()),
                row,
                actions,
                measure,
                palette,
                state,
                state_key,
                None,
                ReleaseHandle::identified(role_index, row_index, 0, role.key.clone(), row.key.clone(), ""),
                place_key,
                active,
                window,
                cx,
            ));
        }
    } else {
        let list_key: SharedString = format!("role-list-{}", role.key).into();
        let list_state = state.borrow_mut().list(list_key, role.rows.len(), measure);
        if let Some(at) = returning_row {
            list_state.remeasure_items(at..at + 1);
            list_state.scroll_to(ListOffset { item_ix: at, offset_in_item: px(0.) });
        }
        let rows_model = Arc::clone(&model);
        let actions = actions.clone();
        let item_parent = block_id.clone();
        let state = Rc::clone(state);
        let measure = *measure;
        let palette = *palette;
        let rendered_list = list_state.clone();
        let navigation_list = list_state.clone();
        keyboard = Some((list_state.clone(), role.rows.len()));
        block = block.child(list_navigation(&block_id, &navigation_list, role.rows.len(), &measure));
        block = block.child(
            list(list_state, move |at, window, cx| {
                let row = &rows_model.roles[role_index].rows[at];
                let state_key: SharedString =
                    format!("{}:{}", rows_model.roles[role_index].key, row.key).into();
                row_view(
                    &child(&item_parent, row.key.clone()),
                    row,
                    &actions,
                    &measure,
                    &palette,
                    &state,
                    state_key,
                    Some((rendered_list.clone(), at)),
                    ReleaseHandle::identified(role_index, at, 0,
                        rows_model.roles[role_index].key.clone(), row.key.clone(), ""),
                    place_key,
                    active,
                    window,
                    cx,
                )
            })
            .w(measure.width())
            .h(measure.row() * 8.0),
        );
        block = block.child(words(
            child(&block_id, "scroll-note"),
            format!("Scroll through {} packages", role.rows.len()).into(),
            ty::CAPTION,
            palette.ink3,
            &measure,
            TextOverflow::Wrap,
        ));
    }
    if let Some(brings) = &role.brings {
        block = block.child(div().pt(measure.space(Space::Tight)).child(words(
            child(&block_id, "brings"),
            brings.clone(),
            ty::SMALL,
            palette.ink2,
            measure,
            TextOverflow::Wrap,
        )));
    }
    if let Some((list, count)) = keyboard {
        block = block.on_key_down(move |event, _, cx| {
            let top = list.logical_scroll_top().item_ix;
            let target = match event.keystroke.key.as_str() {
                "pagedown" => top.saturating_add(8).min(count - 1),
                "pageup" => top.saturating_sub(8),
                "home" => 0,
                "end" => count - 1,
                _ => return,
            };
            list.scroll_to(ListOffset { item_ix: target, offset_in_item: px(0.) });
            cx.refresh_windows();
            cx.stop_propagation();
        });
    }
    block.into_any_element()
}

fn row_view(
    id: &ElementId,
    row: &Row,
    actions: &Actions,
    measure: &Measure,
    palette: &Palette,
    state: &Rc<RefCell<State>>,
    state_key: SharedString,
    list_row: Option<(ListState, usize)>,
    row_address: ReleaseHandle,
    place_key: u64,
    active: bool,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let returning = state.borrow().return_target(place_key, active)
        .is_some_and(|target| target.role == row_address.identity.role && target.row == row_address.identity.row);
    let is_expanded = state.borrow().is_open(&state_key);
    let toggle = Rc::clone(state);
    let mut line = div()
        .id(id.clone())
        .flex()
        .flex_wrap()
        .items_center()
        .gap(measure.space(Space::Snug))
        .min_h(measure.row())
        .px(measure.space(Space::Tight))
        .hover(|style| style.bg(palette.tint))
        .child(package_mark(measure, palette))
        .child(div().flex_1().min_w_0().child(words(
            child(id, "name"),
            row.name.clone(),
            ty::MONO_ROW,
            palette.ink1,
            measure,
            TextOverflow::Ellipsis,
        )));
    if let Some(rest) = &row.at_rest {
        line = line.child(div().flex_none().child(words(
            child(id, "rest"),
            rest.clone(),
            ty::SMALL,
            palette.ink3,
            measure,
            TextOverflow::Wrap,
        )));
    }
    let toggle_key = state_key.clone();
    let toggle_list_row = list_row.clone();
    line = line.child(
        button(
            child(id, "details"),
            if is_expanded {
                "Hide details"
            } else {
                "Details"
            },
            measure,
        )
        .ghost()
        .size(Control::Small)
        .on_click(move |_, cx| {
            toggle.borrow_mut().toggle(toggle_key.clone());
            if let Some((list, at)) = &toggle_list_row {
                list.remeasure_items(*at..*at + 1);
            }
            cx.refresh_windows();
        }),
    );
    let mut lines = vec![(row.why.clone(), ty::SMALL, Ink::Strong)];
    if let Some(about) = &row.about {
        lines.push((about.clone(), ty::CAPTION, Ink::Quiet));
    }
    let trigger = card_on_rest(child(id, "card"), lines, line);
    if !is_expanded {
        return trigger;
    }
    let mut detail = div()
        .flex()
        .flex_col()
        .gap(measure.space(Space::Tight))
        .pl(measure.space(Space::Wide))
        .pb(measure.space(Space::Base))
        .child(words(
            child(id, "why-inline"),
            row.why.clone(),
            ty::SMALL,
            palette.ink2,
            measure,
            TextOverflow::Wrap,
        ));
    if let Some(about) = &row.about {
        detail = detail.child(words(
            child(id, "about-inline"),
            about.clone(),
            ty::LEDE,
            palette.ink2,
            measure,
            TextOverflow::Wrap,
        ));
    }
    let release_count = row.releases.len();
    let return_release = state.borrow().return_target(place_key, active)
        .filter(|target| target.role == row_address.identity.role && target.row == row_address.identity.row);
    if let Some(target) = return_release {
        if let Some(at) = row.releases.iter().position(|release| release.key == target.release) {
            state.borrow_mut().set_release_start(state_key.clone(), at, release_count);
        }
    }
    let release_start = state.borrow().release_start(&state_key, release_count);
    let release_end = release_start.saturating_add(RELEASE_WINDOW).min(release_count);
    if release_count > RELEASE_WINDOW {
        let page_label = format!("Releases {}–{} of {release_count}", release_start + 1, release_end);
        let page_button = |suffix: &'static str, label: &'static str, to: usize, disabled: bool| {
            let state = Rc::clone(state);
            let state_key = state_key.clone();
            let list_row = list_row.clone();
            button(child(id, suffix), label, measure).ghost().size(Control::Small).disabled(disabled)
                .on_click(move |_, cx| set_release_page(&state, state_key.clone(), to, release_count, &list_row, cx))
        };
        detail = detail.child(words(child(id, "release-position"), page_label.into(), ty::CAPTION,
            palette.ink3, measure, TextOverflow::Wrap))
            .child(div().flex().flex_wrap().gap(measure.space(Space::Tight))
                .child(page_button("first-releases", "First releases", 0, release_start == 0))
                .child(page_button("previous-releases", "Previous releases", release_start.saturating_sub(RELEASE_WINDOW), release_start == 0))
                .child(page_button("next-releases", "Next releases", release_end, release_end == release_count))
                .child(page_button("last-releases", "Last releases", release_count.saturating_sub(1), release_end == release_count)));
    }
    for release in row.releases.iter().skip(release_start).take(RELEASE_WINDOW) {
        if let Some(target) = &release.target {
            let target = target.clone();
            let open = Rc::clone(&actions.open_package);
            let button_id = child(id, release.key.clone());
            let focus = state.borrow_mut().focus_for(&target.identity, cx);
            if returning && state.borrow_mut().take_return(&target.identity, place_key, active) {
                let returning_focus = focus.clone();
                window.defer(cx, move |window, cx| window.focus(&returning_focus, cx));
            }
            let kind = release.source_detail.as_ref().map(|source| {
                let source = source.as_ref();
                if source.starts_with("registry+") { "registry" }
                else if source.starts_with("sparse+") { "sparse" }
                else if source.starts_with("git+") { "git" }
                else { "path" }
            });
            let label = kind.map_or_else(
                || format!("Open {} ›", release.version),
                |kind| format!("Open {} · {kind} ›", release.version),
            );
            detail = detail.child(
                button(
                    button_id,
                    label,
                    measure,
                )
                .focus_handle(focus)
                .ghost()
                .on_click(move |window, cx| open(target.clone(), window, cx)),
            );
        } else if let Some(reason) = &release.unavailable {
            detail = detail.child(words(
                child(id, release.key.clone()),
                format!("{} · {reason}", release.version).into(),
                ty::CAPTION,
                palette.ink3,
                measure,
                TextOverflow::Wrap,
            ));
        }
        if let Some(source) = &release.source_detail {
            detail = detail.child(words(
                child(child(id, release.key.clone()), "source-detail"),
                source.clone(),
                ty::CAPTION,
                palette.ink3,
                measure,
                TextOverflow::Wrap,
            ));
        }
    }
    div()
        .flex()
        .flex_col()
        .child(trigger)
        .child(detail)
        .into_any_element()
}

fn set_release_page(
    state: &Rc<RefCell<State>>,
    key: SharedString,
    start: usize,
    count: usize,
    list_row: &Option<(ListState, usize)>,
    cx: &mut App,
) {
    state.borrow_mut().set_release_start(key, start, count);
    if let Some((list, row)) = list_row {
        list.remeasure_items(*row..*row + 1);
    }
    cx.refresh_windows();
}

/// Native buttons provide a keyboard route into every virtual row, including
/// one not yet mounted by the wheel viewport. The list keeps the scroll
/// position; these controls remain present as its visible items change.
fn list_navigation(id: &ElementId, list: &ListState, count: usize, measure: &Measure) -> AnyElement {
    let top = list.logical_scroll_top().item_ix;
    let previous = list.clone();
    let next = list.clone();
    let last = list.clone();
    div().flex().flex_wrap().items_center().gap(measure.space(Space::Tight))
        .child(button(child(id, "previous-packages"), "Previous rows", measure)
            .ghost().size(Control::Small).disabled(top == 0)
            .on_click(move |_, cx| {
                previous.scroll_to(ListOffset { item_ix: top.saturating_sub(8), offset_in_item: px(0.) });
                cx.refresh_windows();
            }))
        .child(button(child(id, "next-packages"), "Next rows", measure)
            .ghost().size(Control::Small).disabled(top.saturating_add(8) >= count)
            .on_click(move |_, cx| {
                next.scroll_to(ListOffset { item_ix: top.saturating_add(8).min(count - 1), offset_in_item: px(0.) });
                cx.refresh_windows();
            }))
        .child(button(child(id, "last-packages"), "Last rows", measure)
            .ghost().size(Control::Small).disabled(top >= count.saturating_sub(8))
            .on_click(move |_, cx| {
                last.scroll_to(ListOffset { item_ix: count - 1, offset_in_item: px(0.) });
                cx.refresh_windows();
            }))
        .into_any_element()
}

/// How strongly a card line speaks.
#[derive(Clone, Copy)]
enum Ink {
    Strong,
    Quiet,
}

/// Rest on `trigger` and a small card opens with `lines`, each wrapping
/// inside the card's width (a tip's single line would cut them).
fn card_on_rest(
    key: ElementId,
    lines: Vec<(SharedString, TypeRole, Ink)>,
    trigger: impl IntoElement,
) -> AnyElement {
    let lines = Arc::new(lines);
    let request_key = key.clone();
    float::trigger(
        key,
        move |bounds| {
            let lines = Arc::clone(&lines);
            let card_key = request_key.clone();
            FloatRequest::new(
                request_key.clone(),
                bounds,
                FloatKind::Lens,
                move |measure, _window, cx| {
                    let palette = cx.palette();
                    let inner = measure.inset(measure.space(Space::Roomy));
                    let mut card = div()
                        .flex()
                        .flex_col()
                        .gap(measure.space(Space::Tight))
                        .p(measure.space(Space::Roomy))
                        .max_w(measure.width());
                    for (at, (text, role, ink)) in lines.iter().enumerate() {
                        let color = match ink {
                            Ink::Strong => palette.ink1,
                            Ink::Quiet => palette.ink3,
                        };
                        card = card.child(div().w(inner.width()).child(words(
                            child(&card_key, format!("line-{at}")),
                            text.clone(),
                            *role,
                            color,
                            &inner,
                            TextOverflow::Wrap,
                        )));
                    }
                    card.into_any_element()
                },
            )
        },
        trigger,
    )
    .into_any_element()
}

fn twice_block(
    id: &ElementId,
    model: Arc<Model>,
    measure: &Measure,
    palette: &Palette,
    state: &Rc<RefCell<State>>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let block_id = child(id, "twice");
    let mut head = div()
        .flex()
        .flex_wrap()
        .items_baseline()
        .gap_x(measure.space(Space::Roomy));
    if let Some((title, caption)) = &model.twice_heading {
        head = head
            .child(words(
                child(&block_id, "label"),
                title.clone(),
                ty::HEAD,
                palette.ink0,
                measure,
                TextOverflow::Wrap,
            ))
            .child(words(
                child(&block_id, "caption"),
                caption.clone(),
                ty::SMALL,
                palette.ink2,
                measure,
                TextOverflow::Wrap,
            ));
    }
    let mut block = div()
        .flex()
        .flex_col()
        .gap(measure.space(Space::Tight))
        .child(head);
    let mut keyboard = None;
    if model.twice.len() <= TWICE_AT_REST {
        for twice in &model.twice {
            let key = twice.name.clone();
            block = block.child(twice_row(
                &child(&block_id, key.clone()),
                twice,
                measure,
                palette,
                state,
                key,
                None,
                window,
                cx,
            ));
        }
    } else {
        let list_state = state
            .borrow_mut()
            .list("twice-list".into(), model.twice.len(), measure);
        let rendered_list = list_state.clone();
        let navigation_list = list_state.clone();
        keyboard = Some((list_state.clone(), model.twice.len()));
        block = block.child(list_navigation(&block_id, &navigation_list, model.twice.len(), &measure));
        let twice_model = Arc::clone(&model);
        let item_parent = block_id.clone();
        let state = Rc::clone(state);
        let measure = *measure;
        let palette = *palette;
        block = block.child(
            list(list_state, move |at, window, cx| {
                let twice = &twice_model.twice[at];
                let key = twice.name.clone();
                twice_row(
                    &child(&item_parent, key.clone()),
                    twice,
                    &measure,
                    &palette,
                    &state,
                    key,
                    Some((rendered_list.clone(), at)),
                    window,
                    cx,
                )
            })
            .w(measure.width())
            .h(measure.row() * 8.0),
        );
        block = block.child(words(
            child(&block_id, "scroll-note"),
            format!("Scroll through {} duplicates", model.twice.len()).into(),
            ty::CAPTION,
            palette.ink3,
            &measure,
            TextOverflow::Wrap,
        ));
    }
    if let Some((list, count)) = keyboard {
        block = block.on_key_down(move |event, _, cx| {
            let top = list.logical_scroll_top().item_ix;
            let target = match event.keystroke.key.as_str() {
                "pagedown" => top.saturating_add(8).min(count - 1),
                "pageup" => top.saturating_sub(8),
                "home" => 0,
                "end" => count - 1,
                _ => return,
            };
            list.scroll_to(ListOffset { item_ix: target, offset_in_item: px(0.) });
            cx.refresh_windows();
            cx.stop_propagation();
        });
    }
    block.into_any_element()
}

fn twice_row(
    id: &ElementId,
    twice: &Twice,
    measure: &Measure,
    palette: &Palette,
    state: &Rc<RefCell<State>>,
    state_key: SharedString,
    list_row: Option<(ListState, usize)>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let is_open = state.borrow().is_open(&state_key);
    let toggle = Rc::clone(state);
    let mut versions = div()
        .flex()
        .flex_wrap()
        .min_w_0()
        .items_baseline()
        .gap(measure.space(Space::Snug));
    for (at, (version, yours)) in twice.copies.iter().enumerate() {
        if at > 0 {
            versions = versions.child(words(
                child(id, format!("dot-{at}")),
                "·".into(),
                ty::MONO_SMALL,
                palette.ink4,
                measure,
                TextOverflow::Clip,
            ));
        }
        let color = if *yours {
            palette.mint.base
        } else {
            palette.ink2
        };
        versions = versions.child(words(
            child(id, format!("v-{at}")),
            version.clone(),
            ty::MONO_SMALL,
            color,
            measure,
            TextOverflow::Wrap,
        ));
    }
    let line = div()
        .id(id.clone())
        .flex()
        .flex_wrap()
        .items_center()
        .gap(measure.space(Space::Snug))
        .min_h(measure.row())
        .px(measure.space(Space::Tight))
        .hover(|style| style.bg(palette.tint))
        .child(package_mark(measure, palette))
        .child(div().flex_1().min_w_0().child(words(
            child(id, "name"),
            twice.name.clone(),
            ty::MONO_ROW,
            if is_open { palette.ink0 } else { palette.ink1 },
            measure,
            TextOverflow::Ellipsis,
        )))
        .child(versions)
        .child(
            button(
                child(id, "details"),
                if is_open { "Hide details" } else { "Details" },
                measure,
            )
            .ghost()
            .size(Control::Small)
            .on_click(move |_, cx| {
                toggle.borrow_mut().toggle(state_key.clone());
                if let Some((list, at)) = &list_row {
                    list.remeasure_items(*at..*at + 1);
                }
                cx.refresh_windows();
            }),
        );
    let mut block = div().flex().flex_col().child(line);
    if is_open {
        let inset = measure.space(Space::Gutter) + measure.space(Space::Snug);
        let mut detail = div()
            .flex()
            .flex_col()
            .gap(measure.space(Space::Tight))
            .pl(inset)
            .pb(measure.space(Space::Snug));
        for (at, path) in twice.paths.iter().enumerate() {
            let yours = twice.copies.get(at).is_some_and(|(_, yours)| *yours);
            detail = detail.child(words(
                child(id, format!("path-{at}")),
                path.clone(),
                ty::MONO_SMALL,
                if yours {
                    palette.mint.base
                } else {
                    palette.ink2
                },
                measure,
                TextOverflow::Wrap,
            ));
        }
        detail = detail.child(words(
            child(id, "verdict"),
            twice.verdict.clone(),
            ty::SMALL,
            palette.ink2,
            measure,
            TextOverflow::Wrap,
        ));
        block = block.child(detail);
    }
    block.into_any_element()
}

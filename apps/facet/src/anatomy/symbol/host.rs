//! What the shell lends the page: the page's own state (which package the
//! list is filtered to), the actions that change it, opening a place in the
//! editor, the folds, and the doors every linked name is.

use super::key::{FoldKey, Key, Sec};
use super::view::Verb;
use crate::anatomy::page::{Door, Doors, Fold};
use crate::motion::presence::Presence;
use gpui::{AnyElement, App, Bounds, Pixels, SharedString, Window};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

/// An action a click runs.
pub type Act = Rc<dyn Fn(&mut Window, &mut App)>;

/// The page's state, as of this frame.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Ui {
    /// The list is filtered to one package.
    pub package: Option<String>,
    /// The list is filtered to one verb.
    pub verb: Option<Verb>,
    /// Imports are shown.
    pub imports: bool,
    /// Tests are included.
    pub tests: bool,
    /// The list is filtered to places where a generic is this type.
    pub fill: Option<String>,
    /// The package menu is open.
    pub menu: bool,
    /// Packages showing every place, not five.
    pub expanded: BTreeSet<String>,
}

/// A change to the page's state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Change {
    /// Filter to one package (`None`: all).
    Package(Option<String>),
    /// Filter to one verb, or clear it when it is the one chosen.
    Verb(Verb),
    /// Include or hide tests.
    Tests,
    /// Filter to where a generic is this type (`None`: clear).
    Fill(Option<String>),
    /// Open or close the package menu.
    Menu(bool),
    /// Show every place of a package.
    Expand(String),
}

impl Ui {
    /// The state after `change`.
    #[must_use]
    pub fn apply(mut self, change: &Change) -> Self {
        match change {
            Change::Package(package) => {
                self.package = package.clone().filter(|package| self.package.as_ref() != Some(package));
                self.menu = false;
            }
            Change::Verb(verb) => {
                if *verb == Verb::Imports {
                    self.imports = !self.imports;
                }
                self.verb = if self.verb == Some(*verb) { None } else { Some(*verb) };
            }
            Change::Tests => self.tests = !self.tests,
            Change::Fill(fill) => {
                self.fill = fill.clone();
                if fill.is_some() {
                    self.package = None;
                }
            }
            Change::Menu(open) => self.menu = *open,
            Change::Expand(package) => {
                self.expanded.insert(package.clone());
            }
        }
        self
    }
}

/// What the page remembers between frames, outside the shell's state: where
/// its sections were laid out (for "scroll to") and which side of the
/// rail's breakpoint it last drew (so a width that hovers at the edge does
/// not flicker).
#[derive(Debug, Default)]
pub struct Spots {
    map: RefCell<BTreeMap<Sec, Bounds<Pixels>>>,
    beside: Cell<Option<bool>>,
}

impl Spots {
    /// A shared, empty table.
    #[must_use]
    pub fn new() -> Rc<Self> {
        Rc::new(Self::default())
    }

    /// Where `section` was laid out.
    #[must_use]
    pub fn get(&self, section: Sec) -> Option<Bounds<Pixels>> {
        self.map.borrow().get(&section).copied()
    }

    /// Records `bounds` under `section`.
    pub fn record(&self, section: Sec, bounds: Bounds<Pixels>) {
        self.map.borrow_mut().insert(section, bounds);
    }

    /// Whether the rail was beside the page the last frame.
    #[must_use]
    pub fn beside(&self) -> Option<bool> {
        self.beside.get()
    }

    /// Remembers the rail's side.
    pub fn set_beside(&self, beside: bool) {
        self.beside.set(Some(beside));
    }
}

/// What the shell lends the page, on top of its doors.
pub trait Host: Doors {
    /// The page's state now.
    fn ui(&self) -> Ui;
    /// An action that changes the page's state.
    fn change(&self, change: Change) -> Act;
    /// An action that opens `path` at `line` in the editor.
    fn open_source(&self, path: &str, line: u32) -> Act;
    /// The fold `key`, made on demand.
    fn unfold(&self, key: &FoldKey) -> Option<Fold>;
    /// A doc reference's action (`[Value]`, `[crate::x]`).
    fn lookup(&self, target: &str) -> Option<Act>;
    /// `element` as a keyboard target labelled `label` that runs `act`.
    fn target(&self, key: &Key, label: SharedString, act: Act, element: AnyElement) -> AnyElement;
    /// An action that scrolls `section` into view.
    fn reveal(&self, section: Sec) -> Act;
    /// What the page remembers between frames.
    fn spots(&self) -> Rc<Spots>;
}

/// A page with no shell: a still (the gallery, tests). State is fixed, the
/// folds in `open` are unrolled, nothing is clickable.
pub struct Fixed {
    /// The state.
    pub ui: Ui,
    /// The folds shown open.
    pub open: BTreeSet<FoldKey>,
    spots: Rc<Spots>,
    doors: crate::anatomy::page::Still,
    presences: RefCell<BTreeMap<FoldKey, Presence>>,
}

impl Fixed {
    /// A still page in state `ui`.
    #[must_use]
    pub fn new(ui: Ui) -> Self {
        Self { ui, open: BTreeSet::new(), spots: Spots::new(), doors: crate::anatomy::page::Still, presences: RefCell::new(BTreeMap::new()) }
    }

    /// The same still with `folds` unrolled.
    #[must_use]
    pub fn with_open(mut self, folds: impl IntoIterator<Item = FoldKey>) -> Self {
        self.open.extend(folds);
        self
    }
}

impl Doors for Fixed {
    fn door(&self, link: &str) -> Option<Door> {
        self.doors.door(link)
    }
    fn fold(&self, key: &'static str) -> Option<Fold> {
        self.doors.fold(key)
    }
    fn track(&self, key: SharedString, label: SharedString, door: Option<&Door>, element: AnyElement) -> AnyElement {
        self.doors.track(key, label, door, element)
    }
    fn say(&self, text: &str) {
        self.doors.say(text);
    }
}

impl Host for Fixed {
    fn ui(&self) -> Ui {
        self.ui.clone()
    }
    fn change(&self, _: Change) -> Act {
        Rc::new(|_, _| {})
    }
    fn open_source(&self, _: &str, _: u32) -> Act {
        Rc::new(|_, _| {})
    }
    fn unfold(&self, key: &FoldKey) -> Option<Fold> {
        let presence = self.presences.borrow_mut().entry(key.clone()).or_insert_with(|| Presence::new(format!("s6-still-{key:?}"))).clone();
        Some(Fold { open: self.open.contains(key), presence, toggle: Rc::new(|_, _| {}) })
    }
    fn lookup(&self, _: &str) -> Option<Act> {
        None
    }
    fn target(&self, _: &Key, _: SharedString, _: Act, element: AnyElement) -> AnyElement {
        element
    }
    fn reveal(&self, _: Sec) -> Act {
        Rc::new(|_, _| {})
    }
    fn spots(&self) -> Rc<Spots> {
        Rc::clone(&self.spots)
    }
}

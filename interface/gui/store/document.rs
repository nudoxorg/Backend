//! Defines the reader's documents for `interface-gui`.
//! This module owns tabs, per-tab history, the page cache, and hover cards.
//! Its narrow surface keeps a page from ever flashing or jumping under the reader.

use std::{collections::HashMap, rc::Rc};

use compiler_ir_vocabulary::EntityKind;
use interface_documents::{
    Page, RelationRole, Signature, Symbol, Target, Text,
};
use interface_identity::{Address, ExactAddress, PackageCoordinate};
use interface_library::{
    PageError, ReopenError, ResolveError,
    render::common::{
        Affordance, Fault, add_affordance, address_parse_detail, projection_detail,
        reopen_phase_slug,
    },
};

/// The key a page is cached and navigated by: its address, spelled exactly once.
///
/// Two spellings of the same declaration are the same key because they are the same string, and
/// the engine is the only thing that mints the spelling.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PageKey(Box<str>);

impl PageKey {
    /// The key for one exact address.
    #[must_use]
    pub fn of(address: &ExactAddress) -> Self {
        Self(address.to_string().into_boxed_str())
    }

    /// The key for one parsed address, which may not yet name an exact declaration.
    #[must_use]
    pub fn parsed(address: &Address) -> Self {
        Self(address.to_string().into_boxed_str())
    }

    /// The key for a whole package's root.
    #[must_use]
    pub fn package(coordinate: &PackageCoordinate) -> Self {
        Self(coordinate.to_string().into_boxed_str())
    }

    /// The spelling, which is what a reader copies.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What the reader area is showing right now.
#[derive(Clone, Debug)]
pub enum PageSlot {
    /// No page has been asked for yet.
    Idle,
    /// A page is on its way. The previous page stays on screen until it lands, and where there is
    /// no previous page the reader draws reserved geometry rather than a shimmer.
    Loading {
        /// The page still on screen, if any.
        previous: Option<Rc<Page>>,
    },
    /// A page is on screen.
    Ready {
        /// The page.
        page: Rc<Page>,
    },
    /// The engine refused, with its own cause retained.
    Failed {
        /// The fault drawn in the reader.
        fault: Fault,
        /// Candidate declarations, when the refusal was ambiguity rather than absence.
        candidates: Box<[Symbol]>,
    },
}

impl PageSlot {
    /// The page a render should draw, whether it is current or merely still on screen.
    #[must_use]
    pub fn visible(&self) -> Option<&Rc<Page>> {
        match self {
            Self::Ready { page } => Some(page),
            Self::Loading { previous } => previous.as_ref(),
            Self::Idle | Self::Failed { .. } => None,
        }
    }

    /// Whether a fetch is outstanding.
    #[must_use]
    pub const fn is_loading(&self) -> bool {
        matches!(self, Self::Loading { .. })
    }
}

/// What a hover card knows about its target.
#[derive(Clone, Debug)]
pub enum HoverCard {
    /// Asked for, not yet answered. The card is drawn at its reserved size so nothing shifts.
    Pending,
    /// Answered.
    Ready {
        /// The target's signature specimen.
        signature: Signature,
        /// The first line of its documentation, when it has one.
        summary: Option<Text>,
        /// What the target is.
        kind: EntityKind,
    },
    /// The engine could not produce a page for the target.
    Missing {
        /// Why, in the engine's own words.
        fault: Fault,
    },
}

/// One reading tab, with its own history.
#[derive(Clone, Debug)]
pub struct Tab {
    key: PageKey,
    title: Box<str>,
    kind: Option<EntityKind>,
    history: Vec<PageKey>,
    cursor: usize,
}

impl Tab {
    /// The page this tab is on.
    #[must_use]
    pub const fn key(&self) -> &PageKey {
        &self.key
    }

    /// The compact label on the tab chip.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// What this tab is showing, for its kind glyph.
    #[must_use]
    pub const fn kind(&self) -> Option<EntityKind> {
        self.kind
    }

    /// Whether there is somewhere to go back to.
    #[must_use]
    pub const fn can_go_back(&self) -> bool {
        self.cursor > 0
    }

    /// Whether there is somewhere to go forward to.
    #[must_use]
    pub fn can_go_forward(&self) -> bool {
        self.cursor.saturating_add(1) < self.history.len()
    }

    fn visit(&mut self, key: PageKey, title: Box<str>, kind: Option<EntityKind>) {
        if self.key == key {
            self.title = title;
            self.kind = kind;
            return;
        }
        self.history.truncate(self.cursor.saturating_add(1));
        self.history.push(key.clone());
        self.cursor = self.history.len().saturating_sub(1);
        self.key = key;
        self.title = title;
        self.kind = kind;
    }

    fn step(&mut self, back: bool) -> Option<PageKey> {
        let next = if back {
            self.cursor.checked_sub(1)?
        } else {
            let forward = self.cursor.saturating_add(1);
            (forward < self.history.len()).then_some(forward)?
        };
        let key = self.history.get(next)?.clone();
        self.cursor = next;
        self.key = key.clone();
        Some(key)
    }
}

/// Every open tab, the page cache behind them, and the disclosure state of the current page.
#[derive(Debug, Default)]
pub struct DocumentStore {
    tabs: Vec<Tab>,
    active: usize,
    slot: SlotHolder,
    cache: HashMap<PageKey, Rc<Page>>,
    cards: HashMap<PageKey, HoverCard>,
    collapsed: Vec<EntityKind>,
    expanded: Vec<RelationRole>,
}

#[derive(Debug)]
struct SlotHolder(PageSlot);

impl Default for SlotHolder {
    fn default() -> Self {
        Self(PageSlot::Idle)
    }
}

impl DocumentStore {
    /// Every open tab, in strip order.
    #[must_use]
    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    /// Which tab is in front.
    #[must_use]
    pub const fn active_index(&self) -> usize {
        self.active
    }

    /// The tab in front, when one is open.
    #[must_use]
    pub fn active(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    /// What the reader area draws.
    #[must_use]
    pub const fn slot(&self) -> &PageSlot {
        &self.slot.0
    }

    /// Whether the tab strip should be drawn at all.
    #[must_use]
    pub fn shows_tab_strip(&self) -> bool {
        self.tabs.len() > 1
    }

    /// Marks a page as being fetched, keeping whatever is on screen.
    pub fn begin(&mut self, key: &PageKey) {
        if let Some(page) = self.cache.get(key).map(Rc::clone) {
            self.adopt(key.clone(), &page);
            self.slot = SlotHolder(PageSlot::Ready { page });
            return;
        }
        let previous = self.slot.0.visible().map(Rc::clone);
        self.slot = SlotHolder(PageSlot::Loading { previous });
    }

    /// Folds one engine reply into the reader, the cache, and the active tab.
    pub fn apply(&mut self, key: &PageKey, reply: Result<Page, PageError>) {
        match reply {
            Ok(page) => {
                let page = Rc::new(page);
                self.cache.insert(key.clone(), Rc::clone(&page));
                self.collapsed.clear();
                self.expanded.clear();
                self.adopt(key.clone(), &page);
                self.slot = SlotHolder(PageSlot::Ready { page });
            }
            Err(error) => {
                let candidates = match &error {
                    PageError::Ambiguous(candidates) => candidates.clone(),
                    _ => Box::new([]),
                };
                self.slot = SlotHolder(PageSlot::Failed {
                    fault: page_fault(&error),
                    candidates,
                });
            }
        }
    }

    fn adopt(&mut self, key: PageKey, page: &Rc<Page>) {
        let title: Box<str> = page.symbol.name.as_str().into();
        let kind = Some(page.symbol.kind);
        match self.tabs.get_mut(self.active) {
            Some(tab) => tab.visit(key, title, kind),
            None => {
                self.tabs.push(Tab {
                    key: key.clone(),
                    title,
                    kind,
                    history: vec![key],
                    cursor: 0,
                });
                self.active = self.tabs.len().saturating_sub(1);
            }
        }
    }

    /// Opens a page in a new tab, optionally leaving the current tab in front.
    pub fn open_tab(&mut self, key: PageKey, title: &str, background: bool) {
        self.tabs.push(Tab {
            key: key.clone(),
            title: title.into(),
            kind: None,
            history: vec![key],
            cursor: 0,
        });
        if !background {
            self.active = self.tabs.len().saturating_sub(1);
        }
    }

    /// Brings one tab to the front.
    pub fn select_tab(&mut self, index: usize) {
        if index < self.tabs.len() {
            self.active = index;
            self.restore();
        }
    }

    /// Moves the front tab by one, wrapping.
    pub fn cycle_tab(&mut self, forward: bool) {
        if self.tabs.len() < 2 {
            return;
        }
        let count = self.tabs.len();
        self.active = if forward {
            self.active.saturating_add(1) % count
        } else {
            self.active.checked_sub(1).unwrap_or(count.saturating_sub(1))
        };
        self.restore();
    }

    /// Closes one tab, returning the key the reader should now be shown.
    pub fn close_tab(&mut self, index: usize) -> Option<PageKey> {
        if index >= self.tabs.len() {
            return None;
        }
        self.tabs.remove(index);
        if self.tabs.is_empty() {
            self.slot = SlotHolder(PageSlot::Idle);
            self.active = 0;
            return None;
        }
        self.active = self.active.min(self.tabs.len().saturating_sub(1));
        self.restore();
        self.active().map(|tab| tab.key().clone())
    }

    /// Steps the front tab's history, returning the key to fetch.
    pub fn navigate(&mut self, back: bool) -> Option<PageKey> {
        let key = self.tabs.get_mut(self.active)?.step(back)?;
        self.restore();
        Some(key)
    }

    fn restore(&mut self) {
        let Some(key) = self.active().map(|tab| tab.key().clone()) else {
            return;
        };
        self.slot = SlotHolder(match self.cache.get(&key) {
            Some(page) => PageSlot::Ready { page: Rc::clone(page) },
            None => PageSlot::Loading { previous: None },
        });
    }

    /// A cached page, when one has already been read.
    #[must_use]
    pub fn cached(&self, key: &PageKey) -> Option<&Rc<Page>> {
        self.cache.get(key)
    }

    /// Whether one member group is collapsed.
    #[must_use]
    pub fn is_collapsed(&self, kind: EntityKind) -> bool {
        self.collapsed.contains(&kind)
    }

    /// Flips one member group open or shut.
    pub fn toggle_group(&mut self, kind: EntityKind) {
        match self.collapsed.iter().position(|held| *held == kind) {
            Some(index) => {
                self.collapsed.remove(index);
            }
            None => self.collapsed.push(kind),
        }
    }

    /// Whether one relation chip has been expanded into rows.
    #[must_use]
    pub fn is_expanded(&self, role: RelationRole) -> bool {
        self.expanded.contains(&role)
    }

    /// Flips one relation chip between a count and its rows.
    pub fn toggle_relation(&mut self, role: RelationRole) {
        match self.expanded.iter().position(|held| *held == role) {
            Some(index) => {
                self.expanded.remove(index);
            }
            None => self.expanded.push(role),
        }
    }

    /// The hover card for one target, when one has been asked for.
    #[must_use]
    pub fn card(&self, key: &PageKey) -> Option<&HoverCard> {
        self.cards.get(key)
    }

    /// Records that a hover card has been asked for, unless one is already known.
    ///
    /// Returns whether the caller should actually go and fetch it, so a pointer resting on the
    /// same token twice costs one command, not two.
    pub fn request_card(&mut self, key: &PageKey) -> bool {
        if self.cards.contains_key(key) {
            return false;
        }
        self.cards.insert(key.clone(), HoverCard::Pending);
        true
    }

    /// Folds one hover-card reply in.
    pub fn apply_card(&mut self, key: &PageKey, reply: Result<Page, PageError>) {
        let card = match reply {
            Ok(page) => HoverCard::Ready {
                signature: page.signature.clone(),
                summary: page.prose.summary(),
                kind: page.symbol.kind,
            },
            Err(error) => HoverCard::Missing {
                fault: page_fault(&error),
            },
        };
        self.cards.insert(key.clone(), card);
    }
}

/// The key one navigable target points at, or `None` when the target is not local.
#[must_use]
pub fn target_key(target: &Target) -> Option<PageKey> {
    match target {
        Target::Local(symbol) => Some(PageKey::of(&symbol.address)),
        Target::External(_) | Target::Unresolved(_) => None,
    }
}

/// Projects one page refusal into the fault the reader draws in place.
///
/// Every arm keeps the exact operand the engine refused: which package, which phase, which entity.
#[must_use]
pub fn page_fault(error: &PageError) -> Fault {
    match error {
        PageError::Resolve(cause) => resolve_fault(cause),
        PageError::Ambiguous(candidates) => Fault::new(
            "address-ambiguous",
            String::new(),
            Affordance::None,
        )
        .detailed(format!("{} declarations share that path", candidates.len())),
        PageError::Reopen(cause) => reopen_fault(cause),
        PageError::Projection(cause) => Fault::new("page-not-projected", String::new(), Affordance::None)
            .detailed(projection_detail(cause)),
        PageError::KeyUnknown { key } => Fault::new(
            "key-unknown",
            key.to_string(),
            Affordance::Resolve {
                text: key.family().to_string(),
            },
        ),
    }
}

fn resolve_fault(cause: &ResolveError) -> Fault {
    match cause {
        ResolveError::Parse { cause } => {
            Fault::new("address-unreadable", String::new(), Affordance::None)
                .detailed(address_parse_detail(*cause))
        }
        ResolveError::PackageUnknown { package } => Fault::new(
            "package-not-on-shelf",
            package.to_string(),
            add_affordance(package),
        ),
        ResolveError::PackageNotReady { package } => Fault::new(
            "package-not-ready",
            package.to_string(),
            Affordance::Packages,
        ),
    }
}

fn reopen_fault(cause: &ReopenError) -> Fault {
    Fault::new(
        "image-not-reopened",
        cause.package.to_string(),
        add_affordance(&cause.package),
    )
    .detailed(format!(
        "{} · {}",
        reopen_phase_slug(cause.phase),
        cause.detail
    ))
}

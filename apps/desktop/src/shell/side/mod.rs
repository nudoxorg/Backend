//! The sidebar: the shelf region (`shell::shelf` re-exports it).
//!
//! It follows `v6/cohesion/COHESION.md`, "The sidebar: primitives": one
//! contextual scope with a way out, four lenses, narrowing in place, state
//! on rows, hoisting, and (with the hand) held chips. The model is pure and
//! lives in files that name no entity or window:
//!
//! - [`scope`]: where the sidebar is, and how it follows the reader;
//! - [`lens`], [`row`], [`state`], [`narrow`], [`outline`], [`listing`]: what
//!   it lists (rows are typed; what a row does is a [`row::Do`], not a
//!   closure);
//! - [`input`]: the keys it reads.
//!
//! [`view`] and [`glyph`] paint it, and this file is the entity that holds
//! the state and carries out what a row does.
//!
//! The region lays itself out from its measured width: wide enough for the
//! shelf, it draws the shelf at its resting width (so an animating column
//! clips it instead of reflowing it); narrow, it draws the spine; in
//! between, only while the shell animates the column, both, crossfaded.

mod glyph;
mod hold;
mod input;
mod lens;
mod listing;
mod narrow;
mod outline;
mod peek;
mod row;
mod scope;
mod state;
mod view;

pub(crate) use input::KEYS;
pub(crate) use listing::{LibraryOrder, beside_your_projects, told_apart};
pub(crate) use outline::{is_test_module, shelf_name};
#[cfg(test)]
pub(crate) use row::TESTS_ROW;
pub(crate) mod twin;

use super::focus::{Act, Target, Targets};
use super::kit::HoverIntent;
use super::region::{Links, Region, RegionCore};
use crate::model::AppSnapshot;
use crate::model::pages::{PackageRef, PageKey, SymbolRef};
use crate::navigation::{Intent, Overlay, Route};
use crate::runtime::store::{Branch, DataStore};
use facet::overlay::float;
use facet::tokens::fluid::{SIDE, SideForm};
use facet::{ActiveFacet as _, Measure, Space};
use gpui::{
    App, Context, InteractiveElement, IntoElement, KeystrokeEvent, ParentElement, Pixels, Render,
    ListAlignment, ListOffset, ListState, SharedString, Styled, Subscription, Task, Window, div,
    px,
};
use input::{Chord, Peek, Query, SideKey, Typed};
use lens::Lens;
use listing::{Inputs, Listing};
use narrow::{Filter, Matched, Narrow};
use row::{Do, Fold, Folds, Item, Row, RowId};
use scope::{Crumbs, Scope};
use state::{StateBook, WorkspaceCrate};
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::navigation::presentation::{ReadingOffset, ReadingText, ShelfRowAnchor, ShelfRowKind};

/// The place a route is: a book (the package as pinned, so reading another
/// release of it is the same place) and the declaration on it. Following
/// clears what belongs to the old place (a narrowing, a lens on another
/// book) and leaves what belongs to the new release of the same one.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct Place {
    book: Option<SharedString>,
    symbol: Option<SymbolRef>,
}

impl Place {
    fn of(route: &Route) -> Self {
        let book = match route {
            Route::Package(route) => Some(route.package.as_str().to_owned().into()),
            Route::Symbol(route) => Some(route.package.as_str().to_owned().into()),
            Route::CargoSource(route) => Some(route.package.as_str().to_owned().into()),
            Route::Orbit(_) | Route::World => None,
        };
        Self {
            book,
            symbol: super::jump::route_symbol(route),
        }
    }
}

/// What the state book was made from: read again only when one of these
/// changes.
#[derive(Clone, Debug, Eq, PartialEq)]
struct BookKey {
    package: PackageRef,
    from: SharedString,
    to: Option<SharedString>,
}

/// The measured row list belongs to this shelf instance. Its cached heights
/// survive paints; only changed rows or a changed measuring width invalidate
/// them. Item ids remain the anchor when a fold inserts rows above the view.
struct RowLayout {
    rows: Rc<Vec<Row>>,
    identities: Rc<Vec<Option<RowKey>>>,
    scroll: ListState,
    width: Pixels,
    scale: f32,
    row_height: Pixels,
    restore: Restore,
    pending_reveal: Option<(u64, RowId)>,
    intent: Rc<Cell<Option<u64>>>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct RowKey {
    kind: ShelfRowKind,
    text: ReadingText,
    occurrence: u16,
}

enum Restore {
    Idle,
    Queued { ticket: u64, offset: ReadingOffset, anchor: Option<ShelfRowAnchor> },
    Measuring { ticket: u64, offset: ReadingOffset, anchor: ShelfRowAnchor },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DeferredScroll { Restore(u64), Reveal(u64) }

impl RowLayout {
    fn new() -> Self {
        let scroll = ListState::new(0, ListAlignment::Top, px(80.0))
            .with_uniform_item_height(px(32.0));
        let intent = Rc::new(Cell::new(Some(1_u64)));
        let input_intent = Rc::clone(&intent);
        // ListState invokes this only for native wheel/scrollbar input, not
        // for our direct scroll_to/scroll_by or measurement adjustments.
        scroll.set_scroll_handler(move |_, _, _| {
            input_intent.set(input_intent.get().and_then(|value| value.checked_add(1)));
        });
        Self {
            rows: Rc::new(Vec::new()),
            identities: Rc::new(Vec::new()),
            scroll,
            width: px(0.0),
            scale: 1.0,
            row_height: px(32.0),
            restore: Restore::Idle,
            pending_reveal: None,
            intent,
        }
    }

    fn next_intent(&self) -> Option<u64> {
        let next = self.intent.get().and_then(|value| value.checked_add(1));
        self.intent.set(next);
        next
    }

    fn cancel_deferred_scroll(&mut self) {
        self.next_intent();
        self.restore = Restore::Idle;
        self.pending_reveal = None;
        self.scroll.clear_pending_scroll_adjustment();
    }

    fn discard_superseded(&mut self) {
        if !matches!(self.restore, Restore::Idle)
            && !matches!(&self.restore, Restore::Queued { ticket, .. } | Restore::Measuring { ticket, .. }
                if self.intent.get() == Some(*ticket)) {
            self.restore = Restore::Idle;
            self.scroll.clear_pending_scroll_adjustment();
        }
        if self.pending_reveal.as_ref().is_some_and(|(ticket, _)| self.intent.get() != Some(*ticket)) {
            self.pending_reveal = None;
        }
    }

    fn row_key(row: &Row) -> Option<(ShelfRowKind, ReadingText)> {
        match row {
            Row::Item(item) => Some((ShelfRowKind::Item, ReadingText::new(item.key.to_string())?)),
            Row::Heading(heading) => Some((ShelfRowKind::Heading, ReadingText::new(heading.words.to_string())?)),
            Row::Note(words) => Some((ShelfRowKind::Note, ReadingText::new(words.to_string())?)),
        }
    }

    fn identities(rows: &[Row]) -> Vec<Option<RowKey>> {
        let mut counts: HashMap<(ShelfRowKind, ReadingText), usize> = HashMap::new();
        rows.iter().map(|row| {
            let (kind, text) = Self::row_key(row)?;
            let count = counts.entry((kind, text.clone())).or_default();
            let occurrence = u16::try_from(*count).ok()?;
            *count += 1;
            Some(RowKey { kind, text, occurrence })
        }).collect()
    }

    fn anchor_index(&self, anchor: &ShelfRowAnchor) -> Option<usize> {
        self.identities.iter().position(|identity| identity.as_ref().is_some_and(|identity|
            identity.kind == anchor.kind && identity.text == anchor.key && identity.occurrence == anchor.occurrence))
    }

    fn observed_anchor_at(identities: &[Option<RowKey>], scroll: &ListState) -> Option<ShelfRowAnchor> {
        let top = scroll.logical_scroll_top();
        let identity = identities.get(top.item_ix)?.as_ref()?;
        let height = scroll.bounds_for_item(top.item_ix)?.size.height;
        if height <= px(0.0) { return None; }
        ShelfRowAnchor::new(identity.kind, identity.text.clone(), usize::from(identity.occurrence),
            (f32::from(top.offset_in_item) / f32::from(height)).clamp(0.0, 1.0))
    }

    fn queue_restore(&mut self, offset: ReadingOffset, anchor: Option<ShelfRowAnchor>) {
        self.pending_reveal = None;
        self.restore = self.next_intent().map_or(Restore::Idle,
            |ticket| Restore::Queued { ticket, offset, anchor });
    }

    fn restoring(&self) -> bool { !matches!(self.restore, Restore::Idle) }

    fn deferred_scroll(&self) -> Option<DeferredScroll> {
        match &self.restore {
            Restore::Measuring { ticket, .. } if self.intent.get() == Some(*ticket) => Some(DeferredScroll::Restore(*ticket)),
            _ => self.pending_reveal.as_ref().and_then(|(ticket, _)|
                (self.intent.get() == Some(*ticket)).then_some(DeferredScroll::Reveal(*ticket))),
        }
    }

    fn deliver_deferred(&mut self, request: DeferredScroll) -> bool {
        self.discard_superseded();
        match request {
            DeferredScroll::Restore(ticket) => self.finish_restore(ticket),
            DeferredScroll::Reveal(ticket) => self.finish_reveal(ticket),
        }
    }

    fn finish_restore(&mut self, ticket: u64) -> bool {
        if self.intent.get() != Some(ticket)
            || !matches!(&self.restore, Restore::Measuring { ticket: current, .. } if *current == ticket) {
            return false;
        }
        let Restore::Measuring { offset, anchor, .. } = std::mem::replace(&mut self.restore, Restore::Idle) else { unreachable!() };
        let Some(index) = self.anchor_index(&anchor) else {
            self.scroll.clear_pending_scroll_adjustment();
            self.restore_pixels(offset);
            return true;
        };
        // The list applied the frozen fraction while measuring this item in
        // its own prepaint. Settlement only releases presentation publication;
        // changing the scroll here would be one painted frame too late.
        if self.scroll.viewport_bounds().size.height <= px(0.0)
            || self.scroll.bounds_for_item(index).is_none() {
            self.restore = Restore::Measuring { ticket, offset, anchor };
            return false;
        }
        true
    }

    fn finish_reveal(&mut self, ticket: u64) -> bool {
        if self.intent.get() != Some(ticket)
            || !self.pending_reveal.as_ref().is_some_and(|(current, _)| *current == ticket)
            || self.scroll.viewport_bounds().size.height <= px(0.0) { return false; }
        let (_, id) = self.pending_reveal.take().expect("checked pending reveal");
        let Some(index) = self.rows.iter().position(|row| row.item().is_some_and(|item| item.id == id)) else { return false; };
        self.reveal_item_geometry(index);
        true
    }

    fn restore_pixels(&self, offset: ReadingOffset) {
        let (_, y) = offset.pixels();
        self.scroll.scroll_to(ListOffset::default());
        self.scroll.scroll_by(-px(y));
    }

    fn scroll_to_user(&mut self, index: usize) {
        self.cancel_deferred_scroll();
        self.scroll.scroll_to(ListOffset { item_ix: index, offset_in_item: px(0.0) });
    }

    fn same_identity(left: &Row, right: &Row) -> bool {
        match (left, right) {
            (Row::Item(left), Row::Item(right)) => left.id == right.id,
            (Row::Heading(left), Row::Heading(right)) => left.words == right.words,
            (Row::Note(left), Row::Note(right)) => left == right,
            _ => false,
        }
    }

    fn reveal_item(&mut self, index: usize) {
        self.cancel_deferred_scroll();
        let ticket = self.intent.get();
        self.reveal_item_geometry(index);
        if self.scroll.viewport_bounds().size.height <= px(0.0)
            && let (Some(ticket), Some(Row::Item(item))) = (ticket, self.rows.get(index)) {
            self.pending_reveal = Some((ticket, item.id.clone()));
        }
    }

    fn reveal_item_geometry(&self, index: usize) {
        let viewport = self.scroll.viewport_bounds();
        if viewport.size.height <= px(0.0) {
            // Before the first list prepaint there is no viewport to reveal
            // against; the list's reveal algorithm would scroll *past* the
            // requested row when its viewport height is zero.
            self.scroll.scroll_to(ListOffset {
                item_ix: index.saturating_sub(self.sticky_ancestors(index).len()),
                offset_in_item: px(0.0),
            });
        } else {
            self.scroll.scroll_to_reveal_item(index);
            // Moving up can expose a deeper sibling before this row, growing
            // the sticky chain. Recompute against the actual new first row
            // until the target clears the overlay or the list reaches top.
            for _ in 0..=u8::MAX {
                let top = self.scroll.logical_scroll_top();
                let inset = self.row_height * (self.visible_sticky_ancestors(top.item_ix).len() as f32);
                let target_top = self.scroll.bounds_for_item(index).map_or_else(|| {
                    viewport.top() + self.row_height * (index.saturating_sub(top.item_ix) as f32)
                        - top.offset_in_item
                }, |bounds| bounds.top());
                let missing = viewport.top() + inset - target_top;
                if missing <= px(0.0) { break; }
                let before = self.scroll.scroll_px_offset_for_scrollbar();
                self.scroll.scroll_by(-missing);
                if self.scroll.scroll_px_offset_for_scrollbar() == before { break; }
            }
        }
    }

    fn sticky_ancestors(&self, first: usize) -> Vec<(usize, &Item)> {
        let Some(Row::Item(row)) = self.rows.get(first) else { return Vec::new(); };
        let mut wanted = row.depth;
        let mut chain = Vec::new();
        for (index, line) in self.rows[..first].iter().enumerate().rev() {
            if wanted == 0 { break; }
            if let Row::Item(item) = line && item.depth < wanted {
                chain.push((index, item));
                wanted = item.depth;
            }
        }
        chain.reverse();
        chain
    }

    fn visible_sticky_ancestors(&self, first: usize) -> Vec<(usize, &Item)> {
        let chain = self.sticky_ancestors(first);
        let height = f32::from(self.scroll.viewport_bounds().size.height);
        let row_height = f32::from(self.row_height).max(1.0);
        // Even in a short window, reserve room for the first actual row.
        let maximum = (height / row_height).floor() as usize;
        let maximum = maximum.saturating_sub(1);
        let skip = chain.len().saturating_sub(maximum);
        chain.into_iter().skip(skip).collect()
    }

    fn update(&mut self, rows: Vec<Row>, width: Pixels, scale: f32, row_height: Pixels) {
        self.discard_superseded();
        let width_changed = self.width != width || self.scale != scale
            || self.row_height != row_height;
        let old = &self.rows;
        let previous_top = self.scroll.logical_scroll_top();
        let previous_row = old.get(previous_top.item_ix).cloned();
        let previous_anchor = matches!(self.restore, Restore::Idle)
            .then(|| Self::observed_anchor_at(&self.identities, &self.scroll)).flatten();
        let previous_offset = self.scroll.scroll_px_offset_for_scrollbar();
        let previous_offset = ReadingOffset::new(f32::from(previous_offset.x), f32::from(previous_offset.y))
            .unwrap_or_default();
        let mut replaced_top = false;
        let mut retained_changed = Vec::new();
        if old.len() == rows.len() {
            // Equal-length refreshes often change two separated notes. Keep
            // every unchanged item between them measured and anchored.
            let mut at = 0;
            while at < rows.len() {
                if Self::same_identity(&old[at], &rows[at]) { at += 1; continue; }
                let start = at;
                while at < rows.len() && !Self::same_identity(&old[at], &rows[at]) { at += 1; }
                replaced_top |= (start..at).contains(&previous_top.item_ix);
                self.scroll.splice(start..at, at - start);
            }
            retained_changed.extend((0..rows.len()).filter(|&i|
                Self::same_identity(&old[i], &rows[i]) && old[i] != rows[i]));
        } else {
            let prefix = old.iter().zip(&rows).take_while(|(a, b)| Self::same_identity(a, b)).count();
            let suffix = old[prefix..].iter().rev().zip(rows[prefix..].iter().rev())
                .take_while(|(a, b)| Self::same_identity(a, b)).count();
            let old_end = old.len() - suffix;
            let new_end = rows.len() - suffix;
            replaced_top = (prefix..old_end).contains(&previous_top.item_ix);
            if old_end != prefix || new_end != prefix {
                self.scroll.splice(prefix..old_end, new_end - prefix);
            }
            retained_changed.extend((0..prefix).filter(|&i| old[i] != rows[i]));
            retained_changed.extend((0..suffix).filter_map(|i| {
                let old_i = old_end + i;
                let new_i = new_end + i;
                (old[old_i] != rows[new_i]).then_some(new_i)
            }));
        }
        let surviving_top = if replaced_top && let Some(previous_row) = previous_row {
            // A middle replacement can contain an unchanged focused item.
            // Retain its semantic row rather than landing on the first note.
            rows.iter().position(|row| Self::same_identity(row, &previous_row))
        } else { None };
        if width_changed {
            // Splice first: proportional remeasurement must capture the new
            // logical anchor when a fold and a scale change share a frame.
            self.scroll.remeasure_with_uniform_item_height(row_height);
            self.width = width;
            self.scale = scale;
            self.row_height = row_height;
        } else {
            // Content may change without changing a row's identity (counts,
            // names, or state). Only invalidate those retained measurements.
            // Retained rows can change paint or text without changing their
            // semantic id. They keep their index but need fresh measurement.
            for i in retained_changed {
                self.scroll.remeasure_items(i..i + 1);
            }
        }
        if old.as_ref() != &rows {
            self.identities = Rc::new(Self::identities(&rows));
            self.rows = Rc::new(rows);
        }
        if matches!(self.restore, Restore::Idle) && let Some(index) = surviving_top {
            if let Some(anchor) = previous_anchor {
                self.scroll.scroll_to_proportional(index, anchor.fraction());
                if let Some(ticket) = self.next_intent() {
                    self.restore = Restore::Measuring { ticket, offset: previous_offset, anchor };
                }
            } else {
                self.scroll.scroll_to(ListOffset { item_ix: index,
                    offset_in_item: previous_top.offset_in_item });
            }
        }
        match std::mem::replace(&mut self.restore, Restore::Idle) {
            Restore::Queued { ticket, offset, anchor: Some(anchor) } => {
                if let Some(index) = self.anchor_index(&anchor) {
                    self.scroll.scroll_to_proportional(index, anchor.fraction());
                    self.restore = Restore::Measuring { ticket, offset, anchor };
                } else {
                    // A folded-away or changed row has no semantic target.
                    // Use the checked pixel displacement from that visit.
                    self.scroll.clear_pending_scroll_adjustment();
                    self.restore_pixels(offset);
                }
            }
            Restore::Queued { offset, anchor: None, .. } => {
                self.scroll.clear_pending_scroll_adjustment();
                self.restore_pixels(offset);
            }
            measuring @ Restore::Measuring { .. } => self.restore = measuring,
            Restore::Idle => {}
        }
    }
}

fn shelf_lens(lens: crate::navigation::presentation::ShelfLens) -> Lens {
    use crate::navigation::presentation::ShelfLens as P;
    match lens { P::Contents => Lens::Contents, P::Versions => Lens::Versions, P::RestsOn => Lens::RestsOn, P::UsedBy => Lens::UsedBy }
}
fn shelf_presentation_lens(lens: Lens) -> crate::navigation::presentation::ShelfLens {
    use crate::navigation::presentation::ShelfLens as P;
    match lens { Lens::Contents => P::Contents, Lens::Versions => P::Versions, Lens::RestsOn => P::RestsOn, Lens::UsedBy => P::UsedBy }
}

/// The shelf region.
pub(crate) struct Shelf {
    core: RegionCore,
    links: Links,
    overlay_surface: bool,
    pub(crate) targets: Targets,
    hover: HoverIntent,
    /// Where the sidebar is (following the reader, or hoisted).
    crumbs: Crumbs,
    /// The place `crumbs` last followed.
    followed: Place,
    reading_visit: crate::navigation::presentation::VisitId,
    /// The lens on the scope, and the book it was chosen in: a new book
    /// opens on its contents, the same book keeps the lens across its pages
    /// and across the releases it is read at.
    lens: Lens,
    lens_book: Option<SharedString>,
    /// Groups the person opened or closed by hand.
    folds: Folds,
    /// The words typed to narrow the scope.
    narrow: Narrow,
    /// The crate of yours whose uses Contents is narrowed to.
    via: Option<WorkspaceCrate>,
    /// The pending `G` of a lens chord, and the timer that turns it into a
    /// letter.
    chord: Chord,
    chord_timer: Option<Task<()>>,
    /// The keystroke interceptor (see [`input`]).
    keys: Option<Subscription>,
    /// A peek is following the selection.
    peeking: bool,
    /// How the room the column has sets its lens strip and its rows.
    form: SideForm,
    /// The follow has moved the reader's row: scroll it into view.
    reveal: bool,
    /// The resting width of the full shelf, from the shell.
    rest: Pixels,
    /// The resting width of the spine, from the shell.
    spine: Pixels,
    /// What was last drawn.
    layout: RowLayout,
    matched: Option<Matched>,
    /// What each item carries, and what it was made from.
    book: Option<(BookKey, Rc<StateBook>)>,
    no_book: Rc<StateBook>,
    /// The comb's pinned and viewed releases as last drawn (tests).
    #[cfg(test)]
    comb: std::cell::Cell<Option<(Option<usize>, Option<usize>)>>,
    #[cfg(test)]
    comb_versions: std::cell::RefCell<Vec<SharedString>>,
    /// The upgrade line's releases as last drawn (tests; the line's words
    /// are not published to the probe).
    #[cfg(test)]
    upgrade: std::cell::RefCell<Option<(SharedString, SharedString)>>,
}

impl Shelf {
    #[cfg(test)]
    pub(super) fn diagnostic_restore_ticket(&self) -> Option<u64> {
        match &self.layout.restore {
            Restore::Queued { ticket, .. } | Restore::Measuring { ticket, .. }
                if self.layout.intent.get() == Some(*ticket) => Some(*ticket),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(super) fn diagnostic_scroll_state(&self) -> ListState {
        self.layout.scroll.clone()
    }

    #[cfg(test)]
    pub(super) fn diagnostic_row_height(&self) -> Pixels {
        self.layout.row_height
    }

    #[cfg(test)]
    pub(super) fn diagnostic_queue_restore(&mut self, offset: ReadingOffset, anchor: Option<ShelfRowAnchor>) {
        self.layout.queue_restore(offset, anchor);
    }

    #[cfg(test)]
    pub(super) fn diagnostic_last_row_anchor(&self) -> Option<ShelfRowAnchor> {
        let index = self.layout.rows.iter().rposition(|row| row.item().is_some())?;
        let key = self.layout.identities.get(index)?.as_ref()?;
        ShelfRowAnchor::new(key.kind, key.text.clone(), usize::from(key.occurrence), 0.25)
    }

    #[cfg(test)]
    pub(super) fn diagnostic_scroll_to_nested_row(&mut self) -> bool {
        let preferred = self.layout.rows.iter().enumerate().find(|(index, row)|
            self.layout.rows.len().saturating_sub(*index) >= 6
                && row.item().is_some_and(|item| item.depth > 0)).map(|(index, _)| index);
        let Some(index) = preferred.or_else(|| self.layout.rows.iter().position(|row|
            row.item().is_some_and(|item| item.depth > 0))) else {
            return false;
        };
        self.layout.scroll_to_user(index);
        true
    }

    /// A shelf called `name` (`shelf`, or `shelf-over` for the overlay one).
    pub(crate) fn new(name: &'static str, links: Links, store: &DataStore) -> Self {
        let snapshot = store.snapshot();
        let route = snapshot.route().clone();
        let reading = &snapshot.session().reading.current.presentation.controls().shelf;
        let mut layout = RowLayout::new();
        layout.queue_restore(reading.offset, reading.anchor.clone());
        Self {
            core: RegionCore::new(
                store,
                &[
                    Branch::Route,
                    Branch::Reading,
                    Branch::Overlay,
                    Branch::Workspace,
                    Branch::GraphFocus,
                ],
            ),
            links,
            overlay_surface: name == "shelf-over",
            targets: Targets::named(name),
            hover: HoverIntent::default(),
            crumbs: Crumbs::following(&route),
            followed: Place::of(&route),
            reading_visit: store.snapshot().session().reading.current.id,
            lens: shelf_lens(store.snapshot().session().reading.current.presentation.controls().shelf.lens),
            lens_book: Place::of(&route).book,
            folds: Folds::from_reading(&store.snapshot().session().reading.current.presentation.controls().shelf),
            narrow: Narrow::from_text(store.snapshot().session().reading.current.presentation.controls().shelf.narrow.as_ref().map_or("", |text| text.as_str())),
            via: store.snapshot().session().reading.current.presentation.controls().shelf.via.as_ref().map(|via| WorkspaceCrate::new(via.as_str().to_owned())),
            chord: Chord::Idle,
            chord_timer: None,
            keys: None,
            peeking: false,
            form: SideForm::Full,
            reveal: true,
            rest: px(264.0),
            spine: px(42.0),
            layout,
            matched: None,
            book: None,
            no_book: Rc::new(StateBook::none()),
            #[cfg(test)]
            comb: std::cell::Cell::new(None),
            #[cfg(test)]
            comb_versions: std::cell::RefCell::new(Vec::new()),
            #[cfg(test)]
            upgrade: std::cell::RefCell::new(None),
        }
    }

    /// The upgrade line's releases as last drawn (tests).
    #[cfg(test)]
    pub(crate) fn upgrade_line(&self) -> Option<(SharedString, SharedString)> {
        self.upgrade.borrow().clone()
    }

    /// The comb's pinned and viewed versions as last drawn (tests).
    #[cfg(test)]
    pub(crate) fn comb_marks(&self) -> Option<(Option<SharedString>, Option<SharedString>)> {
        let versions = self.comb_versions.borrow();
        self.comb.get().map(|(pinned, viewing)| {
            (
                pinned.and_then(|i| versions.get(i).cloned()),
                viewing.and_then(|i| versions.get(i).cloned()),
            )
        })
    }

    #[cfg(test)]
    pub(crate) fn current_symbols(&self) -> Vec<SymbolRef> {
        self.layout.rows
            .iter()
            .filter_map(Row::item)
            .filter(|item| item.current)
            .filter_map(|item| item.source.clone())
            .collect()
    }

    pub(crate) const fn renders(&self) -> u64 {
        self.core.renders()
    }

    /// The shell tells the shelf its resting widths (no notify: a change of
    /// resting width is always a change of the column's bounds too).
    pub(crate) fn set_rest(&mut self, rest: Pixels, spine: Pixels) {
        self.rest = rest;
        self.spine = spine;
    }

    /// Where the list of rows is on screen (window coordinates): what a twin
    /// ring on a row is clipped to.
    pub(crate) fn viewport(&self) -> gpui::Bounds<Pixels> {
        self.layout.scroll.viewport_bounds()
    }

    /// Keeps the focused row on screen after a keyboard walk.
    pub(crate) fn reveal_focused(&mut self) {
        if let Some(index) = self.targets.focused().and_then(|id| {
            self.layout.rows
                .iter()
                .position(|row| row.item().is_some_and(|item| item.key == id))
        }) {
            self.layout.reveal_item(index);
        }
    }

    // ------------------------------------------------------------ the store

    /// What the store holds, listed.
    fn listing(&mut self, snapshot: &AppSnapshot, cx: &mut Context<Self>) -> Listing {
        let route = snapshot.route();
        let settings = match snapshot.overlay() {
            Some(Overlay::Settings(page)) => Some(page),
            _ => None,
        };
        let package = self.crumbs.package().cloned();
        let release_data =
            package.as_ref().and_then(|package| {
                let (pin, compare_to) = release_address(route, package)?;
                match crate::runtime::releases::get(
                    &pin,
                    snapshot.key(),
                    compare_to,
                    cx,
                ) {
                    crate::runtime::releases::Read::Ready(data) => Some(data),
                    crate::runtime::releases::Read::Reading
                    | crate::runtime::releases::Read::Waiting
                    | crate::runtime::releases::Read::Unavailable(_) => None,
                }
            });
        let diffs = release_data
            .as_ref()
            .map(|data| std::sync::Arc::clone(&data.krate));
        let book = self.book_of(package.as_ref(), route, diffs.as_deref());
        let held = hold::chips(
            crate::runtime::hand::hand_view_for(&snapshot.session().hand, snapshot, cx)
                .cards
                .iter()
                .map(|card| (card.held.clone(), card.name.clone(), card.kind)),
        );
        let back = snapshot.session().back.to_vec();
        let store = self.links.store.read(cx);
        let dossier = package.as_ref().map(|package| store.package(package));
        let orbit = store.orbit();
        let current = match store.graph_focus() {
            Some(focus) => focus
                .indexed
                .as_ref()
                .filter(|(owner, _)| Some(owner) == package.as_ref())
                .map(|(_, symbol)| symbol.clone()),
            None => super::jump::route_symbol(route),
        };
        let inputs = Inputs {
            snapshot,
            route,
            crumbs: &self.crumbs,
            lens: self.lens,
            folds: &self.folds,
            filter: Filter {
                query: self.narrow.query(),
                via: self.via.as_ref(),
            },
            book: &book,
            dossier: dossier.as_ref().map(|resource| crate::core::admit_resource(resource, snapshot.key(), store.owner_serving())),
            orbit: crate::core::admit_resource(&orbit, snapshot.key(), store.owner_serving()),
            current,
            diffs,
            settings,
            held,
            trail: hold::trail(&back, route),
        };
        listing::build(&inputs)
    }

    /// The state book for the package being read, made again only when the
    /// package, the pin or the release being read changes.
    fn book_of(
        &mut self,
        package: Option<&PackageRef>,
        route: &Route,
        diffs: Option<&facet::data::release::Crate>,
    ) -> Rc<StateBook> {
        let (Some(package), Some(krate)) = (package, diffs) else {
            return Rc::clone(&self.no_book);
        };
        let Some((pinned, compare_to)) = release_address(route, package) else {
            return Rc::clone(&self.no_book);
        };
        let spelled = |version: &str| {
            crate::runtime::releases::spelled(krate, version)
                .unwrap_or_else(|| version.to_owned().into())
        };
        let Some(version) = pinned.release_version() else {
            return Rc::clone(&self.no_book);
        };
        let key = BookKey {
            package: package.clone(),
            from: spelled(version),
            to: compare_to.map(spelled),
        };
        if let Some((known, book)) = &self.book
            && *known == key
        {
            return Rc::clone(book);
        }
        let book = Rc::new(StateBook::from_release(
            krate,
            package.display_name(),
            &key.from,
            key.to.as_deref(),
        ));
        self.book = Some((key, Rc::clone(&book)));
        book
    }

    // ------------------------------------------------------------ what a row does

    /// Carries out what a row (or a key) asks.
    /// Pure reading visits may return through history. Mounted callbacks may
    /// act only in the native scene that painted them, before taking focus.
    fn action_guard(&self, cx: &App) -> Rc<dyn Fn(&mut App) -> bool> {
        let surface = if self.overlay_surface { super::root::ShelfNativeSurface::Drawer } else { super::root::ShelfNativeSurface::Docked };
        self.action_guard_for(Some(surface), cx)
    }

    fn action_guard_for(&self, surface: Option<super::root::ShelfNativeSurface>, cx: &App) -> Rc<dyn Fn(&mut App) -> bool> {
        let shell = self.links.shell.clone();
        let scope = shell.upgrade().and_then(|shell| shell.read(cx).shelf_input_scope(self.overlay_surface, cx))
            .filter(|scope| surface.is_none_or(|surface| scope.surface() == surface));
        Rc::new(move |cx| {
            scope.as_ref().is_some_and(|scope| shell.upgrade().is_some_and(|shell| shell.read(cx).admits_shelf_input_scope(scope, cx)))
        })
    }

    fn perform(&mut self, does: &Do, cx: &mut Context<Self>) {
        match does {
            Do::Nothing => {}
            Do::Go(route) => self.links.dispatch(Intent::Navigate(route.clone()), cx),
            Do::Fold(id) => self.flip(id.clone(), cx),
            Do::Lens(lens) => self.set_lens(*lens, cx),
            Do::Release(at) => self.links.dispatch(Intent::SetRelease(at.clone()), cx),
            Do::Project(id) => self.links.dispatch(Intent::ActivateProject(id.clone()), cx),
            Do::ProjectTree(id) => {
                self.links.dispatch(Intent::ActivateProject(id.clone()), cx);
                self.links.dispatch(Intent::Navigate(Route::Orbit(crate::navigation::OrbitRoute::Browse(
                    crate::navigation::BrowseRoute::Tree(id.clone()),
                ))), cx);
            }
            Do::Settings(page) => self.links.dispatch(Intent::OpenSettings(*page), cx),
            Do::Via(name) => {
                // Choosing your crate again lets it go; choosing one shows
                // what it uses in Contents.
                self.via = if self.via.as_ref() == Some(name) {
                    None
                } else {
                    Some(name.clone())
                };
                self.lens = Lens::Contents;
                self.list_changed(cx);
            }
            Do::Widen(query) => {
                let find = Route::Orbit(crate::navigation::OrbitRoute::Browse(
                    crate::navigation::BrowseRoute::Find(query.clone()),
                ));
                self.links.dispatch(Intent::Navigate(find), cx);
            }
        }
    }

    fn flip(&mut self, id: RowId, cx: &mut Context<Self>) {
        self.folds.flip(id.clone());
        if let Some(key) = crate::navigation::presentation::ReadingText::new(id.key().to_string()) {
            self.reading_change(crate::navigation::presentation::ReadingChange::ShelfFold(key), cx);
        }
        cx.notify();
    }

    fn set_lens(&mut self, lens: Lens, cx: &mut Context<Self>) {
        if self.lens != lens {
            self.lens = lens;
            self.list_changed(cx);
        }
    }

    /// The list is a different list now: back to its top.
    fn list_changed(&mut self, cx: &mut Context<Self>) {
        use crate::navigation::presentation::{ReadingChange, ReadingText};
        self.reading_change(ReadingChange::ShelfLens(shelf_presentation_lens(self.lens)), cx);
        self.reading_change(ReadingChange::ShelfFilter {
            narrow: (!self.narrow.is_empty()).then(|| ReadingText::new(self.narrow.query())).flatten(),
            via: self.via.as_ref().and_then(|via| ReadingText::new(via.as_str())),
        }, cx);
        self.layout.cancel_deferred_scroll();
        self.layout.scroll.scroll_to(ListOffset::default());
        cx.notify();
    }

    fn reading_change(&self, change: crate::navigation::presentation::ReadingChange, cx: &mut Context<Self>) {
        let snapshot = self.links.snapshot(cx);
        if snapshot.overlay().is_none() && snapshot.session().preview.is_none() {
            self.links.dispatch(Intent::SetReading { visit: snapshot.session().reading.current.id, change }, cx);
        }
    }

    /// Scopes the sidebar into `scope` (browsing: the reader does not move).
    fn hoist(&mut self, scope: Scope, cx: &mut Context<Self>) {
        self.crumbs.hoist(scope);
        self.watch_scope(cx);
        self.narrow.clear();
        self.via = None;
        self.list_changed(cx);
    }

    /// Asks the store for the package the sidebar is scoped to and watches it:
    /// a package hoisted into from the Library is not the one the reader is on.
    fn watch_scope(&mut self, cx: &mut Context<Self>) {
        let snapshot = self.links.snapshot(cx);
        let keys = self.keys(&snapshot);
        let store = self.links.store.clone();
        store.update(cx, |store, cx| {
            for key in keys.iter().cloned() {
                store.ensure(key, cx);
            }
        });
        self.core.watch_keys(store.read(cx), keys);
    }

    /// Steps out one scope. Returns whether there was one.
    fn step_out(&mut self, cx: &mut Context<Self>) -> bool {
        let moved = self.crumbs.step_out();
        if moved {
            self.narrow.clear();
            self.via = None;
            self.list_changed(cx);
        }
        moved
    }

    /// The row the keyboard stands on.
    fn focused_item(&self) -> Option<&Item> {
        let id = self.targets.focused()?;
        self.layout.rows
            .iter()
            .filter_map(Row::item)
            .find(|item| item.key == id)
    }

    // ------------------------------------------------------------ keys

    /// Reads a keystroke the shell has not bound, while the sidebar has the
    /// keyboard. Returns whether it was the sidebar's.
    fn intercept(
        &mut self,
        event: &KeystrokeEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.owns_keyboard(event, window, cx) {
            return false;
        }
        self.key(&event.keystroke, window, cx)
    }

    /// Whether this keystroke is the sidebar's: its zone is the active one,
    /// the shelf is on screen, and the focus is not in a place that owns
    /// the letters (a text input, a menu, the graph, hint mode).
    fn owns_keyboard(
        &self,
        event: &KeystrokeEvent,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> bool {
        const OWN_THE_LETTERS: [&str; 6] = ["Input", "Menu", "Graph", "BrowseCompare", "hints", facet::controls::button::NATIVE_CONTROL];
        if !self.targets.is_active() {
            return false;
        }
        let stack = &event.context_stack;
        let in_shell = stack
            .iter()
            .any(|context| context.contains(super::keys::CONTEXT));
        let elsewhere = stack
            .iter()
            .any(|context| OWN_THE_LETTERS.iter().any(|name| context.contains(name)));
        let snapshot = self.links.snapshot(cx);
        let on_screen = self.links.shell.upgrade()
            .is_some_and(|shell| shell.read(cx).shelf_input_owner(self.overlay_surface));
        let _ = window;
        in_shell && !elsewhere && on_screen
            && (self.overlay_surface || snapshot.overlay().is_none())
            && !matches!(snapshot.overlay(), Some(crate::navigation::Overlay::CommandPalette | crate::navigation::Overlay::AddProject))
    }

    /// One key of the sidebar's. Returns whether it was used.
    fn key(
        &mut self,
        keystroke: &gpui::Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(key) = input::decode(keystroke) else {
            return false;
        };
        // The peek belongs to the row it was opened on: anything that changes
        // the list closes it (moving the selection follows it instead).
        let peek_open = self.peek_is_open(window, cx);
        if peek_open
            && !matches!(
                key,
                SideKey::Up | SideKey::Down | SideKey::Space | SideKey::Escape
            )
        {
            let holds = matches!(key, SideKey::Char('h' | 'H'))
                && self.narrow.is_empty()
                && !self.chord.is_armed();
            let graphs = matches!(key, SideKey::Char('g' | 'G'));
            if !holds && !graphs {
                self.close_peek(window, cx);
            }
        }
        match key {
            SideKey::Up => self.walk(-1, window, cx),
            SideKey::Down => self.walk(1, window, cx),
            SideKey::Space => self.toggle_peek(window, cx),
            SideKey::Char('h' | 'H')
                if peek_open && self.narrow.is_empty() && !self.chord.is_armed() =>
            {
                self.hold_selection(window, cx)
            }
            SideKey::Char(character) => {
                match self.chord.feed(
                    character,
                    Query::of(self.narrow.is_empty()),
                    Peek::of(peek_open),
                ) {
                    Typed::Graph => {
                        self.chord_timer = None;
                        self.close_peek(window, cx);
                        self.graph_selection(cx);
                    }
                    Typed::Armed => self.arm_chord(cx),
                    Typed::Lens(lens) => {
                        self.chord_timer = None;
                        self.set_lens(lens, cx);
                        cx.notify();
                    }
                    Typed::Text(text) => {
                        self.chord_timer = None;
                        self.narrow.push_str(&text);
                        self.list_changed(cx);
                    }
                }
                true
            }
            SideKey::Backspace => {
                let flushed = self.flush_chord_now();
                let popped = self.narrow.pop();
                if flushed || popped {
                    self.list_changed(cx);
                }
                flushed || popped
            }
            SideKey::Left => self.left(cx),
            SideKey::Right => self.right(cx),
            // Esc closes the peek first (the shell's, through the float
            // layer), then clears the narrowing.
            SideKey::Escape => !peek_open && self.escape(cx),
        }
    }

    /// ↑ ↓: moves the selection; an open peek follows it.
    fn walk(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.targets.walk(delta) {
            self.reveal_focused();
            cx.notify();
            self.follow_peek(window, cx);
        }
        true
    }

    // ------------------------------------------------------------ the peek

    fn peek_is_open(&mut self, window: &Window, cx: &mut Context<Self>) -> bool {
        let open = self.peeking && float::is_open(&peek::key(), window, cx);
        self.peeking = open;
        open
    }

    fn close_peek(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.peeking {
            float::close(&peek::key(), window, cx);
            self.peeking = false;
        }
    }

    /// Space: shows the peek of the row the keyboard is on, or puts it away.
    /// Returns whether the key was used (a row with no page has no peek).
    fn toggle_peek(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.peek_is_open(window, cx) {
            self.close_peek(window, cx);
            return true;
        }
        self.show_peek(window, cx)
    }

    fn show_peek(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(item) = self.focused_item().cloned() else {
            return false;
        };
        let Some(page) = item.warm.clone() else {
            return false;
        };
        let Some(anchor) = self.targets.focused_bounds() else {
            return false;
        };
        let store = self.links.store.clone();
        store.update(cx, |store, cx| {
            store.ensure(page.clone(), cx);
        });
        let book = self
            .book
            .as_ref()
            .map_or_else(|| Rc::clone(&self.no_book), |(_, book)| Rc::clone(book));
        let extras = peek::Extras {
            symbol: item.source.clone(),
            path: item.path.clone(),
            book,
            store,
            hoists: item.hoists.is_some(),
        };
        float::open(
            peek::request(page, item.name.clone(), anchor, extras),
            window,
            cx,
        );
        self.peeking = true;
        true
    }

    /// The selection moved: the peek moves with it, once the row it moved to
    /// has been laid out (a row scrolled into view has no place until the
    /// next frame).
    fn follow_peek(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.peek_is_open(window, cx) {
            return;
        }
        let shelf = cx.weak_entity();
        window.on_next_frame(move |window, cx| {
            let _ = shelf.update(cx, |shelf, cx| {
                if !shelf.show_peek(window, cx) {
                    shelf.close_peek(window, cx);
                }
            });
        });
    }

    /// H: holds the row the peek is on in the hand.
    fn hold_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let held = self.focused_item().and_then(|item| match &item.does {
            Do::Go(Route::Symbol(route)) => Some((route.package.clone(), Some(route.id.clone()))),
            Do::Go(Route::Package(route)) => Some((route.package.clone(), None)),
            _ => None,
        });
        let Some((package, id)) = held else {
            return false;
        };
        let at = super::root::now_ms();
        self.links.dispatch(
            Intent::Hold(crate::model::hand::Held {
                package,
                id,
                why: crate::model::hand::HeldWhy::Pin,
                held_at: at,
                touched_at: at,
            }),
            cx,
        );
        self.close_peek(window, cx);
        true
    }

    /// G G: the graph of the row the peek is on.
    fn graph_selection(&mut self, cx: &mut Context<Self>) {
        let route = self.focused_item().and_then(|item| match &item.does {
            Do::Go(route @ Route::Symbol(_)) => route.with_view(crate::navigation::View::Graph),
            _ => None,
        });
        if let Some(route) = route {
            self.links.dispatch(Intent::Navigate(route), cx);
        }
    }

    /// `G` waits for its letter for [`input::CHORD_WINDOW`], then it is a `g`.
    fn arm_chord(&mut self, cx: &mut Context<Self>) {
        self.chord_timer = Some(cx.spawn(async move |shelf, cx| {
            cx.background_executor().timer(input::CHORD_WINDOW).await;
            let _ = shelf.update(cx, |shelf, cx| {
                shelf.chord_timer = None;
                if shelf.flush_chord_now() {
                    shelf.list_changed(cx);
                }
            });
        }));
        cx.notify();
    }

    /// A pending `G` becomes the `g` of the query. Returns whether one was.
    fn flush_chord_now(&mut self) -> bool {
        self.chord_timer = None;
        match self.chord.flush() {
            Some(text) => {
                self.narrow.push_str(text);
                true
            }
            None => false,
        }
    }

    /// ←: closes the group the keyboard is on, or steps out.
    fn left(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.crumbs.is_hoisted()
            && let Some(item) = self.focused_item()
            && item.fold == Some(Fold::Open)
        {
            let id = item.id.clone();
            self.flip(id, cx);
            return true;
        }
        self.step_out(cx)
    }

    /// →: scopes into the row the keyboard is on; a closed group with
    /// nothing to scope into opens.
    fn right(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(item) = self.focused_item() else {
            return false;
        };
        if let Some(scope) = item.hoists.clone() {
            self.hoist(scope, cx);
            return true;
        }
        if item.fold == Some(Fold::Shut) {
            let id = item.id.clone();
            self.flip(id, cx);
            return true;
        }
        false
    }

    /// Esc: clears the narrowing (the typed words first, then the crate).
    /// Returns whether there was one to clear.
    pub(crate) fn escape(&mut self, cx: &mut Context<Self>) -> bool {
        let flushed = self.flush_chord_now();
        let cleared = self.narrow.clear() || self.via.take().is_some();
        if cleared || flushed {
            self.list_changed(cx);
        }
        cleared || flushed
    }

    /// A route change moved the reader: the sidebar follows it.
    fn follow(&mut self, store: &DataStore) {
        let snapshot = store.snapshot();
        let route = snapshot.route();
        let place = Place::of(route);
        let tree =
            crate::runtime::store::route_package(route).map(|package| store.package(&package));
        if place == self.followed {
            // Another release of the same book: what is shown stays shown.
            self.crumbs.retarget(route);
        } else {
            self.crumbs.follow(
                route,
                tree.as_ref()
                    .and_then(|resource| resource.loaded_value())
                    .and_then(|dossier| dossier.outline.known()),
            );
        }
        let reading = &snapshot.session().reading.current.presentation.controls().shelf;
        let visit = snapshot.session().reading.current.id;
        let changed_list = self.lens != shelf_lens(reading.lens)
            || self.narrow.query() != reading.narrow.as_ref().map_or("", ReadingText::as_str)
            || self.via.as_ref().map(WorkspaceCrate::as_str)
                != reading.via.as_ref().map(ReadingText::as_str);
        if self.reading_visit != visit {
            self.layout.queue_restore(reading.offset, reading.anchor.clone());
            self.targets.clear_focus();
            if let Some(crate::navigation::presentation::ReadingFocus::Shelf(key)) = &snapshot.session().reading.current.presentation.controls().focus {
                self.targets.focus(gpui::SharedString::from(key.as_str().to_owned()));
            }
            self.reading_visit = visit;
        } else if changed_list {
            // A same-visit lens/filter delta can arrive through navigation,
            // without the local `list_changed` handler. Its old measured
            // position and pending restore belong to the previous row set.
            self.layout.cancel_deferred_scroll();
            self.layout.scroll.scroll_to(ListOffset::default());
        }
        self.lens = shelf_lens(reading.lens);
        self.lens_book = place.book.clone();
        self.folds = Folds::from_reading(reading);
        self.narrow = Narrow::from_text(reading.narrow.as_ref().map_or("", |query| query.as_str()));
        self.via = reading.via.as_ref().map(|via| WorkspaceCrate::new(via.as_str().to_owned()));
        // History restores its exact position; the current row must not
        // override that offset with the fresh-navigation centering rule.
        if place != self.followed { self.reveal = reading.offset == Default::default() && reading.anchor.is_none(); }
        if reading.anchor.is_some() { self.reveal = false; }
        self.followed = place;
    }
}

/// The exact release whose comparison belongs to the shelf package. When the
/// reader views another release, its route still names the pin. A hoisted
/// unrelated package reads only its own release; a same-name package from a
/// different registry never borrows the reader's pin or comparison.
fn release_address<'a>(route: &'a Route, package: &PackageRef) -> Option<(PackageRef, Option<&'a str>)> {
    if crate::runtime::store::route_package(route).as_ref() == Some(package) {
        let pin = match route {
            Route::Package(route) => PackageRef::parse(route.package.as_str()).ok()?,
            Route::Symbol(route) => PackageRef::parse(route.package.as_str()).ok()?,
            // A Cargo source file reads its exact package; it names no other release.
            Route::CargoSource(route) => PackageRef::parse(route.package.as_str()).ok()?,
            Route::Orbit(_) | Route::World => return None,
        };
        Some((pin, route.at().map(|at| at.as_str())))
    } else {
        Some((package.clone(), None))
    }
}

#[cfg(test)]
mod measured_layout_tests {
    use super::{Do, Item, Row, RowId, RowLayout};
    use super::row::Mark;
    use facet::icons::Kind;
    use gpui::{AppContext as _, ListOffset, px};
    use std::sync::Arc;

    struct MeasuredRows {
        scroll: gpui::ListState,
        ordinary: gpui::Pixels,
        tall_first: bool,
    }

    impl gpui::Render for MeasuredRows {
        fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
            use gpui::{IntoElement as _, Styled as _, div, list};
            let ordinary = self.ordinary;
            let tall_first = self.tall_first;
            list(self.scroll.clone(), move |index, _, _| {
                div().h(if index == 0 && tall_first { ordinary * 5.0 } else { ordinary })
                    .w_full().into_any_element()
            }).size_full()
        }
    }

    fn paint_rows(cx: &mut gpui::VisualTestContext, layout: &RowLayout, ordinary: gpui::Pixels, tall_first: bool) {
        use gpui::{AppContext as _, IntoElement as _, point, size};
        let scroll = layout.scroll.clone();
        cx.draw(point(px(0.0), px(0.0)), size(px(264.0), px(96.0)), |_, cx| {
            cx.new(|_| MeasuredRows { scroll, ordinary, tall_first }).into_any_element()
        });
    }

    fn item(number: usize) -> Row {
        Row::Item(Item::new(RowId::Dependency(Arc::from(format!("row-{number}"))), 0,
            Mark::Kind(Kind::Function), format!("row {number}"), Do::Nothing))
    }

    fn anchor(layout: &RowLayout, index: usize) -> crate::navigation::presentation::ShelfRowAnchor {
        let key = layout.identities[index].as_ref().expect("checked row identity");
        crate::navigation::presentation::ShelfRowAnchor::new(key.kind, key.text.clone(),
            usize::from(key.occurrence), 0.25).expect("bounded row anchor")
    }

    #[gpui::test]
    fn old_restore_cannot_overwrite_native_wheel_or_keyboard_reveal(cx: &mut gpui::TestAppContext) {
        use crate::navigation::presentation::ReadingOffset;
        use gpui::point;
        let cx = cx.add_empty_window();
        let mut layout = RowLayout::new();
        let rows = (0..80).map(item).collect::<Vec<_>>();
        layout.update(rows.clone(), px(264.0), 1.0, px(32.0));
        paint_rows(cx, &layout, px(32.0), false);
        let saved = ReadingOffset::new(0.0, -648.0).expect("visited offset");
        layout.queue_restore(saved, Some(anchor(&layout, 20)));
        layout.update(rows.clone(), px(264.0), 1.0, px(32.0));
        let old = layout.deferred_scroll().expect("restore waits for paint");
        paint_rows(cx, &layout, px(32.0), false);
        let before = layout.scroll.logical_scroll_top();
        cx.simulate_scroll(point(px(50.0), px(50.0)), point(px(0.0), px(-64.0)));
        let wheeled = layout.scroll.logical_scroll_top();
        assert!(wheeled.item_ix != before.item_ix || wheeled.offset_in_item != before.offset_in_item,
            "a real native scroll event moved the mounted list");
        assert!(!layout.deliver_deferred(old), "the previous visit's delayed restore lost wheel ownership");
        let after_callback = layout.scroll.logical_scroll_top();
        assert_eq!(after_callback.item_ix, wheeled.item_ix);
        assert_eq!(after_callback.offset_in_item, wheeled.offset_in_item);

        layout.queue_restore(saved, Some(anchor(&layout, 20)));
        layout.update(rows, px(264.0), 1.0, px(32.0));
        let old = layout.deferred_scroll().expect("second restore waits for paint");
        paint_rows(cx, &layout, px(32.0), false);
        layout.reveal_item(35);
        let revealed = layout.scroll.logical_scroll_top();
        assert!(!layout.deliver_deferred(old), "keyboard focus supersedes the old restore");
        let after_callback = layout.scroll.logical_scroll_top();
        assert_eq!(after_callback.item_ix, revealed.item_ix);
        assert_eq!(after_callback.offset_in_item, revealed.offset_in_item);
        assert!(revealed.item_ix > 20, "keyboard moved the viewport beyond the old anchor");

        layout.queue_restore(saved, Some(anchor(&layout, 20)));
        layout.update((0..80).map(item).collect(), px(264.0), 1.0, px(32.0));
        let old = layout.deferred_scroll().expect("third restore waits for paint");
        layout.scroll_to_user(50); // the same path as a sticky ancestor click
        assert!(!layout.deliver_deferred(old));
        assert_eq!(layout.scroll.logical_scroll_top().item_ix, 50);
    }

    #[test]
    fn first_layout_reveal_is_revoked_by_a_newer_scroll_request() {
        use crate::navigation::presentation::ReadingOffset;
        let mut layout = RowLayout::new();
        layout.update((0..80).map(item).collect(), px(264.0), 1.0, px(32.0));
        assert_eq!(layout.scroll.viewport_bounds().size.height, px(0.0));
        layout.reveal_item(30);
        let old = layout.deferred_scroll().expect("first-layout reveal waits for measured viewport");
        layout.queue_restore(ReadingOffset::new(0.0, -648.0).expect("offset"), Some(anchor(&layout, 20)));
        layout.update(layout.rows.as_ref().clone(), px(264.0), 1.0, px(32.0));
        assert!(!layout.deliver_deferred(old));
        assert_eq!(layout.scroll.logical_scroll_top().item_ix, 20,
            "the old current-row reveal cannot displace a newer history restore");
    }

    #[gpui::test]
    fn newer_restore_wins_even_if_the_old_prepaint_arrives_first(cx: &mut gpui::TestAppContext) {
        use crate::navigation::presentation::ReadingOffset;
        let cx = cx.add_empty_window();
        let mut layout = RowLayout::new();
        let rows = (0..80).map(item).collect::<Vec<_>>();
        layout.update(rows.clone(), px(264.0), 1.0, px(32.0));
        paint_rows(cx, &layout, px(32.0), false);
        let saved = ReadingOffset::new(0.0, -648.0).expect("visited offset");
        layout.queue_restore(saved, Some(anchor(&layout, 20)));
        layout.update(rows.clone(), px(264.0), 1.0, px(32.0));
        let first = layout.deferred_scroll().expect("A prepaint request");
        layout.queue_restore(saved, Some(anchor(&layout, 40)));
        layout.update(rows, px(264.0), 1.0, px(32.0));
        let second = layout.deferred_scroll().expect("B prepaint request");
        assert_ne!(first, second);
        assert!(!layout.deliver_deferred(first), "A cannot consume B's pending measurement");
        paint_rows(cx, &layout, px(32.0), false);
        assert!(layout.deliver_deferred(second));
        assert_eq!(layout.scroll.logical_scroll_top().item_ix, 40);
    }

    #[gpui::test]
    fn an_unmounted_list_retains_its_ticket_until_active_prepaint(cx: &mut gpui::TestAppContext) {
        use crate::navigation::presentation::ReadingOffset;
        use gpui::{Context, IntoElement as _, ParentElement as _, Render, Styled as _, Window, div, list};
        use std::cell::{Cell, RefCell};
        use std::rc::Rc;
        let mut layout = RowLayout::new();
        let rows = (0..80).map(item).collect::<Vec<_>>();
        layout.update(rows.clone(), px(264.0), 1.0, px(32.0));
        layout.queue_restore(ReadingOffset::new(0.0, -648.0).expect("offset"), Some(anchor(&layout, 20)));
        layout.update(rows, px(264.0), 1.0, px(32.0));
        let layout = Rc::new(RefCell::new(layout));
        let delivered = Rc::new(Cell::new(0));
        let active = Rc::new(Cell::new(true));
        let settle = Rc::new(Cell::new(false));

        struct Owner { layout: Rc<RefCell<RowLayout>>, delivered: Rc<Cell<u32>>, settle: Rc<Cell<bool>> }
        impl Render for Owner {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl gpui::IntoElement {
                let request = self.layout.borrow().deferred_scroll();
                let layout = Rc::clone(&self.layout);
                let delivered = Rc::clone(&self.delivered);
                let settle = Rc::clone(&self.settle);
                let scroll = self.layout.borrow().scroll.clone();
                div().size_full()
                    .child(list(scroll, |_, _, _| div().h(px(32.0)).w_full().into_any_element()).size_full())
                    .on_children_prepainted(move |_, _, _| {
                        if settle.get() && let Some(request) = request
                            && layout.borrow_mut().deliver_deferred(request) {
                            delivered.set(delivered.get() + 1);
                        }
                    })
            }
        }
        struct Host { owner: gpui::Entity<Owner>, active: Rc<Cell<bool>> }
        impl Render for Host {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl gpui::IntoElement {
                let root = div().w(px(264.0)).h(px(96.0));
                if self.active.get() { root.child(self.owner.clone()) } else { root }
            }
        }
        let (_host, cx) = cx.add_window_view({
            let layout = Rc::clone(&layout);
            let delivered = Rc::clone(&delivered);
            let active = Rc::clone(&active);
            let settle = Rc::clone(&settle);
            move |_, cx| {
                let owner = cx.new(|_| Owner { layout, delivered, settle });
                Host { owner, active }
            }
        });
        cx.update(|window, cx| { window.refresh(); window.draw(cx).clear(cx); });
        assert!(layout.borrow().deferred_scroll().is_some(), "the pending restore has not been settled");
        active.set(false);
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        assert_eq!(delivered.get(), 0);

        active.set(true);
        settle.set(true);
        cx.update(|window, cx| { window.refresh(); window.draw(cx).clear(cx); });
        assert_eq!(delivered.get(), 1);
        assert_eq!(layout.borrow().scroll.logical_scroll_top().item_ix, 20);
    }

    #[test]
    fn fold_splice_keeps_the_same_logical_row_and_zoom_remeasures_in_place() {
        let mut layout = RowLayout::new();
        let rows = (0..80).map(|n| Row::Note(format!("note {n}").into())).collect::<Vec<_>>();
        layout.update(rows.clone(), px(264.0), 1.0, px(32.0));
        layout.scroll.scroll_to(ListOffset { item_ix: 60, offset_in_item: px(7.0) });

        let mut opened = rows;
        opened.splice(12..12, [Row::Note("child one".into()), Row::Note("child two".into())]);
        let mut simultaneous = RowLayout::new();
        simultaneous.update(layout.rows.as_ref().clone(), px(264.0), 1.0, px(32.0));
        simultaneous.scroll.scroll_to(ListOffset { item_ix: 60, offset_in_item: px(7.0) });
        simultaneous.update(opened.clone(), px(220.0), 2.0, px(64.0));
        assert_eq!(simultaneous.scroll.logical_scroll_top().item_ix, 62,
            "a fold and scale change in one paint remap the anchor before remeasurement");
        layout.update(opened.clone(), px(264.0), 1.0, px(32.0));
        assert_eq!(layout.scroll.item_count(), 82);
        assert_eq!(layout.scroll.logical_scroll_top().item_ix, 62,
            "inserting rows above the viewport preserves the logical row");
        assert_eq!(layout.scroll.logical_scroll_top().offset_in_item, px(7.0));

        layout.update(opened, px(220.0), 2.0, px(64.0));
        assert_eq!(layout.scroll.item_count(), 82);
        assert_eq!(layout.scroll.logical_scroll_top().item_ix, 62,
            "width and text-scale remeasurement retains the row anchor");
    }

    #[test]
    fn first_reveal_before_a_viewport_exists_starts_at_the_requested_row() {
        let mut layout = RowLayout::new();
        layout.update((0..80).map(|n| Row::Note(format!("note {n}").into())).collect(),
            px(264.0), 1.0, px(32.0));
        assert_eq!(layout.scroll.viewport_bounds().size.height, px(0.0));
        layout.reveal_item(50);
        assert_eq!(layout.scroll.logical_scroll_top().item_ix, 50);
    }

    #[test]
    fn separated_note_refreshes_do_not_displace_a_surviving_item_anchor() {
        let mut layout = RowLayout::new();
        let mut rows = (0..90).map(|n| Row::Note(format!("note {n}").into())).collect::<Vec<_>>();
        rows[60] = Row::Item(Item::new(RowId::Dependency(Arc::from("anchor")), 0,
            Mark::Kind(Kind::Function), "anchor", Do::Nothing));
        layout.update(rows.clone(), px(264.0), 1.0, px(32.0));
        layout.scroll.scroll_to(ListOffset { item_ix: 60, offset_in_item: px(7.0) });
        rows[12] = Row::Note("first refreshed explanation".into());
        rows[78] = Row::Note("second refreshed explanation".into());
        layout.update(rows, px(264.0), 1.0, px(32.0));
        assert_eq!(layout.scroll.logical_scroll_top().item_ix, 60);
        assert_eq!(layout.scroll.logical_scroll_top().offset_in_item, px(7.0));
    }

    #[gpui::test]
    fn back_restores_the_same_row_past_an_unmeasured_tall_note(cx: &mut gpui::TestAppContext) {
        use crate::navigation::presentation::ReadingOffset;
        let cx = cx.add_empty_window();
        let mut layout = RowLayout::new();
        let mut rows = vec![Row::Note("a wrapped guidance note five rows high".into())];
        rows.extend((1..80).map(item));
        layout.update(rows.clone(), px(264.0), 1.0, px(32.0));
        paint_rows(cx, &layout, px(32.0), true);
        layout.scroll.scroll_to(ListOffset { item_ix: 20, offset_in_item: px(8.0) });
        paint_rows(cx, &layout, px(32.0), true);
        let anchor = RowLayout::observed_anchor_at(&layout.identities, &layout.scroll)
            .expect("the visited item is actually measured");
        let actual = layout.scroll.scroll_px_offset_for_scrollbar();
        let offset = ReadingOffset::new(f32::from(actual.x), f32::from(actual.y))
            .expect("the observed scroll is a checked visit offset");
        assert!(offset.pixels().1 < -700.0, "the tall note contributes its measured height");

        // A route away replaces every item and releases the old measured
        // prefix. Back sees only the ordinary height hint for that note.
        layout.update(vec![Row::Note("another route".into())], px(264.0), 1.0, px(32.0));
        paint_rows(cx, &layout, px(32.0), false);
        layout.queue_restore(offset, Some(anchor));
        layout.update(rows, px(264.0), 1.0, px(32.0));
        assert_eq!(layout.scroll.logical_scroll_top().item_ix, 20,
            "Back must select the visited semantic row before distant notes are remeasured");
        paint_rows(cx, &layout, px(32.0), true);
        let first_paint = layout.scroll.logical_scroll_top();
        assert_eq!(first_paint.item_ix, 20);
        assert!((f32::from(first_paint.offset_in_item) - 8.0).abs() < 0.1,
            "the list's first prepaint must use the measured fraction, before any parent callback");
        let request = layout.deferred_scroll().expect("Back has a measured target to settle");
        assert!(layout.deliver_deferred(request), "the target item is measured after Back's first paint");
        let top = layout.scroll.logical_scroll_top();
        assert_eq!(top.item_ix, 20);
        assert!((f32::from(top.offset_in_item) - 8.0).abs() < 0.1,
            "Back restores the checked in-row fraction, not a stale absolute prefix");
    }

    #[gpui::test]
    fn broad_fold_and_zoom_preserve_the_measured_items_fraction(cx: &mut gpui::TestAppContext) {
        let cx = cx.add_empty_window();
        let mut layout = RowLayout::new();
        let mut rows = (0..90).map(|n| Row::Note(format!("note {n}").into())).collect::<Vec<_>>();
        rows[60] = item(60);
        layout.update(rows.clone(), px(264.0), 1.0, px(32.0));
        layout.scroll.scroll_to(ListOffset { item_ix: 60, offset_in_item: px(8.0) });
        paint_rows(cx, &layout, px(32.0), false);
        rows.splice(12..12, [Row::Note("inserted one".into()), Row::Note("inserted two".into())]);
        rows[80] = Row::Note("changed after the surviving anchor".into());
        layout.update(rows, px(220.0), 2.0, px(64.0));
        assert_eq!(layout.scroll.logical_scroll_top().item_ix, 62,
            "the changed range includes the old top, but its row identity survives");
        paint_rows(cx, &layout, px(64.0), false);
        assert!((f32::from(layout.scroll.logical_scroll_top().offset_in_item) - 16.0).abs() < 0.1,
            "the first zoom paint must already use the new measured row height");
        let request = layout.deferred_scroll().expect("zoom has a measured target to settle");
        assert!(layout.deliver_deferred(request), "the target is measured at the new scale");
        let top = layout.scroll.logical_scroll_top();
        assert_eq!(top.item_ix, 62);
        assert!((f32::from(top.offset_in_item) - 16.0).abs() < 0.1,
            "eight of 32 logical pixels becomes sixteen of 64 after remeasurement");
    }

    #[gpui::test]
    fn upward_keyboard_reveal_clears_the_measured_sticky_ancestor(cx: &mut gpui::TestAppContext) {
        use gpui::{AppContext as _, Context, IntoElement as _, Render, Styled as _, Window, div, list, point, size};

        let cx = cx.add_empty_window();
        let mut layout = RowLayout::new();
        // A depth-five subtree ends directly before a depth-one target.
        // Moving up first exposes the deep sibling, whose sticky chain is
        // taller than the target's own chain.
        let rows = (0..60).map(|n| {
            Row::Item(Item::new(
                RowId::Dependency(Arc::from(format!("row-{n}"))),
                if n <= 5 { n as u8 } else if n < 20 { 5 } else { 1 },
                Mark::Kind(Kind::Function),
                format!("row {n}"),
                Do::Nothing,
            ))
        }).collect();
        layout.update(rows, px(264.0), 1.0, px(32.0));
        struct ListView(gpui::ListState);
        impl Render for ListView {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                list(self.0.clone(), |_, _, _| div().h(px(32.0)).w_full().into_any_element())
                    .size_full()
            }
        }
        let paint = |cx: &mut gpui::VisualTestContext, scroll: &gpui::ListState| {
            cx.draw(point(px(0.0), px(0.0)), size(px(264.0), px(96.0)), |_, cx| {
                cx.new(|_| ListView(scroll.clone())).into_any_element()
            });
        };
        paint(cx, &layout.scroll);
        layout.scroll.scroll_to(ListOffset { item_ix: 40, offset_in_item: px(0.0) });
        paint(cx, &layout.scroll);
        layout.reveal_item(20);
        paint(cx, &layout.scroll);
        let viewport = layout.scroll.viewport_bounds();
        let first = layout.scroll.logical_scroll_top().item_ix;
        let inset = px(32.0) * (layout.visible_sticky_ancestors(first).len() as f32);
        let focused = layout.scroll.bounds_for_item(20).expect("revealed keyboard row has measured bounds");
        assert!(focused.top() >= viewport.top() + inset,
            "the focused child must paint below the sticky ancestor: {focused:?}, {viewport:?}, {inset:?}");
    }

    #[gpui::test]
    fn freshly_shrunk_viewport_clips_an_old_five_row_sticky_chain(cx: &mut gpui::TestAppContext) {
        use gpui::{Context, IntoElement as _, Render, Styled as _, Window, div, list, point, size};
        use std::cell::RefCell;
        use std::rc::Rc;

        let cx = cx.add_empty_window();
        let mut layout = RowLayout::new();
        layout.update((0..80).map(|n| Row::Item(Item::new(
            RowId::Dependency(Arc::from(format!("row-{n}"))),
            if n <= 5 { n as u8 } else { 5 },
            Mark::Kind(Kind::Function), format!("row {n}"), Do::Nothing,
        ))).collect(), px(264.0), 1.0, px(32.0));
        let layout = Rc::new(RefCell::new(layout));
        struct StickyView(Rc<RefCell<RowLayout>>);
        impl Render for StickyView {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl gpui::IntoElement {
                let layout = self.0.borrow();
                let top = layout.scroll.logical_scroll_top().item_ix;
                let chain = layout.sticky_ancestors(top);
                let row_count = chain.len();
                let mut pinned = div().w_full().flex().flex_col();
                for (index, _) in chain {
                    pinned = pinned.child(div().id(format!("sticky-{index}")).h(px(32.0)).w_full());
                }
                div().relative().size_full()
                    .child(list(layout.scroll.clone(), |_, _, _|
                        div().h(px(32.0)).w_full().into_any_element()).size_full())
                    .child(super::view::clipped_sticky_chain(pinned, px(32.0), row_count))
            }
        }
        let view = cx.update(|_, cx| cx.new(|_| StickyView(Rc::clone(&layout))));
        let paint = |cx: &mut gpui::VisualTestContext, height: f32| {
            cx.draw(point(px(0.0), px(0.0)), size(px(264.0), px(height)), |_, _| view.clone().into_any_element());
        };
        paint(cx, 400.0);
        layout.borrow().scroll.scroll_to(ListOffset { item_ix: 40, offset_in_item: px(0.0) });
        paint(cx, 400.0);
        assert_eq!(layout.borrow().visible_sticky_ancestors(40).len(), 5,
            "the old roomy viewport admits five ancestors");
        paint(cx, 96.0);
        let clip = cx.debug_bounds("shelf-sticky-clip").expect("the production sticky clip painted");
        assert!(clip.size.height <= px(64.0),
            "the new 96px viewport reserves one 32px real row despite the old 400px list bounds: {clip:?}");
        let outer = cx.debug_bounds("sticky-0").expect("outer sticky row mounted");
        let deepest = cx.debug_bounds("sticky-4").expect("deepest sticky row mounted");
        assert!(outer.bottom() <= clip.top() && deepest.top() >= clip.top()
            && deepest.bottom() <= clip.bottom(),
            "the new frame must clip outer ancestors and preserve the deepest useful one: {outer:?}, {deepest:?}, {clip:?}");
        let uncovered = layout.borrow().scroll.bounds_for_item(42).expect("a real row is measured below the chain");
        assert!(uncovered.top() >= clip.bottom() && uncovered.top() < px(96.0),
            "a native list row remains unobscured after shrink: {uncovered:?}, {clip:?}");
        paint(cx, 400.0);
        let grown_clip = cx.debug_bounds("shelf-sticky-clip").expect("the first expanded paint");
        let outer = cx.debug_bounds("sticky-0").expect("outer ancestor stays mounted");
        assert!(outer.top() == grown_clip.top() && outer.bottom() <= grown_clip.bottom(),
            "the newly roomy parent reveals outer ancestors in its first paint: {outer:?}, {grown_clip:?}");
    }
}

impl Region for Shelf {
    fn core(&mut self) -> &mut RegionCore {
        &mut self.core
    }

    fn keys(&self, snapshot: &AppSnapshot) -> Vec<PageKey> {
        if matches!(snapshot.overlay(), Some(Overlay::Settings(_))) {
            return Vec::new();
        }
        let mut keys = vec![PageKey::Orbit];
        // What the reader is on, and what the sidebar has been scoped into.
        for package in crate::runtime::store::route_package(snapshot.route())
            .into_iter()
            .chain(self.crumbs.package().cloned())
        {
            let key = PageKey::Package(package);
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        keys
    }

    fn observe(&mut self, event: &crate::runtime::store::StoreEvent, store: &DataStore) {
        if event.is_branch(Branch::Route) || event.is_branch(Branch::Reading) {
            self.follow(store);
        }
    }
}

impl Render for Shelf {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.core.rendered();
        self.targets.begin();
        if self.keys.is_none() {
            let shelf = cx.weak_entity();
            self.keys = Some(cx.intercept_keystrokes(move |event, window, cx| {
                if shelf
                    .update(cx, |shelf, cx| shelf.intercept(event, window, cx))
                    .unwrap_or(false)
                {
                    cx.stop_propagation();
                }
            }));
        }
        let measure = self.core.measure(cx);
        let facet = cx.facet();
        let palette = facet.palette();
        let width = self.core.width();
        let snapshot = self.links.snapshot(cx);
        let Listing {
            head,
            rows,
            matched,
        } = self.listing(&snapshot, cx);
        let weak = cx.weak_entity();
        let guard = self.action_guard_for(None, cx);
        for row in &rows {
            if let Row::Item(item) = row
                && item.is_target()
            {
                self.targets.push(Target {
                    id: item.key.clone(),
                    label: item.name.clone(),
                    action: super::focus::TargetAction::new(
                        guard.clone(),
                        act(&weak, item.does.clone(), snapshot.session().reading.current.id, guard.clone()),
                    ),
                    peek: item.warm.clone(),
                    source: item.source.clone(),
                });
            }
        }
        self.matched = matched;
        let row_measure = Measure::new(self.rest.max(width), &facet);
        self.layout.update(rows, row_measure.width(), row_measure.scale(), row_measure.row() + row_measure.space(Space::Tight));
        self.settle_focus();

        // Where the column sits between spine and shelf (0 = spine, 1 = shelf), and how far
        // the content has swapped. The column glides on its spring; the content swaps
        // quickly, before the clip can cut a word. The rows are laid out at the column's
        // resting width, and the words that reach furthest right (a trailing count, the
        // narrowing line's "N of M") end one roomy gutter inside it: the clip starts
        // cutting as soon as the column has narrowed by that gutter. So the rows are gone
        // by then, and come back only within that gutter of fully open (the legibility
        // law: a translucent word is whole or gone, and never lingers).
        let rest_measure = Measure::new(self.rest, &facet);
        let span = (self.rest - self.spine).max(px(1.0));
        let travel = ((width - self.spine) / span).clamp(0.0, 1.0);
        let over = (rest_measure.space(Space::Roomy) / span).clamp(0.01, 0.5);
        let open = ((travel - (1.0 - over)) / over).clamp(0.0, 1.0);
        let open = open * open * (3.0 - 2.0 * open);
        self.form = self
            .core
            .modes()
            .settle(&SIDE, rest_measure.fluid_room())
            .mode;
        if open > 0.001 && std::mem::take(&mut self.reveal)
            && let Some(index) = self.layout.rows.iter().position(|row| row.item().is_some_and(|item| item.current))
        {
            self.layout.reveal_item(index);
        }
        let deferred_scroll = (open > 0.001).then(|| self.layout.deferred_scroll()).flatten();
        let mut root = div()
            .id("shelf")
            .relative()
            .size_full()
            .overflow_hidden()
            .border_r_1()
            .border_color(palette.line1.hsla());
        if open > 0.001 {
            root = root.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .h_full()
                    .w(self.rest.max(width))
                    .opacity(open)
                    .child(self.column(&head, &rest_measure, palette, cx)),
            );
        }
        if open < 0.999 {
            root = root.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .h_full()
                    .w(self.spine)
                    .opacity(1.0 - open)
                    .child(self.spine_column(&measure, palette, window, cx)),
            );
        }
        let visit = snapshot.session().reading.current.id;
        let remembered = snapshot.session().reading.current.presentation.controls().shelf.offset;
        let remembered_anchor = snapshot.session().reading.current.presentation.controls().shelf.anchor.clone();
        let scroll = self.layout.scroll.clone();
        let identities = Rc::clone(&self.layout.identities);
        let restoring = self.layout.restoring();
        let shelf = cx.weak_entity();
        let links = self.links.clone();
        let targets = self.targets.clone();
        let overlay_surface = self.overlay_surface;
        root = root.on_children_prepainted(move |_, _, cx| {
            if links.snapshot(cx).overlay().is_some()
                || !links.shell.upgrade().is_some_and(|shell| shell.read(cx).shelf_input_owner(overlay_surface)) { return; }
            // The list child applied the checked in-row fraction during this
            // frame's own measurement. This callback only retires the ticket;
            // the notified frame publishes the settled presentation offset.
            // If this owner was inert or unmounted, a later active paint can
            // retry the same checked ticket.
            if let Some(request) = deferred_scroll
                && shelf.update(cx, |shelf, cx| {
                    if shelf.reading_visit == visit && shelf.layout.deliver_deferred(request) {
                        cx.notify();
                        true
                    } else { false }
                }).unwrap_or(false) { return; }
            if !restoring {
                let actual = scroll.scroll_px_offset_for_scrollbar();
                let anchor = RowLayout::observed_anchor_at(&identities, &scroll);
                if let Some(offset) = crate::navigation::presentation::ReadingOffset::new(f32::from(actual.x), f32::from(actual.y))
                    && (offset != remembered || anchor != remembered_anchor) {
                    links.dispatch(Intent::SetReading { visit, change: crate::navigation::presentation::ReadingChange::ShelfScroll { offset, anchor } }, cx);
                }
            }
            if targets.is_active() && let Some(key) = targets.focused()
                && let Some(key) = crate::navigation::presentation::ReadingText::new(key.to_string()) {
                let focus = crate::navigation::presentation::ReadingFocus::Shelf(key);
                if links.snapshot(cx).session().reading.current.presentation.controls().focus.as_ref() != Some(&focus) {
                    links.dispatch(Intent::SetReading { visit, change: crate::navigation::presentation::ReadingChange::Focus(Some(focus)) }, cx);
                }
            }
        });
        let _ = window;
        // The travelling bevel outlines a row; in the spine there are no rows
        // (its bounds are the last the rows had).
        if open > 0.001 {
            root = root.child(self.targets.glow(&measure));
        }
        root
    }
}

impl Shelf {
    /// Keeps the keyboard on a row that is in the list: when a narrowing (or
    /// a lens) took its row away, it lands on the first one.
    fn settle_focus(&mut self) {
        let Some(id) = self.targets.focused() else {
            return;
        };
        let there = self
            .layout.rows
            .iter()
            .filter_map(Row::item)
            .any(|item| item.key == id && item.is_target());
        if there {
            return;
        }
        match self
            .layout.rows
            .iter()
            .filter_map(Row::item)
            .find(|item| item.is_target())
        {
            Some(first) if !self.narrow.is_empty() || self.via.is_some() => {
                self.targets.focus(first.key.clone())
            }
            _ => self.targets.clear_focus(),
        }
    }
}

/// What a target does when activated: whatever its row does.
fn act(shelf: &gpui::WeakEntity<Shelf>, does: Do, visit: crate::navigation::presentation::VisitId, guard: Rc<dyn Fn(&mut App) -> bool>) -> Act {
    let shelf = shelf.clone();
    Rc::new(move |_: &mut Window, cx: &mut App| {
        if !guard(cx) { return; }
        let _ = shelf.update(cx, |shelf, cx| {
            if shelf.links.snapshot(cx).session().reading.current.id == visit { shelf.perform(&does, cx); }
        });
    })
}

#[cfg(test)]
mod release_address_tests {
    use super::release_address;
    use crate::core::PackageId;
    use crate::model::pages::PackageRef;
    use crate::navigation::{PackageLane, PackageRoute, ReleaseId, Route};

    #[test]
    fn comparison_uses_only_the_exact_routes_pin_and_never_another_registry() {
        let pin = "pkg:cargo/toml@0.8.23?repository_url=https%3A%2F%2Fone.example";
        let route = Route::Package(PackageRoute {
            project: None,
            cargo: None,
            package: PackageId::new(pin).expect("qualified pin"),
            lane: PackageLane::Overview,
            selected: None,
            at: Some(ReleaseId::new("1.1.6").expect("selected release")),
        });
        let viewed = crate::runtime::store::route_package(&route).expect("exact selected release");
        let (source, compare_to) = release_address(&route, &viewed).expect("route source");
        assert_eq!(source.as_str(), pin);
        assert_eq!(compare_to, Some("1.1.6"));

        let other = PackageRef::parse(
            "pkg:cargo/toml@1.1.6?repository_url=https%3A%2F%2Ftwo.example",
        )
        .expect("different qualified source");
        let (source, compare_to) = release_address(&route, &other).expect("independent book");
        assert_eq!(source, other, "a same-name release cannot borrow this pin");
        assert_eq!(compare_to, None, "no cross-source comparison is claimed");

        let local = PackageRef::parse("/tmp/toml").expect("same-name local project");
        let (source, compare_to) = release_address(&route, &local).expect("local book");
        assert_eq!(source, local, "a local root cannot borrow registry evidence");
        assert_eq!(compare_to, None);
    }
}

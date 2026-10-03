//! Session-local reading intent. Never a resource receipt or input capability.
//! Cold persistence currently restores routes only, with fresh default intent.
use super::{BrowseRoute, OrbitRoute, Route};
use std::collections::BTreeSet;
use std::sync::Arc;

pub const MAX_READING_TEXT: usize = 1024;
pub const MAX_READING_FOLDS: usize = 128;

/// Checked monotonic identity; zero is reserved for route-only cold history.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub struct VisitId(u64);
impl VisitId {
    pub const fn initial() -> Self {
        Self(1)
    }
    pub const fn next(self) -> Option<Self> {
        match self.0.checked_add(1) {
            Some(n) => Some(Self(n)),
            None => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct ReadingText(Arc<str>);
impl ReadingText {
    pub fn new(text: impl Into<String>) -> Option<Self> {
        let text = text.into();
        (text.len() <= MAX_READING_TEXT && !text.chars().any(char::is_control))
            .then_some(Self(text.into()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ShelfLens {
    #[default]
    Contents,
    Versions,
    RestsOn,
    UsedBy,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ReaderLens {
    #[default]
    Reference,
    Relations,
    Usage,
    History,
}

/// Fixed-point offsets avoid NaN, infinity and unbounded numeric state.
/// One unit is 1/256 logical pixel; normal viewport offsets are nonpositive.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReadingOffset {
    x: i32,
    y: i32,
}
impl ReadingOffset {
    pub fn new(x: f32, y: f32) -> Option<Self> {
        if !x.is_finite()
            || !y.is_finite()
            || x > 0.0
            || y > 0.0
            || x < -8_000_000.0
            || y < -8_000_000.0
        {
            return None;
        }
        Some(Self {
            x: (x * 256.0).round() as i32,
            y: (y * 256.0).round() as i32,
        })
    }
    pub fn pixels(self) -> (f32, f32) {
        (self.x as f32 / 256.0, self.y as f32 / 256.0)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ShelfReading {
    pub lens: ShelfLens,
    pub narrow: Option<ReadingText>,
    pub via: Option<ReadingText>,
    folds: Arc<BTreeSet<ReadingText>>,
    pub offset: ReadingOffset,
}
impl ShelfReading {
    pub fn folds(&self) -> impl Iterator<Item = &ReadingText> {
        self.folds.iter()
    }
    pub fn flip(&mut self, key: ReadingText) {
        let folds = Arc::make_mut(&mut self.folds);
        if !folds.remove(&key) && folds.len() < MAX_READING_FOLDS {
            folds.insert(key);
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReadingFocus {
    Reader(ReadingText),
    Shelf(ReadingText),
}
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReadingControls {
    pub shelf: ShelfReading,
    pub lens: ReaderLens,
    pub offset: ReadingOffset,
    pub focus: Option<ReadingFocus>,
}

/// Closed route-kind state: comparison preferences cannot be applied to a
/// declaration or inherited by a different newly navigated comparison.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReadingPresentation {
    Library(ReadingControls),
    Package(ReadingControls),
    Compare {
        controls: ReadingControls,
        comparison: facet::browse::compare::Presentation,
    },
    Find(ReadingControls),
    Declaration(ReadingControls),
    Source(ReadingControls),
    World(ReadingControls),
}
impl ReadingPresentation {
    pub fn for_route(route: &Route) -> Self {
        let controls = ReadingControls::default();
        match route {
            Route::Orbit(OrbitRoute::Browse(BrowseRoute::Compare(_))) => Self::Compare {
                controls,
                comparison: Default::default(),
            },
            Route::Orbit(OrbitRoute::Browse(BrowseRoute::Find(_) | BrowseRoute::FindHome)) => {
                Self::Find(controls)
            }
            Route::Orbit(_) => Self::Library(controls),
            Route::Package(_) => Self::Package(controls),
            Route::Symbol(_) => Self::Declaration(controls),
            Route::CargoSource(_) => Self::Source(controls),
            Route::World => Self::World(controls),
        }
    }
    pub fn controls(&self) -> &ReadingControls {
        match self {
            Self::Library(c)
            | Self::Package(c)
            | Self::Find(c)
            | Self::Declaration(c)
            | Self::Source(c)
            | Self::World(c)
            | Self::Compare { controls: c, .. } => c,
        }
    }
    pub fn controls_mut(&mut self) -> &mut ReadingControls {
        match self {
            Self::Library(c)
            | Self::Package(c)
            | Self::Find(c)
            | Self::Declaration(c)
            | Self::Source(c)
            | Self::World(c)
            | Self::Compare { controls: c, .. } => c,
        }
    }
    pub fn compatible(&self, route: &Route) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(&Self::for_route(route))
    }
    pub fn valid(&self) -> bool {
        match self {
            Self::Compare { comparison, .. } => comparison.valid(),
            _ => true,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadingVisit {
    pub id: VisitId,
    pub presentation: ReadingPresentation,
}
impl ReadingVisit {
    pub fn cold(route: &Route) -> Self {
        Self {
            id: VisitId::default(),
            presentation: ReadingPresentation::for_route(route),
        }
    }
}

/// Allocation never wraps. Exhaustion refuses fresh visits while existing
/// history entries remain usable; no historical identity is minted twice.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadingSession {
    pub current: ReadingVisit,
    last: VisitId,
    preview_origin: Option<ReadingVisit>,
}
impl ReadingSession {
    pub fn new(route: &Route) -> Self {
        Self {
            current: ReadingVisit {
                id: VisitId::initial(),
                presentation: ReadingPresentation::for_route(route),
            },
            last: VisitId::initial(),
            preview_origin: None,
        }
    }
    pub fn fresh(&mut self, route: &Route) -> bool {
        let Some(id) = self.last.next() else {
            return false;
        };
        self.last = id;
        self.current = ReadingVisit {
            id,
            presentation: ReadingPresentation::for_route(route),
        };
        true
    }
    pub fn restore(&mut self, visit: ReadingVisit, route: &Route) -> bool {
        // History restores only this allocator's already issued identities.
        // Imported future IDs cannot move current ahead of the watermark.
        if visit.id.0 > self.last.0 || !visit.presentation.compatible(route) || !visit.presentation.valid() {
            return false;
        }
        if visit.id == VisitId::default() {
            self.fresh(route)
        } else {
            self.current = visit;
            true
        }
    }
    pub fn preview(&mut self, route: &Route) -> bool {
        let origin = self
            .preview_origin
            .clone()
            .unwrap_or_else(|| self.current.clone());
        if !self.fresh(route) {
            return false;
        }
        self.preview_origin = Some(origin);
        true
    }
    pub fn commit_preview(&mut self) -> Option<ReadingVisit> {
        self.preview_origin.take()
    }
    pub fn cancel_preview(&mut self) {
        if let Some(origin) = self.preview_origin.take() {
            self.current = origin;
        }
    }
    pub fn normalize(&mut self, route: &Route) {
        if !self.current.presentation.compatible(route) {
            self.current.presentation = ReadingPresentation::for_route(route);
        }
    }
}
impl Default for ReadingSession {
    fn default() -> Self {
        Self::new(&Route::Orbit(OrbitRoute::Home))
    }
}

/// Local deltas cannot overwrite another control's more recent intent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReadingChange {
    Comparison(facet::browse::compare::Presentation),
    ShelfLens(ShelfLens),
    ShelfFilter {
        narrow: Option<ReadingText>,
        via: Option<ReadingText>,
    },
    ShelfFold(ReadingText),
    ShelfOffset(ReadingOffset),
    ReaderLens(ReaderLens),
    ReaderOffset(ReadingOffset),
    Focus(Option<ReadingFocus>),
}
impl ReadingPresentation {
    pub fn apply(&mut self, change: ReadingChange) -> bool {
        if let ReadingChange::Comparison(next) = change {
            if !next.valid() {
                return false;
            }
            if let Self::Compare { comparison, .. } = self {
                *comparison = next;
                return true;
            }
            return false;
        }
        let controls = self.controls_mut();
        match change {
            ReadingChange::Comparison(_) => unreachable!("handled above"),
            ReadingChange::ShelfLens(lens) => {
                controls.shelf.lens = lens;
                controls.shelf.offset = Default::default();
            }
            ReadingChange::ShelfFilter { narrow, via } => {
                controls.shelf.narrow = narrow;
                controls.shelf.via = via;
                controls.shelf.offset = Default::default();
            }
            ReadingChange::ShelfFold(key) => controls.shelf.flip(key),
            ReadingChange::ShelfOffset(offset) => controls.shelf.offset = offset,
            ReadingChange::ReaderLens(lens) => controls.lens = lens,
            ReadingChange::ReaderOffset(offset) => controls.offset = offset,
            ReadingChange::Focus(focus) => controls.focus = focus,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restoring_future_incompatible_or_invalid_visits_refuses_without_mutation() {
        let home = Route::Orbit(OrbitRoute::Home);
        let mut session = ReadingSession::new(&home);
        let original = session.clone();
        let future = ReadingVisit { id: VisitId::initial().next().expect("next"), presentation: ReadingPresentation::for_route(&home) };
        assert!(!session.restore(future, &home));
        assert_eq!(session, original);
        let incompatible = ReadingVisit { id: VisitId::initial(), presentation: ReadingPresentation::for_route(&Route::World) };
        assert!(!session.restore(incompatible, &home));
        assert_eq!(session, original);
        let comparison_route = Route::Orbit(OrbitRoute::Browse(BrowseRoute::Compare(crate::navigation::CompareSet::new([
            crate::model::pages::PackageRef::parse("/fixture/one").expect("first"),
            crate::model::pages::PackageRef::parse("/fixture/two").expect("second"),
        ]).expect("comparison"))));
        let mut invalid = ReadingPresentation::for_route(&comparison_route);
        let ReadingPresentation::Compare { comparison, .. } = &mut invalid else { unreachable!() };
        comparison.page = 4097;
        assert!(!session.restore(ReadingVisit { id: VisitId::initial(), presentation: invalid }, &comparison_route));
        assert_eq!(session, original);
        assert!(session.fresh(&home));
        assert_eq!(session.current.id, VisitId::initial().next().expect("allocator never skipped or reused"));
        assert!(session.restore(original.current.clone(), &home), "an issued past visit remains restorable");
        assert_eq!(session.current, original.current);
        assert!(session.restore(ReadingVisit::cold(&home), &home), "cold route-only entries receive fresh IDs");
        assert_ne!(session.current.id, original.current.id);
    }

    #[test]
    fn allocation_exhaustion_never_reuses_a_live_visit() {
        let route = Route::Orbit(OrbitRoute::Home);
        let mut session = ReadingSession::new(&route);
        session.last = VisitId(u64::MAX);
        let current = session.current.clone();
        assert!(!session.fresh(&route));
        assert_eq!(session.current, current);
        assert!(!session.preview(&route));
        assert_eq!(session.current, current);
    }
    #[test]
    fn presentation_storage_and_offsets_are_bounded() {
        assert!(ReadingText::new("x".repeat(MAX_READING_TEXT + 1)).is_none());
        assert!(ReadingText::new("bad\nkey").is_none());
        assert!(ReadingOffset::new(f32::NAN, -1.0).is_none());
        assert!(ReadingOffset::new(0.0, f32::NEG_INFINITY).is_none());
        assert!(ReadingOffset::new(0.0, -8_000_001.0).is_none());
        let mut shelf = ShelfReading::default();
        for n in 0..(MAX_READING_FOLDS + 10) {
            shelf.flip(ReadingText::new(format!("fold-{n}")).expect("bounded"));
        }
        assert_eq!(shelf.folds().count(), MAX_READING_FOLDS);
    }
    #[test]
    fn preview_cancellation_restores_intent_and_preserves_allocator() {
        let home = Route::Orbit(OrbitRoute::Home);
        let other = Route::World;
        let mut session = ReadingSession::new(&home);
        session.current.presentation.controls_mut().shelf.lens = ShelfLens::UsedBy;
        let origin = session.current.clone();
        assert!(session.preview(&other));
        let preview = session.current.id;
        session.cancel_preview();
        assert_eq!(session.current, origin);
        assert!(session.fresh(&other));
        assert_ne!(session.current.id, preview);
    }
}

//! Bounded words and text from an earlier exact destination.
//!
//! These projections contain no live read address, action, continuation,
//! selected IR head or source capability. A cache hash proves byte integrity,
//! never present owner authority.

use crate::core::{LocalProjectId, VersionedRoot};
use crate::navigation::{BrowseRoute, CargoSourceTarget, OrbitRoute, Route};
use backend_platform::NativePathWire;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub(crate) const MAX_DISPLAY_ROWS: usize = 2_048;
pub(crate) const MAX_DISPLAY_WORDS: usize = 1 << 20;
pub(crate) const MAX_DISPLAY_SOURCE: usize = 2 << 20;
pub(crate) const MAX_DISPLAY_ROW_WORDS: usize = 4 << 10;
pub(crate) use crate::runtime::snapshot::RetainedDisplay;

/// A cache claim compared only with an already requested typed destination.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DisplayAddress {
    route: crate::model::PersistedRoute,
    native_project: Option<NativePathWire>,
}

impl DisplayAddress {
    pub(crate) fn for_route(route: &Route) -> Option<Self> {
        let project = match route {
            Route::Orbit(OrbitRoute::Browse(BrowseRoute::Tree(project))) => Some(project),
            Route::Package(package) => package.cargo.as_ref().map(|browse| browse.requested_project()),
            Route::CargoSource(source) => Some(source.browse.requested_project()),
            Route::Symbol(_) | Route::Orbit(_) | Route::World => None,
        };
        // An unbound legacy Cargo address does not name an exact source scope.
        if matches!(route, Route::CargoSource(source) if source.browse.context().is_none()) {
            return None;
        }
        Some(Self { route: crate::model::persistence::display_route_claim(route),
            native_project: project.map(LocalProjectId::native_wire).transpose().ok()? })
    }
    pub(crate) fn matches(&self, route: &Route) -> bool { Self::for_route(route).as_ref() == Some(self) }
    pub(crate) fn requested_project(&self) -> Option<LocalProjectId> {
        self.native_project.as_ref().and_then(|wire| LocalProjectId::from_native_wire(wire).ok())
    }
}

/// Saved observation bytes. They cannot be converted to VersionedRoot.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DisplayObservation {
    pub(crate) root: [u8; 32],
    pub(crate) producer_epoch: u64,
    pub(crate) cursor: Vec<u8>,
}
impl DisplayObservation {
    pub(crate) fn at(root: VersionedRoot) -> Option<Self> {
        (!root.is_unserved()).then(|| Self { root: *root.root().as_bytes(),
            producer_epoch: root.producer_epoch(), cursor: root.revision().encode_control().into_vec() })
    }
    pub(crate) fn has_shape(&self) -> bool {
        // Compare wire fields; never reconstruct an owner cursor from a cache.
        let root_at = 2 + 32 * 4 + 2;
        self.root != [0; 32] && self.cursor.len() == backend_library::CURSOR_CONTROL_BYTES
            && self.cursor[..2] == backend_library::CURSOR_SCHEMA.to_be_bytes()
            && self.cursor[root_at..root_at + 32] == self.root
    }
}

/// Explicit projection completeness; omitted content never means absence.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) enum CaptureCoverage {
    Complete,
    VisibleExcerpt { first_line: u32, last_line: u32 },
}

/// Plain rows have no destinations, keys, callbacks or current relation proof.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DisplayRow {
    pub(crate) label: Arc<str>,
    pub(crate) detail: Arc<str>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) enum DisplayBody {
    Reading { title: Arc<str>, #[serde(deserialize_with = "bounded_rows")] rows: Vec<DisplayRow> },
    Source { path: Arc<str>, text: Arc<str>, first_line: u32 },
    Markdown { source: Arc<str> },
}

/// Source signatures are historical scope claims, not current file leases.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) enum DisplaySource {
    Indexed,
    Tree { binding: backend_library::browse::ProjectTreeRequestBindingV1 },
    Cargo { binding: backend_library::browse::ProjectTreeRequestBindingV1,
        source_revision: [u8; 32], content_digest: [u8; 32],
        readme_origin: Option<backend_library::CargoPackageReadmeOriginV1> },
}

/// Untrusted serde payload. Only snapshot background admission can create
/// the closed immutable runtime display that GUI consumers receive.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RetainedDisplayWire {
    pub(crate) address: DisplayAddress,
    pub(crate) observation: DisplayObservation,
    pub(crate) coverage: CaptureCoverage,
    pub(crate) source: DisplaySource,
    pub(crate) body: DisplayBody,
}
impl RetainedDisplayWire {
    pub(crate) fn matches(&self, route: &Route) -> bool { self.address.matches(route) && self.has_shape() }
    pub(crate) fn has_shape(&self) -> bool {
        if !self.observation.has_shape() { return false; }
        let text = match &self.body {
            DisplayBody::Reading { title, rows } => {
                if rows.len() > MAX_DISPLAY_ROWS || title.len() > MAX_DISPLAY_ROW_WORDS
                    || rows.iter().any(|row| row.label.len().saturating_add(row.detail.len()) > MAX_DISPLAY_ROW_WORDS) { return false; }
                let bytes = rows.iter().try_fold(title.len(), |bytes, row|
                    bytes.checked_add(row.label.len())?.checked_add(row.detail.len()));
                if bytes.is_none_or(|bytes| bytes > MAX_DISPLAY_WORDS) { return false; }
                None
            }
            DisplayBody::Source { text, first_line, .. } => {
                if *first_line == 0 || text.len() > MAX_DISPLAY_SOURCE { return false; }
                Some(text.as_ref())
            }
            DisplayBody::Markdown { source } => {
                if source.len() > MAX_DISPLAY_SOURCE { return false; }
                Some(source.as_ref())
            }
        };
        if let CaptureCoverage::VisibleExcerpt { first_line, last_line } = self.coverage {
            if first_line == 0 || last_line < first_line || !matches!(self.body, DisplayBody::Source { first_line: body_first, .. } if body_first == first_line) { return false; }
        }
        match &self.source {
            DisplaySource::Indexed => true,
            DisplaySource::Tree { binding } => self.address.requested_project().is_some_and(|project|
                binding.has_admissible_shape() && binding.matches_requested_root(&project.path()))
                && matches!(self.body, DisplayBody::Reading { .. }),
            DisplaySource::Cargo { binding, source_revision, content_digest, readme_origin } => {
                let Some(project) = self.address.requested_project() else { return false; };
                let Some(text) = text else { return false; };
                let native = project.path();
                binding.has_admissible_shape() && binding.matches_requested_root(&native)
                    && *source_revision != [0; 32] && *content_digest != [0; 32]
                    && blake3::hash(text.as_bytes()).as_bytes() == content_digest
                    && readme_origin.as_ref().is_none_or(|origin| origin.has_admissible_shape()
                        && origin.request_binding == *binding
                        && (!matches!(self.body, DisplayBody::Markdown { .. }) || origin.content_digest == *content_digest))
            }
        }
    }
    /// Source target scope must still match the requested route's exact claim.
    pub(crate) fn source_matches_route(&self, route: &Route) -> bool {
        if !self.matches(route) { return false; }
        match (&self.source, route) {
            (DisplaySource::Cargo { binding, readme_origin, .. }, Route::CargoSource(source)) => {
                source.browse.context().is_some_and(|context| context.request_binding() == *binding)
                    && matches!(&self.body, DisplayBody::Source { path, .. } if path.as_ref() == source.target.path().as_str())
                    && match &source.target {
                        CargoSourceTarget::PackageFile(_) => readme_origin.is_none(),
                        CargoSourceTarget::ReadmeLink(link) => readme_origin.as_ref() == Some(link.origin()),
                    }
            }
            (DisplaySource::Cargo { binding, readme_origin, .. }, Route::Package(package)) => package.cargo.as_ref()
                .is_some_and(|context| context.request_binding() == *binding)
                && readme_origin.as_ref().is_some_and(|origin| origin.package.as_str() == package.package.as_str()),
            (DisplaySource::Tree { .. }, Route::Orbit(OrbitRoute::Browse(BrowseRoute::Tree(_)))) => true,
            (DisplaySource::Tree { .. }, _) => false,
            (DisplaySource::Indexed, Route::World | Route::Orbit(OrbitRoute::Browse(BrowseRoute::FindHome | BrowseRoute::Find(_) | BrowseRoute::Compare(_)))) => matches!(self.body, DisplayBody::Reading { .. }),
            (DisplaySource::Indexed, _) => false,
            (DisplaySource::Cargo { .. }, _) => false,
        }
    }
}

fn bounded_rows<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<DisplayRow>, D::Error> {
    struct Rows;
    impl<'de> serde::de::Visitor<'de> for Rows {
        type Value = Vec<DisplayRow>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "at most {MAX_DISPLAY_ROWS} retained display rows") }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
            let mut rows = Vec::with_capacity(sequence.size_hint().unwrap_or(0).min(MAX_DISPLAY_ROWS));
            while let Some(row) = sequence.next_element()? {
                if rows.len() == MAX_DISPLAY_ROWS { return Err(serde::de::Error::custom("too many retained display rows")); }
                rows.push(row);
            }
            Ok(rows)
        }
    }
    deserializer.deserialize_seq(Rows)
}

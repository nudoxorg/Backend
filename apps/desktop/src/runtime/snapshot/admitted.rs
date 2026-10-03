//! Closed background-admitted display. GUI code can borrow it, never forge it.
use crate::model::retained_display::{CaptureCoverage, DisplayBody, DisplayObservation, RetainedDisplayWire};
use crate::model::pages::{SourceOrigin, SourceText};
use crate::navigation::Route;
use std::sync::Arc;
const PREPARED_SOURCE_INDEX_BYTES: usize = 8 << 20;

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct RetainedDisplay {
    route: Route,
    wire: RetainedDisplayWire,
    source: Option<Arc<SourceText>>,
    generation: [u8; 32],
}
impl RetainedDisplay {
    /// Only the snapshot module and its background capture/decode children
    /// can perform expensive validation/preparation or construct this type.
    pub(super) fn admit(wire: RetainedDisplayWire, route: &Route) -> Option<Self> {
        if !wire.source_matches_route(route) { return None; }
        let source = match &wire.body {
            DisplayBody::Source { text, first_line, .. } => Some(Arc::new(SourceText::new(Arc::clone(text), *first_line, SourceOrigin::Excerpt, true).ok()?.with_prepared_line_index(PREPARED_SOURCE_INDEX_BYTES)?)),
            DisplayBody::Markdown { source } => Some(Arc::new(SourceText::new(Arc::clone(source), 1, SourceOrigin::Excerpt, true).ok()?.with_prepared_line_index(PREPARED_SOURCE_INDEX_BYTES)?)),
            DisplayBody::Reading { .. } => None,
        };
        if let CaptureCoverage::VisibleExcerpt { first_line, last_line } = wire.coverage {
            let range = source.as_ref()?.line_range()?;
            if range.first != first_line || range.last != last_line { return None; }
        }
        // Hash the admitted wire on this background path once. This only
        // partitions local pager memory; it confers no semantic authority.
        struct HashWriter(blake3::Hasher);
        impl std::io::Write for HashWriter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> { self.0.update(bytes); Ok(bytes.len()) }
            fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
        }
        let mut hash = HashWriter(blake3::Hasher::new());
        serde_json::to_writer(&mut hash, &wire).ok()?;
        let generation = *hash.0.finalize().as_bytes();
        Some(Self { route: route.clone(), wire, source, generation })
    }
    pub(crate) const fn served(&self) -> bool { false }
    /// Typed equality of the original exact destination; no body scan/hash.
    pub(crate) fn source_matches_route(&self, route: &Route) -> bool { &self.route == route }
    pub(crate) fn observation(&self) -> &DisplayObservation { &self.wire.observation }
    pub(crate) fn coverage(&self) -> &CaptureCoverage { &self.wire.coverage }
    pub(crate) fn body(&self) -> &DisplayBody { &self.wire.body }
    pub(crate) const fn memory_generation(&self) -> [u8; 32] { self.generation }
    pub(crate) fn source_text(&self) -> Option<&Arc<SourceText>> { self.source.as_ref() }
    pub(super) fn wire(&self) -> &RetainedDisplayWire { &self.wire }
}

//! Requested storage layouts retained by an immutable row.

use super::{Fragment, Row};
use std::alloc::Layout;
use std::sync::atomic::AtomicUsize;

impl Row {
    /// Returns the requested storage charged to this row, including its slot.
    ///
    /// This counts `String` capacities, boxed fragment slots and text, the
    /// identity sidecar, captured source and excerpt allocations, and fact text.
    /// Shared source allocations are conservatively charged to every row that
    /// retains them. The `Arc<str>` layout follows the pinned Rust standard
    /// library's two `AtomicUsize` counters followed by its byte slice.
    ///
    /// This is a requested-layout quota, not an allocator or RSS measurement.
    /// Allocator metadata, size-class rounding, and other view indexes are not
    /// included. A caller accounting a container's full capacity must use
    /// [`Self::retained_payload_requested_bytes`] to avoid charging occupied
    /// row slots twice. Returns `None` on layout or arithmetic overflow.
    #[must_use]
    pub fn retained_requested_bytes(&self) -> Option<usize> {
        let mut bytes = self.retained_owned_requested_bytes()?;
        if let Some(path) = self.source.file_path() {
            bytes = bytes.checked_add(arc_str_requested_bytes(path.len())?)?;
        }
        if let Some(excerpt) = self.excerpt.text() {
            bytes = bytes.checked_add(arc_str_requested_bytes(excerpt.len())?)?;
        }
        Some(bytes)
    }

    /// Returns inline and exclusively owned storage, excluding shared source
    /// and excerpt allocations. A whole-view budget accounts shared allocations
    /// separately rather than multiplying a common source path by its row count.
    #[must_use]
    pub fn retained_owned_requested_bytes(&self) -> Option<usize> {
        size_of::<Self>().checked_add(self.retained_payload_requested_bytes()?)
    }

    /// Returns exclusively owned storage excluding the inline row slot.
    /// Shared source and excerpt allocations are excluded as well.
    #[must_use]
    pub fn retained_payload_requested_bytes(&self) -> Option<usize> {
        let fragments = Layout::array::<Fragment>(self.document.len()).ok()?.size();
        let mut bytes = self.label.capacity().checked_add(fragments)?;
        bytes = bytes.checked_add(self.signature.as_ref().map_or(0, String::capacity))?;
        bytes = bytes.checked_add(self.identity_preimage().map_or(0, |p| p.as_str().len()))?;
        for fragment in &self.document {
            bytes = bytes.checked_add(match fragment {
                Fragment::Text(value) | Fragment::Code(value) => value.capacity(),
                Fragment::Link { label, .. } => label.capacity(),
                Fragment::Break => 0,
            })?;
        }
        bytes.checked_add(self.facts.text_bytes())
    }
}

fn arc_str_requested_bytes(length: usize) -> Option<usize> {
    let bytes = Layout::array::<u8>(length).ok()?;
    let (layout, _) = Layout::new::<(AtomicUsize, AtomicUsize)>()
        .extend(bytes)
        .ok()?;
    Some(layout.pad_to_align().size())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Basis, RowId, SourceAvailability, symbol_key};
    use backend_compile::{
        DeclarationFacts, Deprecation, Fact, SourceExcerpt, SourceExcerptExtent,
    };

    fn row() -> Row {
        let basis = Basis::new(
            crate::canonical::view_state_root(&[]),
            crate::canonical::object_version(b"row storage"),
        );
        Row::new_with_document(RowId::Symbol(symbol_key("row")), basis, "row", Vec::new())
    }

    #[test]
    fn retained_layout_counts_capacity_sidecar_and_fragment_slots() {
        let mut row = row();
        let mut label = String::with_capacity(512);
        label.push('x');
        row.label = label;
        let mut signature = String::with_capacity(1024);
        signature.push('s');
        row.signature = Some(signature);
        let mut text = String::with_capacity(2048);
        text.push('d');
        row.document = vec![Fragment::Text(text), Fragment::Break].into_boxed_slice();
        row = row
            .try_with_identity_preimage("row\0occurrence:1")
            .expect("identity");
        assert_eq!(
            row.retained_requested_bytes(),
            Some(
                size_of::<Row>()
                    + 512
                    + 1024
                    + 2048
                    + 2 * size_of::<Fragment>()
                    + "row\0occurrence:1".len()
            )
        );
    }

    #[test]
    fn source_excerpt_and_fact_storage_remain_charged() {
        let mut row = row();
        let before = row.retained_requested_bytes().expect("layout");
        row.source = SourceAvailability::stale_file("src/file.rs").expect("source");
        row.excerpt =
            SourceExcerpt::captured("fn f() {}", SourceExcerptExtent::Complete).expect("excerpt");
        row.facts = DeclarationFacts {
            deprecation: Fact::Present(Deprecation::new(Some("1.0"), Some("use g"))),
            ..DeclarationFacts::UNOBSERVED
        };
        assert_eq!(
            row.retained_requested_bytes(),
            Some(
                before
                    + arc_str_requested_bytes("src/file.rs".len()).expect("source layout")
                    + arc_str_requested_bytes("fn f() {}".len()).expect("excerpt layout")
                    + "1.0".len()
                    + "use g".len()
            )
        );
    }

    #[test]
    fn arc_layout_rejects_overflow() {
        assert_eq!(arc_str_requested_bytes(usize::MAX), None);
    }

    #[test]
    fn arc_layout_matches_the_native_standard_library_allocation() {
        for length in [1, 7, 8, 17, 4096] {
            let path = "p".repeat(length);
            let expected = arc_str_requested_bytes(length).expect("layout");
            let measured = allocation_counter::measure(|| {
                std::hint::black_box(SourceAvailability::stale_file(path).expect("source"));
            });
            assert_eq!(measured.count_total, 1);
            assert_eq!(measured.bytes_total, expected as u64);
        }
    }
}

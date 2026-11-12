//! Prospective requested-layout admission for owned declaration rows.
//!
//! This quota covers requested Rust allocation layouts. It does not estimate
//! allocator metadata, size-class rounding or process RSS.

mod budget;
mod text;
pub(super) use budget::RequestedLayoutBudget;
pub(super) use text::ExactText;

use backend_engine::{
    Basis, DeclarationKind, Fragment, PackageKey, Row, RowId, RowIdentityPreimage,
};
use backend_library::MAX_ROW_IDENTITY_PREIMAGE_BYTES;

/// All text allocations retained by a one-fragment projected declaration.
pub(super) struct ProjectedRowTexts {
    pub(super) label: String,
    pub(super) identity: String,
    pub(super) signature: String,
    pub(super) document: String,
}

/// A row whose complete owned storage is known before allocating its text.
pub(super) struct ProjectedRowPlan {
    lengths: [usize; 4],
    bytes: usize,
}

impl ProjectedRowPlan {
    pub(super) fn new(
        label: usize,
        identity: usize,
        signature: usize,
        document: usize,
    ) -> Result<Self, &'static str> {
        if identity == 0 || identity > MAX_ROW_IDENTITY_PREIMAGE_BYTES {
            return Err("row identity preimage is empty or oversized");
        }
        let lengths = [label, identity, signature, document];
        let bytes = lengths
            .iter()
            .try_fold(size_of::<Row>() + size_of::<Fragment>(), |sum, n| {
                if *n > isize::MAX as usize {
                    return None;
                }
                sum.checked_add(*n)
            })
            .ok_or("row requested layout overflow")?;
        Ok(Self { lengths, bytes })
    }

    /// Includes the occupied Row slot. For a separately charged row container,
    /// subtract `size_of::<Row>()` and charge the container's full capacity once.
    pub(super) const fn retained_requested_bytes(&self) -> usize {
        self.bytes
    }

    pub(super) const fn retained_payload_requested_bytes(&self) -> usize {
        self.bytes - size_of::<Row>()
    }

    /// Each nonempty exact text and the typed fragment box is one allocation.
    /// Any policy charge per allocation is separate from requested layout bytes.
    pub(super) fn allocation_count(&self) -> usize {
        1 + self.lengths.iter().filter(|length| **length != 0).count()
    }

    /// Moves every admitted text owner directly into the retained row. There is
    /// no default document or identity clone to allocate and immediately discard.
    pub(super) fn build(
        self,
        id: RowId,
        basis: Basis,
        package: PackageKey,
        kind: DeclarationKind,
        texts: ProjectedRowTexts,
    ) -> Result<Row, &'static str> {
        for (text, length) in [
            &texts.label,
            &texts.identity,
            &texts.signature,
            &texts.document,
        ]
        .into_iter()
        .zip(self.lengths)
        {
            if text.len() != length || text.capacity() != length {
                return Err("row text did not match its admitted layout");
            }
        }
        let identity = RowIdentityPreimage::try_from(texts.identity)
            .map_err(|_| "row identity preimage is empty or oversized")?;
        let document: Box<[Fragment]> = Box::new([Fragment::Text(texts.document)]);
        let mut row = Row::new_with_document(id, basis, texts.label, document)
            .with_identity_preimage(identity)
            .with_signature(texts.signature)
            .with_kind(kind);
        row.package = Some(package);
        if row.retained_requested_bytes() != Some(self.bytes) {
            return Err("row retained layout did not match admission");
        }
        Ok(row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> ProjectedRowPlan {
        ProjectedRowPlan::new(
            "pkg::schema.sql::book".len(),
            "pkg::schema.sql::book\0sql:4294967295".len(),
            "SQL table book".len(),
            "SQL table in schema.sql".len(),
        )
        .expect("plan")
    }

    #[test]
    fn rejected_prospective_row_performs_no_allocation() {
        let plan = plan();
        let mut budget =
            RequestedLayoutBudget::new(0, plan.retained_requested_bytes() - 1).expect("budget");
        let measured = allocation_counter::measure(|| {
            assert!(budget.retain(plan.retained_requested_bytes(), 0).is_err());
        });
        assert_eq!(measured.count_total, 0);
        assert_eq!(budget.retained(), 0);
    }

    #[test]
    fn exact_row_has_only_its_five_retained_allocations() {
        let (initial, _) = super::super::initial_view().expect("initial view");
        let plan = plan();
        let bytes = plan.retained_requested_bytes();
        let allocations = plan.allocation_count();
        let mut row = None;
        let mut budget = RequestedLayoutBudget::new(0, bytes).expect("budget");
        budget.retain(bytes, 0).expect("exact boundary");
        let measured = allocation_counter::measure(|| {
            let label = ExactText::measure(format_args!("pkg::schema.sql::book"))
                .expect("label")
                .allocate(format_args!("pkg::schema.sql::book"))
                .expect("label");
            let identity =
                ExactText::measure(format_args!("pkg::schema.sql::book\0sql:{}", u32::MAX))
                    .expect("identity")
                    .allocate(format_args!("pkg::schema.sql::book\0sql:{}", u32::MAX))
                    .expect("identity");
            let signature = ExactText::measure(format_args!("SQL table book"))
                .expect("signature")
                .allocate(format_args!("SQL table book"))
                .expect("signature");
            let document = ExactText::measure(format_args!("SQL table in schema.sql"))
                .expect("document")
                .allocate(format_args!("SQL table in schema.sql"))
                .expect("document");
            let id = RowId::Symbol(backend_engine::symbol_key(&identity));
            row = Some(
                plan.build(
                    id,
                    initial.basis(),
                    backend_engine::package_key("pkg"),
                    DeclarationKind::Unknown,
                    ProjectedRowTexts {
                        label,
                        identity,
                        signature,
                        document,
                    },
                )
                .expect("row"),
            );
        });
        assert_eq!(measured.count_total, allocations as u64);
        assert_eq!(measured.count_current, allocations as i64);
        assert_eq!(measured.bytes_total, (bytes - size_of::<Row>()) as u64);
        assert_eq!(measured.bytes_current, (bytes - size_of::<Row>()) as i64);
        let row = row.expect("retained row");
        assert_eq!(row.retained_requested_bytes(), Some(bytes));
        assert_eq!(row.label, "pkg::schema.sql::book");
        assert_eq!(row.signature.as_deref(), Some("SQL table book"));
        assert_eq!(
            &*row.document,
            &[Fragment::Text("SQL table in schema.sql".to_owned())]
        );
        assert_eq!(row.package, Some(backend_engine::package_key("pkg")));
        assert_eq!(row.kind, Some(DeclarationKind::Unknown));
        assert_eq!(row.basis, initial.basis());
        assert_eq!(
            row.identity_preimage().expect("identity").as_str(),
            "pkg::schema.sql::book\0sql:4294967295"
        );
        assert_eq!(
            row.id,
            RowId::Symbol(backend_engine::symbol_key(
                "pkg::schema.sql::book\0sql:4294967295"
            ))
        );
        assert_eq!(budget.retained(), bytes);
    }

    #[test]
    fn projected_row_plan_rejects_unrepresentable_identity_and_layout() {
        assert!(ProjectedRowPlan::new(1, 0, 1, 1).is_err());
        assert!(ProjectedRowPlan::new(1, MAX_ROW_IDENTITY_PREIMAGE_BYTES + 1, 1, 1).is_err());
        assert!(ProjectedRowPlan::new(usize::MAX, 1, 1, 1).is_err());
    }
}

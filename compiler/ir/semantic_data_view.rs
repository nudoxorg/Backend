//! Borrowed, proof-carrying access to the durable canonical product graph.
//!
//! This surface never reconstructs syntax and never repeats wire validation.
//! `FragmentView::validate` records every offset and cardinality once; these
//! accessors only lend the corresponding already-proven cells.  Schema 3
//! carries the entity-root map needed to navigate from a declaration to its
//! canonical product after deduplication.

use heart_identity::{ContentId, IrFragmentDomain};

use crate::{
    AtomId, EntityId, ExternalProductRef, ProductChildRole, ProductId, ProductListId, ProductRef,
    SemanticAtom, SemanticProduct, SemanticProductChild, SemanticProductConstructor,
    wire::{
        SEMANTIC_CHILD_BYTES, SEMANTIC_CONSTRUCTOR_BYTES, SEMANTIC_EXTERNAL_TAG,
        SEMANTIC_LIST_BYTES, SEMANTIC_LOCAL_TAG, SEMANTIC_PRODUCT_BYTES, SemanticDataLayout,
        read_u32,
    },
};

/// Exact cardinalities of a validated canonical semantic graph.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticDataCounts {
    pub atoms: u32,
    pub products: u32,
    pub constructors: u32,
    pub lists: u32,
    pub children: u32,
}

/// One declaration's relationship to the canonical product graph.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticDataEntityRoot {
    /// Schema 3 persisted this entity's canonical product root.
    Captured(ProductId),
    /// Schema 1/2 graph bytes are valid, but they omitted the source-product
    /// mapping.  This is deliberately not represented as a missing root.
    UnavailableInLegacySchema,
}

/// One role-bearing canonical product edge.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticDataChild {
    pub role: ProductChildRole,
    pub target: ProductRef,
}

/// One sequentially decoded canonical semantic atom.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticDataAtom<'fragment> {
    pub ordinal: AtomId,
    pub atom: SemanticAtom<'fragment>,
}

/// Linear borrowed atom traversal.  Renderers should join product head IDs to
/// this cursor once, rather than repeatedly using random `atom` lookups over
/// a variable-width byte lane.
pub struct SemanticDataAtomCursor<'fragment> {
    payload: &'fragment [u8],
    at: usize,
    remaining: u32,
    ordinal: u32,
}

impl<'fragment> Iterator for SemanticDataAtomCursor<'fragment> {
    type Item = SemanticDataAtom<'fragment>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let ordinal = AtomId::new(self.ordinal);
        let length = wire_index(read_u32(self.payload, self.at));
        let start = self.at.checked_add(4)?;
        let end = start.checked_add(length)?;
        let bytes = self.payload.get(start..end)?;
        self.at = end;
        self.remaining -= 1;
        self.ordinal += 1;
        Some(SemanticDataAtom {
            ordinal,
            atom: SemanticAtom { bytes },
        })
    }
}

/// Borrowed access over one validated semantic-data payload.
#[derive(Clone, Copy)]
pub struct SemanticDataView<'fragment> {
    payload: &'fragment [u8],
    layout: SemanticDataLayout,
}

impl<'fragment> SemanticDataView<'fragment> {
    pub(crate) const fn from_validated(
        payload: &'fragment [u8],
        layout: SemanticDataLayout,
    ) -> Self {
        Self { payload, layout }
    }

    #[must_use]
    pub const fn counts(self) -> SemanticDataCounts {
        SemanticDataCounts {
            atoms: self.layout.atom_count,
            products: self.layout.product_count,
            constructors: self.layout.constructor_count,
            lists: self.layout.list_count,
            children: self.layout.child_count,
        }
    }

    /// Iterates canonical atoms in one pass over the variable-width atom
    /// lane.  This is the intended render/discovery join for product heads.
    #[must_use]
    pub const fn atoms(self) -> SemanticDataAtomCursor<'fragment> {
        SemanticDataAtomCursor {
            payload: self.payload,
            at: self.layout.atom_bytes_start,
            remaining: self.layout.atom_count,
            ordinal: 0,
        }
    }

    /// Lends one canonical semantic atom in its immutable graph coordinate.
    /// Repeated random access is linear in `ordinal`; prefer [`Self::atoms`]
    /// for full-graph traversal.
    #[must_use]
    pub fn atom(self, ordinal: AtomId) -> Option<SemanticAtom<'fragment>> {
        if ordinal.raw >= self.layout.atom_count {
            return None;
        }
        let mut cursor = self.layout.atom_bytes_start;
        for _ in 0..ordinal.raw {
            let length = wire_index(read_u32(self.payload, cursor));
            cursor = cursor.checked_add(4)?.checked_add(length)?;
        }
        let length = wire_index(read_u32(self.payload, cursor));
        let bytes = self.payload.get(cursor.checked_add(4)?..cursor.checked_add(4 + length)?)?;
        Some(SemanticAtom { bytes })
    }

    /// Reads one canonical product root and its child-list coordinate.
    #[must_use]
    pub fn product(self, ordinal: ProductId) -> Option<SemanticProduct> {
        let record = self.record(self.layout.products_start, ordinal.raw, SEMANTIC_PRODUCT_BYTES)?;
        Some(SemanticProduct {
            head: AtomId::new(read_u32(record, 0)),
            children: ProductListId::new(read_u32(record, 4)),
        })
    }

    /// Reads the product-aligned constructor cell.
    #[must_use]
    pub fn constructor(self, ordinal: ProductId) -> Option<SemanticProductConstructor> {
        let record = self.record(
            self.layout.constructors_start,
            ordinal.raw,
            SEMANTIC_CONSTRUCTOR_BYTES,
        )?;
        let tag = read_u32(record, 0);
        let payload0 = read_u32(record, 4);
        let payload1 = read_u32(record, 8);
        let product = self.product(ordinal)?;
        let list = self.list(product.children)?;
        SemanticProductConstructor::try_from_parts(tag, payload0, payload1, list.length).ok()
    }

    /// Reads one pooled child-list span.
    #[must_use]
    pub fn list(self, ordinal: ProductListId) -> Option<crate::ListSpan<crate::ProductChildren>> {
        let record = self.record(self.layout.lists_start, ordinal.raw, SEMANTIC_LIST_BYTES)?;
        Some(crate::ListSpan::new(read_u32(record, 0), read_u32(record, 4)))
    }

    /// Reads one canonical product child, including its exact local or
    /// external authority-bearing target.
    #[must_use]
    pub fn child(self, ordinal: u32) -> Option<SemanticDataChild> {
        let record = self.record(self.layout.children_start, ordinal, SEMANTIC_CHILD_BYTES)?;
        let role = ProductChildRole::try_from(record[0]).ok()?;
        let target = match record[1] {
            SEMANTIC_LOCAL_TAG => ProductRef::Local(ProductId::new(read_u32(record, 2))),
            SEMANTIC_EXTERNAL_TAG => {
                let target = read_u32(record, 2);
                let raw: [u8; heart_identity::HASH_BYTES] = record[6..].try_into().ok()?;
                let fragment = ContentId::<IrFragmentDomain>::try_from(raw).ok()?;
                ProductRef::External(ExternalProductRef::bind(fragment, target))
            }
            _ => return None,
        };
        Some(SemanticDataChild { role, target })
    }

    /// Reopens the durable declaration-to-product link.  The result makes
    /// absent legacy provenance explicit rather than pretending the entity
    /// had no semantic root.
    #[must_use]
    pub fn entity_root(self, entity: EntityId) -> Option<SemanticDataEntityRoot> {
        if entity.raw >= self.layout.entity_root_count && self.layout.entity_roots_start.is_some() {
            return None;
        }
        let Some(start) = self.layout.entity_roots_start else {
            return Some(SemanticDataEntityRoot::UnavailableInLegacySchema);
        };
        let at = start.checked_add(wire_index(entity.raw).checked_mul(4)?)?;
        Some(SemanticDataEntityRoot::Captured(ProductId::new(read_u32(
            self.payload, at,
        ))))
    }

    fn record(self, start: usize, ordinal: u32, width: usize) -> Option<&'fragment [u8]> {
        let index = wire_index(ordinal);
        let at = start.checked_add(index.checked_mul(width)?)?;
        self.payload.get(at..at.checked_add(width)?)
    }
}

#[allow(
    clippy::as_conversions,
    reason = "validated envelopes require a native-address-width-compatible directory before constructing this view"
)]
fn wire_index(value: u32) -> usize {
    value as usize
}

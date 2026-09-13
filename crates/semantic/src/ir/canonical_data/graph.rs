//! Borrowed canonical graph views and source-coordinate mapping.

use super::{
    error::CanonicalDataError,
    types::{DataCanonicalization, lane_index},
};
use crate::ir::{
    AtomId, ListSpan, ProductChildren, ProductId, ProductList, ProductListId, SemanticAtom,
    SemanticProduct, SemanticProductChild, SemanticProductConstructor,
};

/// A canonical graph borrowing the caller's output and source atom bytes.
pub struct CanonicalDataGraph<'output, 'bytes> {
    pub(super) atoms: &'output [SemanticAtom<'bytes>],
    pub(super) products: &'output [SemanticProduct],
    pub(super) constructors: &'output [SemanticProductConstructor],
    pub(super) lists: &'output [ListSpan<ProductChildren>],
    pub(super) children: &'output [SemanticProductChild],
    pub(super) atom_map: &'output [u32],
    pub(super) product_map: &'output [u32],
    pub(super) metrics: DataCanonicalization,
}

impl<'output, 'bytes> CanonicalDataGraph<'output, 'bytes> {
    /// Measured resource use and canonical cardinalities.
    #[must_use]
    pub const fn metrics(&self) -> DataCanonicalization {
        self.metrics
    }

    /// Borrow the canonical atom lane.
    #[must_use]
    pub const fn atoms(&self) -> &'output [SemanticAtom<'bytes>] {
        self.atoms
    }

    /// Borrow the canonical product lane.
    #[must_use]
    pub const fn products(&self) -> &'output [SemanticProduct] {
        self.products
    }

    /// Borrow the canonical product-aligned constructor lane.
    #[must_use]
    pub const fn constructors(&self) -> &'output [SemanticProductConstructor] {
        self.constructors
    }

    /// Borrow the canonical pooled-child lane.
    #[must_use]
    pub const fn children(&self) -> &'output [SemanticProductChild] {
        self.children
    }

    /// Source-product to canonical-product roots in source declaration
    /// order.  Durable schema-3 fragments serialize this mapping so reopen
    /// can recover each declaration's canonical graph root after product
    /// deduplication.
    #[must_use]
    pub const fn source_product_roots(&self) -> &'output [u32] {
        self.product_map
    }

    /// Borrow the canonical pooled-list span table.
    #[must_use]
    pub const fn lists(&self) -> &'output [ListSpan<ProductChildren>] {
        self.lists
    }

    /// Map one source atom coordinate to its canonical coordinate.
    pub fn canonical_atom(&self, source: AtomId) -> Result<AtomId, CanonicalDataError> {
        let Some(&canonical) = self.atom_map.get(lane_index(source.raw)) else {
            return Err(CanonicalDataError::CanonicalAtom {
                ordinal: source,
                count: self.atom_map.len(),
            });
        };
        Ok(AtomId::new(canonical))
    }

    /// Map one source product coordinate to its canonical coordinate.
    pub fn canonical_product(&self, source: ProductId) -> Result<ProductId, CanonicalDataError> {
        let Some(&canonical) = self.product_map.get(lane_index(source.raw)) else {
            return Err(CanonicalDataError::CanonicalProduct {
                ordinal: source,
                count: self.product_map.len(),
            });
        };
        Ok(ProductId::new(canonical))
    }

    /// Borrow one canonical pooled product list after checking its output
    /// coordinate against the canonical list table.
    pub fn list(&self, list: ProductListId) -> Result<ProductList<'output>, CanonicalDataError> {
        let span = self.list_span(list)?;
        ProductList::try_from_parts(self.children, span).map_err(|fault| {
            CanonicalDataError::CanonicalListExtent {
                ordinal: list,
                span,
                fault,
            }
        })
    }

    /// Return one canonical list span after checking its list coordinate.
    pub fn list_span(
        &self,
        list: ProductListId,
    ) -> Result<ListSpan<ProductChildren>, CanonicalDataError> {
        self.lists
            .get(lane_index(list.raw))
            .copied()
            .ok_or(CanonicalDataError::CanonicalList {
                ordinal: list,
                count: self.lists.len(),
            })
    }
}

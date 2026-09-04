//! Defines validated semantic discovery over one durable fragment.
//!
//! This is deliberately a borrowed projection, not a second compact parser
//! and not a lossy attempt to reconstruct source syntax.  Every cursor lends
//! the exact already-validated wire plane, so publication reopen, render
//! selection, and corpus census all start from the same semantic truth.

use crate::{
    AtomCursor, DocFactCursor, EntityCursor, ExtensionPoolFault, FragmentView,
    LanguageExtensionReopenError, OccurrenceCursor,
    ReopenedLanguageExtensionSection, TypeFactChildCursor, TypeFactCursor, TypeFactSegment,
    TypeNodeCursor,
};

/// Exact reopen failure while joining the typed extension plane to its shared
/// durable pools.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FragmentDiscoveryError {
    MissingExtensionPools,
    TypeFacts(crate::TypeFactFault),
    ExtensionPools(ExtensionPoolFault),
    LanguageExtensions(LanguageExtensionReopenError),
}

/// One complete borrowed semantic discovery surface over a validated fragment.
#[derive(Clone, Copy)]
pub struct FragmentDiscovery<'fragment> {
    view: &'fragment FragmentView<'fragment>,
}

/// Exact cardinalities of the durable semantic planes.
///
/// The census is intentionally structural: it measures what was reopened
/// without reparsing source or consuming a renderer-side cache.  A caller can
/// compare a source-built IR census with this value after publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticCensus {
    pub entities: u32,
    pub atoms: u32,
    pub compact_type_nodes: u32,
    pub canonical_products: u32,
    pub canonical_product_children: u32,
    /// Schema-3-and-later declaration roots; legacy fragments report zero and expose
    /// `UnavailableInLegacySchema` through the semantic-data view.
    pub canonical_entity_roots: u32,
    pub occurrences: u32,
    pub declared_type_facts: u32,
    pub computed_type_facts: u32,
    pub documentation_fragments: u32,
    pub language_extension_section: bool,
    pub extension_pool_section: bool,
}

impl<'fragment> FragmentDiscovery<'fragment> {
    /// Iterates the compact entity rows in their durable order.
    #[must_use]
    pub fn entities(self) -> EntityCursor<'fragment> {
        self.view.entities()
    }

    /// Iterates the compact type-node lane in its durable order.
    #[must_use]
    pub fn compact_type_nodes(self) -> TypeNodeCursor<'fragment> {
        self.view.type_nodes()
    }

    /// Opens the durable canonical product graph and its schema-aware
    /// declaration roots, without re-canonicalizing or reparsing source.
    #[must_use]
    pub fn semantic_data(self) -> Option<crate::SemanticDataView<'fragment>> {
        self.view.semantic_data()
    }

    /// Iterates the interned atom lane in its durable order.
    #[must_use]
    pub fn atoms(self) -> AtomCursor<'fragment> {
        self.view.atoms()
    }

    /// Lends the typed reference/occurrence plane when the producer emitted it.
    #[must_use]
    pub fn occurrences(self) -> Option<OccurrenceCursor<'fragment>> {
        self.view.occurrences()
    }

    /// Lends declared and observed/computed type facts without widening their
    /// two-segment coordinate contract.
    #[must_use]
    pub fn type_facts(self) -> Option<TypeFactCursor<'fragment>> {
        self.view.type_facts()
    }

    /// Lends the exact type-child pool addressed by the semantic type facts.
    /// A validated fragment cannot fail this reopen; any retained error is an
    /// exact wire fault rather than an empty fallback.
    pub fn type_fact_children(
        self,
    ) -> Option<Result<TypeFactChildCursor<'fragment>, crate::TypeFactFault>> {
        self.type_facts().map(|facts| facts.children())
    }

    /// Lends the documentation fragments exactly as the validated wire plane.
    #[must_use]
    pub fn documentation(self) -> Option<DocFactCursor<'fragment>> {
        self.view.docs()
    }

    /// Opens the schema-aware shared extension pools from the fragment proof.
    /// Schema-4 callers receive exact type-parameter list ranges; legacy
    /// callers receive only the explicitly typed start-only interpretation.
    pub fn extension_pools(
        self,
    ) -> Result<Option<crate::ReopenedExtensionPools<'fragment>>, FragmentDiscoveryError> {
        self.view
            .validated_extension_pools()
            .transpose()
            .map_err(FragmentDiscoveryError::ExtensionPools)
    }

    /// Reopens all seven typed sparse extension columns under the fragment's
    /// profile authority.  No erased map or reparsing path is involved.
    pub fn language_extensions(
        self,
    ) -> Result<Option<ReopenedLanguageExtensionSection<'fragment>>, FragmentDiscoveryError> {
        self.view
            .validated_language_extensions()
            .transpose()
            .map_err(FragmentDiscoveryError::LanguageExtensions)
    }

    /// Counts every reopened semantic plane. Existing fragment validation has
    /// already proven cursor grammar, so an iterator error is retained as a
    /// zero-cost impossible branch rather than silently ignored.
    pub fn census(self) -> SemanticCensus {
        let mut occurrences = 0_u32;
        if let Some(cursor) = self.occurrences() {
            for row in cursor {
                debug_assert!(row.is_ok());
                occurrences = occurrences.saturating_add(1);
            }
        }
        let mut declared_type_facts = 0_u32;
        let mut computed_type_facts = 0_u32;
        if let Some(cursor) = self.type_facts() {
            for row in cursor {
                match row {
                    Ok(row) if row.segment == TypeFactSegment::Declared => {
                        declared_type_facts = declared_type_facts.saturating_add(1);
                    }
                    Ok(_) => computed_type_facts = computed_type_facts.saturating_add(1),
                    Err(_) => debug_assert!(false, "validated type-fact cursor failed"),
                }
            }
        }
        let mut documentation_fragments = 0_u32;
        if let Some(cursor) = self.documentation() {
            for row in cursor {
                debug_assert!(row.is_ok());
                documentation_fragments = documentation_fragments.saturating_add(1);
            }
        }
        SemanticCensus {
            entities: self.entities().len() as u32,
            atoms: self.atoms().len() as u32,
            compact_type_nodes: self.compact_type_nodes().len() as u32,
            canonical_products: self.semantic_data().map_or(0, |graph| graph.counts().products),
            canonical_product_children: self.semantic_data().map_or(0, |graph| graph.counts().children),
            canonical_entity_roots: self
                .semantic_data()
                .map_or(0, |graph| match graph.entity_root(crate::EntityId::new(0)) {
                    Some(crate::SemanticDataEntityRoot::Captured(_)) => self.entities().len() as u32,
                    _ => 0,
                }),
            occurrences,
            declared_type_facts,
            computed_type_facts,
            documentation_fragments,
            language_extension_section: self.view.language_extension_payload().is_some(),
            extension_pool_section: self.view.extension_pool_payload().is_some(),
        }
    }
}

impl<'fragment> FragmentView<'fragment> {
    /// Opens the one canonical durable semantic discovery surface.
    #[must_use]
    pub const fn discover(&'fragment self) -> FragmentDiscovery<'fragment> {
        FragmentDiscovery { view: self }
    }
}

//! Immutable lexical posting base.

use super::plan::terms_for;
use crate::delta::DocumentState;
use crate::{Binding, Error};
use backend_version::CoverageWitness;
use std::collections::{BTreeMap, BTreeSet};
use std::mem::size_of;
use std::sync::Arc;

/// Immutable lexical posting base tied to one complete document root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LexicalBase {
    binding: Binding,
    coverage: CoverageWitness,
    documents: Arc<[u64]>,
    postings: Arc<BTreeMap<String, Arc<[u64]>>>,
    bytes: usize,
}

impl LexicalBase {
    /// Builds one immutable posting base from a complete typed state.
    ///
    /// # Errors
    ///
    /// Returns [`Error::IncompleteCoverage`] for a partial source or
    /// [`Error::SizeLimit`] when the retained byte estimate overflows.
    pub fn from_state(state: &DocumentState) -> Result<Self, Error> {
        if !matches!(
            state.coverage(),
            CoverageWitness::Complete(_) | CoverageWitness::Closed(_)
        ) {
            return Err(Error::IncompleteCoverage);
        }
        let mut postings: BTreeMap<String, BTreeSet<u64>> = BTreeMap::new();
        let mut documents = Vec::new();
        let mut bytes = 0usize;
        for (id, fields) in state.iter() {
            documents.push(id);
            for term in terms_for(fields) {
                bytes = bytes
                    .checked_add(term.len())
                    .and_then(|size| size.checked_add(size_of::<u64>()))
                    .ok_or(Error::SizeLimit)?;
                postings.entry(term).or_default().insert(id);
            }
        }
        let postings = postings
            .into_iter()
            .map(|(term, ids)| (term, Arc::from(ids.into_iter().collect::<Vec<_>>())))
            .collect();
        Ok(Self {
            binding: state.binding(),
            coverage: state.coverage(),
            documents: Arc::from(documents),
            postings: Arc::new(postings),
            bytes,
        })
    }

    /// Returns the exact canonical relation binding.
    #[must_use]
    pub const fn binding(&self) -> Binding {
        self.binding
    }

    /// Returns the complete witness retained with this base.
    #[must_use]
    pub const fn coverage(&self) -> CoverageWitness {
        self.coverage
    }

    /// Returns the immutable document key order.
    #[must_use]
    pub fn documents(&self) -> &[u64] {
        &self.documents
    }

    /// Returns one immutable posting list without allocating.
    #[must_use]
    pub fn posting(&self, term: &str) -> Option<&[u64]> {
        self.postings.get(term).map(AsRef::as_ref)
    }

    /// Returns the approximate retained posting bytes.
    #[must_use]
    pub const fn bytes(&self) -> usize {
        self.bytes
    }
}

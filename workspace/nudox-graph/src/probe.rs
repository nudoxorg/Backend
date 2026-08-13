//! Counters over the `nudox-store` round-trips a query actually makes.
//!
//! # Why this exists
//!
//! An optimisation in this crate is a claim about *how much work the store
//! was asked to do*, and nothing about the rows a query returns can support
//! that claim: a full scan and an index lookup return the same rows. Before
//! this module the crate had one `AtomicUsize` named `packages_scanned`,
//! incremented at exactly one call site, so the only optimisation the test
//! suite could express was "the `Symbols` key-equality path did not
//! enumerate". Every other access path — the per-vertex `Corpus::packages()`
//! round-trips behind `usages`/`mentions`/`implementors`, the per-occurrence
//! `Corpus::package()` lookups behind `Occurrence.target`, the `Packages`
//! entrypoint — was unmeasurable, and therefore free to regress.
//!
//! # The counting rule (doctrine §8)
//!
//! Every counter here is incremented **at the call site that pays the cost**,
//! never accumulated alongside it. `CorpusList` is bumped by the code that
//! awaits `Corpus::packages()`, `PackageEnumerated` by the code that walks a
//! package's entry table. A tally kept in parallel with the thing it counts
//! can drift from it; one derived from it cannot.

use std::sync::atomic::{AtomicUsize, Ordering};

/// One kind of round-trip into `nudox-store` that a resolution can make.
///
/// # Deliberately exhaustive
///
/// This enum is **not** `#[non_exhaustive]`, for the same reason
/// `ProducerError` is not (doctrine §3): it is internal to this workspace, and
/// the whole value of adding a variant is that it breaks every `match` and
/// forces each one to decide what the new round-trip costs. A `_` arm in
/// [`AdapterProbe`] would silently route a future access path into whichever
/// bucket happened to be there, which is exactly the failure this module was
/// written to prevent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StoreProbe {
    /// `Corpus::packages()` — a read lock over the whole corpus map plus one
    /// `Arc` clone per loaded package, allocated fresh on every call. This is
    /// the round-trip an N+1 multiplies.
    CorpusList,

    /// `Corpus::package(id)` or `Corpus::entry(key)` — one O(log n) `BTreeMap`
    /// probe. Cheap individually; the count is what distinguishes "once per
    /// distinct package" from "once per row".
    CorpusLookup,

    /// One package's entire entry table walked end to end. This is the cost a
    /// pushdown exists to avoid, and it is proportional to the corpus, not to
    /// the answer.
    PackageEnumerated,

    /// One `PackageIndexes` probe — `by_name`, `by_kind`, `usages_of`,
    /// `mentions_of`, or `type_refs_in` behind any of the signature-position
    /// reverse edges. Bounded by the size of the *answer*, not the corpus.
    ///
    /// Deliberately one variant for all of them: the thing worth counting is
    /// "did this cost one index lookup or a corpus walk", and splitting it per
    /// index would make a test assert on which structure was consulted rather
    /// than on how much work was done.
    IndexProbe,
}

impl StoreProbe {
    /// Every variant, so a report or a test can iterate the full set without
    /// re-listing it and drifting out of sync with the enum.
    pub const ALL: [StoreProbe; 4] = [
        StoreProbe::CorpusList,
        StoreProbe::CorpusLookup,
        StoreProbe::PackageEnumerated,
        StoreProbe::IndexProbe,
    ];
}

/// A set of store round-trip counters shared with a running query.
///
/// Attached to a [`crate::CorpusAdapter`] via
/// [`crate::CorpusAdapter::new_with_probe`]. Cloning the adapter shares the
/// same counters, which is what lets a test read them after the query stream
/// has been fully drained.
#[derive(Debug, Default)]
pub struct AdapterProbe {
    corpus_list: AtomicUsize,
    corpus_lookup: AtomicUsize,
    package_enumerated: AtomicUsize,
    index_probe: AtomicUsize,
}

impl AdapterProbe {
    /// A fresh probe with every counter at zero.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one round-trip of the given kind.
    ///
    /// Deliberately has no `record_n` companion: every counter here is
    /// incremented by the statement that performs the round-trip, and a bulk
    /// increment would mean some caller computed a cost instead of paying it.
    /// That is the drift doctrine §8 warns about.
    pub fn record(&self, which: StoreProbe) {
        self.slot(which).fetch_add(1, Ordering::Relaxed);
    }

    /// How many round-trips of the given kind have been recorded.
    pub fn count(&self, which: StoreProbe) -> usize {
        self.slot(which).load(Ordering::Relaxed)
    }

    /// Every counter, paired with its kind — for a failure message that says
    /// what the query *did* do, not merely that one number was wrong.
    pub fn snapshot(&self) -> [(StoreProbe, usize); 4] {
        StoreProbe::ALL.map(|p| (p, self.count(p)))
    }

    fn slot(&self, which: StoreProbe) -> &AtomicUsize {
        match which {
            StoreProbe::CorpusList => &self.corpus_list,
            StoreProbe::CorpusLookup => &self.corpus_lookup,
            StoreProbe::PackageEnumerated => &self.package_enumerated,
            StoreProbe::IndexProbe => &self.index_probe,
        }
    }
}

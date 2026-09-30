use super::super::product_state::normalized_version_key;
use super::{
    LineageKey, LineageReleaseDocument, MAX_SEARCH_PAGE_SIZE, SEARCH_TIERS, SearchTier,
    fuzzy_term_matches, gram_tokens, lineage_search_name, normalize, release_gram_field,
    release_search_evidence,
};
use backend_engine::registry::RegistryEcosystem;
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};
use std::ops::Bound;
use std::sync::Arc;

const VERSION_RANK_STRIDE: u64 = 1_u64 << 32;
type ReleaseOrdinal = u32;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum ReleaseVersionOrderKey {
    // Typed releases sort after tags that the ecosystem grammar cannot prove.
    // Since pages read ranks in reverse, this presents typed versions first
    // and leaves unknown tags in deterministic lexical order at the end.
    Unorderable(Arc<str>),
    Orderable(backend_engine::advisory::NormalizedVersion),
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct VersionedReleaseKey {
    version: ReleaseVersionOrderKey,
    // Descending stable key at equal versions gives ascending display order
    // when the posting rank is read from newest to oldest.
    stable_sort_key: Reverse<Arc<str>>,
    identity: Arc<str>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum ReleasePostingField {
    Coordinate,
    AliasValue,
    AliasToken,
    Keyword,
    Description,
    Advisory,
    Generic,
    CoordinateGram1,
    CoordinateGram2,
    CoordinateGram3,
}

#[derive(Default)]
enum OrdinalPosting {
    #[default]
    Empty,
    One(ReleaseOrdinal),
    Many(Vec<ReleaseOrdinal>),
}

impl OrdinalPosting {
    fn len(&self) -> usize {
        match self {
            Self::Empty => 0,
            Self::One(_) => 1,
            Self::Many(ordinals) => ordinals.len(),
        }
    }

    fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }

    fn insert(&mut self, ordinal: ReleaseOrdinal, rank: u64, entries: &[Option<ProjectionEntry>]) {
        match self {
            Self::Empty => *self = Self::One(ordinal),
            Self::One(previous) => {
                let previous_rank = entry_rank(entries, *previous);
                let ordinals = if previous_rank < rank {
                    vec![*previous, ordinal]
                } else {
                    vec![ordinal, *previous]
                };
                *self = Self::Many(ordinals);
            }
            Self::Many(ordinals) => {
                let position = ordinals
                    .binary_search_by_key(&rank, |candidate| entry_rank(entries, *candidate))
                    .expect_err("release ranks are unique within a lineage");
                ordinals.insert(position, ordinal);
            }
        }
    }

    fn remove(&mut self, ordinal: ReleaseOrdinal, rank: u64, entries: &[Option<ProjectionEntry>]) {
        match self {
            Self::Empty => {}
            Self::One(current) if *current == ordinal => *self = Self::Empty,
            Self::One(_) => {}
            Self::Many(ordinals) => {
                if let Ok(position) = ordinals
                    .binary_search_by_key(&rank, |candidate| entry_rank(entries, *candidate))
                {
                    debug_assert_eq!(ordinals[position], ordinal);
                    ordinals.remove(position);
                }
                if ordinals.len() == 1 {
                    *self = Self::One(ordinals[0]);
                } else if ordinals.is_empty() {
                    *self = Self::Empty;
                }
            }
        }
    }
}

#[derive(Clone)]
struct ProjectionEntry {
    identity: Arc<str>,
    rank: u64,
}

#[derive(Default)]
pub(super) struct VersionedReleaseProjection {
    // The arena gives every release a compact stable ordinal. Order keys and
    // identity lookup share Arc string storage; posting lists keep ordinals,
    // not copied identities or wide rank labels.
    order: BTreeMap<VersionedReleaseKey, ReleaseOrdinal>,
    ordinals_by_identity: BTreeMap<Arc<str>, ReleaseOrdinal>,
    entries: Vec<Option<ProjectionEntry>>,
    free_ordinals: Vec<ReleaseOrdinal>,
    postings: BTreeMap<ReleasePostingField, BTreeMap<String, OrdinalPosting>>,
    #[cfg(test)]
    rebalance_count: usize,
}

pub(super) struct ReleasePostingPage {
    pub(super) identities: Vec<String>,
    pub(super) posting_entries_examined: usize,
    pub(super) term_keys_scanned: usize,
    pub(super) more: bool,
}

struct PostingSources<'a> {
    postings: Vec<PostingSource<'a>>,
    term_keys_scanned: usize,
}

#[derive(Clone, Copy)]
enum PostingSource<'a> {
    All,
    Term(&'a OrdinalPosting),
}

impl PostingSource<'_> {
    fn cardinality(&self, all_releases: usize) -> usize {
        match self {
            Self::All => all_releases,
            Self::Term(posting) => posting.len(),
        }
    }
}

enum PostingIterator<'a> {
    All(
        std::iter::Rev<
            std::collections::btree_map::Values<'a, VersionedReleaseKey, ReleaseOrdinal>,
        >,
    ),
    One(Option<ReleaseOrdinal>),
    Many(std::iter::Rev<std::slice::Iter<'a, ReleaseOrdinal>>),
}

impl PostingIterator<'_> {
    fn next(&mut self) -> Option<ReleaseOrdinal> {
        match self {
            Self::All(values) => values.next().copied(),
            Self::One(value) => value.take(),
            Self::Many(values) => values.next().copied(),
        }
    }
}

fn entry_rank(entries: &[Option<ProjectionEntry>], ordinal: ReleaseOrdinal) -> u64 {
    entries
        .get(ordinal as usize)
        .and_then(Option::as_ref)
        .expect("posting ordinal resolves to an active release")
        .rank
}

impl VersionedReleaseProjection {
    pub(super) fn release_count(&self) -> usize {
        self.ordinals_by_identity.len()
    }

    pub(super) fn estimated_logical_payload_bytes(&self) -> usize {
        use std::mem::size_of;

        let ordered = self
            .order
            .iter()
            .map(|(key, _)| {
                size_of::<VersionedReleaseKey>()
                    .saturating_add(match &key.version {
                        ReleaseVersionOrderKey::Orderable(version) => {
                            version.canonical.len().max(format!("{version:?}").len())
                        }
                        ReleaseVersionOrderKey::Unorderable(version) => version.len(),
                    })
                    .saturating_add(key.stable_sort_key.0.len())
            })
            .sum::<usize>();
        let identities = self
            .ordinals_by_identity
            .values()
            .map(|_| size_of::<(Arc<str>, ReleaseOrdinal)>())
            .sum::<usize>();
        let entries = self
            .entries
            .iter()
            .flatten()
            .map(|entry| size_of::<ProjectionEntry>().saturating_add(entry.identity.len()))
            .sum::<usize>();
        let posting_bytes = self
            .postings
            .values()
            .flat_map(|values| values.iter())
            .map(|(value, posting)| {
                size_of::<(String, OrdinalPosting)>()
                    .saturating_add(value.len())
                    .saturating_add(posting.len().saturating_mul(size_of::<ReleaseOrdinal>()))
            })
            .sum::<usize>();
        ordered
            .saturating_add(identities)
            .saturating_add(entries)
            .saturating_add(posting_bytes)
    }

    pub(super) fn insert(
        &mut self,
        identity: &str,
        document: &LineageReleaseDocument,
        ecosystem: RegistryEcosystem,
    ) {
        let identity: Arc<str> = Arc::from(identity);
        let order_key = VersionedReleaseKey {
            version: release_version_order_key(ecosystem, &document.version),
            stable_sort_key: Reverse(Arc::from(document.order_sort_key.as_str())),
            identity: Arc::clone(&identity),
        };
        let ordinal = if let Some(ordinal) = self.free_ordinals.pop() {
            ordinal
        } else {
            let ordinal = ReleaseOrdinal::try_from(self.entries.len())
                .expect("a lineage cannot contain more releases than fit in u32 ordinals");
            self.entries.push(None);
            ordinal
        };
        self.entries[ordinal as usize] = Some(ProjectionEntry {
            identity: Arc::clone(&identity),
            rank: 0,
        });
        self.order.insert(order_key.clone(), ordinal);
        self.ordinals_by_identity
            .insert(Arc::clone(&identity), ordinal);
        if let Some(rank) = self.rank_between_neighbors(&order_key) {
            self.entries[ordinal as usize]
                .as_mut()
                .expect("new ordinal is active")
                .rank = rank;
        } else {
            self.rebalance_ranks();
        }
        let rank = entry_rank(&self.entries, ordinal);

        for (field, value) in release_posting_values(document) {
            self.postings
                .entry(field)
                .or_default()
                .entry(value)
                .or_default()
                .insert(ordinal, rank, &self.entries);
        }
    }

    pub(super) fn remove(
        &mut self,
        identity: &str,
        document: &LineageReleaseDocument,
        ecosystem: RegistryEcosystem,
    ) {
        let Some(ordinal) = self.ordinals_by_identity.remove(identity) else {
            return;
        };
        let rank = entry_rank(&self.entries, ordinal);
        self.order.remove(&VersionedReleaseKey {
            version: release_version_order_key(ecosystem, &document.version),
            stable_sort_key: Reverse(Arc::from(document.order_sort_key.as_str())),
            identity: Arc::from(identity),
        });
        for (field, value) in release_posting_values(document) {
            let remove_field = if let Some(values) = self.postings.get_mut(&field) {
                let remove_value = if let Some(posting) = values.get_mut(&value) {
                    posting.remove(ordinal, rank, &self.entries);
                    posting.is_empty()
                } else {
                    false
                };
                if remove_value {
                    values.remove(&value);
                }
                values.is_empty()
            } else {
                false
            };
            if remove_field {
                self.postings.remove(&field);
            }
        }
        self.entries[ordinal as usize] = None;
        self.free_ordinals.push(ordinal);
    }

    fn rank_between_neighbors(&self, key: &VersionedReleaseKey) -> Option<u64> {
        let lower = self
            .order
            .range(..key.clone())
            .next_back()
            .map(|(_, ordinal)| entry_rank(&self.entries, *ordinal));
        let upper = self
            .order
            .range((Bound::Excluded(key.clone()), Bound::Unbounded))
            .next()
            .map(|(_, ordinal)| entry_rank(&self.entries, *ordinal));
        match (lower, upper) {
            (None, None) => Some(VERSION_RANK_STRIDE),
            (Some(lower), None) => lower.checked_add(VERSION_RANK_STRIDE),
            (None, Some(upper)) => upper.checked_sub(VERSION_RANK_STRIDE),
            (Some(lower), Some(upper)) if upper.saturating_sub(lower) > 1 => {
                Some(lower + (upper - lower) / 2)
            }
            (Some(_), Some(_)) => None,
        }
    }

    fn rebalance_ranks(&mut self) {
        #[cfg(test)]
        {
            self.rebalance_count = self.rebalance_count.saturating_add(1);
        }
        for (offset, ordinal) in self.order.values().enumerate() {
            let rank = u64::try_from(offset)
                .expect("a release lineage cannot contain more releases than fit in u32 ordinals")
                .saturating_add(1)
                .saturating_mul(VERSION_RANK_STRIDE);
            self.entries[*ordinal as usize]
                .as_mut()
                .expect("ordered ordinal is active")
                .rank = rank;
        }
        // Existing posting vectors remain sorted because rebalance preserves
        // the semantic order and only replaces rank labels.
    }

    pub(super) fn top_matches(
        &self,
        key: &LineageKey,
        releases: &BTreeMap<String, LineageReleaseDocument>,
        query: &str,
        limit: usize,
    ) -> ReleasePostingPage {
        let limit = limit.min(MAX_SEARCH_PAGE_SIZE);
        if limit == 0 || self.order.is_empty() {
            return ReleasePostingPage {
                identities: Vec::new(),
                posting_entries_examined: 0,
                term_keys_scanned: 0,
                more: false,
            };
        }
        let query = normalize(query);
        if query.is_empty() {
            let (examined, identities) = self.collect_posting_page(
                &[PostingSource::All],
                key,
                releases,
                &query,
                None,
                limit.saturating_add(1),
            );
            let more = identities.len() > limit;
            let mut identities = identities;
            identities.truncate(limit);
            return ReleasePostingPage {
                identities,
                posting_entries_examined: examined,
                term_keys_scanned: 0,
                more,
            };
        }

        let mut identities = Vec::with_capacity(limit.saturating_add(1));
        let mut posting_entries_examined = 0_usize;
        let mut term_keys_scanned = 0_usize;
        for tier in SEARCH_TIERS {
            let remaining = limit.saturating_add(1).saturating_sub(identities.len());
            if remaining == 0 {
                break;
            }
            let sources = self.postings_for_tier(key, &query, tier);
            term_keys_scanned = term_keys_scanned.saturating_add(sources.term_keys_scanned);
            if sources.postings.is_empty() {
                continue;
            }
            let (examined, hits) = self.collect_posting_page(
                &sources.postings,
                key,
                releases,
                &query,
                Some(tier),
                remaining,
            );
            posting_entries_examined = posting_entries_examined.saturating_add(examined);
            identities.extend(hits);
        }
        let more = identities.len() > limit;
        identities.truncate(limit);
        ReleasePostingPage {
            identities,
            posting_entries_examined,
            term_keys_scanned,
            more,
        }
    }

    fn postings_for_tier<'a>(
        &'a self,
        key: &LineageKey,
        query: &str,
        tier: SearchTier,
    ) -> PostingSources<'a> {
        let name = normalize(lineage_search_name(key.ecosystem, &key.lineage));
        let postings = match tier {
            SearchTier::ExactCoordinate => {
                self.exact_posting(ReleasePostingField::Coordinate, query)
            }
            SearchTier::ExactName | SearchTier::PrefixName => {
                let postings = if name == query
                    || (matches!(tier, SearchTier::PrefixName) && name.starts_with(query))
                {
                    vec![PostingSource::All]
                } else {
                    Vec::new()
                };
                PostingSources {
                    postings,
                    term_keys_scanned: 0,
                }
            }
            SearchTier::ExactAlias => self.exact_posting(ReleasePostingField::AliasValue, query),
            SearchTier::PrefixCoordinate => {
                self.prefix_postings(ReleasePostingField::Coordinate, query)
            }
            SearchTier::PrefixAlias => self.prefix_postings(ReleasePostingField::AliasValue, query),
            SearchTier::Substring => {
                if name.contains(query) {
                    return PostingSources {
                        postings: vec![PostingSource::All],
                        term_keys_scanned: 0,
                    };
                }
                let width = match query.chars().count() {
                    0 => {
                        return PostingSources {
                            postings: Vec::new(),
                            term_keys_scanned: 0,
                        };
                    }
                    1 => 1,
                    2 => 2,
                    _ => 3,
                };
                let field = release_gram_field(width);
                let mut candidates = gram_tokens(query, width)
                    .into_iter()
                    .filter_map(|gram| self.exact_posting(field, &gram).postings.into_iter().next())
                    .collect::<Vec<_>>();
                if candidates.is_empty() {
                    return PostingSources {
                        postings: candidates,
                        term_keys_scanned: 0,
                    };
                }
                candidates.sort_by_key(|posting| posting.cardinality(self.order.len()));
                candidates.truncate(1);
                PostingSources {
                    postings: candidates,
                    term_keys_scanned: 0,
                }
            }
            SearchTier::AliasTerms => {
                self.rare_token_posting(ReleasePostingField::AliasToken, query)
            }
            SearchTier::KeywordTerms => {
                self.rare_token_posting(ReleasePostingField::Keyword, query)
            }
            SearchTier::AdvisoryTerms => {
                self.rare_token_posting(ReleasePostingField::Advisory, query)
            }
            SearchTier::DescriptionTerms => {
                self.rare_token_posting(ReleasePostingField::Description, query)
            }
            SearchTier::GenericTerms => {
                self.rare_token_posting(ReleasePostingField::Generic, query)
            }
            SearchTier::FuzzyName => {
                let postings = if fuzzy_term_matches(&name, query) {
                    vec![PostingSource::All]
                } else {
                    Vec::new()
                };
                PostingSources {
                    postings,
                    term_keys_scanned: 0,
                }
            }
            SearchTier::FuzzyAlias => self.fuzzy_alias_postings(query),
        };
        postings
    }

    fn exact_posting(&self, field: ReleasePostingField, value: &str) -> PostingSources<'_> {
        let postings = self
            .postings
            .get(&field)
            .and_then(|values| values.get(value))
            .filter(|posting| !posting.is_empty())
            .map_or_else(Vec::new, |posting| vec![PostingSource::Term(posting)]);
        PostingSources {
            postings,
            term_keys_scanned: 0,
        }
    }

    fn prefix_postings(&self, field: ReleasePostingField, prefix: &str) -> PostingSources<'_> {
        let Some(values) = self.postings.get(&field) else {
            return PostingSources {
                postings: Vec::new(),
                term_keys_scanned: 0,
            };
        };
        let mut postings = Vec::new();
        let mut term_keys_scanned = 0_usize;
        for (value, posting) in values.range(prefix.to_owned()..) {
            term_keys_scanned = term_keys_scanned.saturating_add(1);
            if !value.starts_with(prefix) {
                break;
            }
            postings.push(PostingSource::Term(posting));
        }
        PostingSources {
            postings,
            term_keys_scanned,
        }
    }

    fn rare_token_posting(&self, field: ReleasePostingField, query: &str) -> PostingSources<'_> {
        let tokens = query
            .split(|character: char| !character.is_alphanumeric())
            .filter(|token| !token.is_empty())
            .collect::<BTreeSet<_>>();
        if tokens.is_empty() || tokens.iter().any(|token| token.chars().count() > 40) {
            return PostingSources {
                postings: Vec::new(),
                term_keys_scanned: 0,
            };
        }
        let Some(values) = self.postings.get(&field) else {
            return PostingSources {
                postings: Vec::new(),
                term_keys_scanned: 0,
            };
        };
        let mut postings = Vec::with_capacity(tokens.len());
        for token in tokens {
            let Some(posting) = values.get(token) else {
                return PostingSources {
                    postings: Vec::new(),
                    term_keys_scanned: 0,
                };
            };
            postings.push(PostingSource::Term(posting));
        }
        postings.sort_by_key(|posting| match posting {
            PostingSource::All => usize::MAX,
            PostingSource::Term(posting) => posting.len(),
        });
        postings.truncate(1);
        PostingSources {
            postings,
            term_keys_scanned: 0,
        }
    }

    fn fuzzy_alias_postings(&self, query: &str) -> PostingSources<'_> {
        let Some(values) = self.postings.get(&ReleasePostingField::AliasValue) else {
            return PostingSources {
                postings: Vec::new(),
                term_keys_scanned: 0,
            };
        };
        let mut postings = Vec::new();
        let mut term_keys_scanned = 0_usize;
        for (alias, posting) in values {
            term_keys_scanned = term_keys_scanned.saturating_add(1);
            if fuzzy_term_matches(alias, query) {
                postings.push(PostingSource::Term(posting));
            }
        }
        PostingSources {
            postings,
            term_keys_scanned,
        }
    }

    fn collect_posting_page(
        &self,
        postings: &[PostingSource<'_>],
        key: &LineageKey,
        releases: &BTreeMap<String, LineageReleaseDocument>,
        query: &str,
        tier: Option<SearchTier>,
        limit: usize,
    ) -> (usize, Vec<String>) {
        if limit == 0 || postings.is_empty() {
            return (0, Vec::new());
        }
        let mut iterators = postings
            .iter()
            .map(|posting| match posting {
                PostingSource::All => PostingIterator::All(self.order.values().rev()),
                PostingSource::Term(OrdinalPosting::Empty) => PostingIterator::One(None),
                PostingSource::Term(OrdinalPosting::One(ordinal)) => {
                    PostingIterator::One(Some(*ordinal))
                }
                PostingSource::Term(OrdinalPosting::Many(ordinals)) => {
                    PostingIterator::Many(ordinals.iter().rev())
                }
            })
            .collect::<Vec<_>>();
        let mut frontier = BinaryHeap::with_capacity(iterators.len());
        let mut examined = 0_usize;
        for (iterator_index, iterator) in iterators.iter_mut().enumerate() {
            if let Some(ordinal) = iterator.next() {
                examined = examined.saturating_add(1);
                frontier.push((entry_rank(&self.entries, ordinal), ordinal, iterator_index));
            }
        }
        let mut selected = Vec::with_capacity(limit);
        let mut last_rank = None;
        while let Some((rank, ordinal, iterator_index)) = frontier.pop() {
            if last_rank != Some(rank) {
                last_rank = Some(rank);
                let Some(entry) = self.entries[ordinal as usize].as_ref() else {
                    continue;
                };
                if let (Some(tier), Some(document)) = (tier, releases.get(entry.identity.as_ref()))
                    && release_search_evidence(key, document, query) == Some(tier.evidence())
                {
                    selected.push(entry.identity.to_string());
                    if selected.len() == limit {
                        break;
                    }
                } else if tier.is_none() {
                    selected.push(entry.identity.to_string());
                    if selected.len() == limit {
                        break;
                    }
                }
            }
            if let Some(ordinal) = iterators[iterator_index].next() {
                examined = examined.saturating_add(1);
                frontier.push((entry_rank(&self.entries, ordinal), ordinal, iterator_index));
            }
        }
        (examined, selected)
    }
}

fn release_posting_values(document: &LineageReleaseDocument) -> Vec<(ReleasePostingField, String)> {
    let coordinate = normalize(&document.coordinate);
    let mut memberships = vec![(ReleasePostingField::Coordinate, coordinate.clone())];
    memberships.extend(
        document
            .aliases
            .iter()
            .cloned()
            .map(|value| (ReleasePostingField::AliasValue, value)),
    );
    for (field, tokens) in [
        (ReleasePostingField::AliasToken, &document.alias_tokens),
        (ReleasePostingField::Keyword, &document.keywords),
        (ReleasePostingField::Description, &document.descriptions),
        (ReleasePostingField::Advisory, &document.advisories),
        (ReleasePostingField::Generic, &document.generic),
    ] {
        memberships.extend(tokens.iter().cloned().map(|value| (field, value)));
    }
    for (width, field) in [
        (1, ReleasePostingField::CoordinateGram1),
        (2, ReleasePostingField::CoordinateGram2),
        (3, ReleasePostingField::CoordinateGram3),
    ] {
        memberships.extend(
            gram_tokens(&coordinate, width)
                .into_iter()
                .map(|value| (field, value)),
        );
    }
    memberships.sort();
    memberships.dedup();
    memberships
}

fn release_version_order_key(
    ecosystem: RegistryEcosystem,
    version: &str,
) -> ReleaseVersionOrderKey {
    normalized_version_key(ecosystem, version).map_or_else(
        || ReleaseVersionOrderKey::Unorderable(Arc::from(normalize(version))),
        ReleaseVersionOrderKey::Orderable,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_engine::registry::{DiscoverySourceIdentity, RegistryEndpoint};

    fn lineage_key() -> LineageKey {
        let endpoint = RegistryEndpoint::new(RegistryEcosystem::Cargo, "https://index.crates.io")
            .expect("admitted Cargo registry");
        LineageKey {
            source: super::super::LineageSearchSource::Discovery(
                super::super::DiscoverySearchSource::Registry(
                    DiscoverySourceIdentity::from_endpoint(&endpoint),
                ),
            ),
            ecosystem: RegistryEcosystem::Cargo,
            lineage: "widget".to_owned(),
        }
    }

    fn release_document(identity: &str, version: &str, aliases: &[&str]) -> LineageReleaseDocument {
        let aliases = aliases
            .iter()
            .map(|alias| normalize(alias))
            .collect::<BTreeSet<_>>();
        let alias_tokens = aliases
            .iter()
            .flat_map(|alias| {
                alias
                    .split(|character: char| !character.is_alphanumeric())
                    .filter(|token| !token.is_empty())
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .collect();
        LineageReleaseDocument {
            coordinate: format!("pkg:cargo/widget@{version}"),
            version: version.to_owned(),
            order_sort_key: identity.to_owned(),
            aliases,
            alias_tokens,
            keywords: BTreeSet::new(),
            descriptions: BTreeSet::new(),
            advisories: BTreeSet::new(),
            generic: BTreeSet::new(),
            fingerprint: [0; 32],
        }
    }

    fn insert_release(
        projection: &mut VersionedReleaseProjection,
        releases: &mut BTreeMap<String, LineageReleaseDocument>,
        identity: &str,
        document: LineageReleaseDocument,
    ) {
        projection.insert(identity, &document, RegistryEcosystem::Cargo);
        releases.insert(identity.to_owned(), document);
    }

    fn ordered_identities(
        projection: &VersionedReleaseProjection,
        key: &LineageKey,
        releases: &BTreeMap<String, LineageReleaseDocument>,
    ) -> Vec<String> {
        projection
            .top_matches(key, releases, "", releases.len().min(MAX_SEARCH_PAGE_SIZE))
            .identities
    }

    fn assert_postings_are_current(
        projection: &VersionedReleaseProjection,
        releases: &BTreeMap<String, LineageReleaseDocument>,
    ) {
        assert_eq!(
            projection.order.len(),
            projection.ordinals_by_identity.len()
        );
        let ordered_ranks = projection
            .order
            .values()
            .map(|ordinal| entry_rank(&projection.entries, *ordinal))
            .collect::<Vec<_>>();
        assert!(ordered_ranks.windows(2).all(|pair| pair[0] < pair[1]));
        for (identity, ordinal) in &projection.ordinals_by_identity {
            let entry = projection.entries[*ordinal as usize]
                .as_ref()
                .expect("identity ordinal is active");
            assert_eq!(entry.identity.as_ref(), identity.as_ref());
            assert!(
                projection
                    .order
                    .values()
                    .any(|candidate| candidate == ordinal)
            );
        }
        for (field, values) in &projection.postings {
            for (value, posting) in values {
                let ordinals = match posting {
                    OrdinalPosting::Empty => Vec::new(),
                    OrdinalPosting::One(ordinal) => vec![*ordinal],
                    OrdinalPosting::Many(ordinals) => ordinals.clone(),
                };
                let mut previous_rank = None;
                for ordinal in ordinals {
                    let entry = projection.entries[ordinal as usize]
                        .as_ref()
                        .expect("posting ordinal resolves to one current identity");
                    assert!(previous_rank.is_none_or(|rank| rank < entry.rank));
                    previous_rank = Some(entry.rank);
                    let document = releases
                        .get(entry.identity.as_ref())
                        .expect("posting identity resolves to its source row");
                    assert!(release_posting_values(document).contains(&(*field, value.clone())));
                }
            }
        }
    }

    fn resident_set_kib() -> Option<u64> {
        let pid = std::process::id().to_string();
        let output = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", pid.as_str()])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        String::from_utf8(output.stdout).ok()?.trim().parse().ok()
    }

    #[test]
    fn rank_rebalance_and_version_update_rebuild_identically_after_cold_reopen() {
        let key = lineage_key();
        let mut versions = vec!["1.0.0".to_owned(), "1.0.1".to_owned()];
        versions.extend((0..70).map(|ordinal| format!("1.0.1-alpha.{ordinal}")));
        versions.extend(["0.9.0".to_owned(), "2.0.0".to_owned()]);

        let mut forward = VersionedReleaseProjection::default();
        let mut forward_releases = BTreeMap::new();
        for version in &versions {
            let identity = format!("release:{version}");
            let aliases = if version == "1.0.1-alpha.69" {
                &["shared", "olduniquealias"][..]
            } else {
                &["shared"][..]
            };
            insert_release(
                &mut forward,
                &mut forward_releases,
                &identity,
                release_document(&identity, version, aliases),
            );
        }
        assert!(
            forward.rebalance_count > 0,
            "insertion exhausted a rank gap"
        );

        let mut reverse = VersionedReleaseProjection::default();
        let mut reverse_releases = BTreeMap::new();
        for version in versions.iter().rev() {
            let identity = format!("release:{version}");
            let aliases = if version == "1.0.1-alpha.69" {
                &["shared", "olduniquealias"][..]
            } else {
                &["shared"][..]
            };
            insert_release(
                &mut reverse,
                &mut reverse_releases,
                &identity,
                release_document(&identity, version, aliases),
            );
        }
        let expected = ordered_identities(&forward, &key, &forward_releases);
        assert_eq!(
            expected,
            ordered_identities(&reverse, &key, &reverse_releases),
            "ephemeral rank labels must not affect cold result order"
        );
        assert_eq!(expected.first().map(String::as_str), Some("release:2.0.0"));

        let identity = "release:1.0.1-alpha.69";
        let previous = forward_releases
            .remove(identity)
            .expect("release is indexed");
        assert!(
            !forward
                .exact_posting(ReleasePostingField::AliasValue, "olduniquealias")
                .postings
                .is_empty(),
            "the old alias exists before the update"
        );
        forward.remove(identity, &previous, RegistryEcosystem::Cargo);
        let updated = release_document(identity, "1.0.1-alpha.71", &["shared", "newuniquealias"]);
        forward.insert(identity, &updated, RegistryEcosystem::Cargo);
        forward_releases.insert(identity.to_owned(), updated);
        let new_alias_page = forward.top_matches(&key, &forward_releases, "newuniquealias", 16);
        assert_eq!(new_alias_page.identities, vec![identity.to_owned()]);
        let ordered = ordered_identities(&forward, &key, &forward_releases);
        let shared_alias_page = forward.top_matches(&key, &forward_releases, "shared", 16);
        assert_eq!(shared_alias_page.identities, ordered[..16].to_vec());
        assert_eq!(shared_alias_page.posting_entries_examined, 17);
        assert!(
            forward
                .exact_posting(ReleasePostingField::AliasValue, "olduniquealias")
                .postings
                .is_empty(),
            "replaced alias membership must not survive the update"
        );
        assert_postings_are_current(&forward, &forward_releases);

        // Rebuild from the persisted post-update rows in a deliberately
        // different order, as a cold restart would.
        let mut cold = VersionedReleaseProjection::default();
        let mut cold_releases = BTreeMap::new();
        for (identity, document) in forward_releases.iter().rev() {
            insert_release(&mut cold, &mut cold_releases, identity, document.clone());
        }
        assert!(cold.rebalance_count > 0);
        assert_eq!(
            ordered_identities(&forward, &key, &forward_releases),
            ordered_identities(&cold, &key, &cold_releases)
        );
        assert_eq!(
            cold.top_matches(&key, &cold_releases, "newuniquealias", 16)
                .identities,
            vec![identity.to_owned()]
        );
        assert_postings_are_current(&cold, &cold_releases);

        let rebalances_before_extreme_inserts = cold.rebalance_count;
        for (identity, version) in [
            ("release:added-oldest-0.7.0", "0.7.0"),
            ("release:added-old-0.8.0", "0.8.0"),
            ("release:added-newest-3.0.0", "3.0.0"),
        ] {
            insert_release(
                &mut cold,
                &mut cold_releases,
                identity,
                release_document(identity, version, &["shared"]),
            );
        }
        for ordinal in 72..212 {
            let identity = format!("release:after-cold-reopen:{ordinal}");
            let version = format!("1.0.1-alpha.{ordinal}");
            insert_release(
                &mut cold,
                &mut cold_releases,
                &identity,
                release_document(&identity, &version, &["shared"]),
            );
        }
        assert!(
            cold.rebalance_count >= rebalances_before_extreme_inserts + 3,
            "end inserts and repeated between-neighbor inserts rebalance after cold reopen"
        );
        assert_postings_are_current(&cold, &cold_releases);

        let mut second_cold = VersionedReleaseProjection::default();
        let mut second_cold_releases = BTreeMap::new();
        for (identity, document) in cold_releases.iter().rev() {
            insert_release(
                &mut second_cold,
                &mut second_cold_releases,
                identity,
                document.clone(),
            );
        }
        assert_eq!(
            ordered_identities(&cold, &key, &cold_releases),
            ordered_identities(&second_cold, &key, &second_cold_releases),
            "a second cold rebuild produces the same order despite different rank labels"
        );
        let ordered = ordered_identities(&cold, &key, &cold_releases);
        let shared_page = cold.top_matches(&key, &cold_releases, "shared", 16);
        assert_eq!(shared_page.identities, ordered[..16].to_vec());
        assert_eq!(shared_page.posting_entries_examined, 17);
        assert_postings_are_current(&second_cold, &second_cold_releases);
    }

    #[test]
    fn unorderable_forge_style_tags_use_a_bounded_lexical_tail() {
        let key = lineage_key();
        let mut projection = VersionedReleaseProjection::default();
        let mut releases = BTreeMap::new();
        for version in ["release-z", "release-a", "1.0.0"] {
            let identity = format!("release:{version}");
            insert_release(
                &mut projection,
                &mut releases,
                &identity,
                release_document(&identity, version, &[]),
            );
        }
        assert_eq!(
            ordered_identities(&projection, &key, &releases),
            vec![
                "release:1.0.0".to_owned(),
                "release:release-z".to_owned(),
                "release:release-a".to_owned(),
            ]
        );
        let page = projection.top_matches(&key, &releases, "", 2);
        assert!(page.more);
        assert_eq!(page.posting_entries_examined, 3);
    }

    #[test]
    fn fuzzy_alias_dictionary_walk_is_counted_separately_from_release_visits() {
        const ALIASES: usize = 16_000;
        let key = lineage_key();
        let mut projection = VersionedReleaseProjection::default();
        let mut releases = BTreeMap::new();
        let aliases = (0..ALIASES)
            .map(|ordinal| format!("zzalias{ordinal:05}"))
            .collect::<BTreeSet<_>>();
        let document = release_document(
            "release:1.0.0",
            "1.0.0",
            &aliases.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        insert_release(&mut projection, &mut releases, "release:1.0.0", document);

        let prefix = projection.prefix_postings(ReleasePostingField::AliasValue, "unrelated");
        assert!(prefix.postings.is_empty());
        assert_eq!(
            prefix.term_keys_scanned, 1,
            "one boundary key was inspected"
        );
        let fuzzy = projection.fuzzy_alias_postings("unrelatedquery");
        assert!(fuzzy.postings.is_empty());
        assert_eq!(fuzzy.term_keys_scanned, ALIASES);

        let page = projection.top_matches(&key, &releases, "unrelatedquery", 16);
        assert!(page.identities.is_empty());
        assert_eq!(page.posting_entries_examined, 0);
        assert!(page.term_keys_scanned >= ALIASES);
        eprintln!(
            "release projection alias diagnostic: releases={} unique_aliases={} estimated_logical_payload_bytes={} term_keys_scanned={} posting_entries_examined={}",
            releases.len(),
            aliases.len(),
            projection.estimated_logical_payload_bytes(),
            page.term_keys_scanned,
            page.posting_entries_examined,
        );
    }

    #[test]
    #[ignore = "manual projection memory and 320-release/16k-alias latency diagnostic"]
    fn projection_scale_320_releases_and_16k_aliases() {
        const RELEASES: usize = 320;
        const ALIASES_PER_RELEASE: usize = 50;
        const SAMPLES: usize = 51;
        let rss_before = resident_set_kib();
        let key = lineage_key();
        let mut releases = BTreeMap::new();
        let documents_build_started = std::time::Instant::now();
        for ordinal in 0..RELEASES {
            let identity = format!("release:{ordinal:04}");
            let version = format!("1.0.{ordinal}");
            let aliases = (0..ALIASES_PER_RELEASE)
                .map(|alias| format!("doc{ordinal:04}-alias{alias:02}"))
                .collect::<Vec<_>>();
            let aliases = aliases.iter().map(String::as_str).collect::<Vec<_>>();
            let document = release_document(&identity, &version, &aliases);
            releases.insert(identity, document);
        }
        let documents_build_elapsed = documents_build_started.elapsed();
        let rss_after_source_documents = resident_set_kib();

        // This is the pre-projection control: the existing identity-to-release
        // map remains resident while the new per-lineage posting projection is
        // rebuilt from the same 320 release rows.
        let mut projection = VersionedReleaseProjection::default();
        let projection_build_started = std::time::Instant::now();
        for (identity, document) in &releases {
            projection.insert(identity, document, RegistryEcosystem::Cargo);
        }
        let projection_build_elapsed = projection_build_started.elapsed();
        let rss_after_projection = resident_set_kib();
        let page = projection.top_matches(&key, &releases, "widget", 16);
        assert_eq!(page.identities.len(), 16);
        assert!(page.more);
        assert_eq!(page.posting_entries_examined, 17);
        assert_eq!(page.term_keys_scanned, 0);

        let mut prefix_samples = Vec::with_capacity(SAMPLES);
        let mut prefix_page = None;
        for _ in 0..SAMPLES {
            let started = std::time::Instant::now();
            let result = projection.top_matches(&key, &releases, "doc", 16);
            prefix_samples.push(started.elapsed());
            prefix_page = Some(result);
        }
        prefix_samples.sort_unstable();
        let prefix_page = prefix_page.expect("sampled one broad prefix page");
        assert_eq!(prefix_page.identities.len(), 16);
        assert!(prefix_page.more);
        assert_eq!(
            prefix_page.term_keys_scanned,
            RELEASES * ALIASES_PER_RELEASE + 1,
            "alias keys plus the coordinate-prefix boundary key were visited"
        );
        assert!(prefix_page.posting_entries_examined >= RELEASES * ALIASES_PER_RELEASE);

        let mut fuzzy_samples = Vec::with_capacity(SAMPLES);
        let mut fuzzy_page = None;
        for _ in 0..SAMPLES {
            let started = std::time::Instant::now();
            let result = projection.top_matches(&key, &releases, "doc0000-alias00", 16);
            fuzzy_samples.push(started.elapsed());
            fuzzy_page = Some(result);
        }
        fuzzy_samples.sort_unstable();
        let fuzzy_page = fuzzy_page.expect("sampled one typo-like alias query");
        assert!(fuzzy_page.identities.len() <= 16);
        assert!(fuzzy_page.term_keys_scanned >= RELEASES * ALIASES_PER_RELEASE);
        let rss_after_queries = resident_set_kib();
        let prefix_p50_us = prefix_samples[SAMPLES * 50 / 100].as_micros();
        let prefix_p95_us = prefix_samples[SAMPLES * 95 / 100].as_micros();
        let p50_us = fuzzy_samples[SAMPLES * 50 / 100].as_micros();
        let p95_us = fuzzy_samples[SAMPLES * 95 / 100].as_micros();
        eprintln!(
            "release projection scale: releases={RELEASES} unique_aliases={} source_documents_build_ms={} projection_rebuild_ms={} estimate_logical_bytes={} rss_before_kib={rss_before:?} rss_after_source_documents_kib={rss_after_source_documents:?} rss_after_projection_kib={rss_after_projection:?} rss_after_queries_kib={rss_after_queries:?} source_documents_rss_delta_kib={:?} projection_incremental_rss_delta_kib={:?} broad_prefix_p50_us={prefix_p50_us} broad_prefix_p95_us={prefix_p95_us} broad_prefix_term_keys_scanned={} broad_prefix_posting_entries_examined={} fuzzy_query_p50_us={p50_us} fuzzy_query_p95_us={p95_us} fuzzy_term_keys_scanned={} fuzzy_posting_entries_examined={} bounded_name_posting_entries_examined={}",
            RELEASES * ALIASES_PER_RELEASE,
            documents_build_elapsed.as_millis(),
            projection_build_elapsed.as_millis(),
            projection.estimated_logical_payload_bytes(),
            rss_before
                .zip(rss_after_source_documents)
                .map(|(before, after)| after.saturating_sub(before)),
            rss_after_source_documents
                .zip(rss_after_projection)
                .map(|(before, after)| after.saturating_sub(before)),
            prefix_page.term_keys_scanned,
            prefix_page.posting_entries_examined,
            fuzzy_page.term_keys_scanned,
            fuzzy_page.posting_entries_examined,
            page.posting_entries_examined,
        );
    }

    #[test]
    #[ignore = "manual 10k-release single-lineage rebuild and bounded top-k diagnostic"]
    fn projection_scale_10k_releases_one_lineage() {
        const RELEASES: usize = 10_000;
        const SAMPLES: usize = 101;
        let rss_before = resident_set_kib();
        let key = lineage_key();
        let mut releases = BTreeMap::new();
        let documents_build_started = std::time::Instant::now();
        for ordinal in 0..RELEASES {
            let identity = format!("release:{ordinal:05}");
            let version = format!("1.0.{ordinal}");
            releases.insert(identity.clone(), release_document(&identity, &version, &[]));
        }
        let documents_build_elapsed = documents_build_started.elapsed();
        let rss_after_source_documents = resident_set_kib();
        let mut projection = VersionedReleaseProjection::default();
        let projection_build_started = std::time::Instant::now();
        for (identity, document) in &releases {
            projection.insert(identity, document, RegistryEcosystem::Cargo);
        }
        let projection_build_elapsed = projection_build_started.elapsed();
        let rss_after_projection = resident_set_kib();
        let mut samples = Vec::with_capacity(SAMPLES);
        let mut last_page = None;
        for _ in 0..SAMPLES {
            let started = std::time::Instant::now();
            let page = projection.top_matches(&key, &releases, "widget", 16);
            samples.push(started.elapsed());
            last_page = Some(page);
        }
        samples.sort_unstable();
        let page = last_page.expect("sampled one bounded page");
        assert_eq!(page.identities.len(), 16);
        assert!(page.more);
        assert_eq!(page.posting_entries_examined, 17);
        assert_eq!(page.term_keys_scanned, 0);
        let p50_us = samples[SAMPLES * 50 / 100].as_micros();
        let p95_us = samples[SAMPLES * 95 / 100].as_micros();
        eprintln!(
            "release projection scale: releases={RELEASES} unique_aliases=0 source_documents_build_ms={} projection_rebuild_ms={} estimate_logical_bytes={} rss_before_kib={rss_before:?} rss_after_source_documents_kib={rss_after_source_documents:?} rss_after_projection_kib={rss_after_projection:?} source_documents_rss_delta_kib={:?} projection_incremental_rss_delta_kib={:?} name_page_p50_us={p50_us} name_page_p95_us={p95_us} term_keys_scanned={} posting_entries_examined={}",
            documents_build_elapsed.as_millis(),
            projection_build_elapsed.as_millis(),
            projection.estimated_logical_payload_bytes(),
            rss_before
                .zip(rss_after_source_documents)
                .map(|(before, after)| after.saturating_sub(before)),
            rss_after_source_documents
                .zip(rss_after_projection)
                .map(|(before, after)| after.saturating_sub(before)),
            page.term_keys_scanned,
            page.posting_entries_examined,
        );
    }
}

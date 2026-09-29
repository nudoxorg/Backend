use super::super::product_state::normalized_version_key;
use super::{
    LineageKey, LineageReleaseDocument, MAX_SEARCH_PAGE_SIZE, SEARCH_TIERS, SearchTier,
    fuzzy_term_matches, gram_tokens, lineage_search_name, normalize, release_search_evidence,
};
use backend_engine::registry::RegistryEcosystem;
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};
use std::ops::Bound;

const VERSION_RANK_STRIDE: u128 = 1_u128 << 64;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum ReleaseVersionOrderKey {
    // Typed releases sort after tags that the ecosystem grammar cannot prove.
    // Since pages read ranks in reverse, this presents typed versions first
    // and leaves unknown tags in deterministic lexical order at the end.
    Unorderable(String),
    Orderable(backend_engine::advisory::NormalizedVersion),
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct VersionedReleaseKey {
    version: ReleaseVersionOrderKey,
    // Descending stable key at equal versions gives ascending display order
    // when the posting rank is read from newest to oldest.
    stable_sort_key: Reverse<String>,
    identity: String,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum ReleasePostingField {
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
pub(super) struct VersionedReleaseProjection {
    order: BTreeMap<VersionedReleaseKey, String>,
    ranks_by_identity: BTreeMap<String, u128>,
    identities_by_rank: BTreeMap<u128, String>,
    all_ranks: BTreeSet<u128>,
    postings: BTreeMap<ReleasePostingField, BTreeMap<String, BTreeSet<u128>>>,
    memberships: BTreeMap<String, Vec<(ReleasePostingField, String)>>,
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
    postings: Vec<&'a BTreeSet<u128>>,
    term_keys_scanned: usize,
}

impl VersionedReleaseProjection {
    pub(super) fn estimated_logical_payload_bytes(&self) -> usize {
        use std::mem::size_of;

        let ordered = self
            .order
            .iter()
            .map(|(key, identity)| {
                size_of::<VersionedReleaseKey>()
                    .saturating_add(match &key.version {
                        ReleaseVersionOrderKey::Orderable(version) => version
                            .canonical
                            .len()
                            .saturating_add(format!("{version:?}").len()),
                        ReleaseVersionOrderKey::Unorderable(version) => version.len(),
                    })
                    .saturating_add(key.stable_sort_key.0.len())
                    .saturating_add(key.identity.len())
                    .saturating_add(size_of::<String>())
                    .saturating_add(identity.len())
            })
            .sum::<usize>();
        let identity_ranks = self
            .ranks_by_identity
            .keys()
            .map(|identity| size_of::<(String, u128)>().saturating_add(identity.len()))
            .sum::<usize>();
        let reverse_ranks = self
            .identities_by_rank
            .values()
            .map(|identity| size_of::<(u128, String)>().saturating_add(identity.len()))
            .sum::<usize>();
        let posting_bytes = self
            .postings
            .values()
            .flat_map(|values| values.iter())
            .map(|(value, ranks)| {
                size_of::<(String, BTreeSet<u128>)>()
                    .saturating_add(value.len())
                    .saturating_add(ranks.len().saturating_mul(size_of::<u128>()))
            })
            .sum::<usize>();
        let membership_bytes = self
            .memberships
            .iter()
            .map(|(identity, memberships)| {
                size_of::<(String, Vec<(ReleasePostingField, String)>)>()
                    .saturating_add(identity.len())
                    .saturating_add(
                        memberships
                            .iter()
                            .map(|(_, value)| {
                                size_of::<(ReleasePostingField, String)>()
                                    .saturating_add(value.len())
                            })
                            .sum::<usize>(),
                    )
            })
            .sum::<usize>();
        ordered
            .saturating_add(identity_ranks)
            .saturating_add(reverse_ranks)
            .saturating_add(self.all_ranks.len().saturating_mul(size_of::<u128>()))
            .saturating_add(posting_bytes)
            .saturating_add(membership_bytes)
    }

    pub(super) fn insert(
        &mut self,
        identity: &str,
        document: &LineageReleaseDocument,
        ecosystem: RegistryEcosystem,
    ) {
        let order_key = VersionedReleaseKey {
            version: release_version_order_key(ecosystem, &document.version),
            stable_sort_key: Reverse(document.order_sort_key.clone()),
            identity: identity.to_owned(),
        };
        self.order.insert(order_key.clone(), identity.to_owned());
        let rank = self.rank_between_neighbors(&order_key);
        let rank = if let Some(rank) = rank {
            rank
        } else {
            self.rebalance_ranks();
            *self
                .ranks_by_identity
                .get(identity)
                .expect("rebalance includes the inserted release")
        };
        self.ranks_by_identity.insert(identity.to_owned(), rank);
        self.identities_by_rank.insert(rank, identity.to_owned());
        self.all_ranks.insert(rank);

        let memberships = release_posting_values(document);
        for (field, value) in &memberships {
            self.postings
                .entry(*field)
                .or_default()
                .entry(value.clone())
                .or_default()
                .insert(rank);
        }
        self.memberships.insert(identity.to_owned(), memberships);
    }

    pub(super) fn remove(
        &mut self,
        identity: &str,
        document: &LineageReleaseDocument,
        ecosystem: RegistryEcosystem,
    ) {
        let Some(rank) = self.ranks_by_identity.remove(identity) else {
            return;
        };
        self.order.remove(&VersionedReleaseKey {
            version: release_version_order_key(ecosystem, &document.version),
            stable_sort_key: Reverse(document.order_sort_key.clone()),
            identity: identity.to_owned(),
        });
        self.identities_by_rank.remove(&rank);
        self.all_ranks.remove(&rank);
        if let Some(memberships) = self.memberships.remove(identity) {
            for (field, value) in memberships {
                let remove_value = if let Some(values) = self.postings.get_mut(&field) {
                    let remove_posting = if let Some(ranks) = values.get_mut(&value) {
                        ranks.remove(&rank);
                        ranks.is_empty()
                    } else {
                        false
                    };
                    if remove_posting {
                        values.remove(&value);
                    }
                    values.is_empty()
                } else {
                    false
                };
                if remove_value {
                    self.postings.remove(&field);
                }
            }
        }
    }

    fn rank_between_neighbors(&self, key: &VersionedReleaseKey) -> Option<u128> {
        let lower = self
            .order
            .range(..key.clone())
            .next_back()
            .and_then(|(_, identity)| self.ranks_by_identity.get(identity))
            .copied();
        let upper = self
            .order
            .range((Bound::Excluded(key.clone()), Bound::Unbounded))
            .next()
            .and_then(|(_, identity)| self.ranks_by_identity.get(identity))
            .copied();
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
        self.ranks_by_identity.clear();
        self.identities_by_rank.clear();
        self.all_ranks.clear();
        for (offset, identity) in self.order.values().enumerate() {
            let rank = u128::try_from(offset)
                .expect("a release lineage cannot contain more than u128::MAX entries")
                .saturating_add(1)
                .saturating_mul(VERSION_RANK_STRIDE);
            self.ranks_by_identity.insert(identity.clone(), rank);
            self.identities_by_rank.insert(rank, identity.clone());
            self.all_ranks.insert(rank);
        }
        for values in self.postings.values_mut() {
            for ranks in values.values_mut() {
                ranks.clear();
            }
        }
        for (identity, memberships) in &self.memberships {
            let Some(rank) = self.ranks_by_identity.get(identity).copied() else {
                continue;
            };
            for (field, value) in memberships {
                self.postings
                    .entry(*field)
                    .or_default()
                    .entry(value.clone())
                    .or_default()
                    .insert(rank);
            }
        }
    }

    pub(super) fn top_matches(
        &self,
        key: &LineageKey,
        releases: &BTreeMap<String, LineageReleaseDocument>,
        query: &str,
        limit: usize,
    ) -> ReleasePostingPage {
        let limit = limit.min(MAX_SEARCH_PAGE_SIZE);
        if limit == 0 || self.all_ranks.is_empty() {
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
                &[&self.all_ranks],
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
                    vec![&self.all_ranks]
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
                        postings: vec![&self.all_ranks],
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
                candidates.sort_by_key(|posting| posting.len());
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
                    vec![&self.all_ranks]
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
            .map_or_else(Vec::new, |posting| vec![posting]);
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
            postings.push(posting);
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
            postings.push(posting);
        }
        postings.sort_by_key(|posting| posting.len());
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
                postings.push(posting);
            }
        }
        PostingSources {
            postings,
            term_keys_scanned,
        }
    }

    fn collect_posting_page(
        &self,
        postings: &[&BTreeSet<u128>],
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
            .map(|posting| posting.iter().rev())
            .collect::<Vec<_>>();
        let mut frontier = BinaryHeap::with_capacity(iterators.len());
        let mut examined = 0_usize;
        for (iterator_index, posting) in iterators.iter_mut().enumerate() {
            if let Some(rank) = posting.next() {
                examined = examined.saturating_add(1);
                frontier.push((*rank, iterator_index));
            }
        }
        let mut selected = Vec::with_capacity(limit);
        let mut last_rank = None;
        while let Some((rank, iterator_index)) = frontier.pop() {
            if last_rank != Some(rank) {
                last_rank = Some(rank);
                if let (Some(tier), Some(identity)) = (tier, self.identities_by_rank.get(&rank))
                    && releases.get(identity).is_some_and(|document| {
                        release_search_evidence(key, document, query) == Some(tier.evidence())
                    })
                {
                    selected.push(identity.clone());
                    if selected.len() == limit {
                        break;
                    }
                } else if tier.is_none()
                    && let Some(identity) = self.identities_by_rank.get(&rank)
                {
                    selected.push(identity.clone());
                    if selected.len() == limit {
                        break;
                    }
                }
            }
            if let Some(rank) = iterators[iterator_index].next() {
                examined = examined.saturating_add(1);
                frontier.push((*rank, iterator_index));
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
        || ReleaseVersionOrderKey::Unorderable(normalize(version)),
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

    fn assert_postings_are_current(projection: &VersionedReleaseProjection) {
        assert_eq!(
            projection.all_ranks,
            projection.identities_by_rank.keys().copied().collect()
        );
        for (identity, rank) in &projection.ranks_by_identity {
            assert_eq!(projection.identities_by_rank.get(rank), Some(identity));
        }
        for (field, values) in &projection.postings {
            for (value, ranks) in values {
                for rank in ranks {
                    let identity = projection
                        .identities_by_rank
                        .get(rank)
                        .expect("posting rank resolves to one current identity");
                    assert!(projection.memberships[identity].contains(&(*field, value.clone())));
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
                &["olduniquealias"][..]
            } else {
                &[]
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
                &["olduniquealias"][..]
            } else {
                &[]
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
        let updated = release_document(identity, "1.0.1-alpha.71", &["newuniquealias"]);
        forward.insert(identity, &updated, RegistryEcosystem::Cargo);
        forward_releases.insert(identity.to_owned(), updated);
        let new_alias_page = forward.top_matches(&key, &forward_releases, "newuniquealias", 16);
        assert_eq!(new_alias_page.identities, vec![identity.to_owned()]);
        assert!(
            forward
                .exact_posting(ReleasePostingField::AliasValue, "olduniquealias")
                .postings
                .is_empty(),
            "replaced alias membership must not survive the update"
        );
        assert_postings_are_current(&forward);

        // Rebuild from the persisted post-update rows in a deliberately
        // different order, as a cold restart would.
        let mut cold = VersionedReleaseProjection::default();
        let mut cold_releases = BTreeMap::new();
        for (identity, document) in forward_releases.iter().rev() {
            insert_release(&mut cold, &mut cold_releases, identity, document.clone());
        }
        assert_eq!(
            ordered_identities(&forward, &key, &forward_releases),
            ordered_identities(&cold, &key, &cold_releases)
        );
        assert_eq!(
            cold.top_matches(&key, &cold_releases, "newuniquealias", 16)
                .identities,
            vec![identity.to_owned()]
        );
        assert_postings_are_current(&cold);
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
        let mut projection = VersionedReleaseProjection::default();
        let mut releases = BTreeMap::new();
        let build_started = std::time::Instant::now();
        for ordinal in 0..RELEASES {
            let identity = format!("release:{ordinal:04}");
            let version = format!("1.0.{ordinal}");
            let aliases = (0..ALIASES_PER_RELEASE)
                .map(|alias| format!("doc{ordinal:04}-alias{alias:02}"))
                .collect::<Vec<_>>();
            let aliases = aliases.iter().map(String::as_str).collect::<Vec<_>>();
            let document = release_document(&identity, &version, &aliases);
            insert_release(&mut projection, &mut releases, &identity, document);
        }
        let build_elapsed = build_started.elapsed();
        let rss_after_build = resident_set_kib();
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
            "release projection scale: releases={RELEASES} unique_aliases={} build_ms={} estimate_logical_bytes={} rss_before_kib={rss_before:?} rss_after_build_kib={rss_after_build:?} rss_after_queries_kib={rss_after_queries:?} broad_prefix_p50_us={prefix_p50_us} broad_prefix_p95_us={prefix_p95_us} broad_prefix_term_keys_scanned={} broad_prefix_posting_entries_examined={} fuzzy_query_p50_us={p50_us} fuzzy_query_p95_us={p95_us} fuzzy_term_keys_scanned={} fuzzy_posting_entries_examined={} bounded_name_posting_entries_examined={}",
            RELEASES * ALIASES_PER_RELEASE,
            build_elapsed.as_millis(),
            projection.estimated_logical_payload_bytes(),
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
        let mut projection = VersionedReleaseProjection::default();
        let mut releases = BTreeMap::new();
        let build_started = std::time::Instant::now();
        for ordinal in 0..RELEASES {
            let identity = format!("release:{ordinal:05}");
            let version = format!("1.0.{ordinal}");
            insert_release(
                &mut projection,
                &mut releases,
                &identity,
                release_document(&identity, &version, &[]),
            );
        }
        let build_elapsed = build_started.elapsed();
        let rss_after_build = resident_set_kib();
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
            "release projection scale: releases={RELEASES} unique_aliases=0 build_ms={} estimate_logical_bytes={} rss_before_kib={rss_before:?} rss_after_build_kib={rss_after_build:?} name_page_p50_us={p50_us} name_page_p95_us={p95_us} term_keys_scanned={} posting_entries_examined={}",
            build_elapsed.as_millis(),
            projection.estimated_logical_payload_bytes(),
            page.term_keys_scanned,
            page.posting_entries_examined,
        );
    }
}

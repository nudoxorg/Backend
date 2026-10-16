//! Typed secondary relation schemas and canonical key encoders.

use crate::{Fragment, PackageKey, Row, RowId, SymbolKey};
use backend_flow::MaterializedIndex;
use backend_version::Relation;
use std::collections::BTreeSet;

pub(super) const INDEX_DOMAIN: u8 = 0x31;

/// A relation whose values are unit membership markers.
#[derive(Debug)]
pub(super) struct DocumentIndexRelation;
impl Relation for DocumentIndexRelation {
    const DOMAIN: u8 = INDEX_DOMAIN;
    const TYPE: u16 = 1;
    type Key = RowId;
    type Value = ();

    fn encode_key(value: &Self::Key, out: &mut Vec<u8>) {
        encode_row_id(value, out);
    }

    fn encode_value(_value: &(), _: &mut Vec<u8>) {}
}

/// A package membership relation.
#[derive(Debug)]
pub(super) struct PackageIndexRelation;
impl Relation for PackageIndexRelation {
    const DOMAIN: u8 = INDEX_DOMAIN;
    const TYPE: u16 = 2;
    type Key = RowId;
    type Value = ();

    fn encode_key(value: &Self::Key, out: &mut Vec<u8>) {
        encode_row_id(value, out);
    }

    fn encode_value(_value: &(), _: &mut Vec<u8>) {}
}

/// An index of symbols that have no explicit package membership. It keeps the
/// legacy single-package outline fallback bounded without scanning the primary
/// view relation.
#[derive(Debug)]
pub(super) struct UnscopedIndexRelation;
impl Relation for UnscopedIndexRelation {
    const DOMAIN: u8 = INDEX_DOMAIN;
    const TYPE: u16 = 8;
    type Key = RowId;
    type Value = ();

    fn encode_key(value: &Self::Key, out: &mut Vec<u8>) {
        encode_row_id(value, out);
    }

    fn encode_value(_value: &(), _: &mut Vec<u8>) {}
}

/// A name order relation keyed by normalized name and stable row identity.
#[derive(Debug)]
pub(super) struct NameIndexRelation;
impl Relation for NameIndexRelation {
    const DOMAIN: u8 = INDEX_DOMAIN;
    const TYPE: u16 = 3;
    type Key = NameKey;
    type Value = ();

    fn encode_key(value: &Self::Key, out: &mut Vec<u8>) {
        append_bytes(out, value.normalized.as_bytes());
        encode_row_id(&value.id, out);
    }

    fn encode_value(_value: &(), _: &mut Vec<u8>) {}
}

/// A bounded name gram membership relation. The row identity is part of the
/// key so an edit path never clones a common gram's posting list.
#[derive(Debug)]
pub(super) struct NamePostingRelation;
impl Relation for NamePostingRelation {
    const DOMAIN: u8 = INDEX_DOMAIN;
    const TYPE: u16 = 4;
    type Key = NamePostingKey;
    type Value = ();

    fn encode_key(value: &Self::Key, out: &mut Vec<u8>) {
        append_bytes(out, &value.gram);
        encode_optional_row_id(value.id, out);
    }

    fn encode_value(_value: &(), _: &mut Vec<u8>) {}
}

/// A bounded full-text gram membership relation. Its distinct key type prevents
/// name-only lookup from accidentally using document/signature candidates.
#[derive(Debug)]
pub(super) struct SearchPostingRelation;
impl Relation for SearchPostingRelation {
    const DOMAIN: u8 = INDEX_DOMAIN;
    const TYPE: u16 = 5;
    type Key = SearchPostingKey;
    type Value = ();

    fn encode_key(value: &Self::Key, out: &mut Vec<u8>) {
        append_bytes(out, &value.gram);
        encode_optional_row_id(value.id, out);
    }

    fn encode_value(_value: &(), _: &mut Vec<u8>) {}
}

/// A package membership relation ordered by package and row identity.
#[derive(Debug)]
pub(super) struct PackageSymbolsRelation;
impl Relation for PackageSymbolsRelation {
    const DOMAIN: u8 = INDEX_DOMAIN;
    const TYPE: u16 = 6;
    type Key = PackageRowKey;
    type Value = ();

    fn encode_key(value: &Self::Key, out: &mut Vec<u8>) {
        append_bytes(out, value.package.as_bytes());
        encode_optional_row_id(value.id, out);
    }

    fn encode_value(_value: &(), _: &mut Vec<u8>) {}
}

/// A package/parent membership relation ordered by its full composite key.
#[derive(Debug)]
pub(super) struct ChildrenRelation;
impl Relation for ChildrenRelation {
    const DOMAIN: u8 = INDEX_DOMAIN;
    const TYPE: u16 = 7;
    type Key = ChildRowKey;
    type Value = ();

    fn encode_key(value: &Self::Key, out: &mut Vec<u8>) {
        append_bytes(out, value.package.as_bytes());
        match value.parent {
            Some(parent) => {
                out.push(1);
                append_bytes(out, parent.as_bytes());
            }
            None => out.push(0),
        }
        encode_optional_row_id(value.id, out);
    }

    fn encode_value(_value: &(), _: &mut Vec<u8>) {}
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct NameKey {
    pub(super) normalized: String,
    pub(super) id: RowId,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct NamePostingKey {
    pub(super) gram: Vec<u8>,
    pub(super) id: Option<RowId>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct SearchPostingKey {
    pub(super) gram: Vec<u8>,
    pub(super) id: Option<RowId>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct PackageRowKey {
    pub(super) package: PackageKey,
    pub(super) id: Option<RowId>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct ChildRowKey {
    pub(super) package: PackageKey,
    pub(super) parent: Option<SymbolKey>,
    pub(super) id: Option<RowId>,
}

pub(super) type DocumentTree = MaterializedIndex<DocumentIndexRelation>;
pub(super) type PackageTree = MaterializedIndex<PackageIndexRelation>;
pub(super) type UnscopedTree = MaterializedIndex<UnscopedIndexRelation>;
pub(super) type NameTree = MaterializedIndex<NameIndexRelation>;
pub(super) type NamePostingTree = MaterializedIndex<NamePostingRelation>;
pub(super) type SearchPostingTree = MaterializedIndex<SearchPostingRelation>;
pub(super) type PackageSymbolsTree = MaterializedIndex<PackageSymbolsRelation>;
pub(super) type ChildrenTree = MaterializedIndex<ChildrenRelation>;

/// Builds the bounded-width scalar postings used by substring search.
///
/// Retaining one-, two-, and three-scalar windows prevents short queries from
/// degenerating into a scan of every indexed symbol. Longer queries use a
/// three-scalar posting and verify the complete normalized text before a hit
/// is admitted.
pub(super) fn search_grams(text: &str) -> BTreeSet<Vec<u8>> {
    let normalized = text.to_lowercase();
    let mut grams = BTreeSet::new();
    // Borrow scalar-aligned windows until duplicates have been removed.
    // Repeated compiler prose must not allocate an owned key for every
    // occurrence of the same gram. UTF-8 byte windows would change Unicode
    // membership, so advance each endpoint by one complete scalar instead.
    for (start, _) in normalized.char_indices() {
        let mut end = start;
        for scalar in normalized[start..].chars().take(3) {
            end += scalar.len_utf8();
            grams.insert(&normalized.as_bytes()[start..end]);
        }
    }
    grams.into_iter().map(<[u8]>::to_vec).collect()
}

pub(super) fn append_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    out.extend_from_slice(bytes);
}

pub(super) fn encode_row_id(id: &RowId, out: &mut Vec<u8>) {
    match id {
        RowId::Package(value) => {
            out.push(0);
            append_bytes(out, value.as_bytes());
        }
        RowId::Symbol(value) => {
            out.push(1);
            append_bytes(out, value.as_bytes());
        }
        RowId::Object(value) => {
            out.push(2);
            append_bytes(out, value.as_bytes());
        }
    }
}

pub(super) fn encode_optional_row_id(id: Option<RowId>, out: &mut Vec<u8>) {
    match id {
        Some(id) => {
            out.push(1);
            encode_row_id(&id, out);
        }
        None => out.push(0),
    }
}

/// Returns the lower-cased searchable projection retained by the lexical
/// postings. Building it only for posting candidates keeps query work bounded
/// while allowing document prose, signatures, and link labels to participate
/// in exact verification after a selective gram seek.
pub(super) fn searchable_text(row: &Row) -> String {
    let mut text = String::with_capacity(row.label.len());
    text.push_str(&row.label);
    if let Some(signature) = &row.signature {
        text.push('\n');
        text.push_str(signature);
    }
    for fragment in &row.document {
        match fragment {
            Fragment::Text(value) | Fragment::Code(value) => {
                text.push('\n');
                text.push_str(value);
            }
            Fragment::Link { label, .. } => {
                text.push('\n');
                text.push_str(label);
            }
            Fragment::Break => text.push('\n'),
        }
    }
    text.to_lowercase()
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    fn owned_window_grams(text: &str) -> BTreeSet<Vec<u8>> {
        let chars = text.to_lowercase().chars().collect::<Vec<_>>();
        let mut grams = BTreeSet::new();
        for width in 1..=chars.len().min(3) {
            for start in 0..=chars.len() - width {
                grams.insert(
                    chars[start..start + width]
                        .iter()
                        .collect::<String>()
                        .into_bytes(),
                );
            }
        }
        grams
    }

    #[test]
    fn borrowed_windows_preserve_scalar_membership_and_canonical_key_order() {
        // Includes expanding lowercase, contextual Greek lowercase, combining
        // marks, multi-byte scalars, NUL, and emoji sequences. No byte window
        // or ASCII-only shortcut may replace the established scalar contract.
        for text in [
            "",
            "A",
            "AB",
            "ABC",
            "ABCD",
            "İİ a\u{307}",
            "ΟΣ ΣΟΣ Σσς",
            "Straße STRASSE",
            "é e\u{301} 中",
            "👩\u{200d}💻\0A",
        ] {
            assert_eq!(
                search_grams(text).into_iter().collect::<Vec<_>>(),
                owned_window_grams(text).into_iter().collect::<Vec<_>>(),
                "scalar gram order differs for {text:?}"
            );
        }

        let alphabet = [
            'A', 'a', 'Σ', 'σ', 'ς', 'İ', '\u{307}', 'é', '中', '💻', '\0',
        ];
        for first in alphabet {
            for second in alphabet {
                for third in alphabet {
                    let text = [first, second, third].into_iter().collect::<String>();
                    assert_eq!(
                        search_grams(&text),
                        owned_window_grams(&text),
                        "scalar membership differs for {text:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn borrowed_windows_preserve_canonical_posting_relation_roots() {
        let coverage = backend_version::CoverageWitness::closed_relation(
            backend_version::ClosedRelationScope::from_scope_root(
                backend_version::ScopeRoot::from_u64(0x6772_616d_626f_7272),
            ),
        );
        let frontier =
            backend_flow::Frontier::new(backend_flow::Time::new(backend_flow::Epoch(7), 0));
        let symbol = RowId::Symbol(crate::symbol_key("borrowed-gram-root"));
        for text in ["İ café ΣΟΣ\0👩\u{200d}💻", "repeated prose repeated prose"] {
            let posting = |gram| {
                (
                    SearchPostingKey {
                        gram,
                        id: Some(symbol),
                    },
                    (),
                )
            };
            let previous = MaterializedIndex::<SearchPostingRelation>::from_entries(
                owned_window_grams(text).into_iter().map(posting),
                coverage,
                frontier.clone(),
            )
            .expect("former scalar-window posting relation");
            let borrowed = MaterializedIndex::<SearchPostingRelation>::from_entries(
                search_grams(text).into_iter().map(posting),
                coverage,
                frontier.clone(),
            )
            .expect("borrowed scalar-window posting relation");
            assert_eq!(borrowed.root(), previous.root());
            assert_eq!(borrowed.coverage(), previous.coverage());
            assert_eq!(borrowed.frontier(), previous.frontier());
            assert_eq!(borrowed.len(), previous.len());
        }
    }

    #[test]
    fn repeated_prose_allocates_gram_keys_per_distinct_window() {
        let short = "İ café Δ needle ".repeat(8);
        let long = "İ café Δ needle ".repeat(2048);
        let mut short_grams = None;
        let short_allocations = allocation_counter::measure(|| {
            short_grams = Some(search_grams(&short));
        });
        let mut long_grams = None;
        let long_allocations = allocation_counter::measure(|| {
            long_grams = Some(search_grams(&long));
        });
        let mut owned_grams = None;
        let owned_allocations = allocation_counter::measure(|| {
            owned_grams = Some(owned_window_grams(&long));
        });

        assert_eq!(short_grams, long_grams);
        assert_eq!(long_grams, owned_grams);
        assert!(
            long_allocations.count_total <= short_allocations.count_total + 4,
            "repetition must not allocate per occurrence: short={short_allocations:?}, long={long_allocations:?}"
        );
        assert!(
            owned_allocations.count_total > long_allocations.count_total * 16,
            "the former owned-window path must be a positive allocation control: owned={owned_allocations:?}, borrowed={long_allocations:?}"
        );
        eprintln!(
            "borrowed_gram_allocations short={short_allocations:?} long={long_allocations:?} owned_positive_control={owned_allocations:?}"
        );
    }
}

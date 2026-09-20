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
pub(super) type PackageSymbolsTree = MaterializedIndex<PackageSymbolsRelation>;
pub(super) type ChildrenTree = MaterializedIndex<ChildrenRelation>;

/// Builds the selective three-scalar postings used by full-text search.
/// One- and two-scalar queries use the compact name order directly, avoiding
/// two additional posting families for every indexed scalar.
pub(super) fn trigrams(text: &str) -> BTreeSet<Vec<u8>> {
    let chars = text.to_lowercase().chars().collect::<Vec<_>>();
    if chars.len() < 3 {
        return chars
            .is_empty()
            .then(BTreeSet::new)
            .unwrap_or_else(|| [chars.iter().collect::<String>().into_bytes()].into());
    }
    (0..=chars.len() - 3)
        .map(|start| {
            chars[start..start + 3]
                .iter()
                .collect::<String>()
                .into_bytes()
        })
        .collect()
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

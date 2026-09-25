//! Why a covered reverse edge's posting list was empty for one symbol.
//!
//! The note is about the symbol the edge was resolved against. A package that
//! recorded `Return` for a different symbol does not answer whether *this*
//! symbol's return type linked.

use std::sync::{Arc, Mutex};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::store::package::{PackageView, TypePosition};

/// Why a graph edge returned no neighbors for the symbol that was queried.
///
/// Closed on purpose: every empty covered edge is one of these four, and a
/// new reason has to update the decision order and the rendered tag together.
/// No `#[non_exhaustive]`, and no catch-all variant.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[schemars(extend("type" = "object"))]
pub enum EdgeEmptyReason {
    /// The posting list is empty and none of the coverage gaps below apply.
    NoSuchEdge,
    /// A nominal written at this edge's position spells the queried symbol
    /// and never became a [`nudox_ir::change::StableRef`].
    UnresolvedNominal {
        /// The spelling as written, not a normalized path.
        spelling: String,
    },
    /// `usages` cannot be trusted: occurrences were never recorded, or
    /// occurrence owners or targets were rejected at the producer boundary.
    OccurrencesNotAttached,
    /// `implementors` reads `ImplementedTrait`, and no loaded package records
    /// that position. The relationship lives on `subtypes`.
    RustOnly,
}

impl EdgeEmptyReason {
    /// Stable tag embedded in the rendered coverage signal.
    pub fn tag(&self) -> &'static str {
        match self {
            Self::NoSuchEdge => "no-such-edge",
            Self::UnresolvedNominal { .. } => "unresolved-nominal",
            Self::OccurrencesNotAttached => "occurrences-not-attached",
            Self::RustOnly => "rust-only",
        }
    }

    /// What the caller should try next.
    ///
    /// `RustOnly` is the edge `subtypes`. The other reasons are not a sibling
    /// graph edge. `signatureTypes` is never the answer: it walks a
    /// declaration forward, and a nominal that did not become a `StableRef`
    /// is not on that edge.
    pub fn answers_instead(&self) -> &'static str {
        match self {
            Self::RustOnly => "subtypes",
            Self::UnresolvedNominal { .. } => "index",
            Self::OccurrencesNotAttached => "refs",
            Self::NoSuchEdge => "search",
        }
    }
}

impl std::fmt::Display for EdgeEmptyReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoSuchEdge => write!(
                f,
                "{}: this symbol's posting list is empty, and no unresolved nominal at this position spells it",
                self.tag()
            ),
            Self::UnresolvedNominal { spelling } => write!(
                f,
                "{}: `{spelling}` is written at this position and did not become a StableRef",
                self.tag()
            ),
            Self::OccurrencesNotAttached => write!(
                f,
                "{}: occurrences were not recorded, or occurrence owners or targets were rejected",
                self.tag()
            ),
            Self::RustOnly => write!(
                f,
                "{}: no loaded package records ImplementedTrait, so implementors is empty by construction for these languages",
                self.tag()
            ),
        }
    }
}

/// Facts the decision order reads. The posting list is already known to be
/// empty.
pub struct EmptyEdgeFacts<'a> {
    pub edge: &'a str,
    pub symbol_name: &'a str,
    /// Some loaded package wrote [`TypePosition::ImplementedTrait`].
    /// Consulted only for `implementors`.
    pub any_package_records_implemented_trait: bool,
    /// The queried symbol's own package called `record_occurrence`.
    pub occurrences_recorded: bool,
    /// An occurrence owner or target was rejected while sealing a package
    /// that this edge scanned.
    pub occurrence_attachments_rejected: bool,
    /// Spellings at this edge's position that did not become a `StableRef`,
    /// across the packages the edge scanned.
    pub unresolved_spellings: &'a [String],
}

/// Classify an empty posting list.
///
/// Decision order:
/// 1. `implementors` and no package records `ImplementedTrait` →
///    [`EdgeEmptyReason::RustOnly`]
/// 2. `usages` and occurrences were not recorded, or owners/targets were
///    rejected → [`EdgeEmptyReason::OccurrencesNotAttached`]
/// 3. a nominal at this edge's position spells the queried symbol and did not
///    link → [`EdgeEmptyReason::UnresolvedNominal`]
/// 4. otherwise [`EdgeEmptyReason::NoSuchEdge`]
pub fn classify_empty_edge(facts: &EmptyEdgeFacts<'_>) -> EdgeEmptyReason {
    if facts.edge == "implementors" && !facts.any_package_records_implemented_trait {
        return EdgeEmptyReason::RustOnly;
    }
    if facts.edge == "usages"
        && (!facts.occurrences_recorded || facts.occurrence_attachments_rejected)
    {
        return EdgeEmptyReason::OccurrencesNotAttached;
    }
    if let Some(spelling) = spelling_for_symbol(facts.unresolved_spellings, facts.symbol_name) {
        return EdgeEmptyReason::UnresolvedNominal { spelling };
    }
    EdgeEmptyReason::NoSuchEdge
}

/// One empty covered edge, already classified for the symbol it was resolved
/// against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmptyEdgeDiagnosis {
    pub edge: String,
    pub answers_instead: &'static str,
    pub reason: EdgeEmptyReason,
}

/// Classify `edge` for `symbol_name` using the packages the posting scan just
/// read.
///
/// `symbol_package` is the package that owns the queried symbol. Unresolved
/// nominals are read from every package in `packages`, because a return type
/// written in a different package can still spell this symbol.
pub fn diagnose_empty_edge(
    edge: &str,
    symbol_name: &str,
    symbol_package: &PackageView,
    packages: &[Arc<PackageView>],
) -> EmptyEdgeDiagnosis {
    let any_package_records_implemented_trait = packages.iter().any(|pkg| {
        pkg.indexes()
            .records_type_position(TypePosition::ImplementedTrait)
    });
    let occurrence_attachments_rejected = symbol_package.occurrence_attachments_rejected()
        || packages
            .iter()
            .any(|pkg| pkg.occurrence_attachments_rejected());
    let mut unresolved_spellings = Vec::new();
    if let Some(position) = type_position_for_edge(edge) {
        for pkg in packages {
            unresolved_spellings.extend(
                pkg.indexes()
                    .unresolved_nominals_at(position)
                    .iter()
                    .cloned(),
            );
        }
    }
    let reason = classify_empty_edge(&EmptyEdgeFacts {
        edge,
        symbol_name,
        any_package_records_implemented_trait,
        occurrences_recorded: symbol_package.indexes().occurrences_recorded,
        occurrence_attachments_rejected,
        unresolved_spellings: &unresolved_spellings,
    });
    EmptyEdgeDiagnosis {
        edge: edge.to_owned(),
        answers_instead: reason.answers_instead(),
        reason,
    }
}

/// Edges whose empty posting list must carry a coverage note.
pub fn is_covered_edge(edge: &str) -> bool {
    matches!(
        edge,
        "implementors" | "returnedBy" | "subtypes" | "acceptedBy" | "heldBy" | "usages"
    )
}

fn type_position_for_edge(edge: &str) -> Option<TypePosition> {
    match edge {
        "implementors" => Some(TypePosition::ImplementedTrait),
        "subtypes" => Some(TypePosition::SuperType),
        "returnedBy" => Some(TypePosition::Return),
        "acceptedBy" => Some(TypePosition::Parameter),
        "heldBy" => Some(TypePosition::FieldType),
        _ => None,
    }
}

/// `spelling` names `symbol_name` when it is that name, or a path whose final
/// segment is that name (`pkg.Widget`, `pkg::Widget`, `pkg/Widget`).
fn spells_symbol(spelling: &str, symbol_name: &str) -> bool {
    if symbol_name.is_empty() {
        return false;
    }
    if spelling == symbol_name {
        return true;
    }
    spelling.strip_suffix(symbol_name).is_some_and(|prefix| {
        prefix.ends_with('.') || prefix.ends_with("::") || prefix.ends_with('/')
    })
}

fn spelling_for_symbol(spellings: &[String], symbol_name: &str) -> Option<String> {
    let mut best: Option<&str> = None;
    for spelling in spellings {
        if !spells_symbol(spelling, symbol_name) {
            continue;
        }
        best = Some(match best {
            None => spelling.as_str(),
            Some(prev) => prefer_spelling(prev, spelling, symbol_name),
        });
    }
    best.map(str::to_owned)
}

fn prefer_spelling<'a>(prev: &'a str, next: &'a str, symbol_name: &str) -> &'a str {
    match (prev == symbol_name, next == symbol_name) {
        (true, false) => prev,
        (false, true) => next,
        _ if prev.len() != next.len() => {
            if prev.len() < next.len() {
                prev
            } else {
                next
            }
        }
        _ if prev <= next => prev,
        _ => next,
    }
}

fn specificity(reason: &EdgeEmptyReason) -> u8 {
    match reason {
        EdgeEmptyReason::RustOnly => 0,
        EdgeEmptyReason::OccurrencesNotAttached => 1,
        EdgeEmptyReason::UnresolvedNominal { .. } => 2,
        EdgeEmptyReason::NoSuchEdge => 3,
    }
}

/// The best empty-edge diagnosis seen while one query resolved neighbors.
///
/// A query can traverse the edge once per symbol. The page carries one note,
/// so a more specific reason replaces a less specific one.
#[derive(Debug, Default)]
pub struct EdgeEmptyLog {
    best: Mutex<Option<EmptyEdgeDiagnosis>>,
}

impl EdgeEmptyLog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&self, diagnosis: EmptyEdgeDiagnosis) {
        let mut slot = self
            .best
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let replace = match slot.as_ref() {
            None => true,
            Some(existing) => {
                let new_rank = specificity(&diagnosis.reason);
                let old_rank = specificity(&existing.reason);
                new_rank < old_rank
                    || (new_rank == old_rank && diagnosis_key(&diagnosis) < diagnosis_key(existing))
            }
        };
        if replace {
            *slot = Some(diagnosis);
        }
    }

    pub fn take(&self) -> Option<EmptyEdgeDiagnosis> {
        self.best
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }
}

fn diagnosis_key(diagnosis: &EmptyEdgeDiagnosis) -> (&str, &str) {
    let spelling = match &diagnosis.reason {
        EdgeEmptyReason::UnresolvedNominal { spelling } => spelling.as_str(),
        EdgeEmptyReason::NoSuchEdge
        | EdgeEmptyReason::OccurrencesNotAttached
        | EdgeEmptyReason::RustOnly => "",
    };
    (diagnosis.edge.as_str(), spelling)
}

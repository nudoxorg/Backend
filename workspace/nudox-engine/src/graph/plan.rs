//! Access-path planning for the two root entrypoints.
//!
//! # Why the plan is a value
//!
//! Trustfall hands an adapter *hints* — [`CandidateValue`]s describing what
//! the query has already proven about a property — and the adapter decides
//! which store index can answer them. Previously that decision was a ladder of
//! `if let Some(CandidateValue::Single(FieldValue::String(_)))` inside
//! `resolve_starting_vertices`, which had three consequences:
//!
//! * The decision could only be observed by running a query and counting what
//!   the store was asked for, so a regression in the *choice* was
//!   indistinguishable from a regression in the *execution*.
//! * `CandidateValue::Multiple` (produced by `@filter(op: "one_of")`) and
//!   `CandidateValue::Impossible` (produced when the engine has already proven
//!   no value can satisfy the query) fell through the `Single` pattern to the
//!   full-scan arm. A `one_of` over three keys therefore cost a whole-corpus
//!   walk, and a provably-empty query cost one too.
//! * A `... on Trait` type coercion — which pins the entry's
//!   [`KindDiscriminant`] exactly as tightly as `kind @filter(op: "=")` does —
//!   was not consulted at all.
//!
//! Naming the plan makes all three testable without a corpus: the planner is a
//! pure function from hints to a [`SymbolPlan`]/[`PackagePlan`], and
//! `tests/plan.rs` enumerates the hint space against it.

use nudox_ir::{
    change::{EcosystemId, IntroId, PackageLineageId, PackageName, StableRef},
    kind::KindDiscriminant,
};
use trustfall::FieldValue;
use trustfall::provider::CandidateValue;

use crate::graph::adapter::Error;

// ---------------------------------------------------------------------------
// Symbols
// ---------------------------------------------------------------------------

/// The access path chosen for the `Symbols` root entrypoint.
///
/// Ordered by cost: every variant above [`SymbolPlan::FullScan`] answers the
/// query with work proportional to the *answer*; `FullScan` is the only one
/// proportional to the corpus.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SymbolPlan {
    /// The engine proved no value can satisfy the query. Yield nothing and
    /// touch the store zero times.
    ///
    /// Reachable from `@filter(op: "one_of", value: ["$empty"])` with an empty
    /// list, or from mutually exclusive filters on one property.
    Empty,

    /// Direct `Corpus::entry` lookups, one per key. No package enumeration.
    Keys(Vec<StableRef>),

    /// `NameIndex::get_exact` per package, one probe per lowercased name.
    ///
    /// Names are stored pre-lowercased because that is the form `NameIndex`
    /// keys on; doing it here rather than at each probe keeps the plan a
    /// faithful description of the lookups that will happen.
    Names(Vec<String>),

    /// `by_kind` per package, one probe per discriminant.
    ///
    /// Reached from `kind @filter(op: "=" | "one_of")` *and* from a `... on
    /// Function` type coercion, which constrains the discriminant identically.
    Kinds(Vec<KindDiscriminant>),

    /// No usable constraint: walk every entry of every loaded package.
    FullScan,
}

/// Choose the `Symbols` access path from the hints the engine offers.
///
/// The caller passes the four hints rather than a `ResolveInfo` so that this
/// function is a pure, directly-testable mapping — `ResolveInfo` cannot be
/// constructed outside the trustfall engine.
///
/// Priority is by selectivity: a key identifies one entry, a name a handful, a
/// kind a bucket. A coercion is consulted only when `kind` carries no filter,
/// because when both are present they describe the same constraint and the
/// explicit filter is at least as tight.
pub fn plan_symbols(
    key: Option<CandidateValue<FieldValue>>,
    name: Option<CandidateValue<FieldValue>>,
    kind: Option<CandidateValue<FieldValue>>,
    coerced_to: Option<&str>,
) -> Result<SymbolPlan, Error> {
    // `Impossible` on *any* required property makes the whole vertex
    // unsatisfiable, whichever property it came from.
    for hint in [&key, &name, &kind] {
        if matches!(hint, Some(CandidateValue::Impossible)) {
            return Ok(SymbolPlan::Empty);
        }
    }

    if let Some(values) = key.and_then(candidate_strings) {
        // An unparseable key is a caller error, not an empty result: silently
        // returning no rows for `cargo:foo` (no `#`) is how a typo becomes a
        // wrong answer instead of a diagnosis.
        let refs = values
            .iter()
            .map(|s| parse_stable_ref(s).ok_or_else(|| Error::InvalidKey(s.clone())))
            .collect::<Result<Vec<_>, _>>()?;
        // `one_of` with an empty list is satisfiable by nothing.
        return Ok(if refs.is_empty() {
            SymbolPlan::Empty
        } else {
            SymbolPlan::Keys(refs)
        });
    }

    if let Some(values) = name.and_then(candidate_strings) {
        let lowered: Vec<String> = values.iter().map(|s| s.to_lowercase()).collect();
        return Ok(if lowered.is_empty() {
            SymbolPlan::Empty
        } else {
            SymbolPlan::Names(lowered)
        });
    }

    if let Some(values) = kind.and_then(candidate_strings) {
        let discs = values
            .iter()
            .map(|s| kind_disc_from_str(s).ok_or_else(|| Error::InvalidKind(s.clone())))
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(if discs.is_empty() {
            SymbolPlan::Empty
        } else {
            SymbolPlan::Kinds(discs)
        });
    }

    // A `... on Function` coercion pins the discriminant exactly as a `kind`
    // equality filter does, and costs the same `by_kind` probe to serve.
    //
    // `Symbol` and `OtherSymbol` are deliberately not mapped: `Symbol` is the
    // interface (no constraint at all), and `OtherSymbol` is the adapter's
    // catch-all for entries with *no* `KindDiscriminant`, which `by_kind`
    // cannot enumerate because it is keyed by discriminant. Both fall through
    // to the scan, which is the honest cost of asking for them.
    if let Some(disc) = coerced_to.and_then(kind_disc_from_str) {
        return Ok(SymbolPlan::Kinds(vec![disc]));
    }

    Ok(SymbolPlan::FullScan)
}

// ---------------------------------------------------------------------------
// Packages
// ---------------------------------------------------------------------------

/// The access path chosen for the `Packages` root entrypoint.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PackagePlan {
    /// The engine proved no package can satisfy the query.
    Empty,

    /// Direct `Corpus::package` lookups, one per lineage.
    Lineages(Vec<PackageLineageId>),

    /// Enumerate every loaded package.
    ///
    /// This is the honest plan for a filter on `name` or `ecosystem` alone:
    /// `Corpus` is a `BTreeMap` keyed by the *pair*, and exposes no range
    /// scan, so neither half can be answered without walking the map. See the
    /// crate report for why that was not fixed here (it is a `nudox-store`
    /// change, outside this crate's scope).
    All,
}

/// Choose the `Packages` access path from the `lineage` hint.
///
/// `list_package_functions.trustfall` — one of the five shipped queries — is
/// exactly `Packages { lineage @filter(op: "=", …) members { … } }`, and until
/// this plan existed it read the entire corpus map to find one package.
pub fn plan_packages(lineage: Option<CandidateValue<FieldValue>>) -> Result<PackagePlan, Error> {
    if matches!(lineage, Some(CandidateValue::Impossible)) {
        return Ok(PackagePlan::Empty);
    }

    let Some(values) = lineage.and_then(candidate_strings) else {
        return Ok(PackagePlan::All);
    };

    let ids = values
        .iter()
        .map(|s| parse_lineage(s).ok_or_else(|| Error::InvalidLineage(s.clone())))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(if ids.is_empty() {
        PackagePlan::Empty
    } else {
        PackagePlan::Lineages(ids)
    })
}

// ---------------------------------------------------------------------------
// Hint decoding
// ---------------------------------------------------------------------------

/// The concrete string values a candidate constrains a property to, if it
/// constrains it to a finite set of strings at all.
///
/// Returns `None` — meaning "no usable pushdown" — for:
///
/// * [`CandidateValue::All`], which is no constraint;
/// * [`CandidateValue::Range`], because none of this crate's lookup keys are
///   range-addressable. `NameIndex` is a `BTreeMap` and does expose
///   `prefix()`, but trustfall never turns `has_prefix` into a candidate (it
///   is a post-processing filter, see `hints/filters.rs`), and an ordered
///   comparison on a *name* is not a query anyone writes;
/// * [`CandidateValue::Multiple`] containing any non-string, which would mean
///   the schema and this decoder disagree about the property's type.
///
/// [`CandidateValue::Impossible`] is handled by the callers before this point,
/// because it means "yield nothing", not "no pushdown available".
fn candidate_strings(candidate: CandidateValue<FieldValue>) -> Option<Vec<String>> {
    match candidate {
        CandidateValue::Single(FieldValue::String(s)) => Some(vec![s.to_string()]),
        CandidateValue::Multiple(values) => values
            .into_iter()
            .map(|v| match v {
                FieldValue::String(s) => Some(s.to_string()),
                _ => None,
            })
            .collect(),
        CandidateValue::Single(_)
        | CandidateValue::Range(_)
        | CandidateValue::All
        | CandidateValue::Impossible => None,
        // `CandidateValue` is `#[non_exhaustive]`: a future variant is a new
        // *opportunity*, and treating it as "no pushdown" is the only safe
        // default — it yields the same rows, more slowly.
        _ => None,
    }
}

/// Parse `"ecosystem:name"` into a [`PackageLineageId`].
pub(crate) fn parse_lineage(s: &str) -> Option<PackageLineageId> {
    let (eco, name) = s.split_once(':')?;
    Some(PackageLineageId::new(
        EcosystemId::new(eco),
        PackageName::new(name),
    ))
}

/// Parse `"ecosystem:name#introhex"` into a [`StableRef`].
pub(crate) fn parse_stable_ref(s: &str) -> Option<StableRef> {
    let (pkg_str, intro_hex) = s.split_once('#')?;
    let lineage = parse_lineage(pkg_str)?;
    let bytes = parse_hex_32(intro_hex)?;
    Some(StableRef::new(lineage, IntroId::from_raw(bytes)))
}

/// Decode exactly 64 lowercase hex chars into 32 bytes.
fn parse_hex_32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
        let hi = (chunk[0] as char).to_digit(16)? as u8;
        let lo = (chunk[1] as char).to_digit(16)? as u8;
        out[i] = (hi << 4) | lo;
    }
    Some(out)
}

/// Match a kind-discriminant name string to the enum variant.
///
/// Shared by the `kind` filter and the type-coercion path, which is what makes
/// `... on Trait` and `kind @filter(op: "=", value: ["Trait"])` provably cost
/// the same: they resolve through one table.
pub(crate) fn kind_disc_from_str(s: &str) -> Option<KindDiscriminant> {
    Some(match s {
        "Module" => KindDiscriminant::Module,
        "Record" => KindDiscriminant::Record,
        "Field" => KindDiscriminant::Field,
        "Function" => KindDiscriminant::Function,
        "Alias" => KindDiscriminant::Alias,
        "Trait" => KindDiscriminant::Trait,
        "Impl" => KindDiscriminant::Impl,
        "Enum" => KindDiscriminant::Enum,
        "Variant" => KindDiscriminant::Variant,
        "Const" => KindDiscriminant::Const,
        "Static" => KindDiscriminant::Static,
        "Reexport" => KindDiscriminant::Reexport,
        "Param" => KindDiscriminant::Param,
        _ => return None,
    })
}

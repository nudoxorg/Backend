//! The embedding seam (§L3, LR-10) — and the index built behind it.
//!
//! # Why this is a trait and not an implementation
//!
//! This is the same shape as [`crate::highlight`], for the same class of
//! reason, and the argument is worth stating in full because the obvious
//! alternative — `nudox-engine` depending on `workspace/registry` — was
//! seriously considered and is worse on every axis that was measured.
//!
//! `workspace/registry`'s vector plane has a real, working local embedder
//! (`FastembedOrt`, fastembed 5.17.3 over ort 2.0.0-rc.12). Reaching it from
//! here would mean an edge `nudox-engine → registry`. Measured on 2026-08-09:
//!
//! | build | unique crates |
//! |---|---|
//! | `registry` default (`remote`) | 325 |
//! | `registry` + `onnx` | 483 |
//!
//! `nudox-engine` is the *one* crate `lindsey` depends on (§1). Putting a
//! 483-crate graph under it — even behind an optional feature — makes the
//! engine's dependency surface a function of a plane the engine has no
//! business knowing about. Three further facts make the edge not merely
//! expensive but wrong:
//!
//! 1. **`registry::vector::core::embed::Embedder` is not object-safe.** It
//!    carries `type Model: EmbeddingModel`, so there is no `dyn Embedder` to
//!    hold. The engine would have to be generic over the model — leaking a
//!    registry type parameter into `EngineConfig`, `EngineHandle`, and
//!    everything that names them, including the GUI.
//! 2. **`ort-sys` downloads a prebuilt runtime at build time** unless
//!    `ORT_LIB_LOCATION` points at a local install. That violates this repo's
//!    no-network-at-build-time standard, and as of 2026-08-09 nothing in this
//!    tree sets that variable (grep: zero hits in `flake.nix`,
//!    `.cargo/config.toml`, any `package.nix`). Provisioning is unsolved, and
//!    an unsolved provisioning problem must not be able to break `cargo check
//!    -p nudox-engine`.
//! 3. **The engine must work with no embedder at all.** `nudox-mcp` serves
//!    text and has no model; the fixture corpus has no model; CI has no model.
//!    A design in which those are degraded builds is a design in which the
//!    common case is the exception.
//!
//! So the engine keeps what it can actually own — *when* embedding happens,
//! that it never blocks IR availability, how partial coverage is reported, and
//! the ranking — and the host supplies *how*. The dependency law (§1) is
//! unchanged by this file: `registry` stays outside the graph, exactly where it
//! was.
//!
//! # No embedder is a supported state, and it is not the same as "no results"
//!
//! [`SharedEmbedder`] is an `Option` for the reason [`crate::highlight`]'s is:
//! "this build has no model" is a real configuration that deserves to be
//! visible in a type. But unlike highlighting — where the degraded rendering is
//! simply uncoloured text — a semantic section with no embedder must not render
//! as *zero results*, because zero results is a claim: it says "we looked and
//! there was nothing". [`SectionState`] is what carries the difference, and
//! `run_search` emits it for section 2 on every query.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

use nudox_ir::change::{IntroId, PackageLineageId};
use serde::{Deserialize, Serialize};

use crate::wire::SharedStr;

// ---------------------------------------------------------------------------
// The port
// ---------------------------------------------------------------------------

/// Which side of a retrieval pair a text is being embedded as.
///
/// Asymmetric models (including `jina-embeddings-v2-base-code`) are trained
/// with distinct instructions per side, so a query embedded as a document
/// silently retrieves worse rather than failing. Making the caller name the
/// side means the engine cannot get it wrong by omission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbedRole {
    /// Text the user typed, embedded to search *with*.
    Query,
    /// Corpus text, embedded to be searched *over*.
    Document,
}

/// Why an embedding call failed.
///
/// One variant per distinct recoverable situation (§3). `Backend` is the
/// catch-all for a host runtime's own error, and it carries the host's message
/// rather than discarding it — the engine cannot interpret it, but the person
/// reading the log can.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The host returned a vector of the wrong width.
    ///
    /// Fatal for the affected batch: a vector that is not
    /// [`EmbedderInfo::dimensions`] wide cannot be scored against the others,
    /// and silently padding or truncating it would produce a ranking derived
    /// from invented components.
    #[error("embedder returned {got}-dimensional vector, expected {expected}")]
    DimensionMismatch { expected: usize, got: usize },

    /// The host returned a vector containing NaN or an infinity.
    ///
    /// Rejected rather than filtered, because a non-finite component makes
    /// every cosine involving that vector non-finite, and `total_cmp` would
    /// then sort it to a stable but meaningless position.
    #[error("embedder returned a non-finite component at index {index}")]
    NonFinite { index: usize },

    /// The host's runtime failed.
    #[error("embedding backend failed: {0}")]
    Backend(String),
}

/// What the host's embedder is, in the terms the engine needs to schedule it.
///
/// Every field is something the engine acts on: `dimensions` validates
/// returned vectors, `max_batch` sizes the batches, `durable_canonical` decides
/// whether results may be cached across runs, `model_id` labels the index so a
/// model swap cannot silently reuse the previous model's vectors.
#[derive(Debug, Clone)]
pub struct EmbedderInfo {
    /// Stable identifier for the model, e.g.
    /// `"jinaai/jina-embeddings-v2-base-code"`.
    pub model_id: SharedStr,
    /// Vector width. Every vector the host returns must be exactly this wide.
    pub dimensions: usize,
    /// Largest batch the host wants to be handed at once.
    ///
    /// The engine never exceeds it. `FastembedOrt` caps at 32.
    pub max_batch: usize,
    /// Whether two runs of this model over the same text produce the same
    /// vector.
    ///
    /// `false` for anything dynamically quantized — measured on
    /// `jina-embeddings-v2-base-code`'s `model_quantized.onnx`, the same text
    /// embedded beside different neighbours differed by 0.0154 per component,
    /// against 1e-7 for fp32. A `false` here means the index may be used within
    /// a process but must never be persisted or published, because a later run
    /// would disagree with it in a way no consumer could detect.
    pub durable_canonical: bool,
}

/// Turns text into vectors.
///
/// Object-safe on purpose — the engine holds `Arc<dyn Embedder>` and must not
/// be generic over the model (see the module docs for what happened to the
/// design that was). The future is boxed for the same reason.
///
/// Implementations must be **cancellation-safe**: the engine drops the future
/// when a package load is superseded or the engine shuts down, and a host that
/// leaves a runtime wedged on drop turns a cancelled search into a permanently
/// broken index.
pub trait Embedder: Send + Sync + 'static {
    /// What this embedder is. Called once per index build, not per batch.
    fn info(&self) -> EmbedderInfo;

    /// Embed `texts` as `role`.
    ///
    /// Returns one vector per input, in input order — the engine pairs results
    /// positionally with the symbols it sent, so a host that reorders or drops
    /// entries corrupts the index rather than degrading it. The engine
    /// validates width and finiteness on return; it does not validate order,
    /// because it cannot.
    ///
    /// `texts.len()` never exceeds [`EmbedderInfo::max_batch`].
    fn embed_batch<'a>(
        &'a self,
        texts: &'a [String],
        role: EmbedRole,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Vec<f32>>, Error>> + Send + 'a>>;
}

/// A shared embedder, or none.
///
/// `Option` rather than a null-object default for the same reason
/// [`crate::highlight::SharedHighlighter`] is one: "this build has no model" is
/// a supported configuration that should be readable from a type rather than
/// inferred from empty output. See [`SectionState::Unavailable`] for how the
/// `None` case reaches the reader without pretending to be a zero-hit result.
pub type SharedEmbedder = Option<Arc<dyn Embedder>>;

// ---------------------------------------------------------------------------
// Section state
// ---------------------------------------------------------------------------

/// Why a section has nothing, or nothing *yet*, to say.
///
/// # Why this is on the wire and not inferred
///
/// A section that returns no rows has three possible meanings, and a reader who
/// cannot tell them apart is being misled by all three:
///
/// * *complete and empty* — we searched everything and there is no match;
/// * *still building* — we searched what exists so far, which is not everything;
/// * *unavailable* — we did not search, because there is nothing to search with.
///
/// Only the first is a claim about the corpus. The GUI renders each
/// differently, and before this existed it had no way to: `SectionStatus` flips
/// to `Ready` on any `Section` event, so an empty batch from a section that had
/// not started reading as "no results" was not a rendering bug but a protocol
/// one.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SectionState {
    /// Every resident package was searched. Rows are the whole answer.
    Complete,
    /// Some resident packages have not been indexed yet.
    ///
    /// The rows delivered alongside this are a **real ranking over `covered`
    /// packages**, not a partial prefix of the final ranking: a package indexed
    /// later can outrank everything already shown. That is why this carries the
    /// counts rather than a bare flag — "3 of 20 packages" is a caveat a reader
    /// can weigh, and "still loading" is not.
    Building { covered: u32, total: u32 },
    /// Nothing was searched, and nothing will be until the condition clears.
    Unavailable { reason: Unavailable },
}

/// The closed set of reasons a section could not run at all.
///
/// Exhaustive (no `#[non_exhaustive]`) on the §3 `ProducerError` rule: this
/// enum is internal to this workspace, and adding a reason *should* break every
/// consumer's match and make each one decide how to render it. A reason that
/// fell into a `_` arm would render as one of the others, which is precisely
/// the confusion [`SectionState`] exists to remove.
///
/// This is also why `NoEmbedder` does not appear here: it used to, and it
/// collapsed two states this enum used to keep apart — `NoRuntime` and
/// [`NoModelConfigured`](Unavailable::NoModelConfigured) — by the same test
/// [`ModelFailed`](Unavailable::ModelFailed)'s doc comment applies one level
/// down: the two "send a reader to completely different places" (one was a
/// rebuild, the other one environment variable), so a reader staring at
/// `NoEmbedder` could not tell which fix applied.
///
/// # `NoRuntime` is gone, not just renamed
///
/// The ONNX embedder runtime (`embed::onnx`, `registry` + `ort-sys`) is now a
/// plain, non-optional dependency of `nudox-engine` — there is no `onnx`
/// cargo feature, so no build of this crate can lack the runtime. `NoRuntime`
/// named exactly that unreachable state ("this binary was built without the
/// runtime — rebuild it"), and an enum member no code path can ever produce is
/// worse than useless here: every consumer of this exhaustive match would have
/// had to keep rendering a branch for a state that could never again be
/// observed. It was removed rather than kept "just in case", on the same rule
/// that keeps this enum exhaustive in the first place — a reason that cannot
/// happen is not a reason, it is dead weight in every reader's match. The only
/// way to have no embedder now is a missing
/// [`MODEL_DIR_ENV`](crate::embed::MODEL_DIR_ENV), i.e. `NoModelConfigured`.
/// See [`remedy`](Unavailable::remedy) for the text each remaining variant
/// tells the reader to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unavailable {
    /// No model directory was configured.
    ///
    /// `embed::onnx::load_from_env` returns `None` when `NUDOX_EMBED_MODEL_DIR`
    /// is unset. No rebuild is needed — setting the variable and restarting is
    /// enough. This is the *only* way `embed::load_from_env` can return no
    /// embedder: the runtime itself is always compiled in.
    NoModelConfigured,
    /// An embedder is installed but no package has finished indexing yet, and
    /// the corpus is empty — there is nothing to have indexed.
    EmptyCorpus,
    /// An embedder is installed and it failed on this query.
    ///
    /// Distinct from [`NoModelConfigured`](Unavailable::NoModelConfigured)
    /// because this one sends a reader somewhere that does not: *the model is
    /// here and something went wrong* — a missing weights file, an
    /// out-of-memory runtime, a revoked API key — which is worth retrying and
    /// worth looking in the log for, not worth reconfiguring anything.
    ModelFailed,
}

impl Unavailable {
    /// What a reader should actually do about this.
    ///
    /// A bare variant name is only actionable to someone who already knows
    /// this build system; the whole point of splitting `NoEmbedder` was that
    /// guessing the wrong remedy between a rebuild and an environment variable
    /// used to cost the rebuild *and* leave the variable unset. Now there is
    /// no rebuild remedy left to guess wrong — every remaining arm names a fix
    /// a reader who has never seen this crate can follow without rebuilding
    /// anything.
    pub fn remedy(&self) -> &'static str {
        match self {
            Unavailable::NoModelConfigured => {
                "the embedding runtime is always compiled in, so no rebuild is \
                 possible or needed — set NUDOX_EMBED_MODEL_DIR to the pinned \
                 model directory (see embed::MODEL_DIR_ENV for exactly what it \
                 must contain) and restart"
            }
            Unavailable::EmptyCorpus => {
                "there is nothing in the corpus yet to search — load a package, \
                 or wait for one already loading to finish indexing"
            }
            Unavailable::ModelFailed => {
                "the model is installed and something went wrong on this query — \
                 check the log for the embedding backend's error (a missing \
                 weights file, an out-of-memory runtime, a revoked key) and retry"
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The index
// ---------------------------------------------------------------------------

/// One symbol's embedded vector, L2-normalised at insert.
///
/// Normalising once at insert rather than per query is what makes scoring a dot
/// product: for unit vectors, cosine *is* the dot product, so the query path
/// does no square roots and no division at all.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
struct Vector {
    intro: IntroId,
    /// Unit-length. Enforced by [`SemanticIndex::insert_package`], not by
    /// convention.
    values: Arc<[f32]>,
}

/// Every embedded package, and which packages have been embedded.
///
/// # Why the coverage set is separate from the vectors
///
/// A package that embeds to zero vectors (no documentable symbols) is
/// *indexed*, and a package that has not been reached yet is not — and both
/// have no vectors. Deriving coverage from `vectors.keys()` would conflate
/// them, and the difference is exactly what [`SectionState::Building`] reports.
/// So membership is recorded where it happens rather than inferred from the
/// payload, which is the §8 "counted, not accumulated alongside" rule applied
/// to a set instead of a tally.
#[derive(Default)]
pub(crate) struct SemanticIndexInner {
    vectors: BTreeMap<PackageLineageId, Vec<Vector>>,
    indexed: BTreeSet<PackageLineageId>,
}

#[derive(Deserialize, Serialize)]
struct PersistedVector {
    intro: IntroId,
    values: Vec<f32>,
}

#[derive(Deserialize, Serialize)]
struct PersistedIndex {
    model_id: String,
    dimensions: usize,
    inner: PersistedIndexInner,
}

#[derive(Deserialize, Serialize)]
struct PersistedIndexInner {
    vectors: Vec<PersistedPackage>,
    indexed: BTreeSet<PackageLineageId>,
}

#[derive(Deserialize, Serialize)]
struct PersistedPackage {
    lineage: PackageLineageId,
    vectors: Vec<PersistedVector>,
}

/// The engine's local semantic index.
///
/// Cheap to clone (`Arc` inside). When a durable embedder and a state path are
/// supplied, every completed package is written atomically and reopened only
/// when the model identity and vector width match.
#[derive(Clone)]
pub(crate) struct SemanticIndex {
    inner: Arc<RwLock<SemanticIndexInner>>,
    persistence: Option<Arc<IndexPersistence>>,
}

struct IndexPersistence {
    path: std::path::PathBuf,
    model_id: String,
    dimensions: usize,
}

impl Default for SemanticIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl SemanticIndex {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(SemanticIndexInner::default())),
            persistence: None,
        }
    }

    pub(crate) fn open(path: std::path::PathBuf, info: &EmbedderInfo) -> Self {
        let persistence = (info.durable_canonical).then(|| {
            Arc::new(IndexPersistence {
                path,
                model_id: info.model_id.to_string(),
                dimensions: info.dimensions,
            })
        });
        let inner = persistence
            .as_deref()
            .and_then(|p| load_persisted(p).ok())
            .filter(|saved| {
                saved.model_id == info.model_id.to_string() && saved.dimensions == info.dimensions
            })
            .map(|saved| SemanticIndexInner {
                vectors: saved
                    .inner
                    .vectors
                    .into_iter()
                    .map(|package| {
                        (
                            package.lineage,
                            package
                                .vectors
                                .into_iter()
                                .map(|vector| Vector {
                                    intro: vector.intro,
                                    values: Arc::from(vector.values),
                                })
                                .collect(),
                        )
                    })
                    .collect(),
                indexed: saved.inner.indexed,
            })
            .unwrap_or_default();
        Self {
            inner: Arc::new(RwLock::new(inner)),
            persistence,
        }
    }

    /// Record that `lineage` has been embedded, with `vectors` as its content.
    ///
    /// Rejects any vector that is not `dimensions` wide or contains a
    /// non-finite component: an index is a thing a ranking is derived from, so
    /// a bad vector is not a bad row, it is a bad *order* over every row.
    ///
    /// Replaces any previous entry for `lineage`, which is what makes
    /// re-indexing after `select_version` correct rather than additive.
    pub(crate) fn insert_package(
        &self,
        lineage: PackageLineageId,
        entries: Vec<(IntroId, Vec<f32>)>,
        dimensions: usize,
    ) -> Result<(), Error> {
        let mut vectors = Vec::with_capacity(entries.len());
        for (intro, mut values) in entries {
            if values.len() != dimensions {
                return Err(Error::DimensionMismatch {
                    expected: dimensions,
                    got: values.len(),
                });
            }
            if let Some(index) = values.iter().position(|v| !v.is_finite()) {
                return Err(Error::NonFinite { index });
            }
            normalize(&mut values);
            vectors.push(Vector {
                intro,
                values: Arc::from(values.as_slice()),
            });
        }

        let mut guard = self
            .inner
            .write()
            .expect("semantic index lock is never held across a panic");
        guard.vectors.insert(lineage.clone(), vectors);
        guard.indexed.insert(lineage);
        let persisted = self.persistence.as_deref().map(|p| snapshot(p, &guard));
        drop(guard);
        if let Some(persisted) = persisted {
            if let Err(error) = persist(persisted.0, persisted.1) {
                tracing::warn!(%error, "semantic index persistence failed");
            }
        }
        Ok(())
    }

    /// How many packages have been embedded.
    pub(crate) fn covered(&self) -> usize {
        self.inner
            .read()
            .expect("semantic index lock is never held across a panic")
            .indexed
            .len()
    }

    /// Whether `lineage` has been embedded.
    pub(crate) fn contains(&self, lineage: &PackageLineageId) -> bool {
        self.inner
            .read()
            .expect("semantic index lock is never held across a panic")
            .indexed
            .contains(lineage)
    }

    /// The `limit` nearest symbols to `query`, as `(lineage, intro, cosine)`.
    ///
    /// `query` need not be normalised; it is normalised here. Scores are
    /// cosines in `[-1, 1]`, and are returned as-is rather than rescaled — the
    /// caller maps them onto the wire's `(0, 1]` relevance contract, and doing
    /// that here would hide the sign from the one place that can act on it.
    ///
    /// Ties break on `(lineage, intro)`, so the order is total for the same
    /// reason `search::compare_candidates` is: `BTreeMap` iteration is already
    /// deterministic, but two genuinely equal cosines must not be left in
    /// whatever order the walk produced.
    pub(crate) fn nearest(
        &self,
        query: &[f32],
        limit: usize,
    ) -> Vec<(PackageLineageId, IntroId, f32)> {
        let mut query = query.to_vec();
        normalize(&mut query);

        let guard = self
            .inner
            .read()
            .expect("semantic index lock is never held across a panic");

        // Scored by *position* in the map rather than by lineage, so the hot
        // loop allocates nothing. A `PackageLineageId` clone per candidate is
        // one clone per vector in the whole index, on every keystroke behind a
        // 24 ms debounce; cloning only the `limit` survivors is the same answer
        // for a fraction of the work.
        //
        // The position is a sound stand-in for the lineage in the comparator
        // because `vectors` is a `BTreeMap`: its iteration order *is* lineage
        // order, so ordering by index and ordering by key agree exactly.
        let mut scored: Vec<(usize, IntroId, f32)> = Vec::new();
        for (index, vectors) in guard.vectors.values().enumerate() {
            for vector in vectors {
                if vector.values.len() != query.len() {
                    // A package embedded by a *different* model than the one
                    // asking. Skipped rather than scored: a dot product across
                    // two models' spaces is a number, but it is not a
                    // similarity, and it would sort into the results as if it
                    // were one.
                    continue;
                }
                scored.push((index, vector.intro, dot(&vector.values, &query)));
            }
        }

        scored.sort_by(|a, b| {
            b.2.total_cmp(&a.2)
                .then_with(|| a.0.cmp(&b.0))
                .then_with(|| a.1.cmp(&b.1))
        });
        scored.truncate(limit);

        let lineages: Vec<&PackageLineageId> = guard.vectors.keys().collect();
        scored
            .into_iter()
            .map(|(index, intro, score)| (lineages[index].clone(), intro, score))
            .collect()
    }
}

fn snapshot(
    persistence: &IndexPersistence,
    inner: &SemanticIndexInner,
) -> (std::path::PathBuf, String) {
    let persisted = PersistedIndex {
        model_id: persistence.model_id.clone(),
        dimensions: persistence.dimensions,
        inner: PersistedIndexInner {
            vectors: inner
                .vectors
                .iter()
                .map(|(lineage, vectors)| PersistedPackage {
                    lineage: lineage.clone(),
                    vectors: vectors
                        .iter()
                        .map(|vector| PersistedVector {
                            intro: vector.intro,
                            values: vector.values.to_vec(),
                        })
                        .collect(),
                })
                .collect(),
            indexed: inner.indexed.clone(),
        },
    };
    (
        persistence.path.clone(),
        serde_json::to_string(&persisted).expect("semantic index state is serializable"),
    )
}

fn load_persisted(persistence: &IndexPersistence) -> Result<PersistedIndex, String> {
    let bytes = std::fs::read(&persistence.path).map_err(|e| e.to_string())?;
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}

fn persist(path: std::path::PathBuf, contents: String) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("semantic index has no parent: {}", path.display()))?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, contents).map_err(|e| e.to_string())?;
    std::fs::rename(&temporary, &path).map_err(|e| e.to_string())
}

/// Scale `values` to unit length, in place.
///
/// A zero vector is left alone rather than divided by zero: it scores 0 against
/// everything, which is the correct similarity for "no signal", whereas NaN
/// would poison the sort.
fn normalize(values: &mut [f32]) {
    let norm: f32 = values.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 0.0 && norm.is_finite() {
        for v in values.iter_mut() {
            *v /= norm;
        }
    }
}

/// Dot product, accumulated in `f64`.
///
/// `f32` accumulation over 768 terms loses enough precision to reorder
/// near-neighbours, and near-neighbours are the entire output of this function.
fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| f64::from(*x) * f64::from(*y))
        .sum::<f64>() as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lineage(name: &str) -> PackageLineageId {
        PackageLineageId::new(
            nudox_ir::change::EcosystemId::new("test"),
            nudox_ir::change::PackageName::new(name),
        )
    }

    fn intro(byte: u8) -> IntroId {
        IntroId::from_raw([byte; 32])
    }

    /// A build with no model is representable, and is not the same value as a
    /// build with one — the distinction `SectionState` is built on.
    #[test]
    fn a_missing_embedder_is_representable() {
        let none: SharedEmbedder = None;
        assert!(none.is_none(), "no-model builds are a supported state");
    }

    /// Cosine of a vector with itself is 1, and with its negation is -1.
    ///
    /// Asserts on the *value*, not on ordering: a scoring function that
    /// returned a constant would satisfy any pure ranking assertion.
    #[test]
    fn identical_vectors_score_one_and_opposite_vectors_score_minus_one() {
        let index = SemanticIndex::new();
        index
            .insert_package(
                lineage("a"),
                vec![
                    (intro(1), vec![3.0, 4.0, 0.0]),
                    (intro(2), vec![-3.0, -4.0, 0.0]),
                ],
                3,
            )
            .expect("well-formed vectors must be accepted");

        let hits = index.nearest(&[3.0, 4.0, 0.0], 2);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].1, intro(1));
        assert!(
            (hits[0].2 - 1.0).abs() < 1e-6,
            "a vector must be maximally similar to itself, got {}",
            hits[0].2
        );
        assert_eq!(hits[1].1, intro(2));
        assert!(
            (hits[1].2 + 1.0).abs() < 1e-6,
            "a vector must be minimally similar to its negation, got {}",
            hits[1].2
        );
    }

    /// A wrong-width vector is rejected at insert, not scored at query.
    ///
    /// This is the §2 "fix the abstraction" test: the failure lands on the
    /// thing that produced the bad vector, once, rather than on every query
    /// that later touches it.
    #[test]
    fn a_wrong_width_vector_is_rejected_at_insert() {
        let index = SemanticIndex::new();
        let err = index
            .insert_package(lineage("a"), vec![(intro(1), vec![1.0, 0.0])], 3)
            .expect_err("a 2-wide vector must not enter a 3-dimensional index");
        assert!(
            matches!(
                err,
                Error::DimensionMismatch {
                    expected: 3,
                    got: 2
                }
            ),
            "got {err:?}"
        );
        assert_eq!(index.covered(), 0, "a rejected package is not covered");
    }

    /// A NaN component is rejected at insert.
    #[test]
    fn a_non_finite_component_is_rejected_at_insert() {
        let index = SemanticIndex::new();
        let err = index
            .insert_package(lineage("a"), vec![(intro(1), vec![1.0, f32::NAN, 0.0])], 3)
            .expect_err("NaN must not enter the index");
        assert!(
            matches!(err, Error::NonFinite { index: 1 }),
            "the error must name which component was bad, got {err:?}"
        );
    }

    /// A package with no documentable symbols is *indexed*, not *missing*.
    ///
    /// The distinction the whole coverage mechanism rests on: if this were
    /// derived from the vector map, an empty package would leave the section
    /// permanently `Building` and the reader would be told the answer was
    /// incomplete forever.
    #[test]
    fn a_package_that_embeds_to_nothing_still_counts_as_covered() {
        let index = SemanticIndex::new();
        index
            .insert_package(lineage("empty"), Vec::new(), 3)
            .expect("an empty package is a valid index entry");
        assert_eq!(index.covered(), 1);
        assert!(index.contains(&lineage("empty")));
        assert!(index.nearest(&[1.0, 0.0, 0.0], 10).is_empty());
    }

    /// Re-indexing a lineage replaces its vectors rather than appending them.
    ///
    /// `select_version` re-points the corpus at a different generation of the
    /// same lineage; an additive index would then rank the old generation's
    /// symbols alongside the new one's, and both would resolve through the same
    /// `StableRef` lookup.
    #[test]
    fn re_indexing_a_package_replaces_its_vectors() {
        let index = SemanticIndex::new();
        index
            .insert_package(lineage("a"), vec![(intro(1), vec![1.0, 0.0, 0.0])], 3)
            .unwrap();
        index
            .insert_package(lineage("a"), vec![(intro(2), vec![1.0, 0.0, 0.0])], 3)
            .unwrap();

        let hits = index.nearest(&[1.0, 0.0, 0.0], 10);
        assert_eq!(
            hits.len(),
            1,
            "the old generation must be gone, got {hits:?}"
        );
        assert_eq!(hits[0].1, intro(2));
        assert_eq!(index.covered(), 1, "one lineage, indexed twice, is one");
    }

    /// A vector from a differently-sized model is skipped, not scored.
    #[test]
    fn vectors_of_another_width_are_never_scored() {
        let index = SemanticIndex::new();
        index
            .insert_package(lineage("a"), vec![(intro(1), vec![1.0, 0.0])], 2)
            .unwrap();
        assert!(
            index.nearest(&[1.0, 0.0, 0.0], 10).is_empty(),
            "a 2-wide vector must not be scored against a 3-wide query"
        );
    }

    /// The order is total: two symbols with identical vectors still have a
    /// fixed relative order, taken from their identity.
    #[test]
    fn equal_scores_are_ordered_by_identity_not_by_walk_order() {
        let index = SemanticIndex::new();
        index
            .insert_package(
                lineage("b"),
                vec![(intro(9), vec![1.0, 0.0]), (intro(2), vec![1.0, 0.0])],
                2,
            )
            .unwrap();
        index
            .insert_package(lineage("a"), vec![(intro(5), vec![1.0, 0.0])], 2)
            .unwrap();

        let hits = index.nearest(&[1.0, 0.0], 10);
        let order: Vec<(String, u8)> = hits
            .iter()
            .map(|(l, i, _)| (l.name.as_str().to_owned(), i.as_bytes()[0]))
            .collect();
        assert_eq!(
            order,
            vec![
                ("a".to_owned(), 5),
                ("b".to_owned(), 2),
                ("b".to_owned(), 9),
            ],
            "ties must break on (lineage, intro)"
        );
    }

    /// A zero vector scores zero rather than NaN.
    #[test]
    #[allow(clippy::float_cmp)] // must be *exactly* 0.0, not NaN
    fn a_zero_vector_scores_zero_and_does_not_poison_the_sort() {
        let index = SemanticIndex::new();
        index
            .insert_package(
                lineage("a"),
                vec![(intro(1), vec![0.0, 0.0]), (intro(2), vec![1.0, 0.0])],
                2,
            )
            .unwrap();
        let hits = index.nearest(&[1.0, 0.0], 10);
        assert_eq!(hits[0].1, intro(2));
        assert_eq!(
            hits[1].2, 0.0,
            "a zero vector must score exactly 0, not NaN"
        );
    }
}

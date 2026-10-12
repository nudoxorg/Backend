//! Opt-in bounded worker observations, never query or coverage authority.

use serde::Serialize;
use std::time::{Duration, Instant};

fn requested(value: Option<&std::ffi::OsStr>) -> bool {
    value.and_then(std::ffi::OsStr::to_str) == Some("1")
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub(super) enum Phase {
    #[default]
    NotRun,
    // Includes unwind time if a phase panics; the final outcome says whether
    // preparation succeeded. This is not an admission or completion claim.
    Observed {
        elapsed_ns: u128,
    },
}

pub(super) fn measure<T>(phase: Option<&mut Phase>, work: impl FnOnce() -> T) -> T {
    let Some(phase) = phase else {
        return work();
    };
    struct Observation<'a> {
        phase: &'a mut Phase,
        start: Instant,
    }
    impl Drop for Observation<'_> {
        fn drop(&mut self) {
            *self.phase = Phase::Observed {
                elapsed_ns: self.start.elapsed().as_nanos(),
            };
        }
    }
    let _observation = Observation {
        phase,
        start: Instant::now(),
    };
    work()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub(super) struct Counts {
    corpus_builds: u64,
    projection_builds: u64,
    projection_opens: u64,
}

impl Counts {
    #[cfg(feature = "cluster-process-journey-hooks")]
    pub(super) fn capture(owner: &super::SearchSnapshotOwner) -> Self {
        Self {
            corpus_builds: owner.corpus_builds(),
            projection_builds: owner.projection_builds(),
            projection_opens: owner.projection_opens(),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
pub(super) struct Generations {
    owner_calls: u64,
    hits: u64,
}

impl Generations {
    #[cfg(feature = "cluster-process-journey-hooks")]
    pub(super) fn capture(
        owner: &super::super::super::generation_residence::SemanticGenerationResidence,
    ) -> Self {
        Self {
            owner_calls: owner.owner_calls(),
            hits: owner.hits(),
        }
    }
}

#[derive(Serialize)]
pub(super) struct Phases {
    pub(super) indexed_sources: Phase,
    pub(super) semantic_corpus: Phase,
    pub(super) lexical_projection: Phase,
}

#[derive(Serialize)]
pub(super) struct Diagnostic {
    schema: &'static str,
    // Only captured identities and counts are emitted: no paths, source text,
    // arbitrary error strings or environment values can grow the receipt.
    workspace: String,
    view: String,
    view_version: String,
    source: Source,
    #[serde(skip)]
    started: Instant,
    before: Counts,
    pub(super) phases: Phases,
    pub(super) corpus: Option<Corpus>,
    pub(super) generation_after: Option<Generations>,
    generation_before: Generations,
    pub(super) lexical_binding: Option<LexicalBinding>,
}

#[derive(Serialize)]
struct Source {
    root: String,
    object: String,
    branch: String,
    log: String,
    schema: u16,
}

impl Source {
    fn from_basis(source: backend_engine::Basis) -> Self {
        Self {
            root: backend_engine::encode_id(source.root.as_bytes()),
            object: backend_engine::encode_id(source.object.as_bytes()),
            branch: backend_engine::encode_id(source.branch.as_bytes()),
            log: backend_engine::encode_id(source.log.as_bytes()),
            schema: source.schema,
        }
    }
}

#[derive(Serialize)]
pub(super) struct Corpus {
    workspace: String,
    evidence_digest: String,
    facts: usize,
}

impl Corpus {
    #[cfg(feature = "cluster-process-journey-hooks")]
    pub(super) fn capture(corpus: &backend_extension_trustfall::SemanticQueryCorpus) -> Self {
        Self {
            workspace: backend_engine::encode_id(corpus.workspace().as_bytes()),
            evidence_digest: backend_engine::encode_id(&corpus.evidence_digest()),
            facts: corpus.facts().len(),
        }
    }
}

#[derive(Serialize)]
pub(super) struct LexicalBinding {
    workspace: String,
    root: String,
    recipe: String,
    authority: String,
    read_manifest: String,
    frontier: String,
}

impl LexicalBinding {
    #[cfg(feature = "cluster-process-journey-hooks")]
    pub(super) fn capture(binding: backend_extension_tantivy::Binding) -> Self {
        Self {
            workspace: backend_engine::encode_id(binding.workspace.as_bytes()),
            root: backend_engine::encode_id(binding.root.as_bytes()),
            recipe: backend_engine::encode_id(binding.recipe.as_bytes()),
            authority: backend_engine::encode_id(binding.authority.as_bytes()),
            read_manifest: backend_engine::encode_id(binding.read_manifest.as_bytes()),
            frontier: backend_engine::encode_id(binding.frontier.as_bytes()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Outcome {
    Prepared,
    CaptureRefused,
    ProjectionRefused,
    Cancelled,
    Panicked,
    Stopped,
}

impl From<&Result<(), super::Failure>> for Outcome {
    fn from(result: &Result<(), super::Failure>) -> Self {
        match result {
            Ok(()) => Self::Prepared,
            Err(super::Failure::Capture(_)) => Self::CaptureRefused,
            Err(super::Failure::Preparation(_)) => Self::ProjectionRefused,
            Err(super::Failure::Cancelled) => Self::Cancelled,
            Err(super::Failure::Panicked) => Self::Panicked,
            Err(super::Failure::Stopped) => Self::Stopped,
        }
    }
}

#[derive(Serialize)]
struct Receipt<'a> {
    // The pair preserves actual counter totals; interpret their difference.
    // Projection counts follow the transferred owner lineage. corpus_builds
    // counts newly admitted corpora on this transferred worker owner.
    #[serde(flatten)]
    diagnostic: &'a Diagnostic,
    elapsed_ns: u128,
    after: Counts,
    outcome: Outcome,
    // Worker completion precedes the control owner's current-view admission.
    authority: &'static str,
}

impl Diagnostic {
    #[cfg(feature = "cluster-process-journey-hooks")]
    pub(super) fn capture(
        capture: &super::Capture,
        projection: &super::SearchSnapshotOwner,
    ) -> Self {
        Self {
            schema: "nudox.search-preparation-worker.v1",
            workspace: backend_engine::encode_id(capture.snapshot.root().as_bytes()),
            view: backend_engine::encode_id(capture.view.root().as_bytes()),
            view_version: backend_engine::encode_id(capture.view.version().as_bytes()),
            source: Source::from_basis(capture.view.basis()),
            started: Instant::now(),
            before: Counts::capture(projection),
            phases: Phases {
                indexed_sources: Phase::NotRun,
                semantic_corpus: Phase::NotRun,
                lexical_projection: Phase::NotRun,
            },
            corpus: None,
            generation_before: Generations::capture(&capture.generations),
            generation_after: None,
            lexical_binding: None,
        }
    }

    #[cfg(feature = "cluster-process-journey-hooks")]
    pub(super) fn from_environment(
        capture: &super::Capture,
        projection: &super::SearchSnapshotOwner,
    ) -> Option<Self> {
        requested(std::env::var_os("BACKEND_JOURNEY_SEARCH_PREPARATION").as_deref())
            .then(|| Self::capture(capture, projection))
    }

    fn bytes(
        &self,
        after: Counts,
        outcome: Outcome,
        elapsed: Duration,
    ) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(&Receipt {
            diagnostic: self,
            elapsed_ns: elapsed.as_nanos(),
            after,
            outcome,
            authority: "worker-observation-only",
        })
    }

    #[cfg(feature = "cluster-process-journey-hooks")]
    pub(super) fn emit(
        &self,
        projection: &super::SearchSnapshotOwner,
        result: &Result<(), super::Failure>,
    ) {
        if let Ok(bytes) = self.bytes(
            Counts::capture(projection),
            Outcome::from(result),
            self.started.elapsed(),
        ) {
            // Diagnostics never replace the genuine result, including a
            // closed stderr. All fields are fixed identities and scalars.
            use std::io::Write as _;
            if bytes.len() <= 4096 {
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "journey search preparation: {}",
                    String::from_utf8_lossy(&bytes)
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::*;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    #[test]
    fn source_projection_preserves_basis_root_distinct_from_visible_view() {
        let source_root = backend_library::view_state_root(&[]);
        let basis = backend_library::Basis::new(
            source_root,
            backend_library::object_version(b"distinct source object"),
        );
        let row = backend_library::Row::new(
            backend_library::RowId::Symbol(backend_library::symbol_key("visible row")),
            basis,
            "visible row",
        );
        let view = backend_library::ViewRoot::new_incomplete(
            backend_library::view_key(b"diagnostic-source-projection"),
            basis,
            backend_library::Cursor::at(source_root, 0).frontier(),
            vec![row],
            Vec::new(),
        )
        .expect("actual immutable view with distinct visible and source roots");
        assert_ne!(view.root(), view.basis().root);
        let source = Source::from_basis(view.basis());
        assert_eq!(
            source.root,
            backend_engine::encode_id(basis.root.as_bytes())
        );
        assert_ne!(
            source.root,
            backend_engine::encode_id(view.root().as_bytes())
        );
        assert_eq!(
            source.object,
            backend_engine::encode_id(basis.object.as_bytes())
        );
        assert_eq!(
            source.branch,
            backend_engine::encode_id(basis.branch.as_bytes())
        );
        assert_eq!(source.log, backend_engine::encode_id(basis.log.as_bytes()));
        assert_eq!(source.schema, basis.schema);
    }

    #[test]
    fn diagnostic_requires_exact_explicit_opt_in() {
        use std::ffi::OsStr;
        assert!(!requested(None));
        assert!(requested(Some(OsStr::new("1"))));
        for value in ["", "0", "01", "true", "1\n", " 1"] {
            assert!(!requested(Some(OsStr::new(value))));
        }
    }

    #[test]
    fn phase_observation_distinguishes_not_run_returned_error_and_unwind() {
        let mut skipped = Phase::NotRun;
        assert_eq!(measure(None, || 7), 7);
        assert_eq!(skipped, Phase::NotRun);
        let refused: Result<(), &'static str> =
            measure(Some(&mut skipped), || Err("actual refusal"));
        assert_eq!(refused, Err("actual refusal"));
        assert!(matches!(skipped, Phase::Observed { .. }));
        let mut unwound = Phase::NotRun;
        assert!(
            catch_unwind(AssertUnwindSafe(|| measure(Some(&mut unwound), || panic!(
                "fixture"
            ))))
            .is_err()
        );
        assert!(matches!(unwound, Phase::Observed { .. }));
    }

    #[test]
    fn bounded_receipt_preserves_unrun_phases_failed_outcome_basis_and_counter_totals() {
        let hex = "f".repeat(64);
        let before = Counts {
            corpus_builds: u64::MAX,
            projection_builds: u64::MAX,
            projection_opens: u64::MAX,
        };
        let mut observation = Diagnostic {
            schema: "nudox.search-preparation-worker.v1",
            workspace: hex.clone(),
            view: hex.clone(),
            view_version: hex.clone(),
            source: Source {
                root: "a".repeat(64),
                object: "b".repeat(64),
                branch: "c".repeat(64),
                log: "d".repeat(64),
                schema: u16::MAX,
            },
            started: Instant::now(),
            before,
            phases: Phases {
                indexed_sources: Phase::Observed {
                    elapsed_ns: u128::MAX,
                },
                semantic_corpus: Phase::Observed {
                    elapsed_ns: u128::MAX,
                },
                lexical_projection: Phase::Observed {
                    elapsed_ns: u128::MAX,
                },
            },
            corpus: Some(Corpus {
                workspace: hex.clone(),
                evidence_digest: hex.clone(),
                facts: usize::MAX,
            }),
            generation_before: Generations {
                owner_calls: u64::MAX,
                hits: u64::MAX,
            },
            generation_after: None,
            lexical_binding: Some(LexicalBinding {
                workspace: hex.clone(),
                root: hex.clone(),
                recipe: hex.clone(),
                authority: hex.clone(),
                read_manifest: hex.clone(),
                frontier: hex.clone(),
            }),
        };
        let bytes = observation
            .bytes(before, Outcome::Cancelled, observation.started.elapsed())
            .expect("bounded fixture receipt");
        assert!(
            bytes.len() <= 4096,
            "fixed receipt exceeds diagnostic envelope"
        );
        let json: serde_json::Value =
            serde_json::from_slice(&bytes).expect("actual serialized diagnostic");
        assert_eq!(json["authority"], "worker-observation-only");
        assert_eq!(json["outcome"], "cancelled");
        assert_eq!(json["phases"]["semantic_corpus"]["state"], "observed");
        assert_eq!(json["phases"]["lexical_projection"]["state"], "observed");
        assert_eq!(json["phases"]["indexed_sources"]["state"], "observed");
        assert_eq!(json["view"], hex);
        assert_eq!(json["source"]["root"], "a".repeat(64));
        assert_eq!(json["source"]["object"], "b".repeat(64));
        assert_eq!(json["source"]["branch"], "c".repeat(64));
        assert_eq!(json["source"]["log"], "d".repeat(64));
        assert_ne!(json["source"]["root"], json["view"]);
        assert_eq!(json["corpus"]["evidence_digest"], hex);
        assert_eq!(json["lexical_binding"]["read_manifest"], hex);
        assert_eq!(json["before"]["projection_opens"], u64::MAX);
        assert_eq!(json["after"]["projection_opens"], u64::MAX);
        assert!(json.get("started").is_none());
        observation.phases.semantic_corpus = Phase::NotRun;
        observation.phases.lexical_projection = Phase::NotRun;
        observation.corpus = None;
        observation.lexical_binding = None;
        let refused: serde_json::Value = serde_json::from_slice(
            &observation
                .bytes(before, Outcome::CaptureRefused, Duration::ZERO)
                .expect("partial diagnostic receipt"),
        )
        .expect("typed refused receipt");
        assert_eq!(refused["outcome"], "capture-refused");
        assert_eq!(refused["phases"]["semantic_corpus"]["state"], "not-run");
        assert_eq!(refused["phases"]["lexical_projection"]["state"], "not-run");
        assert!(refused["lexical_binding"].is_null());
        assert!(refused["corpus"].is_null());
    }

    #[test]
    fn counts_distinguish_corpus_build_projection_build_and_durable_open() {
        let before = Counts {
            corpus_builds: 0,
            projection_builds: 2,
            projection_opens: 0,
        };
        let built = Counts {
            corpus_builds: 1,
            projection_builds: 3,
            projection_opens: 0,
        };
        let restored = Counts {
            corpus_builds: 1,
            projection_builds: 2,
            projection_opens: 1,
        };
        assert_ne!(before, built);
        assert_ne!(built, restored);
        assert_eq!(built.projection_builds - before.projection_builds, 1);
        assert_eq!(restored.projection_opens - before.projection_opens, 1);
        assert_eq!(Outcome::from(&Ok(())), Outcome::Prepared);
        assert_eq!(
            Outcome::from(&Err(super::super::Failure::Cancelled)),
            Outcome::Cancelled
        );
        assert_eq!(
            Outcome::from(&Err(super::super::Failure::Panicked)),
            Outcome::Panicked
        );
    }
}

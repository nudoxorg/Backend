//! Opt-in bounded worker observations, never query or coverage authority.

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const FILE_NAME: &str = "search-preparation-worker-v1.json";
const MAX_BYTES: usize = 4096;
const MAX_RECORDS_PER_PROCESS: usize = 64;
static RECORDS: AtomicUsize = AtomicUsize::new(0);

fn requested(value: Option<&std::ffi::OsStr>) -> bool {
    value.and_then(std::ffi::OsStr::to_str) == Some("1")
}

fn reserve_record(records: &AtomicUsize) -> bool {
    records
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
            (count < MAX_RECORDS_PER_PROCESS).then_some(count + 1)
        })
        .is_ok()
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
    // A residence snapshot, never an inferred I/O or decode-byte counter.
    resident_encoded_image_bytes: usize,
    resident_images: usize,
}

impl Generations {
    pub(super) fn capture(
        owner: &super::super::super::generation_residence::SemanticGenerationResidence,
    ) -> Self {
        let (resident_encoded_image_bytes, resident_images) = owner.resident_image_weight();
        Self {
            owner_calls: owner.owner_calls(),
            hits: owner.hits(),
            resident_encoded_image_bytes,
            resident_images,
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
    process_id: u32,
    // Only captured identities and counts are emitted: no paths, source text,
    // arbitrary error strings or environment values can grow the receipt.
    workspace: String,
    view: String,
    view_version: String,
    source: Source,
    #[serde(skip)]
    started: Instant,
    #[serde(skip)]
    destination: Option<PathBuf>,
    before: Counts,
    pub(super) phases: Phases,
    pub(super) corpus: Option<Corpus>,
    pub(super) generation_after: Option<Generations>,
    generation_before: Generations,
    pub(super) lexical_binding: Option<LexicalBinding>,
    pub(super) selection: Option<SelectionTrace>,
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

#[derive(Serialize)]
pub(super) struct SelectionTrace {
    row_evidence_join_ns: Option<u128>,
    relation_state_ns: Option<u128>,
    document_state_ns: Option<u128>,
    projection_open_advance_ns: Option<u128>,
    view_rows: Option<u64>,
    evidence_facts: Option<usize>,
    lexical_documents: Option<usize>,
    selected_entities: Option<usize>,
    previous_view: Option<String>,
    previous_binding: Option<LexicalBinding>,
    same_view: Option<bool>,
    same_workspace: Option<bool>,
    same_relation_root: Option<bool>,
    same_binding: Option<bool>,
    maintenance: Option<&'static str>,
}

impl SelectionTrace {
    pub(super) fn capture(
        observed: super::super::super::query::SelectionPreparationDiagnostic,
    ) -> Self {
        Self {
            row_evidence_join_ns: observed.row_evidence_join_ns,
            relation_state_ns: observed.relation_state_ns,
            document_state_ns: observed.document_state_ns,
            projection_open_advance_ns: observed.projection_open_advance_ns,
            view_rows: observed.view_rows,
            evidence_facts: observed.evidence_facts,
            lexical_documents: observed.lexical_documents,
            selected_entities: observed.selected_entities,
            previous_view: observed.previous_view,
            previous_binding: observed.previous_binding.map(LexicalBinding::capture),
            same_view: observed.same_view,
            same_workspace: observed.same_workspace,
            same_relation_root: observed.same_relation_root,
            same_binding: observed.same_binding,
            maintenance: observed.maintenance,
        }
    }
}

impl LexicalBinding {
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
    pub(super) fn capture(
        capture: &super::Capture,
        projection: &super::SearchSnapshotOwner,
    ) -> Self {
        Self {
            schema: "nudox.search-preparation-worker.v1",
            process_id: std::process::id(),
            workspace: backend_engine::encode_id(capture.snapshot.root().as_bytes()),
            view: backend_engine::encode_id(capture.view.root().as_bytes()),
            view_version: backend_engine::encode_id(capture.view.version().as_bytes()),
            source: Source::from_basis(capture.view.basis()),
            started: Instant::now(),
            destination: capture
                .diagnostic_workspace
                .as_ref()
                .map(|path| path.join(FILE_NAME)),
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
            selection: None,
        }
    }

    pub(super) fn from_environment(
        capture: &super::Capture,
        projection: &super::SearchSnapshotOwner,
    ) -> Option<Self> {
        if !requested(std::env::var_os("BACKEND_JOURNEY_SEARCH_PREPARATION").as_deref()) {
            return None;
        }
        reserve_record(&RECORDS).then(|| Self::capture(capture, projection))
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
            if bytes.len() <= MAX_BYTES {
                publish(self.destination.as_deref(), &bytes);
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "journey search preparation: {}",
                    String::from_utf8_lossy(&bytes)
                );
            }
        }
    }
}

fn publish(destination: Option<&Path>, bytes: &[u8]) {
    // Automatic CLI startup intentionally closes locald's stderr. Reuse the
    // existing startup-timing writer for one bounded latest observation in
    // the admitted workspace. No append log, durability barrier, directory
    // creation or query-authority input is introduced; failures are ignored.
    if let Some(destination) = destination.filter(|_| bytes.len() <= MAX_BYTES) {
        let _ = backend_platform::durable::write_private_atomic_unflushed(destination, bytes);
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
    fn diagnostic_record_reservation_is_process_bounded() {
        let records = AtomicUsize::new(0);
        for _ in 0..MAX_RECORDS_PER_PROCESS {
            assert!(reserve_record(&records));
        }
        assert!(!reserve_record(&records));
        assert_eq!(records.load(Ordering::Relaxed), MAX_RECORDS_PER_PROCESS);
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
            process_id: u32::MAX,
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
            destination: Some(PathBuf::from("path must never be serialized")),
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
                resident_encoded_image_bytes: usize::MAX,
                resident_images: usize::MAX,
            },
            generation_after: Some(Generations {
                owner_calls: u64::MAX,
                hits: u64::MAX,
                resident_encoded_image_bytes: usize::MAX,
                resident_images: usize::MAX,
            }),
            lexical_binding: Some(LexicalBinding {
                workspace: hex.clone(),
                root: hex.clone(),
                recipe: hex.clone(),
                authority: hex.clone(),
                read_manifest: hex.clone(),
                frontier: hex.clone(),
            }),
            selection: Some(SelectionTrace {
                row_evidence_join_ns: Some(u128::MAX),
                relation_state_ns: Some(u128::MAX),
                document_state_ns: Some(u128::MAX),
                projection_open_advance_ns: Some(u128::MAX),
                view_rows: Some(u64::MAX),
                evidence_facts: Some(usize::MAX),
                lexical_documents: Some(usize::MAX),
                selected_entities: Some(usize::MAX),
                previous_view: Some(hex.clone()),
                previous_binding: Some(LexicalBinding {
                    workspace: hex.clone(),
                    root: hex.clone(),
                    recipe: hex.clone(),
                    authority: hex.clone(),
                    read_manifest: hex.clone(),
                    frontier: hex.clone(),
                }),
                same_view: Some(false),
                same_workspace: Some(false),
                same_relation_root: Some(false),
                same_binding: Some(false),
                maintenance: Some("rebound"),
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
        assert_eq!(json["process_id"], u32::MAX);
        assert_eq!(
            json["generation_before"]["resident_encoded_image_bytes"],
            usize::MAX
        );
        assert_eq!(json["generation_before"]["resident_images"], usize::MAX);
        assert_eq!(
            json["generation_after"]["resident_encoded_image_bytes"],
            usize::MAX
        );
        assert!(json.get("started").is_none());
        assert!(json.get("destination").is_none());
        observation.phases.semantic_corpus = Phase::NotRun;
        observation.phases.lexical_projection = Phase::NotRun;
        observation.corpus = None;
        observation.generation_after = None;
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
        assert!(refused["generation_after"].is_null());
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

    #[cfg(unix)]
    #[test]
    fn shared_observation_parent_is_refused_without_creating_output() {
        use std::os::unix::fs::PermissionsExt as _;
        let directory = tempfile::tempdir().expect("observation parent");
        // Set the deliberately invalid mode explicitly too: this negative
        // control must keep rejecting it even under an owner-only umask.
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o755))
            .expect("nonprivate parent");
        let destination = directory.path().join(FILE_NAME);
        let error = backend_platform::durable::write_private_atomic_unflushed(
            &destination,
            b"must remain unpublished",
        )
        .expect_err("nonprivate parents do not admit private diagnostics");
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        publish(
            Some(&destination),
            b"optional diagnostic failure remains nonfatal",
        );
        assert!(!destination.exists());
        assert_eq!(
            std::fs::read_dir(directory.path())
                .expect("parent entries")
                .count(),
            0,
            "no destination or temporary bytes may be created in a rejected parent"
        );
    }

    #[cfg(unix)]
    #[test]
    fn latest_observation_is_private_bounded_and_never_follows_a_destination_link() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};
        // tempfile's default directory mode follows the process umask. The
        // publisher requires an owner-only parent, just like the admitted
        // production workspace; make that requirement explicit at creation.
        let directory = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .expect("private observation directory");
        let destination = directory.path().join(FILE_NAME);
        publish(None, b"not opted in");
        assert!(!destination.exists());
        publish(Some(&destination), b"first complete observation");
        assert_eq!(
            std::fs::read(&destination).expect("first bytes"),
            b"first complete observation"
        );
        assert_eq!(
            std::fs::metadata(&destination)
                .expect("private metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        publish(Some(&destination), b"second complete observation");
        publish(Some(&destination), &vec![0; MAX_BYTES + 1]);
        assert_eq!(
            std::fs::read(&destination).expect("latest bytes"),
            b"second complete observation"
        );

        let outside = directory.path().join("unrelated");
        std::fs::write(&outside, b"preserve unrelated bytes").expect("unrelated fixture");
        std::fs::remove_file(&destination).expect("replace only owned fixture");
        symlink(&outside, &destination).expect("unsafe destination fixture");
        publish(Some(&destination), b"must not follow");
        assert_eq!(
            std::fs::read(&outside).expect("unrelated remains"),
            b"preserve unrelated bytes"
        );
        assert!(
            std::fs::symlink_metadata(&destination)
                .expect("link remains")
                .file_type()
                .is_symlink()
        );
    }
}

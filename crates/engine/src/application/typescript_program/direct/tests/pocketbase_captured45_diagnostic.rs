//! One explicitly ignored, Root-managed diagnostic over retained real inputs.
//! This does not run during ordinary tests, discover a corpus, or install tools.

use super::*;
use crate::application::compiler_typescript_observation::{
    ObservedStopSite, ObservedTypeScriptBudget,
};
use backend_frontend_typescript::{
    TszFileWorkPolicy, TszProjectAuthority, TszProjectCheckerExecution,
};
use std::sync::{Arc, OnceLock};
use tracing_subscriber::{Layer, layer::SubscriberExt};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FilePin {
    path: String,
    bytes: usize,
    sha256: String,
    content_version: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Capture {
    corpus_commit: String,
    corpus_tree: String,
    source_root: PathBuf,
    source_capture_version: String,
    input_digest: String,
    profile: [u8; 2],
    source_count: usize,
    source_bytes: usize,
    capture_proof_sha256: String,
    borrowed_full_source_inventory_sha256: String,
    sources: Vec<FilePin>,
    configs: Vec<FilePin>,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

// The captured source relation predates the current typed ContentId encoding.
// Preserve its recorded v2 source-fact witness as capture metadata; the admitted
// PackageSource below computes the current SourceFactDomain ContentId from bytes.
fn captured_source_fact_version(bytes: &[u8]) -> [u8; 32] {
    let payload_bytes = u64::try_from(bytes.len()).expect("captured source length fits u64");
    let encoded_payload_bytes = 8_u64
        .checked_add(payload_bytes)
        .expect("captured source length plus prefix fits u64");
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.version.v2\0");
    hasher.update(&[0x56, 0x44]);
    hasher.update(&1_u16.to_be_bytes());
    hasher.update(&[1]);
    hasher.update(&encoded_payload_bytes.to_be_bytes());
    hasher.update(&payload_bytes.to_be_bytes());
    hasher.update(bytes);
    *hasher.finalize().as_bytes()
}

fn verified_source(root: &Path, pin: &FilePin) -> String {
    PackageSource::validate_path(&pin.path).expect("normalized captured path");
    let bytes = fs::read(root.join(&pin.path)).expect("exact retained source must exist");
    assert_eq!(bytes.len(), pin.bytes, "captured length {}", pin.path);
    assert_eq!(
        hex(&Sha256::digest(&bytes)),
        pin.sha256,
        "captured SHA256 {}",
        pin.path
    );
    assert_eq!(
        hex(&captured_source_fact_version(&bytes)),
        pin.content_version,
        "captured legacy source-fact version {}",
        pin.path
    );
    String::from_utf8(bytes).expect("captured UTF-8")
}

fn record(value: serde_json::Value) {
    eprintln!("POCKETBASE_TSZ_DIAGNOSTIC {value}");
}

#[test]
#[ignore = "requires exact retained PocketBase corpus, fresh fleet guard and held native permit"]
fn pocketbase_exact_captured45_reports_native_program_and_checker_phases() {
    let capture: Capture =
        serde_json::from_str(include_str!("pocketbase_captured45_manifest.json"))
            .expect("sealed capture manifest");
    assert_eq!(
        capture.input_digest,
        "d8775fe22daf39cc8449cc1d5f27b957abb61e3884c4c2a1f695bcfdfca779e1"
    );
    assert_eq!(capture.profile, [1, 0]);
    assert_eq!(capture.source_count, 45);
    assert_eq!(capture.sources.len(), capture.source_count);
    assert_eq!(capture.source_bytes, 1_266_249);
    assert_eq!(capture.configs.len(), 3);
    assert!(
        capture
            .sources
            .windows(2)
            .all(|pair| pair[0].path < pair[1].path)
    );
    assert_eq!(
        capture.source_root.canonicalize().unwrap(),
        capture.source_root
    );
    let texts = capture
        .sources
        .iter()
        .map(|pin| verified_source(&capture.source_root, pin))
        .collect::<Vec<_>>();
    assert_eq!(
        texts.iter().map(String::len).sum::<usize>(),
        capture.source_bytes
    );
    for config in &capture.configs {
        verified_source(&capture.source_root, config);
    }
    let sources = capture
        .sources
        .iter()
        .zip(&texts)
        .map(|(pin, source)| {
            PackageSource::new(&pin.path, source).expect("exact sorted package source")
        })
        .collect::<Vec<_>>();

    // Root's enclosing admission must freshly verify the borrowed complete
    // source/dependency inventory. This test verifies the45+3 bytes itself;
    // dependency reads then acquire the production resolver's sealed witness.
    record(serde_json::json!({
        "phase":"captured-input-verified", "corpus_commit":capture.corpus_commit,
        "corpus_tree":capture.corpus_tree, "source_capture_version":capture.source_capture_version,
        "input_digest":capture.input_digest, "source_count":sources.len(),
        "source_bytes":capture.source_bytes, "capture_proof_sha256":capture.capture_proof_sha256,
        "borrowed_full_source_inventory_sha256":capture.borrowed_full_source_inventory_sha256,
        "prior_dependency_equality":"requires enclosing fresh inventory admission",
        "publication_acceptance":false
    }));
    // A single absolute600s deadline includes program admission and every checker
    // round. No per-file/retry reset and no whole-app polling loop.
    let started = Instant::now();
    let deadline = started + Duration::from_secs(600);
    let cancelled = AtomicBool::new(false);
    let layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(std::io::stderr)
        .with_filter(tracing_subscriber::filter::filter_fn(|metadata| {
            metadata.target() == "compiler.typescript_project"
        }));
    tracing::subscriber::set_global_default(tracing_subscriber::registry().with(layer))
        .expect("one exact diagnostic body per fresh native test process");
    assert!(
        tracing::enabled!(target:"compiler.typescript_project", tracing::Level::DEBUG),
        "diagnostic phase events must be enabled; an empty trace is not evidence"
    );
    let host = TypeScriptProjectHost::direct(
        None,
        ToolchainProbeLimits::new(Duration::from_secs(5), NonZeroUsize::new(1024).unwrap())
            .unwrap(),
    );
    let admitted = host
        .admit(&capture.source_root)
        .unwrap()
        .expect("native direct host");
    let input = admitted.inputs();
    assert!(matches!(
        input.authority,
        TypeScriptProjectInputAuthority::Native
    ));
    assert!(
        input.config_candidates.is_empty(),
        "exact corpus has no tsconfig/jsconfig"
    );
    assert_eq!(
        input.workspace_root,
        capture.source_root.as_path(),
        "same captured workspace boundary"
    );
    let forest = build_native_forest_inputs(&input, &sources, deadline, &cancelled)
        .expect("actual native program admission must complete before checking");
    assert!(!forest.configured_forest);
    assert_eq!(
        forest.nodes.len(),
        1,
        "exact inferred corpus has one program"
    );
    assert_eq!(forest.package_membership.len(), 45);
    assert!(
        forest
            .package_membership
            .values()
            .all(|member| matches!(member, TypeScriptForestSourceMembership::Selected { .. })),
        "none of the45 roots may be dropped"
    );
    let stop_site: Arc<OnceLock<ObservedStopSite>> = Arc::new(OnceLock::new());
    let budget = ObservedTypeScriptBudget::new(deadline, &cancelled, forest.work_units)
        .with_stop_sink(Arc::clone(&stop_site));
    let mut authorities = Vec::new();
    for (index, node) in forest.nodes.into_iter().enumerate() {
        let native = node.native;
        native
            .closure_witness
            .validate_current(admitted.witness())
            .unwrap();
        // Actual expanded members are emitted BEFORE checker preparation. They
        // include dependency sources and linked libraries, not just45 roots.
        let members = native.sources.iter().map(|source| serde_json::json!({
            "path":source.path, "bytes":source.source.len(),
            "sha256":hex(&Sha256::digest(source.source.as_bytes())),
            "content_version":hex(ContentId::<SourceFactDomain>::from_canonical_bytes(source.source.as_bytes()).as_ref())
        })).collect::<Vec<_>>();
        let libraries = native
            .libraries
            .iter()
            .map(|library| {
                let source = library
                    .arena
                    .get_source_file_at(library.root_index)
                    .unwrap();
                serde_json::json!({"path":library.file_name, "bytes":source.text.len(),
                "sha256":hex(&Sha256::digest(source.text.as_bytes()))})
            })
            .collect::<Vec<_>>();
        let resolutions = native.options.module_resolutions.iter().map(|request| serde_json::json!({
            "importer":request.importer_path,"specifier":request.specifier,
            "kind":format!("{:?}",request.request_kind),"mode":format!("{:?}",request.resolution_mode),
            "target":format!("{:?}",request.target)
        })).collect::<Vec<_>>();
        let config = node.config.as_ref().map(|(path, content_version)| {
            serde_json::json!({"path":path,"content_version":hex(content_version.as_ref())})
        });
        record(
            serde_json::json!({"phase":"native-program-admitted-before-check", "program":index,
            "elapsed_millis":started.elapsed().as_millis(),"config":config,"references":node.references,
            "source_count":members.len(),"sources":members,"library_count":libraries.len(),"libraries":libraries,
            "module_request_count":resolutions.len(),"module_resolutions":resolutions,
            "options":format!("{:?}",native.options.checker),"environment":hex(native.options.environment.as_bytes()),
            "program_work_units":native.work_units,"forest_work_units":forest.work_units,
            "file_work_units":native.file_work_units,"checked_project_admitted":false}),
        );
        budget.admit_program(
            native.sources.len(),
            native.libraries.len(),
            native.options.module_resolutions.len(),
        );
        let _program = budget.observe_program(index);
        let mut authority = TszProjectAuthority::new();
        let result = authority.update_with_checker_execution_and_file_work(
            native.sources,
            native.options,
            &native.libraries,
            TszProjectCheckerExecution {
                checkpoint: &budget,
                workers: NonZeroUsize::new(2).unwrap(),
            },
            // Exact existing production policy, not a larger allowance or an
            // experiment-specific skip list. Any partial output fails below.
            &TszFileWorkPolicy {
                allowance: native.file_work_units,
                skipped: BTreeSet::new(),
                max_retries: 8,
                check_declaration_files: false,
            },
        );
        record(
            serde_json::json!({"phase":"checker-returned", "program":index,
            "elapsed_millis":started.elapsed().as_millis(),"result":format!("{result:?}"),
            "stop_reason":format!("{:?}",budget.stop_reason()),
            "stop_site":stop_site.get().map(|site| serde_json::json!({
                "reason":format!("{:?}",site.reason),"phase":format!("{:?}",site.phase),"file":site.file,
                "work_charged":site.work_charged,"work_allowance":site.work_allowance})),
            "has_checked_project":authority.project().is_some()}),
        );
        assert!(
            result.is_ok(),
            "incomplete/deadline/cancelled authority is diagnostic failure: {result:?}"
        );
        let project = authority.project().expect("complete checked project");
        record(
            serde_json::json!({"phase":"checked-project-result", "program":index,
            "diagnostic_count":project.check_result().diagnostic_count,
            "checked_file_count":project.check_result().file_results.len(),
            "file_work_skipped":project.file_work_skipped()}),
        );
        assert!(
            project.file_work_skipped().is_empty(),
            "partial file-work coverage is not diagnostic success"
        );
        assert!(
            budget.stop_reason().is_none(),
            "a latched stop cannot admit success"
        );
        project
            .checked_query_session(&budget)
            .expect("real checked query authority");
        native
            .closure_witness
            .validate_current(admitted.witness())
            .unwrap();
        // A captured lock/config can be irrelevant to this resolver's read set.
        // Still preserve every45+3 input pin, then poll the same absolute deadline.
        for pin in capture.sources.iter().chain(&capture.configs) {
            verified_source(&capture.source_root, pin);
        }
        backend_frontend_typescript::TszExecutionCheckpoint::checkpoint(&budget, 0)
            .expect("original deadline/cancellation still holds after postvalidation");
        authorities.push(authority);
    }
    assert_eq!(authorities.len(), 1);
    record(
        serde_json::json!({"phase":"diagnostic-complete", "elapsed_millis":started.elapsed().as_millis(),
        "complete_checked_programs":authorities.len(),"publication_acceptance":false}),
    );
}

//! Private live witness for supervised native Stylo host facts.
//!
//! The caller must obtain `source` through the confined source walk and keep
//! its revision fence until publication. This module authenticates the worker
//! response and both source identities, then drops source bytes and the worker
//! output after sealing the exact projected rows in a nonforgeable witness.
use super::stylo_worker::{StyloWorkerFailure, StyloWorkerPermit, run_stylo_worker};
use backend_engine::builtin::{
    StyloAnalysisRecord, StyloAnalysisRelation, StyloProjectionBasis, StyloTerminal,
    project_stylo_facts, stylo_host_key, stylo_parser_configuration_id, stylo_rows_digest,
};
use backend_semantic::stylo_analysis::StyloParseConfig;
use backend_version::Relation;
use std::{path::PathBuf, sync::atomic::AtomicBool, time::Duration};

/// A successful captured host and its exact projected native rows.
/// Only this module can mint the transient witness after supervised execution.
pub(super) struct StyloHostCapture {
    host_key: [u8; 32],
    host: StyloAnalysisRecord,
    facts: Vec<([u8; 32], StyloAnalysisRecord)>,
    request_digest: [u8; 32],
    producer: [u8; 32],
    recipe: String,
    artifact: [u8; 32],
    proof: [u8; 32],
    encoded_bytes: usize,
}
#[derive(Debug)]
pub(super) enum StyloCaptureFailure {
    Worker(StyloWorkerFailure),
    Limit,
    Invalid(String),
}
impl StyloHostCapture {
    pub(super) fn host_key(&self) -> [u8; 32] {
        self.host_key
    }
    pub(super) fn host(&self) -> &StyloAnalysisRecord {
        &self.host
    }
    pub(super) fn facts(&self) -> &[([u8; 32], StyloAnalysisRecord)] {
        &self.facts
    }
    pub(super) fn producer(&self) -> [u8; 32] {
        self.producer
    }
    pub(super) fn recipe(&self) -> &str {
        &self.recipe
    }
    pub(super) fn artifact(&self) -> [u8; 32] {
        self.artifact
    }
    pub(super) fn encoded_bytes(&self) -> usize {
        self.encoded_bytes
    }
    /// Rechecks the exact rows before an owner transition can use this proof.
    pub(super) fn verify(&self) -> Result<(), StyloCaptureFailure> {
        let (proof, encoded_bytes) = witness(
            self.host_key,
            &self.host,
            &self.facts,
            self.request_digest,
            self.producer,
            &self.recipe,
            self.artifact,
        )?;
        if proof != self.proof || encoded_bytes != self.encoded_bytes {
            return Err(StyloCaptureFailure::Invalid(
                "Stylo capture witness mismatch".into(),
            ));
        }
        Ok(())
    }
}
fn witness(
    host_key: [u8; 32],
    host: &StyloAnalysisRecord,
    facts: &[([u8; 32], StyloAnalysisRecord)],
    request: [u8; 32],
    producer: [u8; 32],
    recipe: &str,
    artifact: [u8; 32],
) -> Result<([u8; 32], usize), StyloCaptureFailure> {
    let mut hash = blake3::Hasher::new();
    hash.update(b"nudox.stylo-supervised-host-witness.v1\0");
    hash.update(&request);
    hash.update(&producer);
    hash.update(&(recipe.len() as u64).to_be_bytes());
    hash.update(recipe.as_bytes());
    hash.update(&artifact);
    let mut total = 0usize;
    for (key, row) in
        std::iter::once((host_key, host)).chain(facts.iter().map(|(key, row)| (*key, row)))
    {
        row.validate().map_err(StyloCaptureFailure::Invalid)?;
        let mut encoded = Vec::new();
        StyloAnalysisRelation::encode_value(row, &mut encoded);
        total = total
            .checked_add(encoded.len())
            .ok_or(StyloCaptureFailure::Limit)?;
        hash.update(&key);
        hash.update(&(encoded.len() as u64).to_be_bytes());
        hash.update(&encoded);
    }
    Ok((*hash.finalize().as_bytes(), total))
}

/// Runs Stylo against the actual captured bytes and seals the exact native
/// claims. Caller-supplied aggregate byte/record budgets are charged before
/// returning; exhausted budgets must be recorded as qualified unavailable.
pub(super) fn capture_stylo_host(
    permit: &StyloWorkerPermit,
    cwd: PathBuf,
    project: [u8; 32],
    path: &str,
    source_bytes: &[u8],
    config: &StyloParseConfig,
    wall_time: Duration,
    memory_limit: Option<usize>,
    remaining_bytes: usize,
    remaining_records: usize,
    cancellation: &AtomicBool,
) -> Result<StyloHostCapture, StyloCaptureFailure> {
    if remaining_bytes == 0 || remaining_records == 0 {
        return Err(StyloCaptureFailure::Limit);
    }
    let host_key = stylo_host_key(project, path).map_err(StyloCaptureFailure::Invalid)?;
    let run = run_stylo_worker(
        permit,
        cwd,
        source_bytes,
        config,
        wall_time,
        memory_limit,
        cancellation,
    )
    .map_err(StyloCaptureFailure::Worker)?;
    let analysis = run.analysis();
    let text = std::str::from_utf8(source_bytes)
        .map_err(|_| StyloCaptureFailure::Invalid("captured Stylo source is not UTF-8".into()))?;
    for occurrence in &analysis.occurrences {
        let start = usize::try_from(occurrence.start)
            .map_err(|_| StyloCaptureFailure::Invalid("Stylo source start overflow".into()))?;
        let end = usize::try_from(occurrence.end)
            .map_err(|_| StyloCaptureFailure::Invalid("Stylo source end overflow".into()))?;
        if text.get(start..end) != Some(occurrence.raw.as_str()) {
            return Err(StyloCaptureFailure::Invalid(
                "Stylo source occurrence does not match captured UTF-8 bytes".into(),
            ));
        }
    }
    let artifact = run.artifact().identity().to_bytes();
    let basis = StyloProjectionBasis {
        project,
        path,
        content: run.content().to_bytes(),
        source: *run.source().as_ref(),
        byte_length: run.byte_length(),
        producer: analysis.support_fingerprint,
        worker_artifact: artifact,
        adapter: super::stylo_admission::stylo_adapter_fingerprint(),
        worker_policy: run.policy(),
    };
    let (analysis_basis, projected) =
        project_stylo_facts(&basis, analysis).map_err(StyloCaptureFailure::Invalid)?;
    if projected.len().saturating_add(1) > remaining_records {
        return Err(StyloCaptureFailure::Limit);
    }
    let facts: Vec<_> = projected
        .into_iter()
        .map(|row| {
            (
                row.key,
                StyloAnalysisRecord::Fact {
                    host: host_key,
                    ordinal: row.ordinal,
                    subordinal: row.subordinal,
                    fact: row.fact,
                },
            )
        })
        .collect();
    let fact_refs: Vec<_> = facts.iter().map(|(key, row)| (*key, row)).collect();
    let fact_digest = stylo_rows_digest(&fact_refs).map_err(StyloCaptureFailure::Invalid)?;
    let host = StyloAnalysisRecord::Host {
        project,
        path: path.to_owned(),
        content: Some(run.content().to_bytes()),
        source: Some(*run.source().as_ref()),
        byte_length: Some(run.byte_length()),
        utf8_bom_removed: Some(analysis.utf8_bom_removed),
        config: Some(analysis.config.clone()),
        configuration: Some(
            stylo_parser_configuration_id(&analysis.recipe, &analysis.config)
                .map_err(StyloCaptureFailure::Invalid)?,
        ),
        preferences: Some(analysis.preferences.clone()),
        preference_stability: Some(analysis.preference_stability),
        analysis_basis: Some(analysis_basis),
        worker_policy: Some(run.policy().clone()),
        source_coverage: Some(analysis.source_coverage),
        source_gaps: analysis.source_gaps.clone(),
        source_observer_recipe: Some(analysis.source_observer_recipe.clone()),
        terminal: StyloTerminal::Parsed(analysis.coverage),
        fact_count: u32::try_from(facts.len()).map_err(|_| StyloCaptureFailure::Limit)?,
        fact_digest,
    };
    let request_digest = run.request_digest();
    let producer = analysis.support_fingerprint;
    let recipe = analysis.recipe.clone();
    let (proof, encoded_bytes) = witness(
        host_key,
        &host,
        &facts,
        request_digest,
        producer,
        &recipe,
        artifact,
    )?;
    if encoded_bytes > remaining_bytes {
        return Err(StyloCaptureFailure::Limit);
    }
    Ok(StyloHostCapture {
        host_key,
        host,
        facts,
        request_digest,
        producer,
        recipe,
        artifact,
        proof,
        encoded_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_compile::ToolchainArtifact;
    use backend_semantic::stylo_analysis::{StyloLimits, StyloSourceKind};
    #[test]
    #[ignore = "requires NUDOX_TEST_STYLO_WORKER pointing to a separately built fixture artifact"]
    fn supervised_capture_binds_exact_rows_and_rejects_tampering() {
        let path =
            PathBuf::from(std::env::var_os("NUDOX_TEST_STYLO_WORKER").expect("worker fixture"));
        let artifact = ToolchainArtifact::from_path(&path, Vec::new()).expect("artifact");
        let permit = StyloWorkerPermit::from_admitted(
            path,
            artifact,
            backend_frontend_stylo::support_fingerprint(),
        )
        .expect("fixture admission");
        let config = StyloParseConfig {
            source_kind: StyloSourceKind::StandaloneUtf8,
            base_url: "https://example.invalid/site.css".into(),
            author_origin: true,
            no_quirks: true,
            allow_imports: false,
            limits: StyloLimits {
                input_bytes: 4096,
                rules: 100,
                depth: 16,
                records: 100,
                diagnostics: 20,
                text_bytes: 4096,
            },
        };
        let cancelled = AtomicBool::new(false);
        let source = b"\xef\xbb\xbf.button { --ink: red; }";
        let capture = || {
            capture_stylo_host(
                &permit,
                std::env::temp_dir(),
                [1; 32],
                "site.css",
                source,
                &config,
                Duration::from_secs(30),
                None,
                1024 * 1024,
                100,
                &cancelled,
            )
            .expect("native capture")
        };
        let started = std::time::Instant::now();
        let first = capture();
        let first_elapsed = started.elapsed();
        eprintln!(
            "stylo_capture_fixture source_bytes={} encoded_row_bytes={} fact_rows={} wall_us={}",
            source.len(),
            first.encoded_bytes(),
            first.facts().len(),
            first_elapsed.as_micros()
        );
        first.verify().expect("exact witness");
        let second = capture();
        second.verify().expect("repeat witness");
        assert_eq!(first.host, second.host);
        assert_eq!(first.facts, second.facts);
        assert_eq!(first.proof, second.proof);
        let observed = first
            .facts
            .iter()
            .filter_map(|(_, row)| match row {
                StyloAnalysisRecord::Fact {
                    fact: backend_engine::builtin::StyloProjectedRow::SourceOccurrence(site),
                    ..
                } => Some(site),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(observed.iter().any(|site| site.raw == "button"));
        assert!(observed.iter().any(|site| site.raw == "--ink"));
        for site in observed {
            assert_eq!(
                &source[site.start as usize..site.end as usize],
                site.raw.as_bytes()
            );
        }
        assert!(matches!(
            &first.host,
            StyloAnalysisRecord::Host {
                utf8_bom_removed: Some(true),
                ..
            }
        ));
        let mut forged = second;
        if let StyloAnalysisRecord::Host { byte_length, .. } = &mut forged.host {
            *byte_length = Some(999);
        }
        assert!(forged.verify().is_err());
        assert!(matches!(
            capture_stylo_host(
                &permit,
                std::env::temp_dir(),
                [1; 32],
                "site.css",
                source,
                &config,
                Duration::from_secs(30),
                None,
                1,
                100,
                &cancelled
            ),
            Err(StyloCaptureFailure::Limit)
        ));
    }
}

use super::*;

fn throughput_row(
    lane: String,
    size_class: String,
    bytes: usize,
    reused_bytes: usize,
    elapsed_ns: u128,
) -> ThroughputMeasurement {
    let elapsed_seconds = elapsed_ns as f64 / 1_000_000_000.0;
    ThroughputMeasurement {
        schema: JSON_SCHEMA,
        lane,
        size_class,
        bytes,
        reused_bytes,
        reuse_ratio: if bytes == 0 {
            0.0
        } else {
            reused_bytes as f64 / bytes as f64
        },
        elapsed_ns,
        bytes_per_second: if elapsed_seconds == 0.0 {
            0.0
        } else {
            bytes as f64 / elapsed_seconds
        },
    }
}

pub(super) fn throughput_measurements(
    ingest: &[IngestMeasurement],
    deltas: &[DeltaMeasurement],
    search: &[SearchMeasurement],
    catalog: &[CatalogMeasurement],
    acquisition: &[AcquisitionMeasurement],
) -> Vec<ThroughputMeasurement> {
    let mut measurements = Vec::new();
    for item in ingest {
        let elapsed = item.wall.p50_ns.unwrap_or(item.wall.min_ns);
        measurements.push(throughput_row(
            format!("ingest/{}", item.phase),
            item.size_class.clone(),
            item.source_bytes,
            item.cas_reused_bytes,
            elapsed,
        ));
    }
    for item in deltas {
        let elapsed = item.wall.p50_ns.unwrap_or(item.wall.min_ns);
        measurements.push(throughput_row(
            format!("delta/{}", item.phase),
            item.size_class.clone(),
            item.bytes_read,
            item.cas_reused_bytes,
            elapsed,
        ));
    }
    for item in search {
        let elapsed = item.warm.p50_ns.unwrap_or(item.warm.min_ns);
        measurements.push(throughput_row(
            format!("search/{}/{}", item.mode, item.cache),
            item.size_class.clone(),
            item.bytes_read,
            0,
            elapsed,
        ));
    }
    for item in catalog {
        let elapsed = item.wall.p50_ns.unwrap_or(item.wall.min_ns);
        let reused =
            usize::from(item.operation.contains("warm") || item.operation.contains("restart"))
                .saturating_mul(item.database_bytes);
        measurements.push(throughput_row(
            format!("catalog/{}", item.operation),
            item.size_class.clone(),
            item.database_bytes,
            reused,
            elapsed,
        ));
    }
    for item in acquisition {
        let elapsed = item.wall.p50_ns.unwrap_or(item.wall.min_ns);
        measurements.push(throughput_row(
            format!("acquisition/{}", item.operation),
            "large".to_owned(),
            item.downloaded_bytes.saturating_add(item.reused_bytes),
            item.reused_bytes,
            elapsed,
        ));
    }
    measurements
}

pub(super) fn compare_backend_1(
    hardware: &Hardware,
    corpus: &CorpusDescription,
) -> Vec<ComparisonMeasurement> {
    let root = Path::new("/Users/mileswirht/Downloads/backend_1");
    let commit = command_output(
        "git",
        &["-C", root.to_string_lossy().as_ref(), "rev-parse", "HEAD"],
    );
    let commit = (commit != "unavailable").then_some(commit);
    let command = "nix shell '.#luna-tools' --command cargo run --locked --offline -p backend-performance-tests --bin integrated -- --profile smoke --output /tmp/backend-1-index-benchmark.json".to_owned();
    let manifest = root.join("tests/performance/Cargo.toml");
    if !root.is_dir() {
        return vec![ComparisonMeasurement {
            schema: JSON_SCHEMA,
            name: "backend_1",
            root: root.display().to_string(),
            commit,
            status: "unavailable",
            command,
            method: "exact-manifest-command",
            sample_count: 0,
            hardware: hardware.clone(),
            corpus: corpus.clone(),
            reason: "comparison checkout is absent".to_owned(),
        }];
    }
    if !manifest.is_file() {
        return vec![ComparisonMeasurement {
        schema: JSON_SCHEMA,
        name: "backend_1",
        root: root.display().to_string(),
        commit,
        status: "unavailable",
        command,
        method: "exact-manifest-command",
        sample_count: 0,
        hardware: hardware.clone(),
        corpus: corpus.clone(),
        reason: "comparison checkout has no backend-performance-tests manifest; exact benchmark command cannot run".to_owned(),
    }];
    }
    vec![ComparisonMeasurement {
        schema: JSON_SCHEMA,
        name: "backend_1",
        root: root.display().to_string(),
        commit,
        status: "unavailable",
        command,
        method: "exact-manifest-command",
        sample_count: 0,
        hardware: hardware.clone(),
        corpus: corpus.clone(),
        reason: "same benchmark manifest exists; isolated comparison execution was not configured"
            .to_owned(),
    }]
}

fn format_ns(value: Option<u128>) -> String {
    value.map_or_else(|| "unavailable".to_owned(), |ns| ns.to_string())
}

pub(super) fn markdown_report(report: &Report) -> String {
    let mut markdown = String::new();
    let _ = writeln!(markdown, "# Nudox index benchmark v3\n");
    let _ = writeln!(markdown, "- Status: `{}`", report.status);
    let _ = writeln!(markdown, "- Commit: `{}`", report.commit);
    let _ = writeln!(
        markdown,
        "- Profile: `{}` ({})",
        report.profile, report.build.profile
    );
    let _ = writeln!(markdown, "- Command: `{}`", report.build.command);
    let _ = writeln!(markdown, "- Nix shell: `{}`", report.build.nix_shell);
    let _ = writeln!(
        markdown,
        "- Machine: {} {} / {} / {} CPUs / peak RSS {} / peak FDs {}",
        report.hardware.os,
        report.hardware.arch,
        report
            .hardware
            .cpu_model
            .as_deref()
            .unwrap_or("unknown CPU"),
        report.hardware.cpu_count.unwrap_or(0),
        report
            .hardware
            .peak_rss_bytes
            .map_or_else(|| "unavailable".to_owned(), |bytes| bytes.to_string()),
        report
            .hardware
            .peak_fd_count
            .map_or_else(|| "unavailable".to_owned(), |count| count.to_string()),
    );
    let _ = writeln!(
        markdown,
        "\n## Corpus\n\n| Class | Files | Bytes | Languages |\n|---|---:|---:|---|\n"
    );
    for corpus in &report.corpora {
        let _ = writeln!(
            markdown,
            "| {} | {} | {} | {} |",
            corpus.name,
            corpus.files,
            corpus.bytes,
            corpus.languages.join(", ")
        );
    }
    let _ = writeln!(markdown, "\n## Key measurements\n");
    for item in report
        .ingest
        .iter()
        .filter(|item| item.size_class == "large")
    {
        let _ = writeln!(
            markdown,
            "- ingest `{}`: p50 {} ns, bytes {}, storage writes {}, reuse {}",
            item.phase,
            format_ns(item.wall.p50_ns),
            item.source_bytes,
            item.bytes_written,
            item.cas_reused_bytes
        );
    }
    for item in report
        .search
        .iter()
        .filter(|item| item.size_class == "large" && item.readers == 1)
    {
        let _ = writeln!(
            markdown,
            "- Tantivy `{}`: p50 {} ns, p95 {}, p99 {}, results {}",
            item.mode,
            format_ns(item.warm.p50_ns),
            format_ns(item.warm.p95_ns),
            format_ns(item.warm.p99_ns),
            item.result_count
        );
    }
    for item in report
        .catalog
        .iter()
        .filter(|item| item.size_class == "large")
    {
        let _ = writeln!(
            markdown,
            "- Turso `{}`: p50 {} ns, rows {}, storage {} bytes",
            item.operation,
            format_ns(item.wall.p50_ns),
            item.rows,
            item.database_bytes
        );
    }
    let _ = writeln!(
        markdown,
        "- discovery: admitted {}, skipped generated {}, skipped .gitignore {}, p50 {} ns",
        report.discovery.admitted_files,
        report.discovery.skipped_generated_files,
        report.discovery.skipped_gitignore_files,
        format_ns(report.discovery.wall.p50_ns)
    );
    for item in &report.lifecycle {
        let _ = writeln!(
            markdown,
            "- lifecycle `{}`: p50 {} ns, exit `{}`, storage {} bytes",
            item.phase,
            format_ns(item.wall.p50_ns),
            item.child_exit,
            item.storage_bytes
        );
    }
    let _ = writeln!(markdown, "\n## Parity\n");
    for item in &report.parity {
        let _ = writeln!(
            markdown,
            "- `{}`: status `{}`, p50 {} ns, expected rows {}, exact wire match {}",
            item.language,
            item.status,
            format_ns(item.wall.p50_ns),
            item.expected_rows,
            item.exact_wire_match
        );
    }
    let _ = writeln!(markdown, "\n## Unavailable or failed\n");
    if report.gate.unavailable.is_empty() && report.gate.failed.is_empty() {
        let _ = writeln!(markdown, "No unavailable or failed cells were recorded.");
    } else {
        for reason in report
            .gate
            .unavailable
            .iter()
            .chain(report.gate.failed.iter())
        {
            let _ = writeln!(markdown, "- {}", reason);
        }
    }
    for comparison in &report.comparison {
        let _ = writeln!(
            markdown,
            "- comparison `{}`: status `{}`, samples {}, method `{}` ({})",
            comparison.name,
            comparison.status,
            comparison.sample_count,
            comparison.method,
            comparison.reason
        );
    }
    markdown
}

//! Deterministic, parallel, content-versioned filesystem ingestion.

use backend_compile::{InputContentSchema, SyntaxFrontend, typed_of};
use backend_engine::{
    ProductSourceRecord, ProductSourceRelation, Relation, product_source_file_key,
};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::thread;

const MAX_SOURCE_BYTES: usize = 512 * 1024;
const MAX_ENCODED_RECORD_BYTES: usize = 1024 * 1024;
const MAX_TOTAL_SOURCE_BYTES: usize = 64 * 1024 * 1024;
const MAX_WORKERS: usize = 8;

/// One complete, deterministic project scan before comparison with the
/// selected versioned relation.
pub(super) struct IndexSnapshot {
    pub(super) source_version: [u8; 32],
    pub(super) graph_version: [u8; 32],
    pub(super) files: Vec<([u8; 32], ProductSourceRecord)>,
    pub(super) facts: Vec<([u8; 32], ProductSourceRecord)>,
}

struct FrontendSet {
    values: [SyntaxFrontend; 7],
}

impl FrontendSet {
    fn build() -> Result<Self, String> {
        Ok(Self {
            values: [
                backend_frontend_rust::syntax_frontend(),
                backend_frontend_python::syntax_frontend(),
                backend_frontend_typescript::syntax_frontend(),
                backend_frontend_go::syntax_frontend(),
                backend_frontend_java::syntax_frontend(),
                backend_frontend_csharp::syntax_frontend(),
                backend_frontend_clang::syntax_frontend(),
            ]
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?
            .try_into()
            .map_err(|_| "frontend registry has the wrong cardinality".to_owned())?,
        })
    }

    fn for_path(&self, path: &Path) -> Option<&SyntaxFrontend> {
        self.values
            .iter()
            .find(|frontend| frontend.supports_path(path))
    }
}

fn frontends() -> Result<&'static FrontendSet, String> {
    static FRONTENDS: OnceLock<FrontendSet> = OnceLock::new();
    if let Some(frontends) = FRONTENDS.get() {
        return Ok(frontends);
    }
    let candidate = FrontendSet::build()?;
    let _ = FRONTENDS.set(candidate);
    FRONTENDS
        .get()
        .ok_or_else(|| "frontend registry failed to initialize".to_owned())
}

/// Reads supported sources and reuses prior analyses behind exact content and
/// producer-version fences.
#[cfg(test)]
pub(super) fn scan_project(
    coordinate: &str,
    project: [u8; 32],
    reusable: &BTreeMap<[u8; 32], ProductSourceRecord>,
) -> Result<IndexSnapshot, String> {
    scan_project_reusing_graph(coordinate, project, reusable, [0; 32], &BTreeMap::new())
}

pub(super) fn scan_project_reusing_graph(
    coordinate: &str,
    project: [u8; 32],
    reusable: &BTreeMap<[u8; 32], ProductSourceRecord>,
    prior_graph_version: [u8; 32],
    reusable_facts: &BTreeMap<[u8; 32], ProductSourceRecord>,
) -> Result<IndexSnapshot, String> {
    let root = Path::new(coordinate)
        .canonicalize()
        .map_err(|error| format!("open project {coordinate}: {error}"))?;
    if !root.is_dir() {
        return Err(format!("project {} is not a directory", root.display()));
    }
    let frontends = frontends()?;
    let mut paths = supported_paths(&root, frontends)?;
    paths.sort();
    if paths.len() > ProductSourceRecord::MAX_PROJECT_FILES {
        return Err("project contains too many supported source files".to_owned());
    }
    let workers = thread::available_parallelism()
        .map_or(1, usize::from)
        .min(MAX_WORKERS)
        .min(paths.len().max(1));
    let chunk = paths.len().div_ceil(workers);
    let mut scanned = thread::scope(|scope| {
        let mut handles = Vec::new();
        for group in paths.chunks(chunk.max(1)) {
            let root = &root;
            handles.push(scope.spawn(move || {
                group
                    .iter()
                    .map(|path| scan_file(root, path, project, reusable, frontends))
                    .collect::<Result<Vec<_>, _>>()
            }));
        }
        let mut output = Vec::with_capacity(paths.len());
        for handle in handles {
            let mut group = handle
                .join()
                .map_err(|_| "source analysis worker panicked".to_owned())??;
            output.append(&mut group);
        }
        Ok::<_, String>(output)
    })?;
    scanned.sort_by(|left, right| left.0.cmp(&right.0));
    let total = scanned.iter().try_fold(0usize, |total, (_, _, _, bytes)| {
        total
            .checked_add(*bytes)
            .ok_or_else(|| "source byte count overflow".to_owned())
    })?;
    if total > MAX_TOTAL_SOURCE_BYTES {
        return Err("project source exceeds the bounded ingest budget".to_owned());
    }

    let mut source = backend_engine::blake3::Hasher::new();
    source.update(b"backend.project-snapshot.v2\0");
    let mut files = Vec::with_capacity(scanned.len());
    for (_, key, record, _) in scanned {
        let file = record
            .file_fields()
            .ok_or_else(|| "frontend produced a non-file record".to_owned())?;
        source.update(&key);
        source.update(&file.content_version);
        source.update(&file.analysis_version);
        files.push((key, record));
    }
    files.sort_by_key(|(key, _)| *key);
    let graph = super::package_graph::scan(&root, project, prior_graph_version, reusable_facts)?;
    Ok(IndexSnapshot {
        source_version: *source.finalize().as_bytes(),
        graph_version: graph.version,
        files,
        facts: graph.facts,
    })
}

fn supported_paths(root: &Path, frontends: &FrontendSet) -> Result<Vec<PathBuf>, String> {
    let mut output = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let mut entries = fs::read_dir(&directory)
            .map_err(|error| format!("read {}: {error}", directory.display()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("read {}: {error}", directory.display()))?;
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries.into_iter().rev() {
            let file_type = entry
                .file_type()
                .map_err(|error| format!("inspect {}: {error}", entry.path().display()))?;
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                if !ignored_directory(&entry.file_name().to_string_lossy()) {
                    pending.push(entry.path());
                }
            } else if file_type.is_file() && frontends.for_path(&entry.path()).is_some() {
                output.push(entry.path());
            }
        }
    }
    Ok(output)
}

fn ignored_directory(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | ".backend"
            | "target"
            | "node_modules"
            | ".venv"
            | "venv"
            | "dist"
            | "build"
            | ".idea"
            | ".vscode"
    )
}

fn scan_file(
    root: &Path,
    path: &Path,
    project: [u8; 32],
    reusable: &BTreeMap<[u8; 32], ProductSourceRecord>,
    frontends: &FrontendSet,
) -> Result<(String, [u8; 32], ProductSourceRecord, usize), String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("inspect source {}: {error}", path.display()))?;
    let length = usize::try_from(metadata.len())
        .map_err(|_| format!("source {} is too large", path.display()))?;
    if length > MAX_SOURCE_BYTES {
        return Err(format!(
            "source {} exceeds the {} byte file limit",
            path.display(),
            MAX_SOURCE_BYTES
        ));
    }
    let bytes =
        fs::read(path).map_err(|error| format!("read source {}: {error}", path.display()))?;
    let relative = path
        .strip_prefix(root)
        .map_err(|_| "source path escaped its project root".to_owned())?
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/");
    let frontend = frontends
        .for_path(path)
        .ok_or_else(|| "unsupported source language".to_owned())?;
    let key = product_source_file_key(project, &relative);
    let content = typed_of::<InputContentSchema>(&bytes).to_bytes();
    let analysis = frontend.producer().to_bytes();

    if let Some(record) = reusable.get(&key)
        && record.file_fields().is_some_and(|fields| {
            fields.project == project
                && fields.path == relative
                && fields.language == frontend.language()
                && fields.content_version == content
                && fields.analysis_version == analysis
        })
    {
        return Ok((relative, key, record.clone(), bytes.len()));
    }

    let analyzed = frontend
        .analyze(Path::new(&relative), &bytes)
        .map_err(|error| format!("analyze source {}: {error}", path.display()))?;
    debug_assert_eq!(analyzed.language(), frontend.language());
    debug_assert_eq!(analyzed.content().to_bytes(), content);
    debug_assert_eq!(analyzed.producer().to_bytes(), analysis);
    let record = ProductSourceRecord::file(
        project,
        relative.clone(),
        analyzed.language(),
        analyzed.content().to_bytes(),
        analyzed.producer().to_bytes(),
        analyzed.declarations().clone(),
    )?;
    let mut encoded = Vec::new();
    ProductSourceRelation::encode_value(&record, &mut encoded);
    if encoded.len() > MAX_ENCODED_RECORD_BYTES {
        return Err(format!(
            "source projection {} exceeds the {} byte record limit",
            path.display(),
            MAX_ENCODED_RECORD_BYTES
        ));
    }
    Ok((relative, key, record, bytes.len()))
}

#[cfg(test)]
#[allow(
    clippy::items_after_statements,
    clippy::needless_borrow,
    clippy::too_many_lines
)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Instant;

    #[test]
    fn every_language_lane_uses_its_grammar_tags() -> Result<(), String> {
        let fixtures = [
            ("fixture.rs", "pub fn ferris() {}", "ferris"),
            ("fixture.py", "def monty():\n    pass", "monty"),
            ("fixture.ts", "export function turing() {}", "turing"),
            ("fixture.go", "func gopher() {}", "gopher"),
            ("fixture.java", "public class Duke {}", "Duke"),
            ("fixture.cs", "public class Anders {}", "Anders"),
            ("fixture.cpp", "struct Bjarne {};", "Bjarne"),
        ];
        for (path, source, expected) in fixtures {
            let frontend = frontends()?
                .for_path(Path::new(path))
                .ok_or_else(|| format!("missing frontend for {path}"))?;
            let found = frontend
                .analyze(Path::new(path), source.as_bytes())
                .map_err(|error| error.to_string())?;
            assert!(
                found
                    .declarations()
                    .iter()
                    .any(|item| item.name() == expected),
                "{path}: expected {expected}, found {:?}",
                found
                    .declarations()
                    .iter()
                    .map(|item| (item.kind(), item.name()))
                    .collect::<Vec<_>>()
            );
        }
        Ok(())
    }

    #[test]
    fn unchanged_files_reuse_the_exact_declaration_allocation() -> Result<(), String> {
        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }

        let unique = format!(
            "backend-syntax-reuse-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_nanos()
        );
        let scratch = Scratch(std::env::temp_dir().join(unique));
        fs::create_dir_all(&scratch.0).map_err(|error| error.to_string())?;
        fs::write(scratch.0.join("lib.rs"), b"pub fn ferris() {}")
            .map_err(|error| error.to_string())?;
        let project = [7; 32];
        let first = scan_project(
            scratch.0.to_str().ok_or("non-UTF-8 scratch path")?,
            project,
            &BTreeMap::new(),
        )?;
        let reusable = first.files.iter().cloned().collect::<BTreeMap<_, _>>();
        let second = scan_project(
            scratch.0.to_str().ok_or("non-UTF-8 scratch path")?,
            project,
            &reusable,
        )?;
        let first_declarations = first.files[0]
            .1
            .file_fields()
            .ok_or("expected first source file")?
            .declarations;
        let second_declarations = second.files[0]
            .1
            .file_fields()
            .ok_or("expected second source file")?
            .declarations;
        assert_eq!(first.source_version, second.source_version);
        assert!(Arc::ptr_eq(first_declarations, second_declarations));
        Ok(())
    }

    #[test]
    fn manifest_only_edit_replaces_one_edge_and_reuses_source_analysis() -> Result<(), String> {
        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }

        let unique = format!(
            "backend-package-delta-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_nanos()
        );
        let scratch = Scratch(std::env::temp_dir().join(unique));
        fs::create_dir_all(scratch.0.join("src")).map_err(|error| error.to_string())?;
        fs::write(scratch.0.join("src/lib.rs"), "pub fn stable() {}\n")
            .map_err(|error| error.to_string())?;
        let manifest = |requirement: &str| {
            format!(
                "[package]\nname = \"delta-fixture\"\nversion = \"1.0.0\"\n\n[dependencies]\nserde = \"{requirement}\"\n"
            )
        };
        fs::write(scratch.0.join("Cargo.toml"), manifest("1"))
            .map_err(|error| error.to_string())?;
        let coordinate = scratch.0.to_str().ok_or("non-UTF-8 scratch path")?;
        let project = [0x4d; 32];
        let first = scan_project(coordinate, project, &BTreeMap::new())?;
        let reusable = first.files.iter().cloned().collect::<BTreeMap<_, _>>();
        let reusable_facts = first.facts.iter().cloned().collect::<BTreeMap<_, _>>();
        let warm = scan_project_reusing_graph(
            coordinate,
            project,
            &reusable,
            first.graph_version,
            &reusable_facts,
        )?;
        assert_eq!(warm.graph_version, first.graph_version);
        assert_eq!(warm.facts, first.facts);
        let first_alias = first.facts.iter().find_map(|(_, record)| match record {
            ProductSourceRecord::Dependency { alias, .. } => Some(alias),
            _ => None,
        });
        let warm_alias = warm.facts.iter().find_map(|(_, record)| match record {
            ProductSourceRecord::Dependency { alias, .. } => Some(alias),
            _ => None,
        });
        assert!(
            first_alias
                .zip(warm_alias)
                .is_some_and(|(left, right)| Arc::ptr_eq(left, right))
        );

        fs::write(scratch.0.join("Cargo.toml"), manifest("2"))
            .map_err(|error| error.to_string())?;
        let second = scan_project_reusing_graph(
            coordinate,
            project,
            &reusable,
            first.graph_version,
            &reusable_facts,
        )?;
        assert_eq!(first.source_version, second.source_version);
        assert_ne!(first.graph_version, second.graph_version);
        assert_eq!(first.files.len(), second.files.len());
        assert!(first.files.iter().zip(&second.files).all(
            |((left_key, left), (right_key, right))| {
                left_key == right_key
                    && left
                        .file_fields()
                        .zip(right.file_fields())
                        .is_some_and(|(left, right)| {
                            Arc::ptr_eq(left.declarations, right.declarations)
                        })
            }
        ));

        let first_facts = first.facts.into_iter().collect::<BTreeMap<_, _>>();
        let second_facts = second.facts.into_iter().collect::<BTreeMap<_, _>>();
        let changed = first_facts
            .iter()
            .filter(|(key, value)| second_facts.get(*key) != Some(*value))
            .count();
        assert_eq!(
            first_facts.keys().collect::<Vec<_>>(),
            second_facts.keys().collect::<Vec<_>>()
        );
        assert_eq!(changed, 1, "only the dependency edge should change");
        Ok(())
    }

    #[test]
    fn source_scan_excludes_generated_and_metadata_trees() -> Result<(), String> {
        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }

        let unique = format!(
            "backend-syntax-ignore-trees-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_nanos()
        );
        let scratch = Scratch(std::env::temp_dir().join(unique));
        fs::create_dir_all(scratch.0.join("src")).map_err(|error| error.to_string())?;
        fs::write(scratch.0.join("src/keep.rs"), b"pub fn keep() {}")
            .map_err(|error| error.to_string())?;
        for directory in [
            ".git",
            ".backend",
            "target",
            "node_modules",
            ".venv",
            "venv",
            "dist",
            "build",
            ".idea",
            ".vscode",
        ] {
            let path = scratch.0.join(directory).join("ignored.rs");
            fs::create_dir_all(path.parent().ok_or("missing ignored parent")?)
                .map_err(|error| error.to_string())?;
            fs::write(path, b"pub fn ignored() {}").map_err(|error| error.to_string())?;
        }

        let snapshot = scan_project(
            scratch.0.to_str().ok_or("non-UTF-8 scratch path")?,
            [8; 32],
            &BTreeMap::new(),
        )?;
        let mut paths = snapshot
            .files
            .iter()
            .filter_map(|(_, record)| record.file_fields().map(|fields| fields.path.to_owned()))
            .collect::<Vec<_>>();
        paths.sort();
        assert_eq!(paths, vec!["src/keep.rs".to_owned()]);
        Ok(())
    }

    /// Opt-in ingest benchmark and mutation probe. This stays ignored so the
    /// ordinary unit suite remains small, but keeps the experiment executable
    /// against the exact private scan path rather than a second harness.
    #[test]
    #[ignore = "bounded stress probe; run explicitly with --ignored"]
    fn stress_scan_project_reports_reuse_and_mutation_costs() -> Result<(), String> {
        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }

        let unique = format!(
            "backend-syntax-stress-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_nanos()
        );
        let scratch = Scratch(std::env::temp_dir().join(unique));
        let source_root = scratch.0.join("src");
        fs::create_dir_all(&source_root).map_err(|error| error.to_string())?;
        const FILES: usize = 2_048;
        for index in 0..FILES {
            fs::write(
                source_root.join(format!("file-{index:04}.rs")),
                format!("pub fn symbol_{index:04}() -> usize {{ {index} }}\n"),
            )
            .map_err(|error| error.to_string())?;
        }
        // The current policy intentionally skips symlinks. Keep both forms in
        // the probe because a directory symlink can otherwise recurse forever.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                source_root.join("file-0000.rs"),
                source_root.join("linked.rs"),
            )
            .map_err(|error| error.to_string())?;
            std::os::unix::fs::symlink(&source_root, scratch.0.join("linked-dir"))
                .map_err(|error| error.to_string())?;
        }
        // `vendor` is deliberately outside the hard-coded ignore set today;
        // report that policy so the output makes the indexing choice visible.
        let vendor = scratch.0.join("vendor");
        fs::create_dir_all(&vendor).map_err(|error| error.to_string())?;
        fs::write(vendor.join("vendored.rs"), "pub fn vendored() {}\n")
            .map_err(|error| error.to_string())?;

        let project = [0x5a; 32];
        let coordinate = scratch.0.to_str().ok_or("non-UTF-8 scratch path")?;
        let started = Instant::now();
        let first = scan_project(coordinate, project, &BTreeMap::new())?;
        let first_ms = started.elapsed().as_secs_f64() * 1_000.0;
        let reusable = first.files.iter().cloned().collect::<BTreeMap<_, _>>();

        let started = Instant::now();
        let second = scan_project(coordinate, project, &reusable)?;
        let second_ms = started.elapsed().as_secs_f64() * 1_000.0;
        let reused_second =
            first
                .files
                .iter()
                .zip(&second.files)
                .filter(|((left_key, left), (right_key, right))| {
                    left_key == right_key
                        && left.file_fields().zip(right.file_fields()).is_some_and(
                            |(left, right)| Arc::ptr_eq(&left.declarations, &right.declarations),
                        )
                })
                .count();
        assert_eq!(first.source_version, second.source_version);

        fs::write(
            source_root.join("file-0000.rs"),
            "pub fn changed_symbol() -> usize { 99 }\n",
        )
        .map_err(|error| error.to_string())?;
        let started = Instant::now();
        let edited = scan_project(coordinate, project, &reusable)?;
        let edit_ms = started.elapsed().as_secs_f64() * 1_000.0;
        let edited_reuse = second
            .files
            .iter()
            .filter_map(|(key, row)| {
                let old = row.file_fields()?;
                let index = edited
                    .files
                    .binary_search_by_key(key, |(candidate, _)| *candidate)
                    .ok()?;
                let new = edited.files[index].1.file_fields()?;
                Some(Arc::ptr_eq(&old.declarations, &new.declarations))
            })
            .filter(|reused| *reused)
            .count();
        assert_ne!(second.source_version, edited.source_version);

        fs::rename(
            source_root.join("file-0001.rs"),
            source_root.join("renamed.rs"),
        )
        .map_err(|error| error.to_string())?;
        let started = Instant::now();
        let renamed = scan_project(coordinate, project, &reusable)?;
        let rename_ms = started.elapsed().as_secs_f64() * 1_000.0;
        assert_ne!(edited.source_version, renamed.source_version);

        fs::remove_file(source_root.join("file-0002.rs")).map_err(|error| error.to_string())?;
        let started = Instant::now();
        let deleted = scan_project(coordinate, project, &reusable)?;
        let delete_ms = started.elapsed().as_secs_f64() * 1_000.0;
        assert_eq!(deleted.files.len(), renamed.files.len() - 1);
        assert_ne!(renamed.source_version, deleted.source_version);

        let vendor_included = deleted.files.iter().any(|(_, record)| {
            record
                .file_fields()
                .is_some_and(|fields| fields.path == "vendor/vendored.rs")
        });
        let symlink_count = deleted
            .files
            .iter()
            .filter(|(_, record)| {
                record
                    .file_fields()
                    .is_some_and(|fields| fields.path.contains("linked"))
            })
            .count();
        eprintln!(
            "ingest_stress files={} first_ms={first_ms:.2} unchanged_ms={second_ms:.2} edit_ms={edit_ms:.2} rename_ms={rename_ms:.2} delete_ms={delete_ms:.2} reused_unchanged={reused_second} reused_after_edit={edited_reuse} vendor_included={vendor_included} symlink_rows={symlink_count}",
            deleted.files.len()
        );
        Ok(())
    }
}

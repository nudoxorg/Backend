//! Parallel, content-versioned project scan.
//!
//! Discovery, per-file faults, and dialect selection share one budget. A file
//! that cannot be read stays a typed row; only the aggregate budget stops the
//! project.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

use backend_compile::{InputContentSchema, SourceLanguage, typed_of};
use backend_engine::{
    ProductSourceRecord, ProductSourceRelation, Relation, SourceUnavailableReason,
    product_source_file_key,
};
use backend_library::{DiscoveryPolicy, discover_source_entries, source_selection_policy};
use backend_semantic::vocabulary::{
    CSharpVersion, CStandard, CxxStandard, GoVersion, JavaRelease, LanguageProfile, PythonVersion,
    RustEdition, TypeScriptSource,
};

use super::{
    CompilerSource, DiscoveryBudget, FrontendSet, IndexSnapshot, IngestBudget,
    MAX_ENCODED_RECORD_BYTES, MAX_SOURCE_BYTES, MAX_TOTAL_SOURCE_BYTES, MAX_WORKERS, ProjectRoot,
    RESULT_QUEUE_PER_WORKER, ScannedFile, SourceFault, frontends,
};

/// Reads supported sources and reuses prior analyses behind exact content and
/// producer-version fences.
pub(in crate::builtin) fn scan_project(
    coordinate: &str,
    project: [u8; 32],
    reusable: &BTreeMap<[u8; 32], ProductSourceRecord>,
) -> Result<IndexSnapshot, String> {
    scan_project_with_policy(coordinate, project, reusable, source_selection_policy())
}

/// Reads supported sources under one shared discovery policy.
///
/// Keeping the policy as an argument makes the same selection contract usable
/// by local ingest and future package/archive graph adapters.  The default
/// entry point above preserves the production behavior for existing callers.
pub(in crate::builtin) fn scan_project_with_policy(
    coordinate: &str,
    project: [u8; 32],
    reusable: &BTreeMap<[u8; 32], ProductSourceRecord>,
    discovery: DiscoveryPolicy,
) -> Result<IndexSnapshot, String> {
    let root = Path::new(coordinate)
        .canonicalize()
        .map_err(|error| format!("open project {coordinate}: {error}"))?;
    if !root.is_dir() {
        return Err(format!("project {} is not a directory", root.display()));
    }
    let root_capability = ProjectRoot::open(&root)?;
    let frontends = frontends()?;
    let mut paths = supported_paths_with_policy(&root, frontends, discovery)?;
    paths.sort();
    preflight_source_bytes(&paths)?;
    let workers = thread::available_parallelism()
        .map_or(1, usize::from)
        .min(MAX_WORKERS)
        .min(paths.len().max(1));
    let chunk = paths.len().div_ceil(workers);
    let scanned = thread::scope(|scope| {
        let queue = workers
            .checked_mul(RESULT_QUEUE_PER_WORKER)
            .ok_or_else(|| "source result queue width overflow".to_owned())?;
        let (sender, receiver) = mpsc::sync_channel(queue);
        let mut handles = Vec::with_capacity(workers);
        for group in paths.chunks(chunk.max(1)) {
            let root = &root;
            let root_capability = &root_capability;
            let sender = sender.clone();
            handles.push(scope.spawn(move || {
                for path in group {
                    let result =
                        scan_file(root, root_capability, path, project, reusable, frontends);
                    let failed = result.is_err();
                    if sender.send(result).is_err() || failed {
                        break;
                    }
                }
            }));
        }
        drop(sender);
        let mut output = Vec::with_capacity(paths.len());
        let mut budget = IngestBudget::default();
        let mut failure = None;
        for result in receiver {
            match result {
                Ok(Some(file)) if failure.is_none() => {
                    if let Err(error) = budget.charge(&file) {
                        failure = Some(error);
                    } else {
                        output.push(file);
                    }
                }
                Err(error) if failure.is_none() => failure = Some(error),
                Ok(_) | Err(_) => {}
            }
        }
        for handle in handles {
            if handle.join().is_err() && failure.is_none() {
                failure = Some("source analysis worker panicked".to_owned());
            }
        }
        failure.map_or(Ok(output), Err)
    })?;
    let mut scanned = scanned;
    scanned.sort_by(|left, right| left.relative.cmp(&right.relative));

    let mut source = blake3::Hasher::new();
    source.update(b"backend.project-snapshot.v2\0");
    let mut files = Vec::with_capacity(scanned.len());
    let mut compiler_sources = Vec::with_capacity(scanned.len());
    for scanned in scanned {
        let ScannedFile {
            key,
            record,
            compiler_source,
            ..
        } = scanned;
        let file = record
            .file_fields()
            .ok_or_else(|| "frontend produced a non-file record".to_owned())?;
        source.update(&key);
        source.update(&file.content_version);
        source.update(&file.analysis_version);
        files.push((key, record));
        compiler_sources.extend(compiler_source);
    }
    files.sort_by_key(|(key, _)| *key);
    Ok(IndexSnapshot {
        source_version: *source.finalize().as_bytes(),
        files,
        compiler_sources,
    })
}

#[cfg(test)]
pub(super) fn supported_paths(
    root: &Path,
    frontends: &FrontendSet,
) -> Result<Vec<PathBuf>, String> {
    supported_paths_with_policy(root, frontends, source_selection_policy())
}

fn supported_paths_with_policy(
    root: &Path,
    frontends: &FrontendSet,
    discovery: DiscoveryPolicy,
) -> Result<Vec<PathBuf>, String> {
    let mut output = Vec::new();
    let mut budget = DiscoveryBudget::default();
    let mut current_directory = None;
    let mut directory_entries = 0_usize;
    for entry in discover_source_entries(root, discovery) {
        let entry = entry.map_err(|error| error.to_string())?;
        if entry.path() == root {
            continue;
        }
        let parent = entry.path().parent().unwrap_or(root);
        if current_directory.as_deref() != Some(parent) {
            current_directory = Some(parent.to_owned());
            directory_entries = 0;
        }
        budget
            .admit_entry(directory_entries)
            .map_err(|error| format!("{error}: {}", entry.path().display()))?;
        directory_entries = directory_entries.saturating_add(1);
        if entry.is_file() && frontends.for_path(entry.path()).is_some() {
            if output.len() >= ProductSourceRecord::MAX_PROJECT_FILES {
                return Err("project contains too many supported source files".to_owned());
            }
            output.push(entry.path().to_owned());
        }
    }
    Ok(output)
}

/// Charges the aggregate source budget before any file is read.
///
/// A file that is oversized or unreadable is not a reason to refuse the
/// project: it is charged as zero bytes here and reported per file by
/// [`scan_file`].  Only the aggregate budget, which protects the process
/// rather than describing a file, can still stop the scan.
fn preflight_source_bytes(paths: &[PathBuf]) -> Result<(), String> {
    let mut total = 0_usize;
    for path in paths {
        let Ok(metadata) = fs::metadata(path) else {
            continue;
        };
        let Ok(length) = usize::try_from(metadata.len()) else {
            continue;
        };
        if length > MAX_SOURCE_BYTES {
            continue;
        }
        total = total
            .checked_add(length)
            .ok_or_else(|| "source byte count overflow".to_owned())?;
        if total > MAX_TOTAL_SOURCE_BYTES {
            return Err("project source exceeds the bounded ingest budget".to_owned());
        }
    }
    Ok(())
}

/// Scans one discovered file into a row, or into a typed per-file fault.
///
/// `Ok(None)` means the file vanished between discovery and read, so the
/// project no longer contains it and the frontier simply omits it.
pub(super) fn scan_file(
    root: &Path,
    root_capability: &ProjectRoot,
    path: &Path,
    project: [u8; 32],
    reusable: &BTreeMap<[u8; 32], ProductSourceRecord>,
    frontends: &FrontendSet,
) -> Result<Option<ScannedFile>, String> {
    match scan_one(root, root_capability, path, project, reusable, frontends) {
        Ok(file) => Ok(Some(file)),
        Err(SourceFault::Vanished) => Ok(None),
        Err(SourceFault::Fatal(message)) => {
            Err(format!("read source {}: {message}", path.display()))
        }
        Err(SourceFault::Unavailable(reason)) => {
            unavailable_file(root, path, project, frontends, reason).map(Some)
        }
    }
}

/// Builds the row for a file whose contents could not be extracted.
fn unavailable_file(
    root: &Path,
    path: &Path,
    project: [u8; 32],
    frontends: &FrontendSet,
    reason: SourceUnavailableReason,
) -> Result<ScannedFile, String> {
    let relative = relative_coordinate(root, path)?;
    let frontend = frontends
        .for_path(path)
        .ok_or_else(|| "unsupported source language".to_owned())?;
    let key = product_source_file_key(project, &relative);
    let record = ProductSourceRecord::file_unavailable(
        project,
        relative.clone(),
        frontend.language(),
        frontend.analysis_version(),
        reason,
    )?;
    let mut encoded = Vec::new();
    ProductSourceRelation::encode_value(&record, &mut encoded);
    Ok(ScannedFile {
        // An unavailable source must not be turned into an empty compiler
        // artifact.  The relation row carries the typed terminal and the
        // semantic lane must retain partial coverage until a later scan can
        // read the real bytes.
        compiler_source: None,
        relative,
        key,
        record,
        source_bytes: 0,
        encoded_record_bytes: encoded.len(),
    })
}

fn relative_coordinate(root: &Path, path: &Path) -> Result<String, String> {
    Ok(path
        .strip_prefix(root)
        .map_err(|_| "source path escaped its project root".to_owned())?
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/"))
}

fn scan_one(
    root: &Path,
    root_capability: &ProjectRoot,
    path: &Path,
    project: [u8; 32],
    reusable: &BTreeMap<[u8; 32], ProductSourceRecord>,
    frontends: &FrontendSet,
) -> Result<ScannedFile, SourceFault> {
    let relative_path = path
        .strip_prefix(root)
        .map_err(|_| SourceFault::Fatal("source path escaped its project root".to_owned()))?;
    let bytes = root_capability.read(relative_path)?;
    let relative = relative_path
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/");
    let source = std::str::from_utf8(&bytes)
        .map_err(|_| SourceFault::Unavailable(SourceUnavailableReason::NotText))?
        .to_owned();
    let frontend = frontends
        .for_path(path)
        .ok_or_else(|| SourceFault::Fatal("unsupported source language".to_owned()))?;
    let profile = profile_fault(frontends, path)?;
    let key = product_source_file_key(project, &relative);
    let content = typed_of::<InputContentSchema>(&bytes).to_bytes();
    let analysis = frontend.analysis_version();

    if let Some(record) = reusable.get(&key)
        && record.file_fields().is_some_and(|fields| {
            fields.project == project
                && fields.path == relative
                && fields.language == frontend.language()
                && fields.content_version == content
                && fields.analysis_version == analysis
        })
    {
        return scanned_file(
            relative,
            key,
            record.clone(),
            source,
            bytes.len(),
            path,
            profile,
        )
        .map_err(SourceFault::Fatal);
    }

    // Structural parsing is an explicit baseline projection for local browsing.
    // Package semantics are compiled and published by the engine application module.
    let analyzed = frontend
        .baseline
        .analyze(Path::new(&relative), &bytes)
        .map_err(|_| SourceFault::Unavailable(SourceUnavailableReason::Unparsed))?;
    debug_assert_eq!(analyzed.language(), frontend.language());
    debug_assert_eq!(analyzed.content().to_bytes(), content);
    // A real source file routinely extracts more detail than one canonical
    // relation row can carry: `memchr 2.8.3` alone produces a 65 686 byte row
    // for `src/arch/x86_64/avx2/memchr.rs` against a 65 464 byte capacity.
    // Constructing through the capacity-aware path sheds derived detail in a
    // fixed order and records how far it had to go, instead of failing the
    // whole project when the relation delta is later prepared.
    let record = ProductSourceRecord::file_within_row_capacity(
        project,
        relative.clone(),
        analyzed.language(),
        analyzed.content().to_bytes(),
        analysis,
        analyzed.declarations().clone(),
    )
    .map_err(SourceFault::Fatal)?
    // Persist the exact `SourceFactDomain` identity the semantic compiler
    // derives from these bytes, so the view can detect an in-place edit by
    // comparing content identity instead of path sets.
    .with_source_identity(backend_version::ContentId::<
        backend_version::SourceFactDomain,
    >::from_canonical_bytes(&bytes))
    .map_err(SourceFault::Fatal)?;
    scanned_file(relative, key, record, source, bytes.len(), path, profile)
        .map_err(SourceFault::Fatal)
}

fn scanned_file(
    relative: String,
    key: [u8; 32],
    record: ProductSourceRecord,
    source: String,
    source_bytes: usize,
    path: &Path,
    profile: LanguageProfile,
) -> Result<ScannedFile, String> {
    let mut encoded = Vec::new();
    ProductSourceRelation::encode_value(&record, &mut encoded);
    let encoded_record_bytes = encoded.len();
    if encoded_record_bytes > MAX_ENCODED_RECORD_BYTES {
        return Err(format!(
            "source projection {} exceeds the {} byte record limit",
            path.display(),
            MAX_ENCODED_RECORD_BYTES
        ));
    }
    Ok(ScannedFile {
        compiler_source: Some(CompilerSource {
            profile,
            relative_path: relative.clone(),
            source,
        }),
        relative,
        key,
        record,
        source_bytes,
        encoded_record_bytes,
    })
}

/// Derives one file's semantic profile from the frontend that claimed it.
///
/// Discovery admits a file because some frontend claims its extension, so the
/// claimed set is the only table that decides which extensions exist.  This
/// function therefore never repeats that list: it asks the frontend set which
/// language owns the path and then picks a dialect, which is the one question
/// the extension still answers for the two languages that have two.
///
/// `None` means the path has no profile — either no frontend claims it, or the
/// language's dialects do not cover this extension.  Callers treat that as one
/// unavailable file, never as a broken project.
///
/// # Errors
/// Returns an error only when the frontend registry itself cannot be built.
pub(in crate::builtin) fn source_profile(path: &Path) -> Result<Option<LanguageProfile>, String> {
    Ok(profile_of(frontends()?, path))
}

/// Resolves a discovered path's profile, or the typed fault its absence is.
///
/// A claimed extension with no semantic profile is one odd file, not a broken
/// project: it is reported per file as
/// [`SourceUnavailableReason::Unparsed`] — nothing could be extracted — and
/// the scan continues.  Returning [`SourceFault::Fatal`] here is what made a
/// single `postcss.config.js` sink an entire checkout.
pub(super) fn profile_fault(
    frontends: &FrontendSet,
    path: &Path,
) -> Result<LanguageProfile, SourceFault> {
    profile_of(frontends, path).ok_or(SourceFault::Unavailable(SourceUnavailableReason::Unparsed))
}

/// Resolves a path's profile against an explicit frontend set.
pub(super) fn profile_of(frontends: &FrontendSet, path: &Path) -> Option<LanguageProfile> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)?;
    let language = frontends.for_path(path)?.language();
    dialect(language, &extension)
}

/// Returns the key the view uses to pair a publication with its files.
///
/// A path names its language and dialect, never a Rust crate's edition: the
/// edition belongs to the crate and is chosen when it is compiled. A
/// publication compiled as edition 2021 must still be recognized as the
/// semantic answer for the `.rs` files [`source_profile`] keys as the default
/// edition, so every Rust edition shares one lane key.
pub(in crate::builtin) const fn lane_profile(profile: LanguageProfile) -> LanguageProfile {
    match profile {
        LanguageProfile::Rust(_) => LanguageProfile::Rust(RustEdition::Rust2024),
        other => other,
    }
}

/// Selects the dialect a claimed extension names inside one language.
///
/// Only TypeScript and Clang carry two dialects; every other language has a
/// single profile, so its extensions cannot disagree.
fn dialect(language: SourceLanguage, extension: &str) -> Option<LanguageProfile> {
    Some(match language {
        SourceLanguage::Rust => LanguageProfile::Rust(RustEdition::Rust2024),
        SourceLanguage::Python => LanguageProfile::Python(PythonVersion::Python314),
        SourceLanguage::Go => LanguageProfile::Go(GoVersion::Go125),
        SourceLanguage::Java => LanguageProfile::Java(JavaRelease::Java25),
        SourceLanguage::CSharp => LanguageProfile::CSharp(CSharpVersion::CSharp14),
        SourceLanguage::TypeScript => LanguageProfile::TypeScript(match extension {
            "tsx" | "jsx" => TypeScriptSource::Tsx,
            _ => TypeScriptSource::TypeScript,
        }),
        SourceLanguage::Clang => match extension {
            "c" | "h" | "m" => LanguageProfile::C(CStandard::C23),
            "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" | "mm" => {
                LanguageProfile::Cxx(CxxStandard::Cxx23)
            }
            _ => return None,
        },
    })
}

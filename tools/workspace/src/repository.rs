//! Filesystem inventory and repository quality orchestration.

use super::{
    MAX_METADATA_BYTES, Metadata, SourceFile, Violation, canonical_dag, validate,
    validate_architecture_guards, validate_contracts, validate_cutover_contract,
    validate_cutover_mutations, validate_json, validate_markdown_links, validate_module_structure,
    validate_policy_control, validate_rust_sources, validate_scope_fixture,
    validate_workspace_lints, workspace_member_ids,
};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Runs the package-graph and repository quality gates against Cargo metadata
/// and the workspace files it names.
///
/// This is the closure used by the command-line tool.  [`validate_json`] stays
/// metadata-only so callers can validate a deliberately small graph fixture
/// without manufacturing a filesystem tree.
///
/// # Errors
///
/// Returns every metadata, dependency, lint, source-policy, Markdown, and
/// control-plane contract violation found.  Missing quality inputs are
/// reported as typed violations rather than silently skipped.
#[allow(
    clippy::too_many_lines,
    reason = "the repository closure keeps all quality inputs and one aggregate diagnostic"
)]
pub fn validate_repository(
    root: impl AsRef<Path>,
    json: &str,
    require_complete: bool,
) -> Result<(), Vec<Violation>> {
    if json.len() > MAX_METADATA_BYTES {
        return Err(vec![Violation::InvalidMetadata(format!(
            "metadata exceeds {MAX_METADATA_BYTES} bytes"
        ))]);
    }
    let metadata: Metadata = match serde_json::from_str(json) {
        Ok(metadata) => metadata,
        Err(error) => return Err(vec![Violation::InvalidMetadata(error.to_string())]),
    };
    let mut violations = validate(&metadata, require_complete)
        .err()
        .unwrap_or_default();
    let root = root.as_ref();
    let root_manifest_path = root.join("Cargo.toml");
    let root_manifest = match fs::read_to_string(&root_manifest_path) {
        Ok(contents) => contents,
        Err(error) => {
            violations.push(Violation::QualityInputError {
                path: display_path(root, &root_manifest_path),
                detail: error.to_string(),
            });
            return Err(violations);
        }
    };

    let member_ids = workspace_member_ids(&metadata);
    let mut package_manifests = Vec::new();
    let mut package_roots = BTreeSet::new();
    let mut source_files = Vec::new();
    let mut markdown_files = Vec::new();
    let mut known_paths = BTreeSet::new();
    add_file_to_inventory(
        root,
        &root_manifest_path,
        &mut known_paths,
        &mut source_files,
        &mut markdown_files,
        &mut violations,
    );
    let root_readme_path = root.join("README.md");
    if root_readme_path.is_file() {
        add_file_to_inventory(
            root,
            &root_readme_path,
            &mut known_paths,
            &mut source_files,
            &mut markdown_files,
            &mut violations,
        );
    }

    for package in metadata
        .packages
        .iter()
        .filter(|package| member_ids.contains(package.id.as_str()))
    {
        let manifest_path = PathBuf::from(&package.manifest_path);
        if let Some(parent) = manifest_path.parent() {
            package_roots.insert(display_path(root, parent));
        }
        let manifest = match fs::read_to_string(&manifest_path) {
            Ok(contents) => contents,
            Err(error) => {
                violations.push(Violation::QualityInputError {
                    path: package.manifest_path.clone(),
                    detail: error.to_string(),
                });
                continue;
            }
        };
        package_manifests.push((package.name.clone(), manifest));
        walk_quality_files(
            root,
            manifest_path.parent().unwrap_or(root),
            &mut known_paths,
            &mut source_files,
            &mut markdown_files,
            &mut violations,
        );
    }

    for relative in ["docs", ".config/contracts"] {
        let path = root.join(relative);
        if path.exists() {
            walk_quality_files(
                root,
                &path,
                &mut known_paths,
                &mut source_files,
                &mut markdown_files,
                &mut violations,
            );
        }
    }
    violations.extend(validate_workspace_lints(&root_manifest, &package_manifests));
    violations.extend(validate_rust_sources(&source_files));
    let module_structure_policy_path = root.join(".config/nix/policy/module-structure.json");
    match fs::read_to_string(&module_structure_policy_path) {
        Ok(contents) => {
            let display = display_path(root, &module_structure_policy_path);
            known_paths.insert(display.clone());
            violations.extend(validate_module_structure(
                &source_files,
                &SourceFile::new(display, contents),
            ));
        }
        Err(error) => violations.push(Violation::QualityInputError {
            path: display_path(root, &module_structure_policy_path),
            detail: error.to_string(),
        }),
    }
    let ownership_policy_path = root.join(".config/nix/policy/canonical-ownership.json");
    match fs::read_to_string(&ownership_policy_path) {
        Ok(contents) => {
            let display = display_path(root, &ownership_policy_path);
            known_paths.insert(display.clone());
            violations.extend(validate_architecture_guards(
                &source_files,
                &SourceFile::new(display, contents),
            ));
        }
        Err(error) => violations.push(Violation::QualityInputError {
            path: display_path(root, &ownership_policy_path),
            detail: error.to_string(),
        }),
    }
    violations.extend(validate_markdown_links(&markdown_files, &known_paths));

    let canonical_path = root.join(".config/contracts/schema.json");
    let canonical = match fs::read_to_string(&canonical_path) {
        Ok(contents) => contents,
        Err(error) => {
            violations.push(Violation::QualityInputError {
                path: display_path(root, &canonical_path),
                detail: error.to_string(),
            });
            String::new()
        }
    };
    let projections = collect_contract_projections(root, &mut violations);
    if !canonical.is_empty() {
        violations.extend(validate_contracts(&canonical, &projections));
    }

    let cutover_contract_path = root.join(".config/nu/cutover/e2e-contract.json");
    match fs::read_to_string(&cutover_contract_path) {
        Ok(contents) => {
            let display = display_path(root, &cutover_contract_path);
            known_paths.insert(display.clone());
            violations.extend(validate_cutover_contract(&SourceFile::new(
                display, contents,
            )));
        }
        Err(error) => violations.push(Violation::QualityInputError {
            path: display_path(root, &cutover_contract_path),
            detail: error.to_string(),
        }),
    }
    let mutation_fixture_path = root.join(".config/fixtures/control-plane/cutover-mutations.json");
    match fs::read_to_string(&mutation_fixture_path) {
        Ok(contents) => {
            let display = display_path(root, &mutation_fixture_path);
            known_paths.insert(display.clone());
            violations.extend(validate_cutover_mutations(&SourceFile::new(
                display, contents,
            )));
        }
        Err(error) => violations.push(Violation::QualityInputError {
            path: display_path(root, &mutation_fixture_path),
            detail: error.to_string(),
        }),
    }
    let policy_path = root.join(".config/nix/control.nix");
    match fs::read_to_string(&policy_path) {
        Ok(contents) => {
            let display = display_path(root, &policy_path);
            known_paths.insert(display.clone());
            if let Ok(dag) = canonical_dag() {
                let product_packages = dag
                    .packages
                    .iter()
                    .map(|package| package.name.clone())
                    .collect();
                violations.extend(validate_policy_control(
                    &SourceFile::new(display, contents),
                    &product_packages,
                ));
            }
        }
        Err(error) => violations.push(Violation::QualityInputError {
            path: display_path(root, &policy_path),
            detail: error.to_string(),
        }),
    }
    let scope_fixture_path = root.join(".config/fixtures/control-plane/scope-exactly-one.json");
    match fs::read_to_string(&scope_fixture_path) {
        Ok(contents) => {
            let display = display_path(root, &scope_fixture_path);
            known_paths.insert(display.clone());
            violations.extend(validate_scope_fixture(
                &SourceFile::new(display, contents),
                &package_roots,
            ));
        }
        Err(error) => violations.push(Violation::QualityInputError {
            path: display_path(root, &scope_fixture_path),
            detail: error.to_string(),
        }),
    }

    if violations.is_empty() {
        Ok(())
    } else {
        Err(violations)
    }
}

/// Runs the repository closure using the `workspace_root` embedded in Cargo
/// metadata, falling back to metadata-only validation for a metadata fixture
/// that deliberately omits the field.
///
/// # Errors
///
/// Returns the same typed violations as [`validate_repository`] or
/// [`validate_json`], depending on whether the metadata names a workspace
/// root.
pub fn validate_workspace_json(json: &str, require_complete: bool) -> Result<(), Vec<Violation>> {
    if json.len() > MAX_METADATA_BYTES {
        return Err(vec![Violation::InvalidMetadata(format!(
            "metadata exceeds {MAX_METADATA_BYTES} bytes"
        ))]);
    }
    let metadata: Metadata = serde_json::from_str(json)
        .map_err(|error| vec![Violation::InvalidMetadata(error.to_string())])?;
    let Some(root) = metadata.workspace_root.as_deref() else {
        return validate_json(json, require_complete);
    };
    validate_repository(root, json, require_complete)
}

const MAX_QUALITY_FILES: usize = 8192;
const MAX_QUALITY_BYTES: usize = 32 * 1024 * 1024;

fn walk_quality_files(
    root: &Path,
    path: &Path,
    known_paths: &mut BTreeSet<String>,
    source_files: &mut Vec<SourceFile>,
    markdown_files: &mut Vec<SourceFile>,
    violations: &mut Vec<Violation>,
) {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    if metadata.file_type().is_symlink() {
        return;
    }
    if metadata.is_file() {
        add_file_to_inventory(
            root,
            path,
            known_paths,
            source_files,
            markdown_files,
            violations,
        );
        return;
    }
    if !metadata.is_dir() || ignored_quality_directory(path) {
        return;
    }
    let Ok(entries) = fs::read_dir(path) else {
        violations.push(Violation::QualityInputError {
            path: display_path(root, path),
            detail: "directory is not readable".to_owned(),
        });
        return;
    };
    let mut entries = entries.filter_map(Result::ok).collect::<Vec<_>>();
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        if known_paths.len() >= MAX_QUALITY_FILES {
            violations.push(Violation::QualityInputError {
                path: display_path(root, path),
                detail: format!("quality inventory exceeds {MAX_QUALITY_FILES} files"),
            });
            break;
        }
        walk_quality_files(
            root,
            &entry.path(),
            known_paths,
            source_files,
            markdown_files,
            violations,
        );
    }
}

fn ignored_quality_directory(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            matches!(
                name,
                ".git" | ".local" | ".codex" | "target" | "node_modules"
            )
        })
}

fn add_file_to_inventory(
    root: &Path,
    path: &Path,
    known_paths: &mut BTreeSet<String>,
    source_files: &mut Vec<SourceFile>,
    markdown_files: &mut Vec<SourceFile>,
    violations: &mut Vec<Violation>,
) {
    let display = display_path(root, path);
    if !known_paths.insert(display.clone()) {
        return;
    }
    let Ok(contents) = fs::read_to_string(path) else {
        // Binary files still contribute to link existence, but are not source
        // inputs.  A non-UTF8 file is therefore intentionally retained only
        // in the path inventory.
        return;
    };
    if contents.len() > MAX_QUALITY_BYTES {
        violations.push(Violation::QualityInputError {
            path: display,
            detail: format!("file exceeds {MAX_QUALITY_BYTES} bytes"),
        });
        return;
    }
    let file = SourceFile::new(display.clone(), contents);
    if has_extension(&display, "rs") {
        source_files.push(file.clone());
    }
    if has_extension(&display, "md") {
        markdown_files.push(file);
    }
}

fn collect_contract_projections(root: &Path, violations: &mut Vec<Violation>) -> Vec<SourceFile> {
    let directory = root.join(".config/contracts/schemas");
    let Ok(entries) = fs::read_dir(&directory) else {
        violations.push(Violation::QualityInputError {
            path: display_path(root, &directory),
            detail: "contract projection directory is not readable".to_owned(),
        });
        return Vec::new();
    };
    let mut paths = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    paths.sort();
    paths
        .into_iter()
        .filter(|path| path.extension().and_then(|extension| extension.to_str()) == Some("json"))
        .filter_map(|path| match fs::read_to_string(&path) {
            Ok(contents) => Some(SourceFile::new(display_path(root, &path), contents)),
            Err(error) => {
                violations.push(Violation::QualityInputError {
                    path: display_path(root, &path),
                    detail: error.to_string(),
                });
                None
            }
        })
        .collect()
}

fn display_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root).map_or_else(
        |_| path.to_string_lossy().into_owned(),
        |path| path.to_string_lossy().replace('\\', "/"),
    )
}

fn has_extension(path: &str, extension: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case(extension))
}

//! Workspace manifest lint policy gates.

use super::Violation;
use std::collections::BTreeMap;

const REQUIRED_RUST_LINTS: &[(&str, &str)] = &[
    ("unsafe_code", "forbid"),
    ("missing_docs", "warn"),
    ("unreachable_pub", "warn"),
    ("unused_lifetimes", "warn"),
    ("unused_qualifications", "warn"),
];

const REQUIRED_CLIPPY_LINTS: &[(&str, &str)] = &[
    ("all", "warn"),
    ("pedantic", "warn"),
    ("dbg_macro", "deny"),
    ("expect_used", "warn"),
    ("mem_forget", "deny"),
    ("panic", "warn"),
    ("todo", "deny"),
    ("unimplemented", "deny"),
    ("unwrap_used", "deny"),
];

/// Checks the required inherited lint tables in a root Cargo manifest and
/// every workspace-member manifest.
///
/// The package list contains `(package name, manifest contents)` pairs.  This
/// keeps the parser independent from filesystem state and makes mutations
/// straightforward to test.
#[must_use]
pub fn validate_workspace_lints(
    root_manifest: &str,
    package_manifests: &[(String, String)],
) -> Vec<Violation> {
    let mut violations = Vec::new();
    let tables = parse_manifest_lint_tables(root_manifest);
    for &(lint, expected) in REQUIRED_RUST_LINTS {
        check_lint_level(
            &mut violations,
            "workspace.lints.rust",
            lint,
            expected,
            &tables,
        );
    }
    for &(lint, expected) in REQUIRED_CLIPPY_LINTS {
        check_lint_level(
            &mut violations,
            "workspace.lints.clippy",
            lint,
            expected,
            &tables,
        );
    }

    for (package, manifest) in package_manifests {
        if !manifest_inherits_workspace_lints(manifest) {
            violations.push(Violation::MissingLintInheritance {
                package: package.clone(),
            });
        }
    }
    violations
}

fn check_lint_level(
    violations: &mut Vec<Violation>,
    table: &str,
    lint: &str,
    expected: &str,
    tables: &BTreeMap<String, BTreeMap<String, String>>,
) {
    let observed = tables.get(table).and_then(|values| values.get(lint));
    if observed.map(String::as_str) != Some(expected) {
        violations.push(Violation::MissingWorkspaceLint {
            table: table.to_owned(),
            lint: lint.to_owned(),
            expected: expected.to_owned(),
            observed: observed.cloned(),
        });
    }
}

fn parse_manifest_lint_tables(manifest: &str) -> BTreeMap<String, BTreeMap<String, String>> {
    let mut tables = BTreeMap::<String, BTreeMap<String, String>>::new();
    let mut section = String::new();
    for line in manifest.lines() {
        let line = strip_toml_comment(line).trim();
        if line.starts_with('[') && line.ends_with(']') {
            line.trim_start_matches('[')
                .trim_end_matches(']')
                .trim()
                .clone_into(&mut section);
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        if (section == "workspace.lints.rust" || section == "workspace.lints.clippy")
            && let Some(level) = lint_level(value.trim())
        {
            tables
                .entry(section.clone())
                .or_default()
                .insert(key.to_owned(), level);
        }
    }
    tables
}

fn strip_toml_comment(line: &str) -> &str {
    let mut quoted = false;
    let mut escaped = false;
    for (index, byte) in line.bytes().enumerate() {
        match byte {
            b'"' if !escaped => quoted = !quoted,
            b'#' if !quoted => return &line[..index],
            b'\\' if quoted => escaped = !escaped,
            _ => escaped = false,
        }
    }
    line
}

fn lint_level(value: &str) -> Option<String> {
    let value = value.trim();
    if let Some(rest) = value.strip_prefix('"') {
        return rest.split('"').next().map(str::to_owned);
    }
    let marker = "level";
    let start = value.find(marker)?;
    let rest = value[start + marker.len()..].trim_start();
    let rest = rest.strip_prefix('=')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    rest.split('"').next().map(str::to_owned)
}

fn manifest_inherits_workspace_lints(manifest: &str) -> bool {
    let mut section = String::new();
    for line in manifest.lines() {
        let line = strip_toml_comment(line).trim();
        if line.starts_with('[') && line.ends_with(']') {
            line.trim_start_matches('[')
                .trim_end_matches(']')
                .trim()
                .clone_into(&mut section);
            continue;
        }
        if section == "lints"
            && let Some((key, value)) = line.split_once('=')
            && key.trim() == "workspace"
            && value.trim() == "true"
        {
            return true;
        }
        // A dotted key is legal outside a table and is equivalent to the
        // explicit `[lints]` form.
        if let Some((key, value)) = line.split_once('=')
            && key.trim() == "lints.workspace"
            && value.trim() == "true"
        {
            return true;
        }
    }
    false
}

//! Repository module-size policy and explicit exception inventory.

use super::SourceFile;
use crate::Violation;
use serde::Deserialize;
use std::collections::BTreeSet;
use std::path::Path;

/// Validates production Rust module sizes against the checked-in policy.
///
/// Test corpus files are excluded by path so adding tests cannot make a
/// production module fail this gate. Generated or otherwise justified files
/// must be named in the exception inventory with a reason.
#[must_use]
pub fn validate_module_structure(
    files: &[SourceFile],
    policy_source: &SourceFile,
) -> Vec<Violation> {
    let policy: ModuleStructurePolicy = match serde_json::from_str(&policy_source.contents) {
        Ok(policy) => policy,
        Err(error) => {
            return vec![Violation::InvalidModuleStructurePolicy {
                path: policy_source.path.clone(),
                detail: error.to_string(),
            }];
        }
    };
    if let Err(detail) = policy.validate() {
        return vec![Violation::InvalidModuleStructurePolicy {
            path: policy_source.path.clone(),
            detail,
        }];
    }

    let known_paths = files
        .iter()
        .map(|file| file.path.as_str())
        .collect::<BTreeSet<_>>();
    let mut invalid_exceptions = Vec::new();
    for exception in &policy.exceptions {
        if !known_paths.contains(exception.path.as_str()) {
            invalid_exceptions.push(format!(
                "exception `{}` does not name an inventoried source file",
                exception.path
            ));
        } else if !is_rust_path(&exception.path) {
            invalid_exceptions.push(format!(
                "exception `{}` must target a Rust source file",
                exception.path
            ));
        }
    }
    if let Some(detail) = invalid_exceptions.into_iter().next() {
        return vec![Violation::InvalidModuleStructurePolicy {
            path: policy_source.path.clone(),
            detail,
        }];
    }

    files
        .iter()
        .filter(|file| {
            is_production_rust(file)
                && !policy
                    .exceptions
                    .iter()
                    .any(|entry| entry.path == file.path)
        })
        .filter_map(|file| {
            let lines = file.contents.lines().count();
            (lines > policy.max_production_lines).then(|| Violation::OversizedProductionModule {
                path: file.path.clone(),
                lines,
                limit: policy.max_production_lines,
            })
        })
        .collect()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModuleStructurePolicy {
    schema_version: u8,
    max_production_lines: usize,
    #[serde(default)]
    exceptions: Vec<ModuleStructureException>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModuleStructureException {
    path: String,
    kind: ModuleStructureExceptionKind,
    reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ModuleStructureExceptionKind {
    Generated,
    Schema,
    TestOnly,
}

impl ModuleStructurePolicy {
    fn validate(&self) -> Result<(), String> {
        if self.schema_version != 1 {
            return Err("schema_version must be 1".to_owned());
        }
        if self.max_production_lines == 0 {
            return Err("max_production_lines must be positive".to_owned());
        }
        let mut paths = BTreeSet::new();
        for exception in &self.exceptions {
            if exception.path.trim().is_empty() {
                return Err("exception path must not be empty".to_owned());
            }
            let normalized_path = exception.path.replace('\\', "/");
            let path = Path::new(&normalized_path);
            if path.is_absolute()
                || path
                    .components()
                    .any(|component| component.as_os_str() == "..")
            {
                return Err(format!(
                    "exception path must be repository-relative: `{}`",
                    exception.path
                ));
            }
            if !paths.insert(&exception.path) {
                return Err(format!(
                    "exception `{}` appears more than once",
                    exception.path
                ));
            }
            if exception.reason.trim().is_empty() {
                return Err(format!(
                    "exception `{}` must include a reason",
                    exception.path
                ));
            }
            if matches!(exception.kind, ModuleStructureExceptionKind::TestOnly)
                && !is_test_path(&exception.path)
            {
                return Err(format!(
                    "test-only exception `{}` must target a test path",
                    exception.path
                ));
            }
            if matches!(exception.kind, ModuleStructureExceptionKind::Generated)
                && !is_generated_path(&exception.path)
            {
                return Err(format!(
                    "generated exception `{}` must target a generated source path",
                    exception.path
                ));
            }
            if matches!(exception.kind, ModuleStructureExceptionKind::Schema)
                && !is_schema_path(&exception.path)
            {
                return Err(format!(
                    "schema exception `{}` must target a schema source path",
                    exception.path
                ));
            }
        }
        Ok(())
    }
}

fn is_production_rust(file: &SourceFile) -> bool {
    is_rust_path(&file.path) && !is_test_path(&file.path)
}

fn is_rust_path(path: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("rs"))
}

fn is_generated_path(path: &str) -> bool {
    path.replace('\\', "/").split('/').any(|component| {
        component == "generated"
            || component == "gen"
            || component.ends_with("_generated.rs")
            || component.ends_with("_gen.rs")
    }) || matches!(
        Path::new(path).file_name().and_then(|name| name.to_str()),
        Some("generated.rs" | "gen.rs")
    )
}

fn is_schema_path(path: &str) -> bool {
    path.replace('\\', "/").split('/').any(|component| {
        component == "schema"
            || component == "schemas"
            || component.ends_with("_schema.rs")
            || component.ends_with("_schemas.rs")
    }) || matches!(
        Path::new(path).file_name().and_then(|name| name.to_str()),
        Some("schema.rs" | "schemas.rs")
    )
}

fn is_test_path(path: &str) -> bool {
    let components = path
        .replace('\\', "/")
        .split('/')
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let Some(file) = components.last() else {
        return false;
    };
    components.iter().any(|component| {
        component == "tests"
            || component.ends_with("_tests")
            || component == "test"
            || component.ends_with("_test")
    }) || file == "tests.rs"
        || file.ends_with("_tests.rs")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> SourceFile {
        SourceFile::new(
            ".config/nix/policy/module-structure.json",
            r#"{
                "schema_version": 1,
                "max_production_lines": 2,
                "exceptions": [
                    {"path":"generated.rs","kind":"generated","reason":"checked-in generated fixture"}
                ]
            }"#,
        )
    }

    #[test]
    fn oversized_production_modules_are_rejected_but_test_and_inventory_exceptions_pass() {
        let files = [
            SourceFile::new("production.rs", "fn one() {}\nfn two() {}\nfn three() {}\n"),
            SourceFile::new("generated.rs", "fn one() {}\nfn two() {}\nfn three() {}\n"),
            SourceFile::new(
                "crates/demo/tests/large.rs",
                "fn one() {}\nfn two() {}\nfn three() {}\n",
            ),
        ];
        let violations = validate_module_structure(&files, &policy());
        assert_eq!(violations.len(), 1);
        assert!(matches!(
            &violations[0],
            Violation::OversizedProductionModule { path, lines: 3, limit: 2 }
                if path == "production.rs"
        ));
    }

    #[test]
    fn malformed_exception_inventory_is_rejected() {
        let policy = SourceFile::new(
            ".config/nix/policy/module-structure.json",
            r#"{"schema_version":1,"max_production_lines":2,"exceptions":[{"path":"generated.rs","kind":"generated","reason":""}]}"#,
        );
        assert!(matches!(
            validate_module_structure(&[], &policy).as_slice(),
            [Violation::InvalidModuleStructurePolicy { detail, .. }]
                if detail.contains("reason")
        ));
    }

    #[test]
    fn generated_and_schema_exceptions_are_narrow_and_must_name_files() {
        let generated = SourceFile::new(
            ".config/nix/policy/module-structure.json",
            r#"{"schema_version":1,"max_production_lines":2,"exceptions":[{"path":"generated.rs","kind":"generated","reason":"generated fixture"}]}"#,
        );
        assert!(
            validate_module_structure(
                &[SourceFile::new("generated.rs", "one\ntwo\nthree\n")],
                &generated,
            )
            .is_empty()
        );

        let schema = SourceFile::new(
            ".config/nix/policy/module-structure.json",
            r#"{"schema_version":1,"max_production_lines":2,"exceptions":[{"path":"schema.rs","kind":"schema","reason":"schema fixture"}]}"#,
        );
        assert!(
            validate_module_structure(
                &[SourceFile::new("schema.rs", "one\ntwo\nthree\n")],
                &schema,
            )
            .is_empty()
        );

        let missing = SourceFile::new(
            ".config/nix/policy/module-structure.json",
            r#"{"schema_version":1,"max_production_lines":2,"exceptions":[{"path":"generated.rs","kind":"generated","reason":"generated fixture"}]}"#,
        );
        assert!(matches!(
            validate_module_structure(&[], &missing).as_slice(),
            [Violation::InvalidModuleStructurePolicy { detail, .. }]
                if detail.contains("does not name")
        ));
    }
}

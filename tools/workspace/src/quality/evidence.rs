//! Canonical control-plane and cutover evidence gates.

use super::{SourceFile, Violation};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Validates the canonical cutover control-plane schema and every JSON Schema
/// projection against it.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "schema validation keeps canonical and projection diagnostics together"
)]
pub fn validate_contracts(canonical: &str, projections: &[SourceFile]) -> Vec<Violation> {
    let mut violations = Vec::new();
    let root: Value = match serde_json::from_str(canonical) {
        Ok(value) => value,
        Err(error) => {
            violations.push(Violation::InvalidContractSchema {
                path: "contracts/schema.json".to_owned(),
                detail: format!("malformed JSON: {error}"),
            });
            return violations;
        }
    };
    let Some(definitions) = root.get("definitions").and_then(Value::as_object) else {
        violations.push(Violation::InvalidContractSchema {
            path: "contracts/schema.json".to_owned(),
            detail: "definitions must be an object".to_owned(),
        });
        return violations;
    };
    let version = root
        .get("schema_version")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let encoding = root.get("encoding").and_then(Value::as_str);
    let hash_domain = root.get("hash_domain").and_then(Value::as_str);
    let unknown_fields = root.get("unknown_fields").and_then(Value::as_str);
    if version != 1
        || encoding != Some("canonical-json-v1")
        || hash_domain != Some("backend.control-plane.v1")
        || unknown_fields != Some("reject")
    {
        violations.push(Violation::InvalidContractSchema {
            path: "contracts/schema.json".to_owned(),
            detail: "schema version, encoding, hash domain, or unknown-field policy drifted"
                .to_owned(),
        });
    }

    let mut expected = BTreeMap::<String, BTreeSet<String>>::new();
    for (name, fields) in definitions {
        let Some(fields) = fields.as_array() else {
            violations.push(Violation::InvalidContractSchema {
                path: "contracts/schema.json".to_owned(),
                detail: format!("definition `{name}` must be an array"),
            });
            continue;
        };
        let mut names = BTreeSet::new();
        for field in fields {
            let Some(field) = field.as_str() else {
                violations.push(Violation::InvalidContractSchema {
                    path: "contracts/schema.json".to_owned(),
                    detail: format!("definition `{name}` has a non-string field"),
                });
                continue;
            };
            if field.is_empty() || !names.insert(field.to_owned()) {
                violations.push(Violation::InvalidContractSchema {
                    path: "contracts/schema.json".to_owned(),
                    detail: format!("definition `{name}` has an empty or duplicate field"),
                });
            }
        }
        expected.insert(name.clone(), names);
    }

    let mut observed = BTreeMap::<String, usize>::new();
    for projection in projections {
        if !has_extension(&projection.path, "json") {
            continue;
        }
        let value: Value = match serde_json::from_str(&projection.contents) {
            Ok(value) => value,
            Err(error) => {
                violations.push(Violation::InvalidContractSchema {
                    path: projection.path.clone(),
                    detail: format!("malformed JSON: {error}"),
                });
                continue;
            }
        };
        let Some(id) = value.get("$id").and_then(Value::as_str) else {
            violations.push(Violation::InvalidContractSchema {
                path: projection.path.clone(),
                detail: "projection has no string $id".to_owned(),
            });
            continue;
        };
        let Some(name) = id.strip_prefix("backend.control-plane.v1/") else {
            violations.push(Violation::InvalidContractSchema {
                path: projection.path.clone(),
                detail: format!("projection id `{id}` has the wrong domain"),
            });
            continue;
        };
        *observed.entry(name.to_owned()).or_default() += 1;
        if value
            .get("x-backend-schema-version")
            .and_then(Value::as_u64)
            != Some(version)
        {
            violations.push(Violation::InvalidContractSchema {
                path: projection.path.clone(),
                detail: "projection schema version differs from canonical schema".to_owned(),
            });
        }
        if value.get("type").and_then(Value::as_str) != Some("object")
            || value.get("additionalProperties") != Some(&Value::Bool(false))
        {
            violations.push(Violation::InvalidContractSchema {
                path: projection.path.clone(),
                detail: "projection must be a closed object schema".to_owned(),
            });
        }
        let required = strict_string_set(value.get("required"));
        let properties_object = value.get("properties").and_then(Value::as_object);
        let properties = properties_object
            .map(|properties| properties.keys().cloned().collect())
            .unwrap_or_default();
        let property_values_are_objects =
            properties_object.is_some_and(|properties| properties.values().all(Value::is_object));
        if required
            .as_ref()
            .is_none_or(|required| expected.get(name) != Some(required) || *required != properties)
            || !property_values_are_objects
        {
            violations.push(Violation::InvalidContractSchema {
                path: projection.path.clone(),
                detail: format!("required/properties fields do not match canonical `{name}`"),
            });
        }
    }
    for name in expected.keys() {
        match observed.get(name).copied().unwrap_or_default() {
            0 => violations.push(Violation::MissingContractProjection(name.clone())),
            1 => {}
            count => violations.push(Violation::DuplicateContractProjection {
                name: name.clone(),
                count,
            }),
        }
    }
    for name in observed.keys() {
        if !expected.contains_key(name) {
            violations.push(Violation::UnexpectedContractProjection(name.clone()));
        }
    }
    violations
}

/// Checks the static invariants consumed by the process-level cutover
/// harness in `.config/nu/cutover/e2e.nu`.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "the documented cutover contract has several independent static thresholds"
)]
pub fn validate_cutover_contract(source: &SourceFile) -> Vec<Violation> {
    let mut violations = Vec::new();
    let value: Value = match serde_json::from_str(&source.contents) {
        Ok(value) => value,
        Err(error) => {
            return vec![Violation::InvalidCutoverContract {
                path: source.path.clone(),
                detail: format!("malformed JSON: {error}"),
            }];
        }
    };
    let Some(object) = value.as_object() else {
        return vec![Violation::InvalidCutoverContract {
            path: source.path.clone(),
            detail: "contract root must be an object".to_owned(),
        }];
    };
    if object.get("schema").and_then(Value::as_u64) != Some(1) {
        violations.push(cutover_violation(
            source,
            "schema must be the documented version 1",
        ));
    }
    if object.get("suite").and_then(Value::as_str) != Some("versioned-engine-cutover-e2e") {
        violations.push(cutover_violation(
            source,
            "suite must be versioned-engine-cutover-e2e",
        ));
    }
    if object
        .get("timeout_seconds")
        .and_then(Value::as_u64)
        .is_none_or(|seconds| seconds == 0)
    {
        violations.push(cutover_violation(
            source,
            "timeout_seconds must be a positive integer",
        ));
    }
    require_string_array(
        &mut violations,
        source,
        object.get("failure_injections"),
        "failure_injections",
        3,
    );
    let counters = object.get("work_counters").and_then(Value::as_object);
    if counters.is_none_or(|counters| counters.len() < 4) {
        violations.push(cutover_violation(
            source,
            "work_counters must contain at least four named counters",
        ));
    } else if counters.is_some_and(|counters| {
        counters
            .values()
            .any(|value| value.as_str().is_none_or(str::is_empty))
    }) {
        violations.push(cutover_violation(
            source,
            "work_counters values must be non-empty strings",
        ));
    }
    let process = object
        .get("process_remote_contract")
        .and_then(Value::as_object);
    let Some(process) = process else {
        violations.push(cutover_violation(
            source,
            "process_remote_contract must be an object",
        ));
        return violations;
    };
    if process.get("locald_profile").and_then(Value::as_str) != Some("builtin") {
        violations.push(cutover_violation(
            source,
            "locald_profile must remain builtin",
        ));
    }
    if process.get("worker_profile").and_then(Value::as_str) != Some("builtin") {
        violations.push(cutover_violation(
            source,
            "worker_profile must remain builtin",
        ));
    }
    for field in ["worker_endpoint_option", "request_envelope"] {
        if process
            .get(field)
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
        {
            violations.push(cutover_violation(
                source,
                &format!("process_remote_contract.{field} must be a non-empty string"),
            ));
        }
    }
    require_string_array(
        &mut violations,
        source,
        process.get("required_observations"),
        "process_remote_contract.required_observations",
        8,
    );
    require_string_array(
        &mut violations,
        source,
        object.get("journeys"),
        "journeys",
        1,
    );
    violations
}

/// Checks the mutation campaign metadata used by the cutover ledger tests.
#[must_use]
pub fn validate_cutover_mutations(source: &SourceFile) -> Vec<Violation> {
    let mut violations = Vec::new();
    let value: Value = match serde_json::from_str(&source.contents) {
        Ok(value) => value,
        Err(error) => {
            return vec![Violation::InvalidCutoverContract {
                path: source.path.clone(),
                detail: format!("malformed JSON: {error}"),
            }];
        }
    };
    if value.get("schema_version").and_then(Value::as_u64) != Some(1) {
        violations.push(cutover_violation(
            source,
            "schema_version must be the documented version 1",
        ));
    }
    let Some(mutations) = value.get("mutations").and_then(Value::as_array) else {
        violations.push(cutover_violation(source, "mutations must be an array"));
        return violations;
    };
    if mutations.len() < 15 {
        violations.push(cutover_violation(
            source,
            "mutation campaign must retain at least fifteen cases",
        ));
    }
    let mut ids = BTreeSet::new();
    for (index, mutation) in mutations.iter().enumerate() {
        let Some(mutation) = mutation.as_object() else {
            violations.push(cutover_violation(
                source,
                &format!("mutation {index} must be an object"),
            ));
            continue;
        };
        for field in ["id", "input", "expected"] {
            if mutation
                .get(field)
                .and_then(Value::as_str)
                .is_none_or(|text| text.trim().is_empty())
            {
                violations.push(cutover_violation(
                    source,
                    &format!("mutation {index} requires non-empty `{field}`"),
                ));
            }
        }
        if let Some(id) = mutation.get("id").and_then(Value::as_str)
            && !ids.insert(id.to_owned())
        {
            violations.push(cutover_violation(
                source,
                &format!("mutation id `{id}` is duplicated"),
            ));
        }
    }
    violations
}

/// Checks the declared exactly-one ownership fixture against workspace package
/// roots. Shared documentation and configuration roots are declared
/// separately from package ownership, so every live path has one owner.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "the ownership fixture has independent structural and package-root checks"
)]
pub fn validate_scope_fixture(
    source: &SourceFile,
    package_roots: &BTreeSet<String>,
) -> Vec<Violation> {
    let mut violations = Vec::new();
    let value: Value = match serde_json::from_str(&source.contents) {
        Ok(value) => value,
        Err(error) => {
            return vec![Violation::InvalidScopeFixture {
                path: source.path.clone(),
                detail: format!("malformed JSON: {error}"),
            }];
        }
    };
    if value.get("fixture").and_then(Value::as_str) != Some("scope-exactly-one") {
        violations.push(scope_violation(source, "fixture must be scope-exactly-one"));
    }
    let Some(inventory) = value.get("inventory").and_then(Value::as_object) else {
        violations.push(scope_violation(source, "inventory must be an object"));
        return violations;
    };
    let Some(mechanical) = inventory
        .get("mechanical_scopes")
        .and_then(Value::as_object)
    else {
        violations.push(scope_violation(
            source,
            "inventory.mechanical_scopes must be an object",
        ));
        return violations;
    };
    let expected_roots = [
        "apps",
        "crates",
        "extensions",
        "frontends",
        "tests",
        "tools",
    ];
    let observed_roots = mechanical
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let expected_root_set = expected_roots.iter().copied().collect::<BTreeSet<_>>();
    if observed_roots != expected_root_set {
        violations.push(scope_violation(
            source,
            "mechanical_scopes must cover exactly apps, crates, extensions, frontends, tests, tools",
        ));
    }
    if mechanical
        .values()
        .any(|value| value.as_str().is_none_or(|scope| scope.trim().is_empty()))
    {
        violations.push(scope_violation(
            source,
            "mechanical scope names must be non-empty strings",
        ));
    }
    for package_root in package_roots {
        let matches = expected_roots
            .iter()
            .filter(|root| package_root == **root || package_root.starts_with(&format!("{root}/")))
            .count();
        if matches != 1 {
            violations.push(scope_violation(
                source,
                &format!("workspace package root `{package_root}` has {matches} mechanical scopes"),
            ));
        }
    }
    let Some(cases) = value.get("cases").and_then(Value::as_array) else {
        violations.push(scope_violation(source, "cases must be an array"));
        return violations;
    };
    let mut case_paths = BTreeSet::new();
    for (index, case) in cases.iter().enumerate() {
        let Some(case) = case.as_object() else {
            violations.push(scope_violation(
                source,
                &format!("case {index} must be an object"),
            ));
            continue;
        };
        let Some(path) = case.get("path").and_then(Value::as_str) else {
            violations.push(scope_violation(
                source,
                &format!("case {index} requires a path"),
            ));
            continue;
        };
        if path.trim().is_empty() || !case_paths.insert(path.to_owned()) {
            violations.push(scope_violation(
                source,
                &format!("case path `{path}` must be non-empty and unique"),
            ));
        }
        if case
            .get("scopes")
            .and_then(Value::as_array)
            .is_none_or(|scopes| scopes.len() != 1)
        {
            violations.push(scope_violation(
                source,
                &format!("case `{path}` must name exactly one scope"),
            ));
        }
    }
    violations
}

fn scope_violation(source: &SourceFile, detail: &str) -> Violation {
    Violation::InvalidScopeFixture {
        path: source.path.clone(),
        detail: detail.to_owned(),
    }
}

fn require_string_array(
    violations: &mut Vec<Violation>,
    source: &SourceFile,
    value: Option<&Value>,
    field: &str,
    minimum: usize,
) {
    let Some(values) = value.and_then(Value::as_array) else {
        violations.push(cutover_violation(
            source,
            &format!("{field} must be an array of strings"),
        ));
        return;
    };
    if values.len() < minimum {
        violations.push(cutover_violation(
            source,
            &format!("{field} must contain at least {minimum} entries"),
        ));
    }
    let mut seen = BTreeSet::new();
    for value in values {
        let Some(value) = value.as_str() else {
            violations.push(cutover_violation(
                source,
                &format!("{field} entries must be strings"),
            ));
            continue;
        };
        if value.trim().is_empty() || !seen.insert(value) {
            violations.push(cutover_violation(
                source,
                &format!("{field} entries must be non-empty and unique"),
            ));
        }
    }
}

fn cutover_violation(source: &SourceFile, detail: &str) -> Violation {
    Violation::InvalidCutoverContract {
        path: source.path.clone(),
        detail: detail.to_owned(),
    }
}

fn strict_string_set(value: Option<&Value>) -> Option<BTreeSet<String>> {
    let values = value?.as_array()?;
    let mut output = BTreeSet::new();
    for value in values {
        let value = value.as_str()?;
        if !output.insert(value.to_owned()) {
            return None;
        }
    }
    Some(output)
}

fn has_extension(path: &str, extension: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case(extension))
}

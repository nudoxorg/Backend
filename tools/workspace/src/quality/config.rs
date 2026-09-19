//! Static policy/configuration gates.

use super::{SourceFile, Violation};
use std::collections::BTreeSet;

/// Checks the generated Koji/Nix control policy for references to the legacy
/// package and owner vocabulary.  The policy is intentionally supplied as a
/// source snapshot: this keeps the gate independent of a Nix evaluator while
/// still checking the concrete strings that select package scopes and
/// measurements.
#[must_use]
pub fn validate_policy_control(
    policy: &SourceFile,
    product_packages: &BTreeSet<String>,
) -> Vec<Violation> {
    let mut violations = Vec::new();
    let required_roots = [
        "^/crates/",
        "^/frontends/",
        "^/extensions/",
        "^/apps/",
        "^/tests/",
        "^/tools/",
    ];
    let mut observed_roots = BTreeSet::new();
    for (line_number, line) in policy.contents.lines().enumerate() {
        let line = strip_nix_comment(line);
        for value in nix_string_values(&line) {
            if let Some(reference) = legacy_policy_reference(&value, product_packages) {
                violations.push(Violation::StalePolicyPackageReference {
                    path: policy.path.clone(),
                    line: line_number + 1,
                    reference,
                });
            }
            for root in required_roots {
                // A policy may split one mechanical root into exact package
                // scopes (for example `^/apps/cli/` and `^/apps/mcp/`).
                // Treat an anchored descendant as coverage of its root while
                // keeping unanchored prose from satisfying the check.
                if value.starts_with(root) {
                    observed_roots.insert(root);
                }
            }
        }
    }
    for root in required_roots {
        if !observed_roots.contains(root) {
            violations.push(Violation::MissingPolicyScope {
                path: policy.path.clone(),
                scope: root.to_owned(),
            });
        }
    }
    violations
}

fn strip_nix_comment(line: &str) -> String {
    let mut quoted = false;
    let mut escaped = false;
    for (index, byte) in line.bytes().enumerate() {
        match byte {
            b'"' if !escaped => quoted = !quoted,
            b'#' if !quoted => return line[..index].to_owned(),
            b'\\' if quoted => escaped = !escaped,
            _ => escaped = false,
        }
    }
    line.to_owned()
}

fn nix_string_values(line: &str) -> Vec<String> {
    let bytes = line.as_bytes();
    let mut values = Vec::new();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] != b'"' {
            index += 1;
            continue;
        }
        index += 1;
        let mut value = String::new();
        let mut escaped = false;
        while index < bytes.len() {
            let byte = bytes[index];
            index += 1;
            if escaped {
                value.push(byte as char);
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                break;
            } else {
                value.push(byte as char);
            }
        }
        values.push(value);
    }
    values
}

fn legacy_policy_reference(value: &str, product_packages: &BTreeSet<String>) -> Option<String> {
    const LEGACY_ROOTS: [&str; 4] = ["^/compiler/", "^/heart/", "^/interface/", "^/server/"];
    if LEGACY_ROOTS.iter().any(|root| value.contains(root)) {
        return Some(value.to_owned());
    }
    let candidate = value.trim();
    let package_like = candidate.starts_with("compiler-")
        || candidate.starts_with("heart-")
        || candidate.starts_with("interface-")
        || candidate.starts_with("server-");
    if package_like && !product_packages.contains(candidate) {
        return Some(candidate.to_owned());
    }
    None
}

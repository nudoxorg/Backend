//! Focused repository quality checks exposed through one small validator API.

use crate::Violation;

/// One repository file supplied to a source, Markdown, or contract gate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceFile {
    /// Repository-relative or otherwise displayable path.
    pub path: String,
    /// UTF-8 file contents.
    pub contents: String,
}

impl SourceFile {
    /// Creates a named source snapshot.
    #[must_use]
    pub fn new(path: impl Into<String>, contents: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            contents: contents.into(),
        }
    }
}

mod config;
mod docs;
mod evidence;
mod lints;
mod source;
mod structure;

pub use config::validate_policy_control;
pub use docs::validate_markdown_links;
pub use evidence::{
    validate_contracts, validate_cutover_contract, validate_cutover_mutations,
    validate_scope_fixture,
};
pub use lints::validate_workspace_lints;
pub use source::{validate_architecture_guards, validate_rust_sources};
pub use structure::validate_module_structure;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn root_manifest() -> String {
        r#"
[workspace.lints.rust]
unsafe_code = "deny"
missing_docs = "warn"
unreachable_pub = "warn"
unused_lifetimes = "warn"
unused_qualifications = "warn"

[workspace.lints.clippy]
all = { level = "warn", priority = -1 }
pedantic = { level = "warn", priority = -1 }
dbg_macro = "deny"
expect_used = "warn"
mem_forget = "deny"
panic = "warn"
todo = "deny"
unimplemented = "deny"
unwrap_used = "deny"
"#
        .to_owned()
    }

    fn inherited_manifest() -> String {
        "[lints]\nworkspace = true\n".to_owned()
    }

    #[test]
    fn lint_level_mutation_is_rejected() {
        let mutated = root_manifest().replace("unwrap_used = \"deny\"", "unwrap_used = \"warn\"");
        let violations = validate_workspace_lints(
            &mutated,
            &[("backend-demo".to_owned(), inherited_manifest())],
        );
        assert!(violations.iter().any(|violation| matches!(
            violation,
            Violation::MissingWorkspaceLint { table, lint, expected, observed }
                if table == "workspace.lints.clippy"
                    && lint == "unwrap_used"
                    && expected == "deny"
                    && observed.as_deref() == Some("warn")
        )));
    }

    #[test]
    fn workspace_forbid_unsafe_is_rejected_in_favor_of_deny() {
        let mutated = root_manifest().replace(
            "unsafe_code = \"deny\"",
            "unsafe_code = \"forbid\"",
        );
        let violations = validate_workspace_lints(
            &mutated,
            &[("backend-demo".to_owned(), inherited_manifest())],
        );
        assert!(violations.iter().any(|violation| matches!(
            violation,
            Violation::MissingWorkspaceLint { table, lint, expected, observed }
                if table == "workspace.lints.rust"
                    && lint == "unsafe_code"
                    && expected == "deny"
                    && observed.as_deref() == Some("forbid")
        )));
    }

    #[test]
    fn missing_lint_inheritance_is_rejected() {
        let violations = validate_workspace_lints(
            &root_manifest(),
            &[(
                "backend-demo".to_owned(),
                "[package]\nname = \"demo\"\n".to_owned(),
            )],
        );
        assert!(violations.iter().any(|violation| matches!(
            violation,
            Violation::MissingLintInheritance { package } if package == "backend-demo"
        )));
    }

    #[test]
    fn production_escape_hatches_are_rejected_but_literals_and_test_modules_are_ignored() {
        let source = SourceFile::new(
            "crates/demo/src/lib.rs",
            r#"
fn production() {
    let _literal = "panic!() unwrap() expect() todo!() unimplemented!()";
    /* panic!() */
    let _ = Option::<u8>::None.unwrap();
    unreachable!();
}
#[cfg(test)]
mod tests { fn only_test() { panic!(); } }
"#,
        );
        let violations = validate_rust_sources(&[source]);
        assert!(violations.iter().any(|violation| matches!(
            violation,
            Violation::ForbiddenProductionConstruct { construct, .. } if construct == "unwrap()"
        )));
        assert!(!violations.iter().any(|violation| matches!(
            violation,
            Violation::ForbiddenProductionConstruct { construct, .. } if construct == "panic!"
        )));
        assert!(violations.iter().any(|violation| matches!(
            violation,
            Violation::ForbiddenProductionConstruct { construct, .. }
                if construct == "unreachable!"
        )));

        let helper_names = SourceFile::new(
            "crates/demo/src/lib.rs",
            "fn unwrap(value: u8) -> u8 { value }\nfn expect(value: u8) -> u8 { value }\nfn production() { let _ = unwrap(1); let _ = expect(1); }\n",
        );
        assert!(
            !validate_rust_sources(&[helper_names])
                .iter()
                .any(|violation| matches!(
                    violation,
                    Violation::ForbiddenProductionConstruct { .. }
                ))
        );

        let generic_method = SourceFile::new(
            "crates/demo/src/lib.rs",
            "trait OptionalExt { fn unwrap<T>(&self) -> T; }\nfn production<T, V: OptionalExt>(value: &V) { let _: T = value.unwrap::<T>(); }\n",
        );
        assert!(
            validate_rust_sources(&[generic_method])
                .iter()
                .any(|violation| {
                    matches!(
                        violation,
                        Violation::ForbiddenProductionConstruct { construct, .. }
                            if construct == "unwrap()"
                    )
                })
        );

        let async_test = SourceFile::new(
            "crates/demo/src/lib.rs",
            "#[tokio::test]\nasync fn case() { panic!(); }\n",
        );
        assert!(
            !validate_rust_sources(&[async_test])
                .iter()
                .any(|violation| matches!(
                    violation,
                    Violation::ForbiddenProductionConstruct { .. }
                ))
        );

        let unix_test = SourceFile::new(
            "crates/demo/src/lib.rs",
            "#[cfg(all(test, unix))]\nmod tests { fn case() { panic!(); } }\n",
        );
        assert!(
            !validate_rust_sources(&[unix_test])
                .iter()
                .any(|violation| matches!(
                    violation,
                    Violation::ForbiddenProductionConstruct { .. }
                ))
        );

        let any_test = SourceFile::new(
            "crates/demo/src/lib.rs",
            "#[cfg(any(test, unix))]\nmod maybe_production { fn case() { panic!(); } }\n",
        );
        assert!(
            validate_rust_sources(&[any_test])
                .iter()
                .any(|violation| matches!(
                    violation,
                    Violation::ForbiddenProductionConstruct { .. }
                ))
        );
    }

    fn ownership_policy() -> SourceFile {
        SourceFile::new(
            ".config/nix/policy/canonical-ownership.json",
            r#"{
                "schema_version": 1,
                "rules": [
                    {"domain":"version-node-object-identity","owner":"crates/demo/src/lib.rs","literal":"V2NODE\\0"},
                    {"domain":"product-closure-manifest-row-object","owner":"crates/demo/src/lib.rs","encoder":"canonical_relation_row"},
                    {"domain":"local-ldc2","owner":"crates/demo/src/lib.rs","literal":"LDC2"},
                    {"domain":"replication-frame","owner":"crates/demo/src/lib.rs","encoder":"encode_message"},
                    {"domain":"control-work-spec-wire","owner":"crates/demo/src/lib.rs","encoder":"append_canonical"},
                    {"domain":"control-record-wire","owner":"crates/demo/src/lib.rs","encoder":"append_receipt_unchecked"},
                    {"domain":"dispatch-authority-statement","owner":"crates/demo/src/lib.rs","encoder":"keyed_authority_statement"},
                    {"domain":"store-manifest-descriptor-node-envelope","owner":"crates/demo/src/lib.rs","literal":"LUNA_MANIFEST_INDEX_V2\\0"},
                    {"domain":"flow-checkpoint-manifest","owner":"crates/demo/src/lib.rs","encoder":"encode_manifest"}
                ]
            }"#,
        )
    }

    #[test]
    fn architecture_guard_mutations_turn_red_and_shared_signers_remain_allowed() {
        let owner = SourceFile::new(
            "crates/demo/src/lib.rs",
            "fn sign_with_key(value: &[u8]) -> &[u8] { value }\nfn verify_with_key(value: &[u8]) -> bool { !value.is_empty() }\n",
        );
        let policy = ownership_policy();
        assert!(validate_architecture_guards(std::slice::from_ref(&owner), &policy).is_empty());

        let removed = SourceFile::new(
            "crates/demo/src/removed.rs",
            "fn production() { let _ = backend_version::ObjectVersion::<S>::from_authenticated_digest([0; 32]); }\n",
        );
        assert!(
            validate_architecture_guards(&[owner.clone(), removed], &policy)
                .iter()
                .any(|violation| matches!(
                    violation,
                    Violation::RemovedIdentityConstructor { symbol, .. }
                        if symbol == "from_authenticated_digest"
                ))
        );

        let authorityless = SourceFile::new(
            "crates/demo/src/publication.rs",
            "fn production() { publish_inner(None, || Ok::<(), ()>(())); }\n",
        );
        assert!(
            validate_architecture_guards(&[owner.clone(), authorityless], &policy)
                .iter()
                .any(|violation| matches!(
                    violation,
                    Violation::AuthoritylessStorePublication { call, .. }
                        if call == "publish_inner(None)"
                ))
        );

        let duplicate_literal = SourceFile::new(
            "crates/demo/src/duplicate.rs",
            "const DUPLICATE: &[u8] = b\"LDC2\";\n",
        );
        assert!(
            validate_architecture_guards(&[owner.clone(), duplicate_literal], &policy)
                .iter()
                .any(|violation| matches!(
                    violation,
                    Violation::CanonicalGrammarOutsideOwner { domain, marker, .. }
                        if domain == "local-ldc2" && marker == "LDC2"
                ))
        );

        let duplicate_encoder = SourceFile::new(
            "crates/demo/src/duplicate_encoder.rs",
            "fn canonical_relation_row(key: &[u8], value: &[u8]) -> Vec<u8> { [key, value].concat() }\n",
        );
        assert!(
            validate_architecture_guards(&[owner, duplicate_encoder], &policy)
                .iter()
                .any(|violation| matches!(
                    violation,
                    Violation::CanonicalGrammarOutsideOwner { domain, marker, .. }
                        if domain == "product-closure-manifest-row-object"
                            && marker == "canonical_relation_row"
                ))
        );
    }

    #[test]
    fn new_canonical_grammar_mutations_turn_red() {
        let owner = SourceFile::new("crates/demo/src/lib.rs", "");
        let policy = ownership_policy();
        let mutations = [
            (
                "control-work-spec-wire",
                "append_canonical",
                "fn append_canonical(value: &[u8], out: &mut Vec<u8>) { out.extend_from_slice(value); }\n",
            ),
            (
                "control-record-wire",
                "append_receipt_unchecked",
                "fn append_receipt_unchecked(bytes: &[u8], output: &mut Vec<u8>) { output.extend_from_slice(bytes); }\n",
            ),
            (
                "dispatch-authority-statement",
                "keyed_authority_statement",
                "fn keyed_authority_statement(value: &[u8]) -> Vec<u8> { value.to_vec() }\n",
            ),
            (
                "store-manifest-descriptor-node-envelope",
                "LUNA_MANIFEST_INDEX_V2\\0",
                "const DUPLICATE: &[u8] = b\"LUNA_MANIFEST_INDEX_V2\\0\";\n",
            ),
            (
                "flow-checkpoint-manifest",
                "encode_manifest",
                "fn encode_manifest(value: &[u8]) -> Vec<u8> { value.to_vec() }\n",
            ),
        ];
        for (domain, marker, contents) in mutations {
            let duplicate = SourceFile::new("crates/demo/src/duplicate.rs", contents);
            assert!(
                validate_architecture_guards(&[owner.clone(), duplicate], &policy)
                    .iter()
                    .any(|violation| matches!(
                        violation,
                        Violation::CanonicalGrammarOutsideOwner { domain: found, marker: observed, .. }
                            if found == domain && observed == marker
                    )),
                "mutation for {domain}/{marker} did not turn red"
            );
        }
    }

    #[test]
    fn architecture_policy_mutations_are_rejected() {
        let owner = SourceFile::new("crates/demo/src/lib.rs", "");
        let policy = ownership_policy();
        let mutated = SourceFile::new(
            policy.path.clone(),
            policy
                .contents
                .replace("\"schema_version\": 1", "\"schema_version\": 2"),
        );
        assert!(
            validate_architecture_guards(&[owner], &mutated)
                .iter()
                .any(|violation| matches!(
                    violation,
                    Violation::InvalidCanonicalOwnershipPolicy { detail, .. }
                        if detail.contains("schema_version must be 1")
                ))
        );

        let duplicate_marker = SourceFile::new(
            policy.path.clone(),
            policy.contents.replace(
                "                ]",
                "                ,{\"domain\":\"local-ldc2\",\"owner\":\"crates/demo/src/lib.rs\",\"literal\":\"LDC2\"}\n                ]",
            ),
        );
        assert!(
            validate_architecture_guards(
                &[SourceFile::new("crates/demo/src/lib.rs", "")],
                &duplicate_marker
            )
            .iter()
            .any(|violation| matches!(
                violation,
                Violation::InvalidCanonicalOwnershipPolicy { detail, .. }
                    if detail.contains("appears more than once")
            ))
        );

        let missing_owner = SourceFile::new(
            policy.path.clone(),
            policy
                .contents
                .replace("crates/demo/src/lib.rs", "crates/demo/src/missing.rs"),
        );
        assert!(
            validate_architecture_guards(
                &[SourceFile::new("crates/demo/src/lib.rs", "")],
                &missing_owner
            )
            .iter()
            .any(|violation| matches!(
                violation,
                Violation::InvalidCanonicalOwnershipPolicy { detail, .. }
                    if detail.contains("not an inventoried")
            ))
        );
    }

    #[test]
    fn broad_allow_mutation_is_rejected() {
        let source = SourceFile::new(
            "crates/demo/src/lib.rs",
            "#![allow(clippy::all)]\n pub fn value() -> u8 { 1 }\n",
        );
        let violations = validate_rust_sources(&[source]);
        assert!(violations.iter().any(|violation| matches!(
            violation,
            Violation::BroadLintAllow { lints, .. } if lints == &["clippy::all".to_owned()]
        )));

        let test_only = SourceFile::new(
            "crates/demo/src/lib.rs",
            "#[allow(clippy::all)]\n#[cfg(test)]\nmod tests { fn case() { panic!(); } }\n",
        );
        assert!(
            !validate_rust_sources(&[test_only])
                .iter()
                .any(|violation| matches!(violation, Violation::BroadLintAllow { .. }))
        );

        let conditional = SourceFile::new(
            "crates/demo/src/lib.rs",
            "#[cfg_attr(feature = \"debug\", allow(clippy::all))]\npub fn value() -> u8 { 1 }\n",
        );
        assert!(
            validate_rust_sources(&[conditional])
                .iter()
                .any(|violation| matches!(violation, Violation::BroadLintAllow { .. }))
        );

        let test_conditional = SourceFile::new(
            "crates/demo/src/lib.rs",
            "#[cfg_attr(test, allow(clippy::all))]\nfn case() { panic!(); }\n",
        );
        assert!(
            !validate_rust_sources(&[test_conditional])
                .iter()
                .any(|violation| matches!(violation, Violation::BroadLintAllow { .. }))
        );

        let inner_test_allow = SourceFile::new(
            "crates/demo/src/lib.rs",
            "#[cfg(test)]\nmod tests {\n    #![allow(clippy::expect_used, clippy::unwrap_used)]\n    fn case() { panic!(); }\n}\n",
        );
        assert!(
            !validate_rust_sources(&[inner_test_allow])
                .iter()
                .any(|violation| matches!(violation, Violation::BroadLintAllow { .. }))
        );

        let actual = SourceFile::new(
            "crates/flow/src/arrangement/checkpoint/lazy.rs",
            include_str!("../../../crates/flow/src/arrangement/checkpoint/lazy.rs"),
        );
        assert!(
            !validate_rust_sources(&[actual])
                .iter()
                .any(|violation| matches!(violation, Violation::BroadLintAllow { .. }))
        );
    }

    #[test]
    fn duplicate_recursive_kernel_mutation_is_rejected() {
        let source = SourceFile::new(
            "crates/semantic/src/duplicate.rs",
            r"
use std::sync::Arc;
struct DuplicateNode {
    left: Option<Arc<Self>>,
    right: Option<Arc<Self>>,
}
",
        );
        let violations = validate_rust_sources(&[source]);
        assert!(violations.iter().any(|violation| matches!(
            violation,
            Violation::DuplicatePathCopyKernel { name, .. } if name == "DuplicateNode"
        )));

        let named = SourceFile::new(
            "crates/semantic/src/named_duplicate.rs",
            r"
use std::sync::Arc;
struct NamedNode {
    left: Option<Arc<NamedNode>>,
    children: Option<Arc<NamedNode>>,
}
",
        );
        assert!(
            validate_rust_sources(&[named])
                .iter()
                .any(|violation| matches!(
                    violation,
                    Violation::DuplicatePathCopyKernel { name, .. } if name == "NamedNode"
                ))
        );

        let test_node = SourceFile::new(
            "crates/semantic/src/lib.rs",
            "#[cfg(test)]\nmod tests { struct TestNode { left: Option<Arc<Self>>, right: Option<Arc<Self>> } }\n",
        );
        assert!(
            !validate_rust_sources(&[test_node])
                .iter()
                .any(|violation| matches!(violation, Violation::DuplicatePathCopyKernel { .. }))
        );
    }

    #[test]
    fn broken_markdown_mutation_is_rejected() {
        let files = [SourceFile::new(
            "docs/index.md",
            "[valid](guide.md)\n[broken](missing.md)\n<!-- [comment](also-missing.md) -->\n```md\n[example](also-missing.md)\n```\n",
        )];
        let known = BTreeSet::from(["docs/index.md".to_owned(), "docs/guide.md".to_owned()]);
        let violations = validate_markdown_links(&files, &known);
        assert_eq!(
            violations,
            vec![Violation::BrokenMarkdownLink {
                path: "docs/index.md".to_owned(),
                line: 2,
                target: "missing.md".to_owned(),
            }]
        );
    }

    #[test]
    fn markdown_reference_and_image_mutations_are_rejected() {
        let files = [SourceFile::new(
            "docs/index.md",
            "[guide][Guide]\n![logo][logo]\n[Guide]: guide.md\n[logo]: logo.svg\n[missing][unknown]\n",
        )];
        let known = BTreeSet::from([
            "docs/index.md".to_owned(),
            "docs/guide.md".to_owned(),
            "docs/logo.svg".to_owned(),
        ]);
        let valid = validate_markdown_links(&files, &known);
        assert_eq!(
            valid,
            vec![Violation::BrokenMarkdownLink {
                path: "docs/index.md".to_owned(),
                line: 5,
                target: "unknown".to_owned(),
            }]
        );

        let mutated = [SourceFile::new(
            "docs/index.md",
            files[0]
                .contents
                .replace("[logo]: logo.svg", "[logo]: missing.svg"),
        )];
        assert!(
            validate_markdown_links(&mutated, &known)
                .iter()
                .any(|violation| {
                    matches!(
                        violation,
                        Violation::BrokenMarkdownLink { target, .. } if target == "missing.svg"
                    )
                })
        );
    }

    #[test]
    fn contract_projection_mutation_is_rejected() {
        let canonical = r#"{
            "schema_version": 1,
            "encoding": "canonical-json-v1",
            "hash_domain": "backend.control-plane.v1",
            "unknown_fields": "reject",
            "definitions": {"Demo": ["value"]}
        }"#;
        let projection = SourceFile::new(
            "contracts/schemas/demo.json",
            r#"{
                "$schema":"https://json-schema.org/draft/2020-12/schema",
                "$id":"backend.control-plane.v1/Demo",
                "x-backend-schema-version":1,
                "type":"object",
                "additionalProperties":false,
                "required":[],
                "properties":{}
            }"#,
        );
        let projection_contents = projection.contents.clone();
        let violations = validate_contracts(canonical, &[projection]);
        assert!(violations.iter().any(|violation| matches!(
            violation,
            Violation::InvalidContractSchema { detail, .. }
                if detail.contains("required/properties fields")
        )));

        let non_string_required = SourceFile::new(
            "contracts/schemas/demo.json",
            projection_contents
                .replace("\"required\":[]", "\"required\":[1]")
                .replace(
                    "\"properties\":{}",
                    "\"properties\":{\"value\":{\"type\":\"string\"}}",
                ),
        );
        assert!(
            validate_contracts(canonical, &[non_string_required])
                .iter()
                .any(|violation| matches!(
                    violation,
                    Violation::InvalidContractSchema { detail, .. }
                        if detail.contains("required/properties fields")
                ))
        );
    }

    #[test]
    fn cutover_contract_mutations_are_rejected() {
        let valid = SourceFile::new(
            ".config/nu/cutover/e2e-contract.json",
            r#"{
                "schema": 1,
                "suite": "versioned-engine-cutover-e2e",
                "timeout_seconds": 45,
                "work_counters": {"a":"a","b":"b","c":"c","d":"d"},
                "process_remote_contract": {
                    "locald_profile":"builtin",
                    "worker_profile":"builtin",
                    "worker_endpoint_option":"--worker-endpoint",
                    "request_envelope":"EngineRequest::Replicate",
                    "required_observations":["a","b","c","d","e","f","g","h"]
                },
                "failure_injections":["a","b","c"],
                "journeys":["journey"]
            }"#,
        );
        assert!(validate_cutover_contract(&valid).is_empty());
        let mutated = SourceFile::new(
            valid.path.clone(),
            valid.contents.replace("\"schema\": 1", "\"schema\": 2"),
        );
        assert!(
            validate_cutover_contract(&mutated)
                .iter()
                .any(|violation| matches!(
                    violation,
                    Violation::InvalidCutoverContract { detail, .. }
                        if detail.contains("schema must be")
                ))
        );
    }

    #[test]
    fn mutation_fixture_contract_rejects_duplicate_or_missing_cases() {
        let source = SourceFile::new(
            ".config/fixtures/control-plane/cutover-mutations.json",
            r#"{"schema_version":1,"mutations":[{"id":"same","input":"x","expected":"y"}]}"#,
        );
        let violations = validate_cutover_mutations(&source);
        assert!(violations.iter().any(|violation| matches!(
            violation,
            Violation::InvalidCutoverContract { detail, .. }
                if detail.contains("at least fifteen")
        )));
    }

    #[test]
    fn scope_fixture_mutations_are_rejected_against_package_roots() {
        let valid = SourceFile::new(
            ".config/fixtures/control-plane/scope-exactly-one.json",
            r#"{
                "fixture":"scope-exactly-one",
                "inventory": {
                    "mechanical_scopes": {
                        "apps":"target-product",
                        "crates":"target-engine",
                        "extensions":"target-extension",
                        "frontends":"target-frontend",
                        "tests":"integration",
                        "tools":"target-tools"
                    }
                },
                "cases":[{"path":"docs/file.md","scopes":["architecture-evidence"]}]
            }"#,
        );
        let package_roots = BTreeSet::from(["apps/cli".to_owned(), "tools/workspace".to_owned()]);
        assert!(validate_scope_fixture(&valid, &package_roots).is_empty());
        let mutated = SourceFile::new(
            valid.path.clone(),
            valid
                .contents
                .replace("\"tools\":\"target-tools\"", "\"tooling\":\"target-tools\""),
        );
        assert!(
            validate_scope_fixture(&mutated, &package_roots)
                .iter()
                .any(|violation| matches!(
                    violation,
                    Violation::InvalidScopeFixture { detail, .. }
                        if detail.contains("mechanical_scopes must cover")
                ))
        );
    }

    #[test]
    fn policy_legacy_package_and_scope_mutations_are_rejected() {
        let policy = SourceFile::new(
            ".config/nix/control.nix",
            r#"
name = "interface-cli"
patterns = [ "^/interface/" "^/crates/" ]
"#,
        );
        let packages = BTreeSet::from(["backend-cli".to_owned()]);
        let violations = validate_policy_control(&policy, &packages);
        assert!(violations.iter().any(|violation| matches!(
            violation,
            Violation::StalePolicyPackageReference { reference, .. }
                if reference == "interface-cli"
        )));
        assert!(violations.iter().any(|violation| matches!(
            violation,
            Violation::StalePolicyPackageReference { reference, .. }
                if reference == "^/interface/"
        )));
        assert!(violations.iter().any(|violation| matches!(
            violation,
            Violation::MissingPolicyScope { scope, .. } if scope == "^/frontends/"
        )));

        let valid_policy = SourceFile::new(
            ".config/nix/control.nix",
            "patterns = [ \"^/crates/\" \"^/frontends/\" \"^/extensions/\" \"^/apps/\" \"^/tests/\" \"^/tools/\" ]\npackage = \"backend-cli\"\n",
        );
        assert!(validate_policy_control(&valid_policy, &packages).is_empty());
    }
}

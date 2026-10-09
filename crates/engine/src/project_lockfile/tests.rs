use super::*;
use sha2::{Digest, Sha256};

fn packages(inventory: &LockfileInventory) -> Vec<&str> {
    inventory
        .members
        .iter()
        .map(|member| match member {
            LockedMember::Package(PackageReference::Purl(package)) => package.as_str(),
            other => panic!("expected a package URL, got {other:?}"),
        })
        .collect()
}
fn npm(mut document: serde_json::Value) -> Result<LockfileInventory, LockfileError> {
    // Synthetic registry controls explicitly choose an origin matching their
    // declared package tuple. Authentic fixture bytes never pass this builder.
    fn registry_rows(value: &mut serde_json::Value, hint: &str) {
        if let Some(object) = value.as_object_mut() {
            if let Some(version) = object.get("version").and_then(serde_json::Value::as_str) {
                if !object.contains_key("resolved") {
                    let name = object
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_else(|| hint.rsplit("node_modules/").next().unwrap_or(hint));
                    let (name, version) = version
                        .strip_prefix("npm:")
                        .and_then(|alias| alias.rsplit_once('@'))
                        .unwrap_or((name, version));
                    let basename = name.rsplit('/').next().unwrap_or(name);
                    let source =
                        format!("https://registry.npmjs.org/{name}/-/{basename}-{version}.tgz");
                    object.insert("resolved".into(), serde_json::json!(source));
                }
            }
            for (key, value) in object.iter_mut() {
                registry_rows(value, key);
            }
        }
    }
    registry_rows(&mut document, "");
    LockfileFormat::Npm.parse(&document.to_string())
}

#[test]
fn authentic_npm_versions_preserve_registry_packages_not_integrity_lines() {
    for (text, hash, count, unavailable) in [
        (
            include_str!("fixtures/npm-v1-wave.json"),
            "11966b5debc49a1f76a049ad406e6e894991ddcc7af50e3cd80c9abffba78c65",
            4,
            0,
        ),
        (
            include_str!("fixtures/npm-v2-trustfall.json"),
            "bc42e7902ed1a522a06d9bac8693a22d956cc410ef6f62423188068544c20002",
            320,
            1,
        ),
        (
            include_str!("fixtures/npm-v3-fastify.json"),
            "04176318cf177e02faa08261c58fc4f8387c22151ac762e784a6ca77a5225d7e",
            50,
            0,
        ),
    ] {
        assert_eq!(format!("{:x}", Sha256::digest(text.as_bytes())), hash);
        let parsed = LockfileFormat::Npm.parse(text).expect("real npm lock");
        let members = packages(&parsed);
        assert_eq!(members.len(), count);
        assert_eq!(parsed.unresolved.len(), unavailable);
        assert!(
            members
                .iter()
                .all(|member| member.starts_with("pkg:npm/") && !member.contains("integrity"))
        );
        if unavailable == 1 {
            assert_eq!(
                parsed.unresolved[0].source.as_ref().unwrap().as_str(),
                "../pkg"
            );
        }
    }
}

#[test]
fn npm_v1_walks_nested_releases_and_v2_uses_only_packages() {
    let parsed = npm(serde_json::json!({"lockfileVersion":1,"dependencies":{
        "a":{"version":"1.0.0","dependencies":{"a":{"version":"2.0.0"},"b":{"version":"1.0.0"}}},
        "b":{"version":"1.0.0"}
    }}))
    .unwrap();
    assert_eq!(
        packages(&parsed),
        ["pkg:npm/a@1.0.0", "pkg:npm/a@2.0.0", "pkg:npm/b@1.0.0"]
    );
    let parsed = npm(serde_json::json!({"lockfileVersion":2,"packages":{
        "":{},"node_modules/@scope/pkg":{"version":"1.2.3-beta.2+build.7"},
        "node_modules/alias":{"name":"@real/pkg","version":"2.0.0"},
        "node_modules/a/node_modules/@scope/pkg":{"version":"1.2.3-beta.2+build.7"}
    },"dependencies":{"ignored":{"version":"9.0.0"}}}))
    .unwrap();
    assert_eq!(
        packages(&parsed),
        [
            "pkg:npm/@real/pkg@2.0.0",
            "pkg:npm/@scope/pkg@1.2.3-beta.2+build.7"
        ]
    );
}

#[test]
fn npm_aliases_and_sources_do_not_invent_registry_authority() {
    for version in ["1.2.3", "npm:@real/pkg@1.2.3"] {
        let parsed = npm(serde_json::json!({"lockfileVersion":3,"packages":{
            "node_modules/alias":{"version":version,"resolved":"https://foreign.example/pkg.tgz"}
        }}))
        .unwrap();
        assert!(parsed.members.is_empty());
        assert_eq!(
            parsed.unresolved[0].version.as_ref().unwrap().as_str(),
            version
        );
        assert_eq!(
            parsed.unresolved[0].source.as_ref().unwrap().as_str(),
            "https://foreign.example/pkg.tgz"
        );
    }
    let parsed = npm(serde_json::json!({"lockfileVersion":3,"packages":{
        "node_modules/alias":{"version":"npm:@real/pkg@1.2.3","resolved":"https://registry.npmjs.org/@real/pkg/-/pkg-1.2.3.tgz"}
    }})).unwrap();
    assert_eq!(packages(&parsed), ["pkg:npm/@real/pkg@1.2.3"]);
    for (location, name) in [("node_modules/Foo", "foo"), ("node_modules/foo", "Foo")] {
        assert!(
            npm(serde_json::json!({"lockfileVersion":3,"packages":{
                (location):{"name":name,"version":"1.0.0"}
            }}))
            .is_err()
        );
    }
}

#[test]
fn npm_workspace_links_are_paths_and_dangling_links_are_not_complete() {
    let parsed = npm(serde_json::json!({"lockfileVersion":3,"packages":{
        "":{},"packages/app":{"name":"app"},
        "node_modules/app":{"link":true,"resolved":"packages/app"}
    }}))
    .unwrap();
    assert_eq!(
        parsed.members.as_ref(),
        [LockedMember::Workspace("packages/app".into())]
    );
    assert!(parsed.unresolved.is_empty());
    assert!(
        npm(serde_json::json!({"lockfileVersion":3,"packages":{
            "node_modules/app":{"link":true,"resolved":"packages/absent"}
        }}))
        .is_err()
    );
}

#[test]
fn npm_v1_empty_install_tree_may_omit_dependencies_without_guessing_other_schemas() {
    for document in [
        r#"{"name":"empty","version":"1.0.0","lockfileVersion":1}"#,
        r#"{"name":"empty","version":"1.0.0","lockfileVersion":1,"dependencies":{}}"#,
    ] {
        let parsed = LockfileFormat::Npm.parse(document).unwrap();
        assert!(parsed.members.is_empty());
        assert!(parsed.unresolved.is_empty());
    }
    for document in [
        r#"{"lockfileVersion":1,"dependencies":null}"#,
        r#"{"lockfileVersion":1,"packages":{}}"#,
        r#"{"lockfileVersion":2}"#,
        r#"{"lockfileVersion":3}"#,
    ] {
        assert!(LockfileFormat::Npm.parse(document).is_err(), "{document}");
    }
}

#[test]
fn npm_refuses_unknown_schema_malformed_duplicate_and_unpinned_rows() {
    for document in [
        r#"{"lockfileVersion":4,"packages":{}}"#,
        r#"{"lockfileVersion":3,"packages":{"node_modules/a":{"version":"^1.0.0"}}}"#,
        r#"{"lockfileVersion":3,"packages":{"node_modules/a":{"version":"1.0.0"},"node_modules/a":{"version":"2.0.0"}}}"#,
        r#"{"lockfileVersion":3,"packages":{"node_modules/a":{"version":"1.0.0","version":"2.0.0"}}}"#,
        r#"{"lockfileVersion":3,"packages":{ broken } }"#,
    ] {
        assert!(LockfileFormat::Npm.parse(document).is_err(), "{document}");
    }
}

#[test]
fn cargo_preserves_workspace_git_and_alternate_registry_rows_as_partial() {
    let parsed = LockfileFormat::Cargo
        .parse(
            r#"
version = 4
[[package]]
name = "root"
version = "0.1.0"
[[package]]
name = "serde"
version = "1.0.228"
source = "registry+https://github.com/rust-lang/crates.io-index"
[[package]]
name = "git-package"
version = "2.0.0"
source = "git+https://example.test/repo#abcdef"
[[package]]
name = "private-package"
version = "3.0.0"
source = "registry+https://registry.example/index"
"#,
        )
        .unwrap();
    assert_eq!(packages(&parsed), ["pkg:cargo/serde@1.0.228"]);
    assert_eq!(parsed.unresolved.len(), 3);
    assert!(
        parsed
            .unresolved
            .iter()
            .any(|row| row.name.as_str() == "root" && row.source.is_none())
    );
    assert!(parsed.unresolved.iter().any(|row| {
        row.source
            .as_ref()
            .is_some_and(|source| source.as_str().starts_with("git+"))
    }));
}

#[test]
fn authentic_uv_schema_retains_packages_and_actual_workspace_paths() {
    let text = include_str!("fixtures/uv-v1-r3-turso.lock");
    assert_eq!(
        format!("{:x}", Sha256::digest(text.as_bytes())),
        "474796e3635bcdc1fb03cb3ce120cc5e43c20821afaa2efd3b38ebba6daa1d78"
    );
    let parsed = LockfileFormat::Uv.parse(text).unwrap();
    assert_eq!(
        parsed
            .members
            .iter()
            .filter(|member| matches!(member, LockedMember::Package(_)))
            .count(),
        30
    );
    assert_eq!(
        parsed
            .members
            .iter()
            .filter(|member| matches!(member, LockedMember::Workspace(_)))
            .count(),
        2
    );
    assert!(parsed.unresolved.is_empty());
    assert!(
        LockfileFormat::Uv
            .parse(&text.replacen("revision = 3", "revision = 99", 1))
            .is_err()
    );
}

#[test]
fn python_toml_uses_schema_and_source_not_incidental_version_fields() {
    let parsed = LockfileFormat::Poetry
        .parse(
            r#"
[[package]]
name = "Requests"
version = "2.32.4"
[[package]]
name = "custom"
version = "1.0.0"
[package.source]
type = "git"
url = "https://example.test/repo"
[metadata]
lock-version = "2.1"
"#,
        )
        .unwrap();
    assert_eq!(packages(&parsed), ["pkg:pypi/requests@2.32.4"]);
    assert_eq!(parsed.unresolved.len(), 1);
    assert!(
        parsed.unresolved[0]
            .source
            .as_ref()
            .unwrap()
            .as_str()
            .contains("example.test")
    );
    let parsed = LockfileFormat::Uv
        .parse(
            r#"
version = 1
revision = 3
[[package]]
name = "custom"
version = "1.0.0"
source = { registry = "https://private.example/simple" }
"#,
        )
        .unwrap();
    assert!(parsed.members.is_empty());
    assert_eq!(parsed.unresolved.len(), 1);
    assert!(
        LockfileFormat::Poetry
            .parse("[metadata]\nlock-version='99'\npackage=[]")
            .is_err()
    );
}

#[test]
fn requirements_use_full_pinned_pep508_and_refuse_ranges_urls_and_options() {
    let parsed = LockfileFormat::Requirements.parse("# pinned\nRequests[security]==2.32.4 ; python_version >= '3.10'\nfoo_bar==1.0+local # comment\n").unwrap();
    assert_eq!(
        packages(&parsed),
        ["pkg:pypi/foo-bar@1.0+local", "pkg:pypi/requests@2.32.4"]
    );
    for text in [
        "foo>=1.0",
        "foo==1.*",
        "foo @ https://example.test/a.whl",
        "-r other.txt",
        "foo==1.0 --hash=sha256:abc",
        "foo==1.0\\",
    ] {
        assert!(LockfileFormat::Requirements.parse(text).is_err(), "{text}");
    }
}

#[test]
fn go_multiline_require_replace_exclude_and_retract_are_distinct_facts() {
    let parsed = LockfileFormat::GoMod
        .parse(
            r#"
module example.test/root
go 1.24.0
require (
    example.test/a v1.0.0 // indirect
    "example.test/b/v2" v2.1.0
    example.test/c v0.0.0-20250101010101-abcdef123456
    example.test/d v1.0.0
)
replace (
    example.test/a => example.test/actual v1.2.3+incompatible
    example.test/b/v2 => ../b
)
exclude example.test/d v1.0.0
retract [v1.0.0, v1.9.0] // Own releases, no dependency
"#,
        )
        .unwrap();
    assert_eq!(
        packages(&parsed),
        [
            "pkg:golang/example.test/actual@v1.2.3+incompatible",
            "pkg:golang/example.test/c@v0.0.0-20250101010101-abcdef123456"
        ]
    );
    assert_eq!(parsed.unresolved.len(), 2);
    assert!(parsed.unresolved.iter().any(|row| {
        row.source
            .as_ref()
            .is_some_and(|source| source.as_str() == "../b")
    }));
    for text in [
        "require example.test/a v1.0.0",
        "module example.test/a\nrequire (\na v1.0.0",
        "module example.test/a\nrequire a latest",
        "module example.test/a\nretract [bad, v1.0.0]",
    ] {
        assert!(LockfileFormat::GoMod.parse(text).is_err());
    }
}

#[test]
fn ordinary_large_lockfile_is_complete_and_resource_bounds_never_truncate() {
    let mut rows = serde_json::Map::new();
    for index in 0..1444 {
        rows.insert(
            format!("node_modules/p{index}"),
            serde_json::json!({"version":"1.0.0"}),
        );
    }
    let parsed = npm(serde_json::json!({"lockfileVersion":3,"packages":rows})).unwrap();
    assert_eq!(packages(&parsed).len(), 1444);
    let mut inventory = Inventory::default();
    for _ in 0..MAX_ENTRIES {
        inventory.entry().unwrap();
    }
    assert_eq!(inventory.entry(), Err(LockfileError::Bound));
    assert_eq!(
        LockfileFormat::Npm.parse(&" ".repeat(MAX_PROJECT_LOCKFILE_BYTES + 1)),
        Err(LockfileError::Bound)
    );
    let mut inventory = Inventory::default();
    let long = "x".repeat(MAX_FIELD_BYTES);
    for index in 0..1000 {
        if inventory
            .unresolved(
                &format!("p{index}"),
                None,
                Some(&long),
                "source unavailable",
            )
            .is_err()
        {
            assert!(inventory.payload_bytes > backend_library::MAX_PROJECT_MEMBER_PAYLOAD_BYTES);
            return;
        }
    }
    panic!("aggregate payload must refuse before 1000 long source rows");
}

#[test]
fn authentic_caddy_multiline_require_inventory_keeps_all_165_modules() {
    let text = include_str!("fixtures/go-caddy.mod");
    assert_eq!(
        format!("{:x}", Sha256::digest(text.as_bytes())),
        "62fdd786b2f9ced74c818132ebaeba320e25528ed316202902918afa73473b72"
    );
    let parsed = LockfileFormat::GoMod.parse(text).unwrap();
    assert_eq!(packages(&parsed).len(), 165);
    assert!(parsed.unresolved.is_empty());
    assert!(packages(&parsed).contains(&"pkg:golang/github.com/BurntSushi/toml@v1.6.0"));
}

#[test]
fn unknown_source_authentication_material_is_not_public_display_data() {
    for source in [
        "https://user:password@private.example/pkg.tgz",
        "https://private.example/pkg.tgz?secret=opaque",
        "url = \"https://private.example\"\ncredential = \"opaque\"",
        "registry = \"https://private.example\"\npriority = \"primary\"",
    ] {
        let mut inventory = Inventory::default();
        inventory
            .unresolved("private", Some("1.0.0"), Some(source), "source unavailable")
            .unwrap();
        let record = inventory.unresolved.into_values().next().unwrap();
        let display = record.source.unwrap();
        assert!(display.as_str().starts_with("redacted-source-sha256:"));
        assert!(!display.as_str().contains("opaque"));
        assert!(!display.as_str().contains("password"));
        assert!(!display.as_str().chars().any(char::is_control));
    }
    // Redaction must not bypass either raw evidence or retained display bounds.
    let mut inventory = Inventory::default();
    assert_eq!(
        inventory.unresolved(
            "private",
            None,
            Some(&"x".repeat(MAX_FIELD_BYTES + 1)),
            "source unavailable"
        ),
        Err(LockfileError::Bound)
    );
    inventory
        .unresolved("private", None, Some("\n"), "source unavailable")
        .unwrap();
    let source = inventory
        .unresolved
        .values()
        .next()
        .unwrap()
        .source
        .as_ref()
        .unwrap();
    assert_eq!(
        inventory.payload_bytes,
        "private".len() + source.as_str().len() + "source unavailable".len()
    );
    let charged = inventory.payload_bytes;
    inventory
        .unresolved(
            "private",
            None,
            Some("\n"),
            "a longer reason must not replace an already charged unresolved identity",
        )
        .unwrap();
    assert_eq!(inventory.payload_bytes, charged);
    assert_eq!(
        inventory
            .unresolved
            .values()
            .next()
            .unwrap()
            .reason
            .as_str(),
        "source unavailable"
    );
    assert_eq!(field("private\nsource"), Err(LockfileError::Bound));
}

#[test]
fn member_and_raw_json_node_bounds_are_independent_of_unique_registry_payload() {
    let mut inventory = Inventory::default();
    for index in 0..MAX_PROJECT_LOCKFILE_MEMBERS {
        inventory.workspace(&format!("p{index:x}")).unwrap();
    }
    assert_eq!(inventory.workspace("extra"), Err(LockfileError::Bound));
    let raw = format!("[{}]", "0,".repeat(MAX_ENTRIES * 64));
    assert_eq!(json_inventory_bound(&raw), Err(LockfileError::Bound));
}

#[test]
fn metadata_package_names_and_install_locations_are_not_requirements_or_traversals() {
    for location in [
        "node_modules/a/../../node_modules/evil",
        "./node_modules/evil",
        "packages/../node_modules/evil",
    ] {
        assert!(
            npm(
                serde_json::json!({"lockfileVersion":3,"packages":{(location):{"version":"1.0.0"}}})
            )
            .is_err()
        );
    }
    for name in ["foo[extra]", "foo>=1", "foo ; python_version > '3.10'"] {
        let text = format!(
            "version = 1\n[[package]]\nname = {:?}\nversion = '1.0.0'\nsource = {{registry = 'https://pypi.org/simple'}}\n",
            name
        );
        assert!(LockfileFormat::Uv.parse(&text).is_err());
    }
}

#[test]
fn npm_omitted_origin_and_uninstalled_peer_rows_remain_explicitly_unresolved() {
    let parsed = LockfileFormat::Npm
        .parse(
            r#"{"lockfileVersion":3,"packages":{
        "":{},"node_modules/peer":{"peer":true,"optional":true},
        "node_modules/unproven":{"version":"1.2.3"}
    }}"#,
        )
        .unwrap();
    assert!(parsed.members.is_empty());
    assert_eq!(parsed.unresolved.len(), 2);
    assert!(
        parsed
            .unresolved
            .iter()
            .any(|row| row.name.as_str() == "peer" && row.version.is_none())
    );
    assert!(
        parsed
            .unresolved
            .iter()
            .any(|row| row.name.as_str() == "unproven" && row.source.is_none())
    );
}

#[test]
fn outside_python_workspace_sources_are_partial_without_reading_foreign_paths() {
    for (format, text) in [
        (
            LockfileFormat::Uv,
            "version=1\n[[package]]\nname='shared'\nversion='1.0'\nsource={editable='../shared'}\n",
        ),
        (
            LockfileFormat::Poetry,
            "[[package]]\nname='shared'\nversion='1.0'\n[package.source]\ntype='directory'\nurl='/foreign/shared'\n[metadata]\nlock-version='2.1'\n",
        ),
    ] {
        let parsed = format.parse(text).unwrap();
        assert!(parsed.members.is_empty());
        assert_eq!(parsed.unresolved.len(), 1);
        assert!(
            parsed.unresolved[0]
                .reason
                .as_str()
                .contains("outside the lockfile root")
        );
    }
}

#[test]
fn complete_release_validation_does_not_accept_advisory_abbreviations_or_empty_suffixes() {
    for version in [
        "1",
        "1.2",
        "1.2.3.4",
        "01.2.3",
        "1.2.3-",
        "1.2.3-a..b",
        "1.2.3-01",
        "1.2.3+",
        "1.2.3+x..y",
        "1.2.3+x+y",
    ] {
        assert!(
            package_coordinate("npm", "a", version).is_err(),
            "{version}"
        );
    }
    assert!(package_coordinate("npm", "a", "1.2.3-beta.2+build.07").is_ok());
    assert!(
        LockfileFormat::Cargo
            .parse("[[package]]\nname='root'\nversion='not-a-release'\n")
            .is_err()
    );
}

#[test]
fn go_metadata_directives_are_validated_without_becoming_dependency_members() {
    for directive in [
        "go banana",
        "go 1.2.3.4",
        "go 1.23\ngo 1.24",
        "toolchain banana",
        "toolchain go1.bad",
        "godebug missing-equals",
        "godebug =1",
        "godebug key=",
    ] {
        assert!(
            LockfileFormat::GoMod
                .parse(&format!("module example.test/a\n{directive}\n"))
                .is_err(),
            "{directive}"
        );
    }
    let parsed = LockfileFormat::GoMod
        .parse("module example.test/a\ngo 1.26.0\ntoolchain go1.26.1\ngodebug default=go1.26\n")
        .unwrap();
    assert!(parsed.members.is_empty());
    assert!(parsed.unresolved.is_empty());
}

#[test]
fn cargo_future_schema_is_not_a_complete_current_inventory() {
    assert!(
        LockfileFormat::Cargo
            .parse("version=99\n[[package]]\nname='root'\nversion='1.0.0'\n")
            .is_err()
    );
}

#[test]
fn npm_registry_origin_requires_exact_host_and_matching_release_tarball() {
    for source in [
        "https://registry.npmjs.org.attacker.invalid/a/-/a-1.0.0.tgz",
        "https://registry.npmjs.org/a",
        "https://registry.npmjs.org/other/-/other-1.0.0.tgz",
        "https://registry.npmjs.org/a/-/a-9.0.0.tgz",
        "https://user@registry.npmjs.org/a/-/a-1.0.0.tgz",
        "https://registry.npmjs.org/a/-/a-1.0.0.tgz?token=private",
    ] {
        let parsed = npm(serde_json::json!({"lockfileVersion":3,"packages":{"node_modules/a":{"version":"1.0.0","resolved":source}}})).unwrap();
        assert!(
            parsed.members.is_empty(),
            "foreign or mismatched source must not import"
        );
        assert_eq!(parsed.unresolved.len(), 1);
    }
}

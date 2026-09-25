//! Loader, reader, and README projection tests over real fixture folders.

use super::cargo::{CargoDependency, CargoMetadata, CargoPackage};
use super::{
    CargoFailure, DependencyKind, LocalPackage, LocalPackageLoader, LocalPackageSource,
    ReadmeBlock, origin_url, readme,
};
use crate::core::LocalProjectId;
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

struct Scratch(PathBuf);

type Outcome = Result<(), String>;

fn text(error: impl std::fmt::Display) -> String {
    error.to_string()
}

impl Scratch {
    fn new(label: &str) -> Result<Self, String> {
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "nudox-local-package-{label}-{}-{suffix}",
            std::process::id()
        ));
        fs::create_dir_all(&path).map_err(text)?;
        Ok(Self(path))
    }

    fn write(&self, relative: &str, contents: &str) -> Outcome {
        let path = self.0.join(relative);
        let parent = path.parent().ok_or("fixture path has no parent")?;
        fs::create_dir_all(parent).map_err(text)?;
        fs::write(path, contents).map_err(text)
    }

    fn project(&self) -> Result<LocalProjectId, String> {
        LocalProjectId::from_path(&self.0).map_err(text)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _removed = fs::remove_dir_all(&self.0);
    }
}

/// A workspace with a root package, glob members, an excluded member, and
/// every dependency table, plus a README and a forge remote.
fn workspace_fixture(label: &str) -> Result<Scratch, String> {
    let scratch = Scratch::new(label)?;
    scratch.write(
        "Cargo.toml",
        r#"[workspace]
members = ["crates/*", "nested/*/beta"]
exclude = ["crates/ignored"]

[workspace.package]
version = "0.4.2"
license = "MIT OR Apache-2.0"
rust-version = "1.80"

[package]
name = "fixture-root"
version.workspace = true
license.workspace = true
rust-version.workspace = true
description = "A fixture workspace root."
keywords = ["docs", "offline"]
categories = ["development-tools"]

[dependencies]
alpha = { path = "crates/alpha" }
serde = { version = "1", optional = true, default-features = false, features = ["derive"] }

[dev-dependencies]
alpha = { path = "crates/alpha" }

[build-dependencies]
cc = "1.0"

[features]
default = ["serde"]
serde = ["dep:serde"]
"#,
    )?;
    write_members(&scratch)?;
    scratch.write(
        "README.md",
        "# Fixture Root v0.4\n\nReads package facts offline.\n\n## Usage\n\n- no network\n* bounded\n\n```rust\nfixture_root::root();\n```\n",
    )?;
    scratch.write(
        ".git/config",
        "[core]\n\tbare = false\n[remote \"upstream\"]\n\turl = https://example.invalid/wrong.git\n[remote \"origin\"]\n\turl = git@codeberg.org:owner/fixture.git\n",
    )?;
    Ok(scratch)
}

/// Workspace members: two included by glob, one excluded, one build output.
fn write_members(scratch: &Scratch) -> Outcome {
    scratch.write("src/lib.rs", "pub fn root() {}\n")?;
    scratch.write(
        "crates/alpha/Cargo.toml",
        r#"[package]
name = "alpha"
version = "0.1.0"

[dependencies]
serde = { version = "1", optional = true, default-features = false, features = ["derive"] }

[features]
default = ["serde"]
"#,
    )?;
    scratch.write("crates/alpha/src/lib.rs", "pub fn alpha() {}\n")?;
    scratch.write(
        "crates/ignored/Cargo.toml",
        "[package]\nname = \"ignored\"\nversion = \"9.9.9\"\n[dependencies]\nmust-not-count = \"1\"\n",
    )?;
    scratch.write(
        "nested/deep/beta/Cargo.toml",
        "[package]\nname = \"beta\"\nversion = \"0.2.0\"\n[dependencies]\nrenamed = { package = \"real-name\", version = \"2\" }\n",
    )?;
    scratch.write("nested/deep/beta/src/lib.rs", "pub fn beta() {}\n")?;
    scratch.write(
        "target/debug/Cargo.toml",
        "[package]\nname = \"build-output\"\nversion = \"0.0.0\"\n",
    )?;
    Ok(())
}

fn requirement_of<'package>(
    package: &'package LocalPackage,
    kind: DependencyKind,
    name: &str,
) -> Vec<&'package str> {
    package
        .dependencies
        .iter()
        .filter(|dependency| dependency.kind == kind && dependency.name.as_ref() == name)
        .map(|dependency| dependency.requirement.as_ref())
        .collect()
}

/// The fixture's feature graph, as both readers must report it.
fn assert_fixture_features(package: &LocalPackage) -> Outcome {
    let default = package
        .features
        .iter()
        .find(|feature| feature.name.as_ref() == "default")
        .ok_or("default feature missing")?;
    assert_eq!(default.users, 2);
    assert_eq!(default.members.as_ref(), [Arc::<str>::from("serde")]);
    assert!(
        package
            .features
            .iter()
            .any(|feature| feature.name.as_ref() == "serde"
                && feature.members.as_ref() == [Arc::<str>::from("dep:serde")])
    );
    Ok(())
}

/// The root package's own fields, as both readers must report them.
fn assert_fixture_identity(package: &LocalPackage) {
    assert_eq!(package.name.as_ref(), "fixture-root");
    assert_eq!(package.version.as_deref(), Some("0.4.2"));
    assert_eq!(package.license.as_deref(), Some("MIT OR Apache-2.0"));
    assert_eq!(
        package.description.as_deref(),
        Some("A fixture workspace root.")
    );
    assert_eq!(
        package
            .keywords
            .iter()
            .map(AsRef::as_ref)
            .collect::<Vec<&str>>(),
        ["docs", "offline"]
    );
    assert_eq!(
        package.categories.as_ref(),
        [Arc::<str>::from("development-tools")]
    );
    assert_eq!(
        package.repository.as_deref(),
        Some("git@codeberg.org:owner/fixture.git")
    );
}

#[test]
fn manifest_fallback_reads_members_tables_features_and_inheritance() -> Outcome {
    let scratch = workspace_fixture("manifest")?;
    let package = LocalPackageLoader::without_cargo().load(&scratch.project()?);
    assert_eq!(
        package.source,
        LocalPackageSource::Manifest(CargoFailure::Disabled)
    );
    assert_fixture_identity(&package);
    assert_eq!(package.rust_version.as_deref(), Some("1.80"));
    // Root + crates/alpha + nested/deep/beta; crates/ignored is excluded and
    // target/ is never walked.
    assert_eq!(package.members, 3);
    for outsider in ["must-not-count", "build-output"] {
        assert!(requirement_of(&package, DependencyKind::Normal, outsider).is_empty());
    }
    assert_eq!(
        requirement_of(&package, DependencyKind::Normal, "alpha"),
        ["path: crates/alpha"]
    );
    assert_eq!(
        requirement_of(&package, DependencyKind::Development, "alpha"),
        ["path: crates/alpha"]
    );
    assert_eq!(
        requirement_of(&package, DependencyKind::Build, "cc"),
        ["1.0"]
    );
    assert_eq!(
        requirement_of(&package, DependencyKind::Normal, "renamed"),
        ["2 · package: real-name"]
    );
    let serde = package
        .dependencies
        .iter()
        .find(|dependency| dependency.name.as_ref() == "serde")
        .ok_or("serde dependency missing")?;
    assert_eq!(
        serde.requirement.as_ref(),
        "1 · optional · default-features: false · features: derive"
    );
    assert_eq!(
        serde.users, 2,
        "root and alpha declare the same requirement"
    );
    assert_fixture_features(&package)?;
    Ok(())
}

#[test]
fn a_missing_cargo_program_falls_back_to_the_manifest_reader() -> Outcome {
    let scratch = workspace_fixture("no-cargo")?;
    let loader = LocalPackageLoader::default().with_cargo(scratch.0.join("no-such-cargo-binary"));
    let package = loader.load(&scratch.project()?);
    assert_eq!(
        package.source,
        LocalPackageSource::Manifest(CargoFailure::Spawn)
    );
    assert_eq!(package.name.as_ref(), "fixture-root");
    assert_eq!(package.members, 3);
    Ok(())
}

#[test]
fn cargo_reader_resolves_the_same_workspace_offline() -> Outcome {
    let scratch = workspace_fixture("cargo")?;
    let package = LocalPackageLoader::default().load(&scratch.project()?);
    assert_eq!(package.source, LocalPackageSource::Cargo, "{package:#?}");
    // Cargo resolves `version.workspace = true` itself.
    assert_fixture_identity(&package);
    assert_eq!(package.members, 3);
    assert!(requirement_of(&package, DependencyKind::Normal, "must-not-count").is_empty());
    let serde = requirement_of(&package, DependencyKind::Normal, "serde");
    assert_eq!(serde.len(), 1);
    assert!(serde[0].starts_with("^1"), "{serde:?}");
    for detail in [
        "registry",
        "optional",
        "default-features: false",
        "features: derive",
    ] {
        assert!(serde[0].contains(detail), "{detail} missing from {serde:?}");
    }
    assert!(
        requirement_of(&package, DependencyKind::Normal, "renamed")[0]
            .contains("package: real-name")
    );
    assert!(requirement_of(&package, DependencyKind::Development, "alpha")[0].contains("path: "));
    assert!(matches!(
        package.readme.first(),
        Some(ReadmeBlock::Heading { level: 1, .. })
    ));
    assert_fixture_features(&package)
}

/// A folder that is one member of a bigger workspace is that package: its
/// dossier names its own dependencies, never its siblings'.
#[test]
fn a_member_folder_reads_only_its_own_package() -> Outcome {
    let scratch = workspace_fixture("member")?;
    let member = LocalProjectId::from_path(&scratch.0.join("nested/deep/beta")).map_err(text)?;
    let package = LocalPackageLoader::default().load(&member);
    assert_eq!(package.source, LocalPackageSource::Cargo, "{package:#?}");
    assert_eq!(package.name.as_ref(), "beta");
    assert_eq!(package.version.as_deref(), Some("0.2.0"));
    assert_eq!(package.members, 1);
    let names = package
        .dependencies
        .iter()
        .map(|dependency| dependency.name.to_string())
        .collect::<Vec<_>>();
    assert_eq!(names, ["renamed"], "only beta's own dependency: {names:?}");
    Ok(())
}

#[test]
fn a_folder_without_a_manifest_is_named_by_its_readme_or_folder() -> Outcome {
    let titled = Scratch::new("no-manifest-readme")?;
    titled.write(
        "README.md",
        "# Forge Project\n\nA project without a package root.\n",
    )?;
    let package = LocalPackageLoader::without_cargo().load(&titled.project()?);
    assert_eq!(package.source, LocalPackageSource::NoManifest);
    assert_eq!(package.name.as_ref(), "forge-project");
    assert_eq!(
        package.description.as_deref(),
        Some("A project without a package root.")
    );
    assert_eq!(package.members, 0);
    assert!(package.dependencies.is_empty());
    assert!(package.version.is_none() && package.license.is_none());

    let bare = Scratch::new("no-manifest-bare")?;
    let package = LocalPackageLoader::default().load(&bare.project()?);
    assert_eq!(package.source, LocalPackageSource::NoManifest);
    let folder = bare.0.file_name().and_then(|name| name.to_str());
    assert_eq!(Some(package.name.as_ref()), folder);
    assert!(package.readme.is_empty() && package.description.is_none());
    Ok(())
}

#[test]
fn repository_falls_back_to_a_linked_worktree_origin() -> Outcome {
    let main = Scratch::new("git-common")?;
    main.write(
        ".git/config",
        "[remote \"origin\"]\n  url   =   https://github.com/owner/project\n",
    )?;
    main.write(".git/worktrees/linked/commondir", "../..\n")?;
    let linked = Scratch::new("git-linked")?;
    linked.write(
        ".git",
        &format!(
            "gitdir: {}\n",
            main.0.join(".git/worktrees/linked").display()
        ),
    )?;
    linked.write(
        "Cargo.toml",
        "[package]\nname = \"linked\"\nversion = \"0.1.0\"\n",
    )?;
    let package = LocalPackageLoader::without_cargo().load(&linked.project()?);
    assert_eq!(
        package.repository.as_deref(),
        Some("https://github.com/owner/project")
    );
    Ok(())
}

#[test]
fn origin_url_reads_only_the_origin_section() {
    let config = "[remote \"fork\"]\nurl = a\n[remote \"origin\"]\n\tfetch = +refs/*\n\turl = b\n[branch \"main\"]\nurl = c\n";
    assert_eq!(origin_url(config).as_deref(), Some("b"));
    assert_eq!(origin_url("[remote \"fork\"]\nurl = a\n"), None);
}

#[test]
fn readme_projection_retains_document_structure() {
    let blocks = readme::parse(
        "# Name\n\nA useful\ncrate.\n\n### Deep\n- fast\n* small\n\n```rust\nfn main() {}\n\n  indented();\n```\n\n```\nplain\n```\n```sh\nunterminated\n",
    );
    assert_eq!(
        blocks,
        [
            ReadmeBlock::Heading {
                level: 1,
                text: "Name".into()
            },
            ReadmeBlock::Paragraph("A useful crate.".into()),
            ReadmeBlock::Heading {
                level: 3,
                text: "Deep".into()
            },
            ReadmeBlock::Bullet("fast".into()),
            ReadmeBlock::Bullet("small".into()),
            ReadmeBlock::Code {
                language: Some("rust".into()),
                text: "fn main() {}\n\n  indented();".into()
            },
            ReadmeBlock::Code {
                language: None,
                text: "plain".into()
            },
            ReadmeBlock::Code {
                language: Some("sh".into()),
                text: "unterminated".into()
            },
        ]
    );
}

#[test]
fn readme_summary_skips_headings_and_code_and_titles_drop_versions() {
    let source = "# Vector Tools v2.1\n\n```text\nnot prose\n```\n\nFirst\nparagraph.\n\nSecond.\n";
    assert_eq!(readme::first_paragraph(source), "First paragraph.");
    // A word merely starting with `v` is part of the name.
    assert_eq!(readme::title(source).as_deref(), Some("vector-tools"));
    assert_eq!(readme::title("no heading"), None);
}

#[test]
fn cargo_requirement_keeps_every_resolution_dimension() {
    let dependency = CargoDependency {
        name: "serde".to_owned(),
        req: "^1.0".to_owned(),
        kind: Some("dev".to_owned()),
        rename: Some("serde_alias".to_owned()),
        optional: true,
        uses_default_features: false,
        features: vec!["derive".to_owned()],
        target: Some("cfg(unix)".to_owned()),
        source: Some("registry+https://github.com/rust-lang/crates.io-index".to_owned()),
        registry: None,
        path: None,
    };
    assert_eq!(
        super::cargo::requirement(&dependency),
        "^1.0 · registry · package: serde · optional · default-features: false · features: derive · target: cfg(unix)"
    );
}

#[test]
fn cargo_projection_uses_only_workspace_members() -> Outcome {
    let package =
        |id: &str, manifest_path: &str, features: BTreeMap<String, Vec<String>>| CargoPackage {
            id: id.to_owned(),
            name: id.to_owned(),
            version: "1.2.3".to_owned(),
            description: Some(format!("{id} description")),
            license: Some("MIT".to_owned()),
            repository: None,
            homepage: None,
            documentation: None,
            keywords: vec![],
            categories: vec![],
            readme: None,
            rust_version: None,
            manifest_path: manifest_path.to_owned(),
            dependencies: vec![],
            features,
        };
    let scratch = Scratch::new("cargo-projection")?;
    let metadata = CargoMetadata {
        packages: vec![
            package(
                "second",
                "/nonexistent/second/Cargo.toml",
                BTreeMap::from([("runtime".to_owned(), vec!["dep:serde".to_owned()])]),
            ),
            package(
                "first",
                "/nonexistent/first/Cargo.toml",
                BTreeMap::from([("default".to_owned(), vec!["serde".to_owned()])]),
            ),
            package(
                "outsider",
                "/nonexistent/outsider/Cargo.toml",
                BTreeMap::new(),
            ),
        ],
        workspace_members: vec!["first".to_owned(), "second".to_owned()],
    };
    let projected = super::cargo::project(scratch.project()?, &scratch.0, metadata);
    // A virtual workspace has no root package: no version is invented.
    let folder = scratch.0.file_name().and_then(|name| name.to_str());
    assert_eq!(Some(projected.name.as_ref()), folder);
    assert_eq!(projected.version, None);
    assert_eq!(projected.license, None);
    assert_eq!(projected.members, 2);
    let names = projected
        .features
        .iter()
        .map(|feature| feature.name.as_ref())
        .collect::<Vec<_>>();
    assert_eq!(names, ["default", "runtime"]);
    Ok(())
}

#[cfg(unix)]
fn fake_cargo(scratch: &Scratch, body: &str) -> Result<PathBuf, String> {
    use std::os::unix::fs::PermissionsExt as _;
    let path = scratch.0.join("fake-cargo");
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).map_err(text)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).map_err(text)?;
    Ok(path)
}

#[cfg(unix)]
#[test]
fn a_hung_cargo_is_killed_at_the_time_bound() -> Outcome {
    let scratch = workspace_fixture("timeout")?;
    let program = fake_cargo(&scratch, "exec sleep 30")?;
    let loader = LocalPackageLoader::default()
        .with_cargo(program)
        .with_timeout(Duration::from_millis(200));
    let started = Instant::now();
    let package = loader.load(&scratch.project()?);
    assert_eq!(
        package.source,
        LocalPackageSource::Manifest(CargoFailure::Timeout)
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(package.name.as_ref(), "fixture-root");
    Ok(())
}

#[cfg(unix)]
#[test]
fn an_oversized_cargo_answer_is_refused_at_the_output_bound() -> Outcome {
    let scratch = workspace_fixture("overflow")?;
    let program = fake_cargo(&scratch, "exec yes nudox")?;
    let loader = LocalPackageLoader::default()
        .with_cargo(program)
        .with_max_output(4096)
        .with_timeout(Duration::from_secs(20));
    let started = Instant::now();
    let package = loader.load(&scratch.project()?);
    assert_eq!(
        package.source,
        LocalPackageSource::Manifest(CargoFailure::OutputLimit)
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_cargo_answer_that_is_not_metadata_is_a_decode_failure() -> Outcome {
    let scratch = workspace_fixture("decode")?;
    let program = fake_cargo(&scratch, "echo '{\"not\": \"metadata\"}'")?;
    let package = LocalPackageLoader::default()
        .with_cargo(program)
        .load(&scratch.project()?);
    assert_eq!(
        package.source,
        LocalPackageSource::Manifest(CargoFailure::Decode)
    );
    Ok(())
}

#[test]
fn recursive_member_globs_skip_build_directories_and_respect_exclude() -> Outcome {
    let scratch = Scratch::new("recursive")?;
    scratch.write(
        "Cargo.toml",
        "[workspace]\nmembers = [\"components/**\"]\nexclude = [\"components/skip/**\"]\n",
    )?;
    scratch.write(
        "components/a/Cargo.toml",
        "[package]\nname = \"a\"\n[dependencies]\nx = \"1\"\n",
    )?;
    scratch.write(
        "components/a/deep/b/Cargo.toml",
        "[package]\nname = \"b\"\n[dependencies]\nx = \"1\"\n",
    )?;
    scratch.write(
        "components/skip/c/Cargo.toml",
        "[package]\nname = \"c\"\n[dependencies]\nhidden = \"1\"\n",
    )?;
    scratch.write(
        "components/target/d/Cargo.toml",
        "[package]\nname = \"d\"\n[dependencies]\nhidden = \"1\"\n",
    )?;
    scratch.write(
        "components/node_modules/e/Cargo.toml",
        "[package]\nname = \"e\"\n[dependencies]\nhidden = \"1\"\n",
    )?;
    let package = LocalPackageLoader::without_cargo().load(&scratch.project()?);
    // The virtual root plus a and a/deep/b.
    assert_eq!(package.members, 3);
    assert_eq!(package.dependencies.len(), 1);
    assert_eq!(package.dependencies[0].name.as_ref(), "x");
    assert_eq!(package.dependencies[0].users, 2);
    assert_eq!(package.version, None, "a virtual root has no version");
    Ok(())
}

#[test]
fn member_patterns_match_like_cargo_globs() {
    use super::manifest::member_pattern_matches;
    assert!(member_pattern_matches("crates/ignored", "crates/ignored"));
    assert!(member_pattern_matches("crates/*", "crates/one"));
    assert!(!member_pattern_matches("crates/*", "crates/one/two"));
    assert!(!member_pattern_matches("crates/*", "crates"));
    assert!(member_pattern_matches("nested/**", "nested/a/b"));
    assert!(!member_pattern_matches("nested/**", "nestedness/a"));
}

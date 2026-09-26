//! Parsed local dependency facts reused while manifest bytes are unchanged.
//!
//! The residence hashes the same files the parser would read, including
//! ancestor `Cargo.toml` files that can supply workspace inheritance. A
//! matching digest moves the previous fact forward. A changed digest parses
//! that root again. Removal drops the fact. Parse failures stay with the
//! digest so the same bytes fail again without a second parse.

use super::local_dependency_facts;
use backend_library::PackageDependencySourceFacts;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
struct Entry {
    digest: [u8; 32],
    /// Set by measurement so the next refresh parses this root again.
    reparse: bool,
    outcome: Result<Option<PackageDependencySourceFacts>, String>,
}

/// Local dependency facts addressed by manifest bytes.
#[derive(Clone, Debug, Default)]
pub(crate) struct LocalManifestResidence {
    entries: BTreeMap<PathBuf, Entry>,
    witness: [u8; 32],
    parses: u64,
}

impl LocalManifestResidence {
    /// Blake3 of the roots and the bytes that produced their facts.
    #[must_use]
    pub(crate) const fn witness(&self) -> [u8; 32] {
        self.witness
    }

    /// How many roots have been parsed since this residence was created.
    #[must_use]
    pub(super) const fn parses(&self) -> u64 {
        self.parses
    }

    /// Facts from roots whose manifests parsed.
    pub(crate) fn facts(&self) -> impl Iterator<Item = &PackageDependencySourceFacts> {
        self.entries
            .values()
            .filter_map(|entry| match &entry.outcome {
                Ok(Some(fact)) => Some(fact),
                _ => None,
            })
    }

    /// Drops one stored digest so the next refresh parses that root again.
    ///
    /// Measurement uses this to time a one-file miss without rewriting the
    /// manifest between samples.
    pub(super) fn reparse_on_next_refresh(&mut self, root: &Path) {
        if let Some(entry) = self.entries.get_mut(root) {
            entry.reparse = true;
        }
    }

    /// Aligns stored facts with `roots`.
    ///
    /// An input error restores every entry already moved and leaves the
    /// previous residence intact. A parse error is stored and returned, so a
    /// later refresh of the same bytes does not parse again.
    pub(crate) fn refresh<'a>(
        &mut self,
        roots: impl IntoIterator<Item = &'a Path>,
    ) -> Result<(), String> {
        let roots = roots
            .into_iter()
            .map(Path::to_path_buf)
            .collect::<BTreeSet<_>>();
        let mut next = BTreeMap::new();
        let mut failure = None;
        for root in &roots {
            let digest = match input_digest(root) {
                Ok(digest) => digest,
                Err(error) => {
                    self.entries.append(&mut next);
                    return Err(error);
                }
            };
            if let Some(mut entry) = self.entries.remove(root)
                && entry.digest == digest
                && !entry.reparse
            {
                if let Err(error) = &entry.outcome {
                    failure.get_or_insert_with(|| error.clone());
                }
                entry.reparse = false;
                next.insert(root.clone(), entry);
                continue;
            }
            self.parses = self.parses.saturating_add(1);
            let outcome = local_dependency_facts(root);
            if let Err(error) = &outcome {
                failure.get_or_insert_with(|| error.clone());
            }
            next.insert(
                root.clone(),
                Entry {
                    digest,
                    reparse: false,
                    outcome,
                },
            );
        }
        self.entries = next;
        self.witness = witness_of(&self.entries);
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

fn witness_of(entries: &BTreeMap<PathBuf, Entry>) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.local-manifest.residence.v1\0");
    for (path, entry) in entries {
        hasher.update(path.as_os_str().as_encoded_bytes());
        hasher.update(&[0]);
        hasher.update(&entry.digest);
        let tag = match &entry.outcome {
            Ok(Some(_)) => 1u8,
            Ok(None) => 2,
            Err(_) => 3,
        };
        hasher.update(&[tag]);
    }
    *hasher.finalize().as_bytes()
}

fn input_digest(root: &Path) -> Result<[u8; 32], String> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.local-manifest.inputs.v1\0");
    if root.join("Cargo.toml").is_file() {
        hasher.update(b"cargo");
        hash_path(&mut hasher, &root.join("Cargo.toml"))?;
        let mut ancestor = root.parent();
        while let Some(dir) = ancestor {
            let candidate = dir.join("Cargo.toml");
            if candidate.is_file() {
                hash_path(&mut hasher, &candidate)?;
            }
            ancestor = dir.parent();
        }
    } else if root.join("package.json").is_file() {
        hasher.update(b"npm");
        hash_path(&mut hasher, &root.join("package.json"))?;
    } else if root.join("pyproject.toml").is_file() {
        hasher.update(b"python");
        hash_path(&mut hasher, &root.join("pyproject.toml"))?;
    } else if root.join("go.mod").is_file() {
        hasher.update(b"go");
        hash_path(&mut hasher, &root.join("go.mod"))?;
    } else if root.join("pom.xml").is_file() {
        hasher.update(b"maven");
        hash_path(&mut hasher, &root.join("pom.xml"))?;
    } else {
        let specs = nuspecs(root)?;
        if specs.is_empty() {
            hasher.update(b"absent");
        } else {
            hasher.update(b"nuget");
            for path in &specs {
                hash_path(&mut hasher, path)?;
            }
        }
    }
    Ok(*hasher.finalize().as_bytes())
}

fn hash_path(hasher: &mut blake3::Hasher, path: &Path) -> Result<(), String> {
    let bytes = std::fs::read(path)
        .map_err(|error| format!("read local manifest {}: {error}", path.display()))?;
    hasher.update(path.as_os_str().as_encoded_bytes());
    hasher.update(&[0]);
    let len =
        u64::try_from(bytes.len()).map_err(|_| "local manifest byte count overflow".to_owned())?;
    hasher.update(&len.to_le_bytes());
    hasher.update(&bytes);
    Ok(())
}

fn nuspecs(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut specs = Vec::new();
    for entry in std::fs::read_dir(root)
        .map_err(|error| format!("read indexed project {}: {error}", root.display()))?
    {
        let path = entry.map_err(|error| error.to_string())?.path();
        let is_nuspec = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("nuspec"));
        if is_nuspec && path.is_file() {
            specs.push(path);
        }
    }
    specs.sort();
    Ok(specs)
}

/// Times a full parse, a byte-identical refresh, and a one-root miss.
#[allow(clippy::expect_used, clippy::indexing_slicing, clippy::print_stdout)]
pub(super) fn measure_manifest_residence() {
    const PROJECTS: usize = 128;
    const DEPS: usize = 16;
    const SAMPLES: usize = 32;
    const WARMUPS: usize = 4;
    let root =
        std::env::temp_dir().join(format!("nudox-manifest-residence-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("bench directory");
    let mut roots = Vec::with_capacity(PROJECTS);
    for index in 0..PROJECTS {
        let project = root.join(format!("pkg-{index}"));
        std::fs::create_dir_all(&project).expect("project");
        std::fs::write(project.join("Cargo.toml"), cargo_manifest(index, DEPS)).expect("manifest");
        roots.push(project);
    }
    let mut cold_parses = 0u64;
    let cold = time(WARMUPS, SAMPLES, || {
        let mut residence = LocalManifestResidence::default();
        residence
            .refresh(roots.iter().map(PathBuf::as_path))
            .expect("cold");
        cold_parses = cold_parses.saturating_add(residence.parses());
    });
    let mut warm_residence = LocalManifestResidence::default();
    warm_residence
        .refresh(roots.iter().map(PathBuf::as_path))
        .expect("prime");
    let warm_before = warm_residence.parses();
    let warm = time(WARMUPS, SAMPLES, || {
        warm_residence
            .refresh(roots.iter().map(PathBuf::as_path))
            .expect("warm");
    });
    let warm_parses = warm_residence.parses() - warm_before;
    let edited = roots.first().expect("project").clone();
    let delta_before = warm_residence.parses();
    let delta = time(WARMUPS, SAMPLES, || {
        warm_residence.reparse_on_next_refresh(&edited);
        warm_residence
            .refresh(roots.iter().map(PathBuf::as_path))
            .expect("delta");
    });
    let delta_parses = warm_residence.parses() - delta_before;
    let (cold_median, cold_p95) = percentiles(&cold);
    let (warm_median, warm_p95) = percentiles(&warm);
    let (delta_median, delta_p95) = percentiles(&delta);
    let calls = u64::try_from(WARMUPS + SAMPLES).expect("calls");
    println!(
        "manifest_residence projects={PROJECTS} deps={DEPS} cold_median_ns={cold_median} cold_p95_ns={cold_p95} cold_parses_per_call={} warm_median_ns={warm_median} warm_p95_ns={warm_p95} warm_parses={warm_parses} delta_median_ns={delta_median} delta_p95_ns={delta_p95} delta_parses_per_call={}",
        cold_parses / calls,
        delta_parses / calls
    );
    let _ = std::fs::remove_dir_all(root);
}

fn cargo_manifest(index: usize, dependencies: usize) -> String {
    let mut manifest =
        format!("[package]\nname = \"pkg-{index}\"\nversion = \"1.0.0\"\n\n[dependencies]\n");
    for dependency in 0..dependencies {
        manifest.push_str(&format!("dep-{dependency} = \"1\"\n"));
    }
    manifest
}

fn time<T>(warmups: usize, samples: usize, mut body: impl FnMut() -> T) -> Vec<u128> {
    for _ in 0..warmups {
        let _ = body();
    }
    let mut samples_ns = Vec::with_capacity(samples);
    for _ in 0..samples {
        let started = std::time::Instant::now();
        let _ = body();
        samples_ns.push(started.elapsed().as_nanos());
    }
    samples_ns
}

fn percentiles(samples: &[u128]) -> (u128, u128) {
    let mut ordered = samples.to_vec();
    ordered.sort_unstable();
    let median = ordered[ordered.len() / 2];
    let p95 = ordered[ordered.len() * 95 / 100];
    (median, p95)
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn fixture(name: &str) -> PathBuf {
        let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "nudox-manifest-residence-{name}-{}-{sequence}-{nanos}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("fixture");
        path
    }

    fn named(residence: &LocalManifestResidence, source: &str) -> bool {
        residence.facts().any(|fact| fact.0.as_str() == source)
    }

    fn write(path: &Path, bytes: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent");
        }
        fs::write(path, bytes).expect("write");
    }

    #[test]
    fn unchanged_bytes_reuse_the_parsed_fact() {
        let root = fixture("reuse");
        let project = root.join("demo");
        write(
            &project.join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"1.0.0\"\n\n[dependencies]\nserde = \"1\"\n",
        );
        let mut residence = LocalManifestResidence::default();
        residence.refresh([project.as_path()]).expect("parse");
        let direct = local_dependency_facts(&project)
            .expect("direct")
            .expect("fact");
        assert_eq!(residence.facts().next(), Some(&direct));
        assert_eq!(residence.parses(), 1);
        let witness = residence.witness();
        residence.refresh([project.as_path()]).expect("reuse");
        assert_eq!(residence.parses(), 1);
        assert_eq!(residence.witness(), witness);
        assert_eq!(residence.facts().next(), Some(&direct));
        fs::write(
            project.join("Cargo.toml"),
            fs::read(project.join("Cargo.toml")).expect("read"),
        )
        .expect("rewrite");
        residence
            .refresh([project.as_path()])
            .expect("identical rewrite");
        assert_eq!(residence.parses(), 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn workspace_parent_edit_reparses_members_and_keeps_literal_versions() {
        let root = fixture("workspace");
        let workspace = root.join("workspace");
        let child = workspace.join("child");
        let literal = workspace.join("literal");
        let outside = root.join("outside");
        write(
            &workspace.join("Cargo.toml"),
            "[workspace.package]\nversion = \"1.0.0\"\n",
        );
        write(
            &child.join("Cargo.toml"),
            "[package]\nname = \"child\"\nversion.workspace = true\n\n[dependencies]\nserde = \"1\"\n",
        );
        write(
            &literal.join("Cargo.toml"),
            "[package]\nname = \"literal\"\nversion = \"9.9.9\"\n\n[dependencies]\nlibc = \"0.2\"\n",
        );
        write(
            &outside.join("Cargo.toml"),
            "[package]\nname = \"outside\"\nversion = \"3.0.0\"\n",
        );
        let roots = [child.as_path(), literal.as_path(), outside.as_path()];
        let mut residence = LocalManifestResidence::default();
        residence.refresh(roots).expect("prime");
        assert_eq!(residence.parses(), 3);
        write(
            &workspace.join("Cargo.toml"),
            "[workspace.package]\nversion = \"2.0.0\"\n",
        );
        residence.refresh(roots).expect("parent edit");
        assert_eq!(
            residence.parses(),
            5,
            "child and literal see the ancestor bytes"
        );
        assert!(named(&residence, "pkg:cargo/child@2.0.0"));
        assert!(!named(&residence, "pkg:cargo/child@1.0.0"));
        assert!(named(&residence, "pkg:cargo/literal@9.9.9"));
        assert!(named(&residence, "pkg:cargo/outside@3.0.0"));
        let parses = residence.parses();
        residence.refresh(roots).expect("stable after parent edit");
        assert_eq!(residence.parses(), parses);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_cached_parse_error_is_not_parsed_again_until_the_bytes_change() {
        let root = fixture("error");
        let project = root.join("broken");
        write(&project.join("Cargo.toml"), "[workspace]\nmembers = []\n");
        let mut residence = LocalManifestResidence::default();
        let Err(first) = residence.refresh([project.as_path()]) else {
            panic!("virtual workspace must fail");
        };
        assert_eq!(residence.parses(), 1);
        let Err(second) = residence.refresh([project.as_path()]) else {
            panic!("cached failure");
        };
        assert_eq!(second, first);
        assert_eq!(residence.parses(), 1);
        write(
            &project.join("Cargo.toml"),
            "[package]\nname = \"fixed\"\nversion = \"1.0.0\"\n",
        );
        residence.refresh([project.as_path()]).expect("fixed");
        assert_eq!(residence.parses(), 2);
        assert!(named(&residence, "pkg:cargo/fixed@1.0.0"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn removing_a_root_drops_its_fact_and_an_added_npm_manifest_is_parsed() {
        let root = fixture("membership");
        let cargo = root.join("cargo");
        let npm = root.join("npm");
        write(
            &cargo.join("Cargo.toml"),
            "[package]\nname = \"kept\"\nversion = \"1.0.0\"\n",
        );
        fs::create_dir_all(&npm).expect("npm dir");
        let mut residence = LocalManifestResidence::default();
        residence
            .refresh([cargo.as_path(), npm.as_path()])
            .expect("prime");
        assert_eq!(residence.facts().count(), 1);
        write(
            &npm.join("package.json"),
            r#"{"name":"left-pad","version":"1.2.3","dependencies":{"ms":"2.0.0"}}"#,
        );
        residence
            .refresh([cargo.as_path(), npm.as_path()])
            .expect("npm appears");
        assert_eq!(residence.parses(), 3);
        assert!(named(&residence, "pkg:npm/left-pad@1.2.3"));
        let parses = residence.parses();
        write(
            &cargo.join("package.json"),
            r#"{"name":"ignored","version":"9.9.9"}"#,
        );
        residence
            .refresh([cargo.as_path(), npm.as_path()])
            .expect("cargo stays authoritative");
        assert_eq!(residence.parses(), parses);
        assert!(named(&residence, "pkg:cargo/kept@1.0.0"));
        assert!(!named(&residence, "pkg:npm/ignored@9.9.9"));
        residence.refresh([npm.as_path()]).expect("drop cargo");
        assert!(!named(&residence, "pkg:cargo/kept@1.0.0"));
        assert_eq!(residence.facts().count(), 1);
        let _ = fs::remove_dir_all(root);
    }
}

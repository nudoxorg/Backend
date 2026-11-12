//! Resolve the native dependency closure from Cargo.lock for support identity.
use std::{
    collections::{BTreeSet, VecDeque},
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};
fn source_tree(root: &Path) -> String {
    fn collect(base: &Path, dir: &Path, files: &mut Vec<PathBuf>) {
        // Directory watches detect added and removed files, not just edits.
        println!("cargo:rerun-if-changed={}", dir.display());
        let mut entries: Vec<_> = fs::read_dir(dir)
            .expect("vendor directory")
            .map(|e| e.expect("vendor entry"))
            .collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let path = entry.path();
            let ty = entry.file_type().expect("vendor type");
            assert!(!ty.is_symlink(), "vendor source cannot contain symlinks");
            if ty.is_dir() {
                collect(base, &path, files);
            } else if ty.is_file() {
                files.push(
                    path.strip_prefix(base)
                        .expect("vendor relative path")
                        .to_owned(),
                );
            } else {
                panic!("unexpected vendor entry");
            }
        }
    }
    let mut files = Vec::new();
    collect(root, root, &mut files);
    files.sort();
    let mut hash = blake3::Hasher::new();
    hash.update(b"nudox.stylo-vendor-tree.v1\0");
    for relative in files {
        let name = relative.to_str().expect("UTF-8 vendor path");
        let path = root.join(&relative);
        let content = fs::read(&path).expect("vendor source");
        println!("cargo:rerun-if-changed={}", path.display());
        hash.update(&(name.len() as u64).to_le_bytes());
        hash.update(name.as_bytes());
        hash.update(&(content.len() as u64).to_le_bytes());
        hash.update(&content);
    }
    hash.finalize().to_hex().to_string()
}
fn main() {
    let root =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory")).join("../..");
    let lock = fs::read_to_string(root.join("Cargo.lock")).expect("workspace lockfile");
    let doc: toml::Value = lock.parse().expect("valid Cargo.lock");
    let packages = doc
        .get("package")
        .and_then(toml::Value::as_array)
        .expect("package list");
    let value = |p: &toml::Value, name: &str| {
        p.get(name)
            .and_then(toml::Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    let mut queue = VecDeque::from([("stylo".to_owned(), "0.23.0".to_owned())]);
    let mut seen = BTreeSet::new();
    while let Some((name, version)) = queue.pop_front() {
        let candidates: Vec<_> = packages
            .iter()
            .filter(|p| {
                value(p, "name") == name && (version.is_empty() || value(p, "version") == version)
            })
            .collect();
        assert_eq!(
            candidates.len(),
            1,
            "ambiguous locked native dependency {name} {version}"
        );
        let p = candidates[0];
        let name = value(p, "name");
        let version = value(p, "version");
        let mut source = value(p, "source");
        let mut checksum = value(p, "checksum");
        if source.is_empty() && checksum.is_empty() {
            let path = match (name.as_str(), version.as_str()) {
                ("stylo", "0.23.0") => root.join("vendor/stylo-native"),
                ("selectors", "0.42.0") => root.join("vendor/selectors-native"),
                ("stylo_derive", "0.23.0") => root.join("vendor/stylo-derive-native"),
                _ => panic!("unapproved path dependency in native closure: {name}@{version}"),
            };
            checksum = source_tree(&path);
            source = "audited-vendor-tree-blake3".to_owned();
        }
        assert!(
            !source.is_empty() && !checksum.is_empty(),
            "native dependency missing provenance"
        );
        if !seen.insert((name, version, source, checksum)) {
            continue;
        }
        if let Some(deps) = p.get("dependencies").and_then(toml::Value::as_array) {
            for dep in deps {
                let dep = dep.as_str().expect("dependency string");
                let mut words = dep.split_whitespace();
                let name = words.next().expect("dependency name").to_owned();
                let version = words
                    .next()
                    .filter(|s| !s.starts_with('('))
                    .unwrap_or("")
                    .to_owned();
                queue.push_back((name, version));
            }
        }
    }
    let rustc = env::var_os("RUSTC").expect("RUSTC");
    let version = Command::new(rustc)
        .arg("--version")
        .output()
        .expect("rustc version");
    assert!(version.status.success(), "cannot inspect rustc");
    let version = String::from_utf8(version.stdout).expect("rustc UTF-8");
    let mut basis = format!(
        "compiler={};target={};features=servo;",
        version.trim(),
        env::var("TARGET").expect("target")
    );
    for (name, version, source, checksum) in seen {
        basis.push_str(&format!("{name}@{version}:{source}:{checksum};"));
    }
    println!("cargo:rustc-env=STYLO_NATIVE_BUILD_BASIS={basis}");
    println!(
        "cargo:rerun-if-changed={}",
        root.join("Cargo.lock").display()
    );
}

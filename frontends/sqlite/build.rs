//! Binds the SQLite adapter and vendored native source closure to its recipe.
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};

fn collect(root: &Path, path: &Path, files: &mut Vec<PathBuf>) {
    let metadata = fs::symlink_metadata(path).expect("SQLite support source metadata");
    assert!(
        !metadata.file_type().is_symlink(),
        "SQLite support source contains a symlink"
    );
    println!("cargo:rerun-if-changed={}", path.display());
    if metadata.is_dir() {
        let mut entries = fs::read_dir(path)
            .expect("SQLite support directory")
            .map(|entry| entry.expect("SQLite support entry").path())
            .collect::<Vec<_>>();
        entries.sort();
        for child in entries {
            collect(root, &child, files);
        }
    } else {
        assert!(
            metadata.is_file() && metadata.len() <= 4 * 1024 * 1024,
            "invalid SQLite support file"
        );
        assert!(path.starts_with(root), "SQLite support escaped workspace");
        files.push(path.to_owned());
        assert!(files.len() <= 4096, "too many SQLite support files");
    }
}
fn main() {
    let package =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"));
    let root = package
        .parent()
        .and_then(Path::parent)
        .expect("repository root");
    let mut files = Vec::new();
    for relative in [
        "frontends/sqlite/Cargo.toml",
        "frontends/sqlite/Cargo.lock",
        "frontends/sqlite/build.rs",
        "frontends/sqlite/src",
        "frontends/sql-contract/Cargo.toml",
        "frontends/sql-contract/src",
        "vendor/syntaqlite-native",
    ] {
        collect(root, &root.join(relative), &mut files);
    }
    files.sort();
    let mut hash = Sha256::new();
    hash.update(b"nudox.sqlite-native-support.v1\0");
    for path in files {
        let relative = path.strip_prefix(root).expect("relative support path");
        let name = relative
            .to_str()
            .expect("UTF-8 support path")
            .replace('\\', "/");
        let bytes = fs::read(&path).expect("SQLite support source bytes");
        hash.update((name.len() as u64).to_be_bytes());
        hash.update(name.as_bytes());
        hash.update((bytes.len() as u64).to_be_bytes());
        hash.update(bytes);
    }
    let digest = hash.finalize();
    let value: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    println!("cargo:rustc-env=SQLITE_SUPPORT_SHA256={value}");
}

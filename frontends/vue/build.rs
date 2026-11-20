//! Build-bound identity for the exact vendored native TS parser inputs.
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};
fn collect(root: &Path, directory: &Path, output: &mut Vec<PathBuf>) {
    println!("cargo:rerun-if-changed={}", directory.display());
    let mut entries = fs::read_dir(directory)
        .expect("TSZ source directory")
        .map(|entry| entry.expect("TSZ source entry").path())
        .collect::<Vec<_>>();
    entries.sort();
    for path in entries {
        let meta = fs::symlink_metadata(&path).expect("TSZ source metadata");
        assert!(
            !meta.file_type().is_symlink(),
            "TSZ source must not contain symlinks"
        );
        if meta.is_dir() {
            collect(root, &path, output);
        } else if meta.is_file() {
            output.push(
                path.strip_prefix(root)
                    .expect("relative TSZ source")
                    .to_path_buf(),
            );
        }
    }
}
fn main() {
    let root =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory")).join("../..");
    let mut files = Vec::new();
    for part in ["tsz-parser", "tsz-scanner", "tsz-common", "tsz-binder"] {
        collect(
            &root,
            &root.join("third_party/tsz/crates").join(part),
            &mut files,
        );
    }
    files.push(PathBuf::from("Cargo.lock"));
    files.push(PathBuf::from("third_party/tsz/Cargo.toml"));
    files.push(PathBuf::from("third_party/tsz/UPSTREAM_REVISION"));
    files.sort();
    let mut hash = blake3::Hasher::new();
    hash.update(b"nudox.vue-native-build-basis.v1\0");
    for relative in files {
        let path = root.join(&relative);
        println!("cargo:rerun-if-changed={}", path.display());
        let name = relative.to_str().expect("UTF-8 source path");
        let bytes = fs::read(path).expect("read source input");
        hash.update(&(name.len() as u64).to_le_bytes());
        hash.update(name.as_bytes());
        hash.update(&(bytes.len() as u64).to_le_bytes());
        hash.update(&bytes);
    }
    let rustc = env::var_os("RUSTC").expect("RUSTC");
    let version = Command::new(rustc)
        .arg("--version")
        .output()
        .expect("rustc version");
    assert!(version.status.success(), "rustc version failed");
    hash.update(&version.stdout);
    println!(
        "cargo:rustc-env=VUE_NATIVE_BUILD_BASIS={}",
        hash.finalize().to_hex()
    );
}

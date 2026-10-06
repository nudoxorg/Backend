//! Embeds the Go package-loader source and its vendored dependency closure.

use std::{
    env, fs,
    path::{Path, PathBuf},
};

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let source_root = manifest_dir.join("src/legacy/oracle");
    let mut files = Vec::new();
    collect(&source_root, &source_root, &mut files);
    files.sort_by(|left, right| left.0.cmp(&right.0));
    assert!(files.iter().any(|(name, _)| name == "vendor/modules.txt"));
    assert!(
        files
            .iter()
            .any(|(name, _)| name == "vendor/golang.org/x/tools/go/packages/packages.go")
    );

    let mut generated =
        String::from("pub(crate) const GO_ORACLE_SOURCE_FILES: &[(&str, &[u8])] = &[\n");
    for (relative, absolute) in files {
        println!("cargo:rerun-if-changed={}", absolute.display());
        generated.push_str(&format!(
            "    ({relative:?}, include_bytes!({absolute:?})),\n",
            relative = relative,
            absolute = absolute,
        ));
    }
    generated.push_str("];\n");
    let output =
        PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR")).join("go_oracle_sources.rs");
    fs::write(output, generated).expect("write generated embedded Go file table");
    println!("cargo:rerun-if-changed={}", source_root.display());
}

fn collect(root: &Path, directory: &Path, output: &mut Vec<(String, PathBuf)>) {
    let mut entries = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("read Go oracle source directory {directory:?}: {error}"))
        .map(|entry| entry.expect("read Go oracle directory entry").path())
        .collect::<Vec<_>>();
    entries.sort();
    for path in entries {
        let file_type = fs::symlink_metadata(&path)
            .unwrap_or_else(|error| panic!("stat Go oracle source {path:?}: {error}"))
            .file_type();
        if file_type.is_symlink() {
            panic!("Go oracle embedded source must not be a symlink: {path:?}");
        }
        if file_type.is_dir() {
            collect(root, &path, output);
            continue;
        }
        if !file_type.is_file() {
            panic!("Go oracle embedded source is not a regular file: {path:?}");
        }
        let relative = path.strip_prefix(root).expect("source under oracle root");
        let name = relative
            .to_str()
            .unwrap_or_else(|| panic!("Go oracle source path is not UTF-8: {relative:?}"))
            .replace('\\', "/");
        let is_root_manifest = name == "go.mod" || name == "go.sum";
        let is_vendor = name.starts_with("vendor/");
        let is_go_source = name.ends_with(".go") && !name.ends_with("_test.go");
        if is_root_manifest || is_vendor || is_go_source {
            if name.starts_with("._") || name.contains("/._") {
                panic!("AppleDouble metadata is not source: {name}");
            }
            output.push((name, path));
        }
    }
}

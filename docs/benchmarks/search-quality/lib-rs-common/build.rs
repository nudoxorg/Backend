use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const PINNED_REVISION: &str = "4642a01664e14f4ae30a3804a55556b0770119d9";

fn main() {
    println!("cargo:rerun-if-env-changed=LIB_RS_MIRROR");
    let root = PathBuf::from(
        env::var_os("LIB_RS_MIRROR").expect("LIB_RS_MIRROR must name the pinned upstream checkout"),
    );
    let output = Command::new("git")
        .args([
            "-C",
            root.to_str().expect("UTF-8 upstream path"),
            "rev-parse",
            "HEAD",
        ])
        .output()
        .expect("run git to verify the upstream revision");
    assert!(
        output.status.success(),
        "could not inspect LIB_RS_MIRROR revision"
    );
    let revision = String::from_utf8(output.stdout).expect("git revision is UTF-8");
    assert_eq!(
        revision.trim(),
        PINNED_REVISION,
        "LIB_RS_MIRROR revision is not the frozen revision"
    );

    let source = root.join("search_index/src/lib_search_index.rs");
    assert!(
        source.is_file(),
        "pinned upstream search index source is missing"
    );
    println!("cargo:rerun-if-changed={}", source.display());
    let generated = format!("#[path = {:?}]\nmod upstream_search_index;\n", source);
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR"));
    fs::write(out_dir.join("upstream_search_index.rs"), generated)
        .expect("write upstream module include");
}

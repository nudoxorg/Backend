//! Defines build behavior for `compiler-driver`, whose purpose is to run bounded native toolchains and lower their output into canonical IR.
//! Resolves the exact link-time libclang authority for the direct Clang semantic frontend.
//! `LIBCLANG_PATH` is the single declared authority: it must name one exact libclang shared
//! object or one directory containing exactly one. Without it the crate builds in its graceful
//! typed-unavailable mode and no ambient loader, runtime search, or host location is consulted.

use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=LIBCLANG_PATH");
    println!("cargo:rustc-check-cfg=cfg(clang_native)");
    let Some(path) = declared_library_path() else {
        return;
    };
    println!("cargo:rerun-if-changed={}", path.display());
    let directory = path.parent().unwrap_or_else(|| {
        panic!(
            "LIBCLANG_PATH authority {} must have a containing directory",
            path.display()
        )
    });
    println!("cargo:rustc-link-search=native={}", directory.display());
    println!("cargo:rustc-link-lib=dylib=clang");
    if matches!(
        env::var("CARGO_CFG_TARGET_OS").as_deref(),
        Ok("linux") | Ok("macos")
    ) {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", directory.display());
    }
    println!("cargo:rustc-cfg=clang_native");
}

/// Resolves the declared exact libclang shared object, or `None` when the build must ship its
/// typed-unavailable mode.
fn declared_library_path() -> Option<PathBuf> {
    let configured = PathBuf::from(env::var_os("LIBCLANG_PATH")?);
    if configured.is_file() {
        let name = configured
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_else(|| panic!("LIBCLANG_PATH must be valid UTF-8"));
        assert!(
            name.starts_with("libclang") || name.starts_with("clang"),
            "LIBCLANG_PATH does not name a libclang shared object"
        );
        return Some(configured);
    }
    assert!(
        configured.is_dir(),
        "LIBCLANG_PATH must name one exact libclang shared object or its sealed directory"
    );
    let mut candidate = None;
    for entry in fs::read_dir(&configured).expect("LIBCLANG_PATH directory must be readable") {
        let entry = entry.expect("LIBCLANG_PATH directory entry must be readable");
        let path = entry.path();
        let is_shared = path.is_file()
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(is_libclang_shared_name);
        if !is_shared {
            continue;
        }
        assert!(
            candidate.is_none(),
            "LIBCLANG_PATH directory must contain exactly one libclang shared object"
        );
        candidate = Some(path);
    }
    Some(candidate.expect("LIBCLANG_PATH directory contains no libclang shared object"))
}

fn is_libclang_shared_name(name: &str) -> bool {
    name == "libclang.dll"
        || name == "libclang.dylib"
        || name == "libclang.so"
        || name.starts_with("libclang.so.")
}

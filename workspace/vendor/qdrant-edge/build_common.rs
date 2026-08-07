/// Assert that a vendored C source this build is about to compile is actually
/// present, and abort the build naming it if it is not.
///
/// ## Why this is a hard error and not a `cargo:warning`
///
/// It used to be a warning that returned early. That choice was wrong in a way
/// worth recording, because it cost this program two days (LIMITATIONS.md L7).
///
/// The kernels in `cpp/` and `src/segment/spaces/metric_f16/cpp/` are the *only*
/// definitions of the symbols that `encoded_vectors_u8.rs`,
/// `encoded_vectors_binary.rs` and `spaces/metric_f16/neon/*.rs` declare in
/// their `unsafe extern "C"` blocks. Skipping the `cc` invocation therefore does
/// not produce a degraded build — it produces an `rlib` with dangling references
/// that `cargo check` happily accepts (check never links) and that explodes at
/// the *final* link of every downstream binary as:
///
/// ```text
/// ld: Undefined symbols for architecture arm64:
///   "_impl_score_dot_neon", "_dotProduct_half_4x4", …
///     referenced from … libqdrant_edge-*.rlib
/// ```
///
/// which names neither this crate's vendoring nor the directory that is missing.
/// The blast radius lands on whoever is building `driver`, not on whoever broke
/// the checkout.
///
/// The "but a warning still lets the crate type-check" argument does not apply
/// here: `qdrant-edge` is in the root manifest's `exclude` list, so
/// `cargo check --workspace` never builds it at all. The only builds that reach
/// this build script are ones that are going to link it (`registry/local`), and
/// every one of those needs the kernels. There is no configuration in this
/// workspace that benefits from the early return, and one — `driver` — that is
/// broken by it.
pub fn require_vendored_source(relative_path: &str, provides: &str) {
    if std::path::Path::new(relative_path).exists() {
        return;
    }
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .expect("CARGO_MANIFEST_DIR is always set by cargo for a build script");
    panic!(
        "\n\
         qdrant-edge: vendored C source is missing from this checkout.\n\
         \n\
         missing:  {manifest_dir}/{relative_path}\n\
         provides: {provides}\n\
         \n\
         Nothing in the Rust tree defines those symbols; they exist only in that\n\
         file. Building without it yields an rlib that passes `cargo check` and\n\
         then fails at the link step of every downstream binary with undefined\n\
         symbols that name neither this crate nor this directory. Failing here\n\
         instead is the whole point.\n\
         \n\
         To obtain it, take the file from the published crate — it is byte-identical\n\
         to what belongs here:\n\
         \n\
           curl -sSLO https://static.crates.io/crates/qdrant-edge/qdrant-edge-0.7.2.crate\n\
           tar xzf qdrant-edge-0.7.2.crate\n\
           cp -R qdrant-edge-0.7.2/cpp                                  {manifest_dir}/\n\
           cp -R qdrant-edge-0.7.2/src/segment/spaces/metric_f16/cpp    {manifest_dir}/src/segment/spaces/metric_f16/\n\
         \n\
         See workspace/vendor/README.md for the full vendoring contract.\n"
    );
}

pub fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // Required for tango benchmarks, see:
    // https://github.com/bazhenov/tango/blob/v0.6.0/README.md#getting-started


    // Matches all platforms that have `nix::fcntl::posix_fadvise` function.
    // https://github.com/nix-rust/nix/blob/v0.29.0/src/fcntl.rs#L35-L42
    println!("cargo:rustc-check-cfg=cfg(posix_fadvise_supported)");
    if matches!(
        std::env::var("CARGO_CFG_TARGET_OS").unwrap().as_str(),
        "linux" | "freebsd" | "android" | "fuchsia" | "emscripten" | "wasi"
    ) || matches!(
        std::env::var("CARGO_CFG_TARGET_ENV").unwrap().as_str(),
        "uclibc"
    ) {
        println!("cargo:rustc-cfg=posix_fadvise_supported")
    }

    // Matches all platforms, that have `nix::sys::statfs::statfs` function.
    // https://github.com/nix-rust/nix/blob/v0.29.0/src/sys/mod.rs#L131
    println!("cargo:rustc-check-cfg=cfg(fs_type_check_supported)");
    if matches!(
        std::env::var("CARGO_CFG_TARGET_OS").unwrap().as_str(),
        "linux"
            | "freebsd"
            | "android"
            | "openbsd"
            | "ios"
            | "macos"
            | "watchos"
            | "tvos"
            | "visionos"
    ) {
        println!("cargo:rustc-cfg=fs_type_check_supported")
    }
}

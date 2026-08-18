use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=zstd/contrib/seekable_format/zstdseek_compress.c");
    println!("cargo:rerun-if-changed=zstd/contrib/seekable_format/zstdseek_decompress.c");
    println!("cargo:rerun-if-changed=zstd/contrib/seekable_format/zstd_seekable.h");
    println!("cargo:rerun-if-changed=xxh64.c");

    // `zstd-sys` (a direct dependency, see Cargo.toml) declares `links =
    // "zstd"` and is also `index::pack`'s own zstd dependency (via `zstd` ->
    // `zstd-safe` -> `zstd-sys`). Depending on it directly means cargo
    // resolves both consumers to the SAME zstd-sys unit -- one compiled
    // libzstd, linked once -- instead of this crate building an independent
    // second copy, which is what upstream's build.rs did whenever its
    // `pkg-config` probe failed (the default outside a devshell with
    // PKG_CONFIG_PATH pointed at a system zstd). Two copies both define
    // every `ZSTD_*` symbol; macOS `ld` only *warns* on the duplicate
    // instead of failing the build, so that fallback silently corrupted
    // memory at runtime instead of failing loudly (see
    // workspace/vendor/README.md and the 34 `index::pack` SIGSEGVs it
    // caused).
    //
    // `zstd-sys`'s build script exports the include path for the exact zstd
    // source tree it compiled as `cargo:include=<path>`, which cargo turns
    // into `DEP_ZSTD_INCLUDE` for any direct dependent's build script (the
    // env var name comes from `links = "zstd"`, not from this crate's own
    // name). Reading it here -- rather than hardcoding a path into either
    // crate's source tree -- is what keeps the seekable wrapper's headers
    // version-matched to whichever zstd `zstd-sys` actually built and
    // linked, with no path guessing and no environment variable this build
    // depends on beyond the one cargo itself wires up.
    let dep_include = env::var("DEP_ZSTD_INCLUDE")
        .expect("DEP_ZSTD_INCLUDE is not set; zstd-sys must provide the shared zstd headers");
    let zstd_include = PathBuf::from(dep_include);
    // zstd-sys exports only the top-level `zstd/lib` include path; the
    // wrapper's `#include "mem.h"` / `#include "xxhash.h"` (unqualified,
    // resolved via an explicit -I) live one level down, in `common/`, which
    // is part of the same zstd source tree zstd-sys already compiled.
    let zstd_common_include = zstd_include.join("common");

    cc::Build::new()
        .include(&zstd_include)
        .include(&zstd_common_include)
        .file("zstd/contrib/seekable_format/zstdseek_compress.c")
        .file("zstd/contrib/seekable_format/zstdseek_decompress.c")
        .file(zstd_common_include.join("xxhash.c"))
        // xxhash.c from zstd-sys's own tree (same version as the libzstd
        // being linked against), NOT recompiled from a second vendored
        // copy. It is compiled here WITHOUT `XXH_PRIVATE_API` (unlike
        // zstd-sys's internal use of it, which compiles xxhash statically/
        // inlined and exports nothing) so it produces one real, externally
        // visible `ZSTD_XXH64` (the `XXH_NAMESPACE` macro in xxhash.h
        // renames it) for `xxh64.c` and the wrapper files below to call.
        // This mirrors upstream's own behavior exactly (it also compiled
        // its bundled copy of this same file without XXH_PRIVATE_API) --
        // confirmed to introduce no new duplicate symbol: XXH64 never
        // appeared in the pre-fix duplicate-symbol list, only `ZSTD_*`
        // compress/decompress entry points did.
        .file("xxh64.c")
        .opt_level(3)
        .warnings(false)
        .compile("zstdseek");
    // zstd-sys emits the library search path but intentionally leaves the
    // link directive to consumers. The seekable wrapper is one such consumer.
    println!("cargo:rustc-link-lib=static=zstd");
}

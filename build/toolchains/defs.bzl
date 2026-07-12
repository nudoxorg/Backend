# Custom CXX tools that use absolute paths instead of bare command names.
#
# The prelude's from_any_dir.py shim (used by cc-rs build scripts) calls
# os.execl() which does NOT search PATH — it requires an absolute path.
# The default prelude toolchain passes bare "clang", which fails.
# This rule passes absolute paths so the shim can exec the compiler directly.
#
# On Nix: clang/ar come from the active devshell (absolute Nix store paths
# baked in at the time the wrapper genrule runs).  On non-Nix macOS/Linux
# the system binaries at /usr/bin work fine.

load("@prelude//cxx:cxx_toolchain_types.bzl", "LinkerType")
load("@prelude//toolchains:cxx.bzl", "CxxToolsInfo")

def _absolute_clang_tools_impl(ctx: AnalysisContext) -> list[Provider]:
    return [
        DefaultInfo(),
        CxxToolsInfo(
            compiler = ctx.attrs.clang,
            compiler_type = "clang",
            cxx_compiler = ctx.attrs.clangxx,
            asm_compiler = ctx.attrs.clang,
            asm_compiler_type = "clang",
            rc_compiler = None,
            cvtres_compiler = None,
            archiver = ctx.attrs.ar,
            archiver_type = "gnu",
            linker = ctx.attrs.clangxx,
            linker_type = LinkerType(ctx.attrs.linker_type),
        ),
    ]

# Produces a CxxToolsInfo with resolved absolute paths.
# Override clang/clangxx/ar when providing a Nix-specific target.
absolute_clang_tools = rule(
    impl = _absolute_clang_tools_impl,
    attrs = {
        "ar": attrs.string(default = "/usr/bin/ar"),
        "clang": attrs.string(default = "/usr/bin/clang"),
        "clangxx": attrs.string(default = "/usr/bin/clang++"),
        "linker_type": attrs.string(
            default = select({
                "DEFAULT": "gnu",
                "config//os:macos": "darwin",
            }),
        ),
    },
)

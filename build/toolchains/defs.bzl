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

load(
    "@prelude//cxx:cxx_toolchain_types.bzl",
    "BinaryUtilitiesInfo",
    "CCompilerInfo",
    "CxxCompilerInfo",
    "CxxInternalTools",
    "CxxPlatformInfo",
    "CxxToolchainInfo",
    "LinkerInfo",
    "LinkerType",
    "PicBehavior",
    "RuntimeDependencyHandling",
    "ShlibInterfacesMode",
)
load("@prelude//cxx:headers.bzl", "HeaderMode")
load("@prelude//cxx:linker.bzl", "is_pdb_generated")
load("@prelude//linking:link_info.bzl", "LinkOrdering", "LinkStyle")
load("@prelude//linking:lto.bzl", "LtoMode")
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

# Local replacement for @nix//toolchains:cxx.bzl nix_cxx_toolchain.
# Identical to the upstream rule except it passes runtime_dependency_handling,
# which CxxToolchainInfo now requires and the external cell hasn't added yet.
def _nix_cxx_toolchain_impl(ctx: AnalysisContext) -> list[Provider]:
    nix_cc = ctx.attrs.nix_cc[DefaultInfo].sub_targets
    compiler = nix_cc["cc"][RunInfo]
    cxx_compiler = nix_cc["c++"][RunInfo]
    compiler_type = "clang" if host_info().os.is_macos else "g++"
    archiver = nix_cc["ar"][RunInfo]
    archiver_type = "gnu"
    archiver_supports_argfiles = True
    asm_compiler = compiler
    asm_compiler_type = compiler_type
    linker = cxx_compiler
    linker_type = LinkerType("gnu")
    pic_behavior = PicBehavior("supported")
    binary_extension = ""
    object_file_extension = "o"
    static_library_extension = "a"
    shared_library_name_default_prefix = "lib"
    shared_library_name_format = "{}.so"
    shared_library_versioned_name_format = "{}.so.{}"
    additional_linker_flags = []
    if host_info().os.is_macos:
        archiver_supports_argfiles = False
        linker_type = LinkerType("darwin")
        pic_behavior = PicBehavior("always_enabled")
    elif host_info().os.is_windows:
        fail("not supported")
    llvm_link = RunInfo(args = ["llvm-link"]) if compiler_type == "clang" else None
    return [
        DefaultInfo(),
        CxxToolchainInfo(
            internal_tools = ctx.attrs._internal_tools[CxxInternalTools],
            linker_info = LinkerInfo(
                linker = RunInfo(args = [linker]),
                linker_flags = additional_linker_flags + ctx.attrs.link_flags,
                archiver = archiver,
                archiver_type = archiver_type,
                archiver_supports_argfiles = archiver_supports_argfiles,
                generate_linker_maps = False,
                lto_mode = LtoMode("none"),
                type = linker_type,
                link_binaries_locally = True,
                archive_objects_locally = True,
                use_archiver_flags = True,
                static_dep_runtime_ld_flags = [],
                static_pic_dep_runtime_ld_flags = [],
                shared_dep_runtime_ld_flags = [],
                independent_shlib_interface_linker_flags = [],
                shlib_interfaces = ShlibInterfacesMode("stub_from_library"),
                link_style = LinkStyle(ctx.attrs.link_style),
                link_weight = 1,
                binary_extension = binary_extension,
                object_file_extension = object_file_extension,
                shared_library_name_default_prefix = shared_library_name_default_prefix,
                shared_library_name_format = shared_library_name_format,
                shared_library_versioned_name_format = shared_library_versioned_name_format,
                static_library_extension = static_library_extension,
                force_full_hybrid_if_capable = False,
                is_pdb_generated = is_pdb_generated(linker_type, ctx.attrs.link_flags),
                link_ordering = ctx.attrs.link_ordering,
            ),
            bolt_enabled = False,
            binary_utilities_info = BinaryUtilitiesInfo(
                nm = nix_cc["nm"][RunInfo],
                objcopy = nix_cc["objcopy"][RunInfo],
                ranlib = nix_cc["ranlib"][RunInfo],
                strip = nix_cc["strip"][RunInfo],
                dwp = None,
                bolt_msdk = None,
            ),
            cxx_compiler_info = CxxCompilerInfo(
                compiler = RunInfo(args = [cxx_compiler]),
                preprocessor_flags = [],
                compiler_flags = ctx.attrs.cxx_flags,
                compiler_type = compiler_type,
            ),
            c_compiler_info = CCompilerInfo(
                compiler = RunInfo(args = [compiler]),
                preprocessor_flags = [],
                compiler_flags = ctx.attrs.c_flags,
                compiler_type = compiler_type,
            ),
            as_compiler_info = CCompilerInfo(
                compiler = RunInfo(args = [compiler]),
                compiler_type = compiler_type,
            ),
            asm_compiler_info = CCompilerInfo(
                compiler = RunInfo(args = [asm_compiler]),
                compiler_type = asm_compiler_type,
            ),
            header_mode = HeaderMode("symlink_tree_only"),
            cpp_dep_tracking_mode = ctx.attrs.cpp_dep_tracking_mode,
            pic_behavior = pic_behavior,
            llvm_link = llvm_link,
            runtime_dependency_handling = RuntimeDependencyHandling("no_symlink"),
        ),
        CxxPlatformInfo(name = "aarch64" if host_info().arch.is_aarch64 else "x86_64"),
    ]

nix_cxx_toolchain = rule(
    impl = _nix_cxx_toolchain_impl,
    attrs = {
        "_internal_tools": attrs.default_only(attrs.exec_dep(providers = [CxxInternalTools], default = "prelude//cxx/tools:internal_tools")),
        "c_flags": attrs.list(attrs.string(), default = []),
        "cpp_dep_tracking_mode": attrs.string(default = "makefile"),
        "cxx_flags": attrs.list(attrs.string(), default = []),
        "link_flags": attrs.list(attrs.string(), default = []),
        "link_ordering": attrs.option(attrs.enum(LinkOrdering.values()), default = None),
        "link_style": attrs.string(default = "shared"),
        "nix_cc": attrs.exec_dep(),
    },
    is_toolchain_rule = True,
)

def _system_binary_impl(ctx: AnalysisContext) -> list[Provider]:
    return [
        DefaultInfo(),
        RunInfo(args = cmd_args(ctx.attrs.binary)),
    ]

# Wraps a system binary (found on PATH at runtime) as a Buck2 executable
# target.  Used as a non-Nix fallback for targets that are normally built
# hermetically via flake.package() when [nix] toolchain = 0.
system_binary = rule(
    impl = _system_binary_impl,
    attrs = {
        "binary": attrs.string(doc = "Binary name or absolute path; resolved via PATH at runtime"),
    },
)

# Stub for @nix//:flake.bzl.
#
# The @nix cell resolves here on machines without Nix (base .buckconfig sets
# `nix = nix/build/stubs/nix`). When the Nix devshell is active, its ignored
# `.buckconfig.local` points that cell at the real buck2.nix source instead.
#
# flake.package() creates empty filegroup placeholders so target references
# (e.g. :nix_cc, :rustc) resolve without error at BUCK evaluation time.
# None of those targets are ever selected when `[nix] toolchain = 0` (the
# non-Nix default); all toolchain aliases point to system toolchains instead.

def _stub_package(*, name, binary = None, binaries = [], path, package = None, target_compatible_with = None, visibility = []):
    native.filegroup(name = name, srcs = [], visibility = visibility)

flake = struct(package = _stub_package)

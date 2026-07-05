_TP = "//build/third-party"

_MEMBERS = {
    "heart":     "//workspace/heart:heart",
    "ir":        "//workspace/compiler/intermediate-representation:ir",
    "compiler":  "//workspace/compiler:compiler",
    "registry":  "//workspace/registry:registry",
    "runtime":   "//workspace/runtime:runtime",
    "server":    "//workspace/server:server-lib",
}

def crate(n):
    return _TP + ":" + n

def workspace(n):
    return _MEMBERS[n]

def deps(crates = [], members = [], raw = []):
    return [crate(c) for c in crates] + [_MEMBERS[m] for m in members] + raw

def rust_crate(name, deps = [], crate_root = "lib.rs", edition = "2024",
               srcs = None, features = None, visibility = None, **kw):
    native.rust_library(
        name = name,
        srcs = srcs if srcs != None else native.glob(["**/*.rs"]),
        crate_root = crate_root,
        edition = edition,
        deps = deps,
        features = features,
        visibility = visibility or ["PUBLIC"],
        **kw
    )

def rust_bin(name, srcs, crate_root, deps = [], edition = "2024", visibility = None, **kw):
    native.rust_binary(
        name = name,
        srcs = srcs,
        crate_root = crate_root,
        edition = edition,
        deps = deps,
        visibility = visibility or ["PUBLIC"],
        **kw
    )

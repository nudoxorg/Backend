//! Versioned fleet corpus manifest and its deterministic, seeded selection.
//!
//! The legacy inventory froze a rectangular twenty-per-lane table by hand and
//! derived `REAL_PACKAGE_COUNT` from that frozen rectangle. This module
//! replaces the hand-frozen tables with a versioned manifest of real
//! candidate coordinates. Every candidate records the driver table it is
//! expected to resolve through (`table`) and the evidence that witnessed the
//! coordinate (`origin`). No coordinate is synthesized: every row below is a
//! real package/project coordinate observed in a committed repository table,
//! lockfile, or dependency list, or in a package cache provisioned on the
//! manifest host (marked [`CandidateOrigin::ProvisionedHostCache`]).
//!
//! The default manifest (version 210 rows) is:
//! - rust: 40, typescript: 30, python: 30,
//!   go: 30, java: 40, csharp: 20,
//!   clang: 20.
//! Two lanes (csharp and clang) cannot reach the thirty-candidate planning
//! target from the committed evidence available in this repository; their
//! shortfall is recorded here rather than padded with fabricated rows, and
//! the manifest is shaped so new candidates can be appended per lane.
//!
//! The selection is a seeded bijective stride over the flattened manifest. It
//! is fully deterministic: `(version, seed)` fixes the selected rows and their
//! order, so two runs with the same seed produce byte-identical selections. A
//! candidate that is not provisioned on the running host still yields a typed
//! `Unavailable` terminal; it is never dropped.

use super::*;

/// Manifest revision. Bump whenever candidate membership changes; a seeded
/// selection is only meaningful together with this version.
pub(crate) const FLEET_MANIFEST_VERSION: u32 = 1;

/// Default selection seed. Two different seeds select different rows.
pub(crate) const FLEET_SELECTION_SEED: u64 = 0xF1E7_5EED_00C0_FFEE;

/// Requested audit size and the minimum the capacity terminal demands.
pub(crate) const FLEET_SELECTION_TARGET: usize = 200;
pub(crate) const FLEET_PACKAGE_MINIMUM: usize = 200;

/// Where a candidate coordinate was witnessed. Provenance is part of the
/// manifest so that a reviewer can tell committed corpus membership from a
/// host-provisioned cache extension.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CandidateOrigin {
    /// A coordinate copied from a committed driver corpus table.
    LockedDriverTable,
    /// A coordinate witnessed in another committed repository artifact (a
    /// lockfile, dependency table, or lane evidence file).
    CommittedRepositoryEvidence,
    /// A real coordinate present in a package cache provisioned on the
    /// manifest host. It is unavailable by construction unless that cache is
    /// named as the lane root on the running host.
    ProvisionedHostCache,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ManifestCandidate {
    pub(crate) coordinate: PackageCoordinate,
    pub(crate) table: CorpusTable,
    pub(crate) origin: CandidateOrigin,
}

const LOCKED: CandidateOrigin = CandidateOrigin::LockedDriverTable;
const COMMITTED: CandidateOrigin = CandidateOrigin::CommittedRepositoryEvidence;
const HOST: CandidateOrigin = CandidateOrigin::ProvisionedHostCache;

#[rustfmt::skip]
pub(crate) const RUST_CANDIDATES: [ManifestCandidate; 40] = [
    ManifestCandidate { coordinate: PackageCoordinate::Purl("cargo:serde@1.0.229"), table: CorpusTable::RustDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("cargo:thiserror@2.0.20"), table: CorpusTable::RustDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("cargo:ab_glyph@0.2.32"), table: CorpusTable::RustDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("cargo:block2@0.6.2"), table: CorpusTable::RustDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("cargo:cookie_store@0.7.0"), table: CorpusTable::RustDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("cargo:dragonbox_ecma@0.1.12"), table: CorpusTable::RustDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("cargo:futures-executor@0.3.34"), table: CorpusTable::RustDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("cargo:hashbrown@0.14.5"), table: CorpusTable::RustDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("cargo:jni-sys-macros@0.4.1"), table: CorpusTable::RustDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("cargo:ndk-context@0.1.1"), table: CorpusTable::RustDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("cargo:ownedbytes@0.9.0"), table: CorpusTable::RustDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("cargo:proptest@1.11.0"), table: CorpusTable::RustDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("cargo:rayon@1.11.0"), table: CorpusTable::RustDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("cargo:semver-parser@0.7.0"), table: CorpusTable::RustDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("cargo:tantivy@0.25.0"), table: CorpusTable::RustDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("cargo:typed-arena@2.0.2"), table: CorpusTable::RustDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("cargo:wasmtime-internal-versioned-export-macros@47.0.3"), table: CorpusTable::RustDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("cargo:windows_x86_64_gnullvm@0.48.5"), table: CorpusTable::RustDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("cargo:backend-semantic@0.1.0"), table: CorpusTable::RustDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("cargo:backend-version@0.1.0"), table: CorpusTable::RustDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("cargo:accesskit@0.24.1"), table: CorpusTable::RustDriver, origin: COMMITTED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("cargo:atomic@0.5.3"), table: CorpusTable::RustDriver, origin: COMMITTED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("cargo:bytemuck@1.25.2"), table: CorpusTable::RustDriver, origin: COMMITTED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("cargo:core-graphics-types@0.2.0"), table: CorpusTable::RustDriver, origin: COMMITTED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("cargo:ena@0.14.4"), table: CorpusTable::RustDriver, origin: COMMITTED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("cargo:futures-core@0.3.34"), table: CorpusTable::RustDriver, origin: COMMITTED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("cargo:httpdate@1.0.3"), table: CorpusTable::RustDriver, origin: COMMITTED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("cargo:khronos-egl@6.0.0"), table: CorpusTable::RustDriver, origin: COMMITTED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("cargo:metal@0.33.0"), table: CorpusTable::RustDriver, origin: COMMITTED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("cargo:objc2-foundation@0.2.2"), table: CorpusTable::RustDriver, origin: COMMITTED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("cargo:pathfinder_simd@0.5.6"), table: CorpusTable::RustDriver, origin: COMMITTED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("cargo:prost-derive@0.14.4"), table: CorpusTable::RustDriver, origin: COMMITTED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("cargo:ra_ap_vfs-notify@0.0.341"), table: CorpusTable::RustDriver, origin: COMMITTED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("cargo:rustc-literal-escaper@0.0.7"), table: CorpusTable::RustDriver, origin: COMMITTED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("cargo:slab@0.4.12"), table: CorpusTable::RustDriver, origin: COMMITTED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("cargo:tantivy-stacker@0.7.0"), table: CorpusTable::RustDriver, origin: COMMITTED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("cargo:tree-sitter-cpp@0.23.4"), table: CorpusTable::RustDriver, origin: COMMITTED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("cargo:utf8-zero@0.8.1"), table: CorpusTable::RustDriver, origin: COMMITTED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("cargo:windows-core@0.57.0"), table: CorpusTable::RustDriver, origin: COMMITTED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("cargo:winnow@0.5.40"), table: CorpusTable::RustDriver, origin: COMMITTED },
];

#[rustfmt::skip]
pub(crate) const TYPESCRIPT_CANDIDATES: [ManifestCandidate; 30] = [
    ManifestCandidate { coordinate: PackageCoordinate::Purl("npm:lodash@4.17.21"), table: CorpusTable::TypeScriptDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("npm:zod@3.25.76"), table: CorpusTable::TypeScriptDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("npm:@types/react@18.3.12"), table: CorpusTable::TypeScriptDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("npm:@babel/parser@7.26.8"), table: CorpusTable::TypeScriptDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("npm:typescript@5.7.2"), table: CorpusTable::TypeScriptDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("npm:react@18.3.1"), table: CorpusTable::TypeScriptDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("npm:express@4.21.1"), table: CorpusTable::TypeScriptDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("npm:@types/node@22.10.1"), table: CorpusTable::TypeScriptDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("npm:date-fns@4.1.0"), table: CorpusTable::TypeScriptDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("npm:axios@1.7.9"), table: CorpusTable::TypeScriptDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("npm:chalk@5.3.0"), table: CorpusTable::TypeScriptDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("npm:commander@13.1.0"), table: CorpusTable::TypeScriptDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("npm:fastify@5.2.1"), table: CorpusTable::TypeScriptDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("npm:uuid@11.0.3"), table: CorpusTable::TypeScriptDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("npm:pino@9.6.0"), table: CorpusTable::TypeScriptDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("npm:hono@4.6.12"), table: CorpusTable::TypeScriptDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("npm:yup@1.6.1"), table: CorpusTable::TypeScriptDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("npm:rxjs@7.8.1"), table: CorpusTable::TypeScriptDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("npm:@tanstack/query-core@5.62.8"), table: CorpusTable::TypeScriptDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("npm:eslint@9.17.0"), table: CorpusTable::TypeScriptDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("npm:react@18.2.0"), table: CorpusTable::TypeScriptDriver, origin: COMMITTED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("npm:@types/react@19.2.18"), table: CorpusTable::TypeScriptDriver, origin: COMMITTED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("npm:lodash@4.18.1"), table: CorpusTable::TypeScriptDriver, origin: COMMITTED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("npm:@ai-sdk/provider@3.0.8"), table: CorpusTable::TypeScriptDriver, origin: HOST },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("npm:@msgpackr-extract/msgpackr-extract-darwin-arm64@3.0.4"), table: CorpusTable::TypeScriptDriver, origin: HOST },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("npm:@standard-schema/spec@1.1.0"), table: CorpusTable::TypeScriptDriver, origin: HOST },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("npm:cross-spawn@7.0.6"), table: CorpusTable::TypeScriptDriver, origin: HOST },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("npm:detect-libc@2.1.2"), table: CorpusTable::TypeScriptDriver, origin: HOST },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("npm:effect@4.0.0-beta.83"), table: CorpusTable::TypeScriptDriver, origin: HOST },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("npm:fast-check@4.9.0"), table: CorpusTable::TypeScriptDriver, origin: HOST },
];

#[rustfmt::skip]
pub(crate) const PYTHON_CANDIDATES: [ManifestCandidate; 30] = [
    ManifestCandidate { coordinate: PackageCoordinate::Purl("pypi:requests@2.32.3"), table: CorpusTable::PythonDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("pypi:attrs@25.3.0"), table: CorpusTable::PythonDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("pypi:flask@3.1.1"), table: CorpusTable::PythonDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("pypi:six@1.17.0"), table: CorpusTable::PythonDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("pypi:wcwidth@0.2.13"), table: CorpusTable::PythonDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("pypi:idna@3.10"), table: CorpusTable::PythonDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("pypi:certifi@2025.7.14"), table: CorpusTable::PythonDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("pypi:packaging@25.0"), table: CorpusTable::PythonDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("pypi:pyparsing@3.2.3"), table: CorpusTable::PythonDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("pypi:iniconfig@2.1.0"), table: CorpusTable::PythonDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("pypi:pluggy@1.6.0"), table: CorpusTable::PythonDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("pypi:click@8.2.1"), table: CorpusTable::PythonDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("pypi:itsdangerous@2.2.0"), table: CorpusTable::PythonDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("pypi:jinja2@3.1.6"), table: CorpusTable::PythonDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("pypi:markupsafe@3.0.2"), table: CorpusTable::PythonDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("pypi:werkzeug@3.1.3"), table: CorpusTable::PythonDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("pypi:colorama@0.4.6"), table: CorpusTable::PythonDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("pypi:PyYAML@6.0.2"), table: CorpusTable::PythonDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("pypi:tomli@2.2.1"), table: CorpusTable::PythonDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("pypi:webencodings@0.5.1"), table: CorpusTable::PythonDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("pypi:afdko@5.0.1"), table: CorpusTable::PythonDriver, origin: HOST },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("pypi:anyio@4.14.2"), table: CorpusTable::PythonDriver, origin: HOST },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("pypi:appdirs@1.4.4"), table: CorpusTable::PythonDriver, origin: HOST },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("pypi:attrs@26.1.0"), table: CorpusTable::PythonDriver, origin: HOST },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("pypi:babel@2.18.0"), table: CorpusTable::PythonDriver, origin: HOST },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("pypi:beancount@3.2.3"), table: CorpusTable::PythonDriver, origin: HOST },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("pypi:beangulp@0.2.0"), table: CorpusTable::PythonDriver, origin: HOST },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("pypi:beanquery@0.2.0"), table: CorpusTable::PythonDriver, origin: HOST },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("pypi:beautifulsoup4@4.15.0"), table: CorpusTable::PythonDriver, origin: HOST },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("pypi:blinker@1.9.0"), table: CorpusTable::PythonDriver, origin: HOST },
];

#[rustfmt::skip]
pub(crate) const GO_CANDIDATES: [ManifestCandidate; 30] = [
    ManifestCandidate { coordinate: PackageCoordinate::Purl("golang:github.com/google/uuid@v1.6.0"), table: CorpusTable::GoDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("golang:gopkg.in/yaml.v3@v3.0.1"), table: CorpusTable::GoDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("golang:github.com/gorilla/mux@v1.8.1"), table: CorpusTable::GoDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("golang:github.com/google/go-cmp@v0.6.0"), table: CorpusTable::GoDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("golang:golang.org/x/sync@v0.10.0"), table: CorpusTable::GoDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("golang:golang.org/x/mod@v0.17.0"), table: CorpusTable::GoDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("golang:golang.org/x/time@v0.5.0"), table: CorpusTable::GoDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("golang:golang.org/x/tools@v0.30.0"), table: CorpusTable::GoDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("golang:github.com/pelletier/go-toml/v2@v2.2.2"), table: CorpusTable::GoDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("golang:github.com/oklog/ulid/v2@v2.1.0"), table: CorpusTable::GoDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("golang:github.com/cespare/xxhash/v2@v2.3.0"), table: CorpusTable::GoDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("golang:github.com/BurntSushi/toml@v1.4.0"), table: CorpusTable::GoDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("golang:github.com/zeebo/xxh3@v1.0.2"), table: CorpusTable::GoDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("golang:github.com/minio/highwayhash@v1.0.2"), table: CorpusTable::GoDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("golang:github.com/julienschmidt/httprouter@v1.3.0"), table: CorpusTable::GoDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("golang:github.com/go-chi/chi/v5@v5.0.12"), table: CorpusTable::GoDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("golang:github.com/rs/zerolog@v1.33.0"), table: CorpusTable::GoDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("golang:github.com/davecgh/go-spew@v1.1.1"), table: CorpusTable::GoDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("golang:github.com/pkg/errors@v0.9.1"), table: CorpusTable::GoDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("golang:github.com/mitchellh/go-homedir@v1.1.0"), table: CorpusTable::GoDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("golang:rsc.io/quote@v1.5.2"), table: CorpusTable::GoDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("golang:golang.org/x/mod@v0.23.0"), table: CorpusTable::GoDriver, origin: COMMITTED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("golang:golang.org/x/sync@v0.11.0"), table: CorpusTable::GoDriver, origin: COMMITTED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("golang:github.com/coreos/go-systemd/v22@v22.5.0"), table: CorpusTable::GoDriver, origin: HOST },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("golang:github.com/davecgh/go-spew@v1.1.0"), table: CorpusTable::GoDriver, origin: HOST },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("golang:github.com/mattn/go-colorable@v0.1.13"), table: CorpusTable::GoDriver, origin: HOST },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("golang:github.com/mattn/go-isatty@v0.0.19"), table: CorpusTable::GoDriver, origin: HOST },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("golang:github.com/pborman/getopt@v0.0.0-20170112200414-7148bc3a4c30"), table: CorpusTable::GoDriver, origin: HOST },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("golang:github.com/pelletier/go-toml/v2@v2.2.4"), table: CorpusTable::GoDriver, origin: HOST },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("golang:github.com/pmezard/go-difflib@v1.0.0"), table: CorpusTable::GoDriver, origin: HOST },
];

#[rustfmt::skip]
pub(crate) const JAVA_CANDIDATES: [ManifestCandidate; 40] = [
    ManifestCandidate { coordinate: PackageCoordinate::Purl("maven:org.apache.commons:commons-lang3@3.14.0"), table: CorpusTable::JavaDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("maven:com.google.guava:guava@33.0.0-jre"), table: CorpusTable::JavaDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("maven:com.fasterxml.jackson.core:jackson-databind@2.16.1"), table: CorpusTable::JavaDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("maven:org.junit.jupiter:junit-jupiter-api@5.10.1"), table: CorpusTable::JavaDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("maven:io.projectreactor:reactor-core@3.6.2"), table: CorpusTable::JavaDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("maven:org.apache.commons:commons-csv@1.10.0"), table: CorpusTable::JavaDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("maven:org.jsoup:jsoup@1.17.2"), table: CorpusTable::JavaDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("maven:org.ow2.asm:asm@9.6"), table: CorpusTable::JavaDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("maven:com.google.code.gson:gson@2.10.1"), table: CorpusTable::JavaDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("maven:io.vavr:vavr@0.10.4"), table: CorpusTable::JavaDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("maven:org.jctools:jctools-core@4.0.2"), table: CorpusTable::JavaDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("maven:org.apache.commons:commons-math3@3.6.1"), table: CorpusTable::JavaDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("maven:org.eclipse.collections:eclipse-collections@11.1.0"), table: CorpusTable::JavaDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("maven:org.bitbucket.b_c:jose4j@0.9.6"), table: CorpusTable::JavaDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("maven:org.tukaani:xz@1.9"), table: CorpusTable::JavaDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("maven:org.apache.commons:commons-compress@1.26.0"), table: CorpusTable::JavaDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("maven:org.apache.commons:commons-pool2@2.12.0"), table: CorpusTable::JavaDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("maven:org.jetbrains:annotations@24.0.1"), table: CorpusTable::JavaDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("maven:com.zaxxer:HikariCP@5.1.0"), table: CorpusTable::JavaDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("maven:org.apache.commons:commons-text@1.11.0"), table: CorpusTable::JavaDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("maven:com.fasterxml.jackson.core:jackson-annotations@2.16.1"), table: CorpusTable::JavaDriver, origin: COMMITTED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("maven:com.fasterxml.jackson.core:jackson-core@2.16.1"), table: CorpusTable::JavaDriver, origin: COMMITTED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("maven:com.google.code.findbugs:jsr305@3.0.2"), table: CorpusTable::JavaDriver, origin: COMMITTED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("maven:com.google.errorprone:error_prone_annotations@2.18.0"), table: CorpusTable::JavaDriver, origin: COMMITTED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("maven:com.google.guava:failureaccess@1.0.2"), table: CorpusTable::JavaDriver, origin: COMMITTED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("maven:com.google.j2objc:j2objc-annotations@2.8"), table: CorpusTable::JavaDriver, origin: COMMITTED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("maven:commons-codec:commons-codec@1.16.0"), table: CorpusTable::JavaDriver, origin: COMMITTED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("maven:commons-io:commons-io@2.15.1"), table: CorpusTable::JavaDriver, origin: COMMITTED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("maven:io.dropwizard.metrics:metrics-core@4.2.25"), table: CorpusTable::JavaDriver, origin: COMMITTED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("maven:io.dropwizard.metrics:metrics-healthchecks@4.2.25"), table: CorpusTable::JavaDriver, origin: COMMITTED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("maven:io.micrometer:micrometer-commons@1.12.1"), table: CorpusTable::JavaDriver, origin: COMMITTED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("maven:io.micrometer:micrometer-core@1.12.1"), table: CorpusTable::JavaDriver, origin: COMMITTED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("maven:org.apiguardian:apiguardian-api@1.1.2"), table: CorpusTable::JavaDriver, origin: COMMITTED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("maven:org.checkerframework:checker-qual@3.36.0"), table: CorpusTable::JavaDriver, origin: COMMITTED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("maven:org.eclipse.collections:eclipse-collections-api@11.1.0"), table: CorpusTable::JavaDriver, origin: COMMITTED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("maven:org.junit.platform:junit-platform-commons@1.10.1"), table: CorpusTable::JavaDriver, origin: COMMITTED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("maven:org.opentest4j:opentest4j@1.3.0"), table: CorpusTable::JavaDriver, origin: COMMITTED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("maven:org.reactivestreams:reactive-streams@1.0.4"), table: CorpusTable::JavaDriver, origin: COMMITTED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("maven:org.slf4j:slf4j-api@2.0.10"), table: CorpusTable::JavaDriver, origin: COMMITTED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("maven:org.jspecify:jspecify@1.0.0"), table: CorpusTable::JavaDriver, origin: COMMITTED },
];

#[rustfmt::skip]
pub(crate) const CSHARP_CANDIDATES: [ManifestCandidate; 20] = [
    ManifestCandidate { coordinate: PackageCoordinate::Purl("nuget:nullable@1.3.1"), table: CorpusTable::CSharpDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("nuget:xunit.assert.source@2.9.3"), table: CorpusTable::CSharpDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("nuget:isexternalinit@1.0.3"), table: CorpusTable::CSharpDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("nuget:polyfill@2.0.0"), table: CorpusTable::CSharpDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("nuget:devlooped.tablestorage.source@5.5.0"), table: CorpusTable::CSharpDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("nuget:tinyioc@1.4.0-rc1"), table: CorpusTable::CSharpDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("nuget:ramltoopenapiconverter.sourceonly@0.21.0"), table: CorpusTable::CSharpDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("nuget:ramltoopenapiconverter.sourceonly@0.8.0"), table: CorpusTable::CSharpDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("nuget:esp-net-source@0.6.4"), table: CorpusTable::CSharpDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("nuget:esp-net-source@0.2.3"), table: CorpusTable::CSharpDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("nuget:nullability.source@2.3.0"), table: CorpusTable::CSharpDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("nuget:nullability.source@2.1.0"), table: CorpusTable::CSharpDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("nuget:morelinq.source.moreenumerable.distinctby@1.0.2"), table: CorpusTable::CSharpDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("nuget:morelinq.source.moreenumerable.pairwise@1.0.2"), table: CorpusTable::CSharpDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("nuget:morelinq.source.moreenumerable.acquire@1.0.2"), table: CorpusTable::CSharpDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("nuget:morelinq.source.moreenumerable.assertcount@1.0.2"), table: CorpusTable::CSharpDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("nuget:morelinq.source.moreenumerable.batch@1.0.2"), table: CorpusTable::CSharpDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("nuget:morelinq.source.moreenumerable.generate@1.0.2"), table: CorpusTable::CSharpDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("nuget:morelinq.source.moreenumerable.generatebyindex@1.0.2"), table: CorpusTable::CSharpDriver, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Purl("nuget:tinyioc@1.3.0"), table: CorpusTable::CSharpDriver, origin: LOCKED },
];

#[rustfmt::skip]
pub(crate) const CLANG_CANDIDATES: [ManifestCandidate; 20] = [
    ManifestCandidate { coordinate: PackageCoordinate::Project("stb"), table: CorpusTable::ClangEvidence, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Project("sqlite-amalgamation"), table: CorpusTable::ClangEvidence, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Project("redis"), table: CorpusTable::ClangEvidence, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Project("lua"), table: CorpusTable::ClangEvidence, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Project("json-c"), table: CorpusTable::ClangEvidence, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Project("yaml-cpp"), table: CorpusTable::ClangEvidence, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Project("kilo"), table: CorpusTable::ClangEvidence, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Project("zlib"), table: CorpusTable::ClangEvidence, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Project("Vulkan-Headers"), table: CorpusTable::ClangEvidence, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Project("buck2-with-prelude"), table: CorpusTable::ClangEvidence, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Project("klib"), table: CorpusTable::ClangEvidence, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Project("miniaudio"), table: CorpusTable::ClangEvidence, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Project("vurtun-lib"), table: CorpusTable::ClangEvidence, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Project("rxi-map"), table: CorpusTable::ClangEvidence, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Project("q3vm"), table: CorpusTable::ClangEvidence, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Project("STC"), table: CorpusTable::ClangEvidence, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Project("pugixml"), table: CorpusTable::ClangEvidence, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Project("cJSON"), table: CorpusTable::ClangEvidence, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Project("nng"), table: CorpusTable::ClangEvidence, origin: LOCKED },
    ManifestCandidate { coordinate: PackageCoordinate::Project("Unity"), table: CorpusTable::ClangEvidence, origin: LOCKED },
];

/// Flattened, lane-major candidate manifest. Lane order is fixed by
/// `CorpusLanguage::ALL`.
pub(crate) const FLEET_CANDIDATE_LANES: [&[ManifestCandidate]; CorpusLanguage::ALL.len()] = [
    &RUST_CANDIDATES,
    &TYPESCRIPT_CANDIDATES,
    &PYTHON_CANDIDATES,
    &GO_CANDIDATES,
    &JAVA_CANDIDATES,
    &CSHARP_CANDIDATES,
    &CLANG_CANDIDATES,
];

/// Total number of candidate coordinates across every lane. This is the
/// capacity of the manifest, independent of any seed.
pub(crate) const fn manifest_candidate_count() -> usize {
    let mut total = 0;
    let mut lane = 0;
    while lane < FLEET_CANDIDATE_LANES.len() {
        total += FLEET_CANDIDATE_LANES[lane].len();
        lane += 1;
    }
    total
}

/// Number of rows the seeded selection yields: the requested target, capped by
/// the manifest capacity.
pub(crate) const fn selection_len() -> usize {
    let total = manifest_candidate_count();
    if total < FLEET_SELECTION_TARGET {
        total
    } else {
        FLEET_SELECTION_TARGET
    }
}

const fn gcd(mut a: usize, mut b: usize) -> usize {
    while b != 0 {
        let remainder = a % b;
        a = b;
        b = remainder;
    }
    a
}

/// A stride coprime to the manifest capacity, derived from the seed. Coprimality
/// makes the walk `offset + index * stride (mod capacity)` a bijection, so a
/// selection never repeats a row and never silently drops one.
pub(crate) const fn selection_stride(seed: u64) -> usize {
    let total = manifest_candidate_count();
    if total < 2 {
        return 1;
    }
    let mut stride = 1 + (seed as usize % (total - 1));
    while gcd(stride, total) != 1 {
        stride += 1;
        if stride >= total {
            stride = 1;
        }
    }
    stride
}

/// The start of the stride walk, mixed from the seed.
pub(crate) const fn selection_offset(seed: u64) -> usize {
    let total = manifest_candidate_count();
    if total == 0 {
        return 0;
    }
    let mut mixed = seed;
    mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    mixed ^= mixed >> 31;
    (mixed as usize) % total
}

const SELECTION_LEN: usize = selection_len();

const fn build_selection(seed: u64) -> [usize; SELECTION_LEN] {
    let total = manifest_candidate_count();
    let mut selected = [0_usize; SELECTION_LEN];
    if total == 0 {
        return selected;
    }
    let stride = selection_stride(seed);
    let offset = selection_offset(seed);
    let mut index = 0;
    while index < selected.len() {
        selected[index] = (offset + index * stride) % total;
        index += 1;
    }
    selected
}

/// The default seeded selection, fixed at compile time. `seed` and
/// `FLEET_MANIFEST_VERSION` together pin these exact flat manifest indices.
pub(crate) const FLEET_SELECTION: [usize; SELECTION_LEN] = build_selection(FLEET_SELECTION_SEED);

/// Locate a flat manifest index as `(lane_index, index_within_lane)`. The
/// selection builder only ever emits indices below the manifest capacity, so
/// this is total for every value in [`FLEET_SELECTION`].
pub(crate) fn candidate_at(flat: usize) -> Option<(usize, usize)> {
    let mut remaining = flat;
    for (lane, candidates) in FLEET_CANDIDATE_LANES.iter().enumerate() {
        if remaining < candidates.len() {
            return Some((lane, remaining));
        }
        remaining -= candidates.len();
    }
    None
}

/// Flat manifest indices selected for an arbitrary seed. Used to demonstrate
/// that the same seed reproduces a selection and a different seed does not.
pub(crate) fn selected_indices_for_seed(seed: u64) -> Vec<usize> {
    build_selection(seed).to_vec()
}

/// Canonical bytes of a seeded selection: the selected coordinates in order,
/// newline separated. Equal bytes prove an identical selection and order.
pub(crate) fn selection_bytes_for_seed(seed: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    for flat in selected_indices_for_seed(seed) {
        let (lane, local) =
            candidate_at(flat).expect("seeded selection index always addresses a manifest row");
        bytes.extend_from_slice(
            FLEET_CANDIDATE_LANES[lane][local]
                .coordinate
                .raw()
                .as_bytes(),
        );
        bytes.push(b'\n');
    }
    bytes
}

/// Candidate count per lane, in `CorpusLanguage::ALL` order. Reported by the
/// inventory invariant so a shortfall is visible, never fatal.
pub(crate) fn manifest_lane_counts() -> [usize; CorpusLanguage::ALL.len()] {
    let mut counts = [0_usize; CorpusLanguage::ALL.len()];
    for (lane, candidates) in FLEET_CANDIDATE_LANES.iter().enumerate() {
        counts[lane] = candidates.len();
    }
    counts
}

/// Candidate count by provenance (locked, committed, host-cache). Provenance
/// is retained and reported so a reviewer can audit how far the manifest has
/// grown beyond the original frozen tables.
pub(crate) fn manifest_origin_counts() -> [usize; 3] {
    let mut counts = [0_usize; 3];
    for candidates in FLEET_CANDIDATE_LANES.iter() {
        for candidate in candidates.iter() {
            let index = match candidate.origin {
                CandidateOrigin::LockedDriverTable => 0,
                CandidateOrigin::CommittedRepositoryEvidence => 1,
                CandidateOrigin::ProvisionedHostCache => 2,
            };
            counts[index] += 1;
        }
    }
    counts
}

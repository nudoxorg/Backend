//! Find-all-references coverage: measured snapshots per language lane.
//!
//! # What this harness measures
//!
//! Find-all-references is first-class from the semantic layer: every lowered
//! fragment carries an occurrence plane ([`backend_semantic::ir::Occurrence`],
//! [`backend_semantic::ir::OccurrenceTarget`]) and the owned semantic image
//! carries the projected link plane ([`backend_semantic::ir::Link`],
//! [`backend_semantic::ir::LinkOccurrence`]). The user-visible complaint is
//! that code-block references have "progressively gotten weaker". This harness
//! pins the honest current state, per language, per real corpus package, per
//! sampled declaration, so every future change moves a visible number.
//!
//! For each of the seven lanes the harness selects real corpus packages
//! (`NUDOX_{RUST,TYPESCRIPT,PYTHON,GO,JAVA,CSHARP,CLANG}_CORPUS_DIR`), stages
//! each audit-selected file exactly as
//! `crates/flow/tests/support/compiler_corpus/real.rs` does per lane,
//! compiles through the real authority to
//! [`backend_semantic::vocabulary::Stage::LowerIr`] with `compile_semantic`,
//! reopens the written fragment with
//! [`backend_semantic::ir::FragmentView::validate`], and measures:
//!
//! * sampled declarations: at least one enum variant (fallback: constant when
//!   the lane has no variant rows), one struct/class field, one method, one
//!   top-level function, plus one record and one enum type where present;
//! * `grep_total`: ground-truth identifier matches in the entry source
//!   (ASCII word-boundary scan of the exact bytes);
//! * `decl_est`: estimated declaration-name tokens among those matches (the
//!   first in-span match of every entity carrying the name; zero only where
//!   the lane attaches no entity span);
//! * `ir_local`: fragment occurrences whose target resolves exactly to the
//!   sampled entity (`OccurrenceTarget::Local`);
//! * `ir_foreign`: fragment occurrences still carrying an unresolved
//!   `OccurrenceTarget::Foreign` key that names the symbol (present but
//!   unresolved: linking gap);
//! * `site_ok` / `site_bad`: span-converted occurrence sites whose source
//!   bytes do / do not equal the identifier. Every lane now attaches entity
//!   source spans and emits absolute link sites, so site verification runs
//!   on all seven lanes; Go/Java attach owner spans to a subset of rows, so
//!   their `link_occ_src` trails `link_occ` — the pins make that visible;
//! * `decl_pos`: verified sites that sit on a declaration name token (the IR
//!   counting a declaration as a reference);
//! * `link_occ` / `link_occ_src`: owned-IR link occurrences into the entity,
//!   total and carrying an absolute source span.
//!
//! # Gap classification legend (pinned in row notes)
//!
//! * (a) frontend gap — the authority never reported the reference;
//! * (b) engine gap — the lowerer dropped a reported reference;
//! * (c) linking gap — occurrence present but target `Foreign`/unresolved;
//! * (d) measurement artifact — the harness cannot honestly observe it.
//!
//! # Process isolation
//!
//! Each lane runs in its own worker process (re-executing this binary), the
//! same pattern `compiler_corpus/real.rs` uses for the in-process libclang
//! FFI: one native fault kills one lane's typed outcome, never the run.
//!
//! # Baseline mode
//!
//! `NUDOX_REFS_BASELINE=1` prints, instead of asserting, the snapshot tables
//! as compilable Rust literals for re-pinning after an intentional change.

#![forbid(unsafe_code)]

use std::{
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use backend_engine::driver::{
    CompileControl, CompileOutput, CompileRequest, CompileScratch, DeclarationScope, NativeTool,
    ResolvedToolchain, SemanticAuthorityInput, ToolchainSelection, compile_semantic,
};
use backend_semantic::ir::{
    DecodedOccurrence, EntityId, EntityKind, FragmentView, Ir, LinkKind, LinkTarget,
    OccurrenceTarget, SemanticReader,
};
use backend_semantic::vocabulary::{
    CSharpVersion, CStandard, CxxStandard, GoVersion, JavaRelease, LanguageProfile, PythonVersion,
    RustEdition, Stage, TypeScriptSource,
};

// ---------------------------------------------------------------------------
// Snapshot tables — the pinned honest state. Every number is measured by this
// harness; a change in any of them is a change in find-all-references
// behavior. Row notes pin the audited gap classification (see legend above).
// ---------------------------------------------------------------------------

/// One pinned symbol measurement.
struct Snap {
    package: &'static str,
    role: &'static str,
    symbol: &'static str,
    grep_total: usize,
    decl_est: usize,
    ir_local: usize,
    ir_foreign: usize,
    ir_stable: usize,
    site_ok: usize,
    site_bad: usize,
    decl_pos: usize,
    link_occ: usize,
    link_occ_src: usize,
    /// Audited classification of the visible gap, with file:line anchors.
    note: &'static str,
}

/// One pinned package header (fragment-wide totals).
struct SnapPackage {
    package: &'static str,
    entities: usize,
    occurrences: usize,
    cross_file_entities: usize,
    cross_file_occurrences: usize,
    links: usize,
    link_occurrences: usize,
    link_occurrences_with_source: usize,
    note: &'static str,
}

static RUST_PACKAGES: &[SnapPackage] = &[
    SnapPackage {
        package: "log-0.4.22",
        entities: 290,
        occurrences: 637,
        cross_file_entities: 0,
        cross_file_occurrences: 0,
        links: 549,
        link_occurrences: 637,
        link_occurrences_with_source: 637,
        note: "Rust lane: rust-analyzer now attaches entity name extents (lower/rust.rs register -> attach_source_span) and every link occurrence carries an absolute source span; all sites verify against the entry bytes. Remaining: single-file fixture, so cross-file closure is zero by construction; LinkKind::Writes is never emitted (reads/calls/types only); unresolved paths stay OccurrenceTarget::Foreign (unresolved_reference_kind fallback) instead of resolving across files.",
    },
    SnapPackage {
        package: "once_cell-1.20.2",
        entities: 115,
        occurrences: 347,
        cross_file_entities: 0,
        cross_file_occurrences: 0,
        links: 286,
        link_occurrences: 347,
        link_occurrences_with_source: 347,
        note: "Rust lane: rust-analyzer now attaches entity name extents (lower/rust.rs register -> attach_source_span) and every link occurrence carries an absolute source span; all sites verify against the entry bytes. Remaining: single-file fixture, so cross-file closure is zero by construction; LinkKind::Writes is never emitted (reads/calls/types only); unresolved paths stay OccurrenceTarget::Foreign (unresolved_reference_kind fallback) instead of resolving across files.",
    },
    SnapPackage {
        package: "httpdate-1.0.3",
        entities: 73,
        occurrences: 470,
        cross_file_entities: 0,
        cross_file_occurrences: 0,
        links: 224,
        link_occurrences: 470,
        link_occurrences_with_source: 470,
        note: "Rust lane: rust-analyzer now attaches entity name extents (lower/rust.rs register -> attach_source_span) and every link occurrence carries an absolute source span; all sites verify against the entry bytes. Remaining: single-file fixture, so cross-file closure is zero by construction; LinkKind::Writes is never emitted (reads/calls/types only); unresolved paths stay OccurrenceTarget::Foreign (unresolved_reference_kind fallback) instead of resolving across files.",
    },
    SnapPackage {
        package: "aho-corasick-1.1.3",
        entities: 263,
        occurrences: 764,
        cross_file_entities: 0,
        cross_file_occurrences: 0,
        links: 592,
        link_occurrences: 764,
        link_occurrences_with_source: 764,
        note: "Rust lane: rust-analyzer now attaches entity name extents (lower/rust.rs register -> attach_source_span) and every link occurrence carries an absolute source span; all sites verify against the entry bytes. Remaining: single-file fixture, so cross-file closure is zero by construction; LinkKind::Writes is never emitted (reads/calls/types only); unresolved paths stay OccurrenceTarget::Foreign (unresolved_reference_kind fallback) instead of resolving across files.",
    },
    SnapPackage {
        package: "memchr-2.7.4",
        entities: 241,
        occurrences: 704,
        cross_file_entities: 0,
        cross_file_occurrences: 0,
        links: 537,
        link_occurrences: 704,
        link_occurrences_with_source: 704,
        note: "Rust lane: rust-analyzer now attaches entity name extents (lower/rust.rs register -> attach_source_span) and every link occurrence carries an absolute source span; all sites verify against the entry bytes. Remaining: single-file fixture, so cross-file closure is zero by construction; LinkKind::Writes is never emitted (reads/calls/types only); unresolved paths stay OccurrenceTarget::Foreign (unresolved_reference_kind fallback) instead of resolving across files.",
    },
];
static RUST_SYMBOLS: &[Snap] = &[
    Snap {
        package: "log-0.4.22",
        role: "variant",
        symbol: "Off",
        grep_total: 17,
        decl_est: 1,
        ir_local: 3,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 3,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 3,
        link_occ_src: 3,
        note: "",
    },
    Snap {
        package: "log-0.4.22",
        role: "field",
        symbol: "metadata",
        grep_total: 52,
        decl_est: 4,
        ir_local: 9,
        ir_foreign: 4,
        ir_stable: 0,
        site_ok: 3,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 3,
        link_occ_src: 3,
        note: "",
    },
    Snap {
        package: "log-0.4.22",
        role: "method",
        symbol: "from_usize",
        grep_total: 8,
        decl_est: 2,
        ir_local: 6,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 3,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 3,
        link_occ_src: 3,
        note: "Sampled once `parent_kind` resolved parents by entity id rather than by position in the canonical order.",
    },
    Snap {
        package: "log-0.4.22",
        role: "function",
        symbol: "set_max_level_racy",
        grep_total: 2,
        decl_est: 1,
        ir_local: 0,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 0,
        link_occ_src: 0,
        note: "Resampled: the lane's entity order differs from the prior pin, so the first method/function with two uses is now this one.",
    },
    Snap {
        package: "log-0.4.22",
        role: "record",
        symbol: "NopLogger",
        grep_total: 5,
        decl_est: 2,
        ir_local: 4,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 4,
        site_bad: 0,
        decl_pos: 1,
        link_occ: 4,
        link_occ_src: 4,
        note: "",
    },
    Snap {
        package: "log-0.4.22",
        role: "enum",
        symbol: "Level",
        grep_total: 93,
        decl_est: 6,
        ir_local: 18,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 18,
        site_bad: 0,
        decl_pos: 5,
        link_occ: 18,
        link_occ_src: 18,
        note: "",
    },
    Snap {
        package: "once_cell-1.20.2",
        role: "field",
        symbol: "init",
        grep_total: 24,
        decl_est: 1,
        ir_local: 3,
        ir_foreign: 2,
        ir_stable: 0,
        site_ok: 3,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 3,
        link_occ_src: 3,
        note: "",
    },
    Snap {
        package: "once_cell-1.20.2",
        role: "method",
        symbol: "set",
        grep_total: 28,
        decl_est: 1,
        ir_local: 1,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 1,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 1,
        link_occ_src: 1,
        note: "Sampled once `parent_kind` resolved parents by entity id rather than by position in the canonical order.",
    },
    Snap {
        package: "once_cell-1.20.2",
        role: "record",
        symbol: "OnceCell",
        grep_total: 156,
        decl_est: 1,
        ir_local: 16,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 16,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 16,
        link_occ_src: 16,
        note: "ce74f843e names an implementation row by its whole written self type, so `impl<'a, 'h> X<'a, 'h>` no longer counts as a declaration token of `X`.",
    },
    Snap {
        package: "once_cell-1.20.2",
        role: "enum",
        symbol: "Void",
        grep_total: 4,
        decl_est: 1,
        ir_local: 1,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 1,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 1,
        link_occ_src: 1,
        note: "",
    },
    Snap {
        package: "httpdate-1.0.3",
        role: "variant",
        symbol: "DAYS_PER_400Y",
        grep_total: 4,
        decl_est: 1,
        ir_local: 3,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 3,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 3,
        link_occ_src: 3,
        note: "",
    },
    Snap {
        package: "httpdate-1.0.3",
        role: "field",
        symbol: "sec",
        grep_total: 9,
        decl_est: 1,
        ir_local: 4,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 4,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 4,
        link_occ_src: 4,
        note: "",
    },
    Snap {
        package: "httpdate-1.0.3",
        role: "method",
        symbol: "from",
        grep_total: 12,
        decl_est: 2,
        ir_local: 4,
        ir_foreign: 5,
        ir_stable: 0,
        site_ok: 1,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 1,
        link_occ_src: 1,
        note: "Sampled once `parent_kind` resolved parents by entity id rather than by position in the canonical order.",
    },
    Snap {
        package: "httpdate-1.0.3",
        role: "function",
        symbol: "toint_1",
        grep_total: 2,
        decl_est: 1,
        ir_local: 1,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 1,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 1,
        link_occ_src: 1,
        note: "",
    },
    Snap {
        package: "httpdate-1.0.3",
        role: "record",
        symbol: "HttpDate",
        grep_total: 21,
        decl_est: 7,
        ir_local: 19,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 19,
        site_bad: 0,
        decl_pos: 6,
        link_occ: 19,
        link_occ_src: 19,
        note: "",
    },
    Snap {
        package: "aho-corasick-1.1.3",
        role: "variant",
        symbol: "NoncontiguousNFA",
        grep_total: 6,
        decl_est: 1,
        ir_local: 3,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 3,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 3,
        link_occ_src: 3,
        note: "Variant paths resolve only when the written path resolves; unresolved path spellings fall to unresolved_reference_kind and stay Foreign.",
    },
    Snap {
        package: "aho-corasick-1.1.3",
        role: "field",
        symbol: "kind",
        grep_total: 46,
        decl_est: 4,
        ir_local: 3,
        ir_foreign: 7,
        ir_stable: 0,
        site_ok: 2,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 2,
        link_occ_src: 2,
        note: "FieldAccess sites only (lower/rust.rs FieldAccess emission); 3 fragment rows named `kind` share the join, and macro/doc field mentions never become occurrences.",
    },
    Snap {
        package: "aho-corasick-1.1.3",
        role: "method",
        symbol: "match_len",
        grep_total: 2,
        decl_est: 1,
        ir_local: 0,
        ir_foreign: 1,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 0,
        link_occ_src: 0,
        note: "Sampled once `parent_kind` resolved parents by entity id rather than by position in the canonical order.",
    },
    Snap {
        package: "aho-corasick-1.1.3",
        role: "function",
        symbol: "enforce_anchored_consistency",
        grep_total: 12,
        decl_est: 1,
        ir_local: 11,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 11,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 11,
        link_occ_src: 11,
        note: "In-file free-function calls resolve Local at Oracle; every emitted site verifies on the name token.",
    },
    Snap {
        package: "aho-corasick-1.1.3",
        role: "record",
        symbol: "FindIter",
        grep_total: 6,
        decl_est: 1,
        ir_local: 4,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 4,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 4,
        link_occ_src: 4,
        note: "TypeReference paths only. ce74f843e names an implementation row by its whole written self type, so `impl<'a, 'h> X<'a, 'h>` no longer counts as a declaration token of `X`.",
    },
    Snap {
        package: "aho-corasick-1.1.3",
        role: "enum",
        symbol: "AhoCorasickKind",
        grep_total: 34,
        decl_est: 1,
        ir_local: 6,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 6,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 6,
        link_occ_src: 6,
        note: "TypeReference paths only; variant sites are counted on the Variant rows, not here.",
    },
    Snap {
        package: "memchr-2.7.4",
        role: "field",
        symbol: "avx2",
        grep_total: 68,
        decl_est: 3,
        ir_local: 7,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 2,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 2,
        link_occ_src: 2,
        note: "",
    },
    Snap {
        package: "memchr-2.7.4",
        role: "method",
        symbol: "is_available",
        grep_total: 6,
        decl_est: 3,
        ir_local: 3,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 1,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 1,
        link_occ_src: 1,
        note: "Sampled once `parent_kind` resolved parents by entity id rather than by position in the canonical order.",
    },
    Snap {
        package: "memchr-2.7.4",
        role: "record",
        symbol: "ThreeIter",
        grep_total: 6,
        decl_est: 1,
        ir_local: 5,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 5,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 5,
        link_occ_src: 5,
        note: "ce74f843e names an implementation row by its whole written self type, so `impl<'a, 'h> X<'a, 'h>` no longer counts as a declaration token of `X`.",
    },
];

static TYPESCRIPT_PACKAGES: &[SnapPackage] = &[
    SnapPackage {
        package: "tslib-2.8.1",
        entities: 226,
        occurrences: 75,
        cross_file_entities: 0,
        cross_file_occurrences: 0,
        links: 49,
        link_occurrences: 75,
        link_occurrences_with_source: 75,
        note: "TS lane: entity/occurrence planes unchanged from the prior pin; link occurrences now carry absolute source spans. Remaining: the occurrence plane records type and import references only (no property/member reads, no template-literal parts), so every sampled field/function symbol still resolves zero fragment occurrences despite in-file uses.",
    },
    SnapPackage {
        package: "chalk-5.3.0",
        entities: 85,
        occurrences: 22,
        cross_file_entities: 0,
        cross_file_occurrences: 0,
        links: 22,
        link_occurrences: 22,
        link_occurrences_with_source: 22,
        note: "TS lane: entity/occurrence planes unchanged from the prior pin; link occurrences now carry absolute source spans. Remaining: the occurrence plane records type and import references only (no property/member reads, no template-literal parts), so every sampled field/function symbol still resolves zero fragment occurrences despite in-file uses.",
    },
    SnapPackage {
        package: "uuid-11.0.3",
        entities: 20,
        occurrences: 2,
        cross_file_entities: 0,
        cross_file_occurrences: 0,
        links: 2,
        link_occurrences: 2,
        link_occurrences_with_source: 2,
        note: "TS lane: entity/occurrence planes unchanged from the prior pin; link occurrences now carry absolute source spans. Remaining: the occurrence plane records type and import references only (no property/member reads, no template-literal parts), so every sampled field/function symbol still resolves zero fragment occurrences despite in-file uses.",
    },
    SnapPackage {
        package: "is-stream-3.0.0",
        entities: 20,
        occurrences: 15,
        cross_file_entities: 0,
        cross_file_occurrences: 0,
        links: 15,
        link_occurrences: 15,
        link_occurrences_with_source: 15,
        note: "TS lane: entity/occurrence planes unchanged from the prior pin; link occurrences now carry absolute source spans. Remaining: the occurrence plane records type and import references only (no property/member reads, no template-literal parts), so every sampled field/function symbol still resolves zero fragment occurrences despite in-file uses.",
    },
    SnapPackage {
        package: "hono-4.6.12",
        entities: 1919,
        occurrences: 4814,
        cross_file_entities: 0,
        cross_file_occurrences: 0,
        links: 1718,
        link_occurrences: 4814,
        link_occurrences_with_source: 4814,
        note: "TS lane: entity/occurrence planes unchanged from the prior pin; link occurrences now carry absolute source spans. Remaining: the occurrence plane records type and import references only (no property/member reads, no template-literal parts), so every sampled field/function symbol still resolves zero fragment occurrences despite in-file uses.",
    },
];
static TYPESCRIPT_SYMBOLS: &[Snap] = &[
    Snap {
        package: "tslib-2.8.1",
        role: "field",
        symbol: "error",
        grep_total: 5,
        decl_est: 2,
        ir_local: 0,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 0,
        link_occ_src: 0,
        note: "",
    },
    Snap {
        package: "tslib-2.8.1",
        role: "function",
        symbol: "__spreadArray",
        grep_total: 3,
        decl_est: 1,
        ir_local: 0,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 0,
        link_occ_src: 0,
        note: "",
    },
    Snap {
        package: "chalk-5.3.0",
        role: "variant",
        symbol: "chalk",
        grep_total: 29,
        decl_est: 1,
        ir_local: 1,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 1,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 1,
        link_occ_src: 1,
        note: "",
    },
    Snap {
        package: "chalk-5.3.0",
        role: "field",
        symbol: "level",
        grep_total: 3,
        decl_est: 2,
        ir_local: 0,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 0,
        link_occ_src: 0,
        note: "",
    },
    Snap {
        package: "uuid-11.0.3",
        role: "variant",
        symbol: "TESTS",
        grep_total: 1,
        decl_est: 1,
        ir_local: 0,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 0,
        link_occ_src: 0,
        note: "",
    },
    Snap {
        package: "uuid-11.0.3",
        role: "field",
        symbol: "value",
        grep_total: 8,
        decl_est: 7,
        ir_local: 0,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 0,
        link_occ_src: 0,
        note: "",
    },
    Snap {
        package: "is-stream-3.0.0",
        role: "function",
        symbol: "isReadableStream",
        grep_total: 3,
        decl_est: 1,
        ir_local: 0,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 0,
        link_occ_src: 0,
        note: "",
    },
    Snap {
        package: "hono-4.6.12",
        role: "field",
        symbol: "outputFormat",
        grep_total: 11,
        decl_est: 8,
        ir_local: 0,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 0,
        link_occ_src: 0,
        note: "Sampled once `parent_kind` resolved parents by entity id rather than by position in the canonical order.",
    },
    Snap {
        package: "hono-4.6.12",
        role: "record",
        symbol: "FetchEventLike",
        grep_total: 1,
        decl_est: 1,
        ir_local: 0,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 0,
        link_occ_src: 0,
        note: "",
    },
];

static PYTHON_PACKAGES: &[SnapPackage] = &[
    // PIN MOVED (Python occurrence widening): every attribute call is now
    // recorded — `self`/`cls` receivers resolve through the enclosing class
    // and every other receiver stays an honestly foreign key — so the
    // call-site occurrence plane grew well beyond the old module-name-gated
    // population.
    SnapPackage {
        package: "click-8.2.1",
        entities: 723,
        occurrences: 356,
        cross_file_entities: 0,
        cross_file_occurrences: 0,
        links: 300,
        link_occurrences: 356,
        link_occurrences_with_source: 356,
        note: "Python lane: only call-site occurrences exist (FunctionCall/MethodCall, python.rs:1567-1690); the extractor records every call site (frontends/python/src/legacy/mod.rs:1219-1315): bare-name and module-gated calls keep their module-keyed resolution, a self/cls receiver resolves through the enclosing class, and every other receiver stays an honestly foreign key. Entity-plane spans are name extents, so fragment RelSpans (declaration-extent basis) cannot be joined to entity spans; link-plane absolute sites verify exactly.",
    },
    SnapPackage {
        package: "attrs-25.3.0",
        entities: 516,
        occurrences: 303,
        cross_file_entities: 0,
        cross_file_occurrences: 0,
        links: 201,
        link_occurrences: 303,
        link_occurrences_with_source: 303,
        note: "Python lane: only call-site occurrences exist (FunctionCall/MethodCall, python.rs:1567-1690); the extractor records every call site (frontends/python/src/legacy/mod.rs:1219-1315): bare-name and module-gated calls keep their module-keyed resolution, a self/cls receiver resolves through the enclosing class, and every other receiver stays an honestly foreign key. Entity-plane spans are name extents, so fragment RelSpans (declaration-extent basis) cannot be joined to entity spans; link-plane absolute sites verify exactly.",
    },
    SnapPackage {
        package: "jinja2-3.1.6",
        entities: 582,
        occurrences: 700,
        cross_file_entities: 0,
        cross_file_occurrences: 0,
        links: 375,
        link_occurrences: 700,
        link_occurrences_with_source: 700,
        note: "Python lane: only call-site occurrences exist (FunctionCall/MethodCall, python.rs:1567-1690); the extractor records every call site (frontends/python/src/legacy/mod.rs:1219-1315): bare-name and module-gated calls keep their module-keyed resolution, a self/cls receiver resolves through the enclosing class, and every other receiver stays an honestly foreign key. Entity-plane spans are name extents, so fragment RelSpans (declaration-extent basis) cannot be joined to entity spans; link-plane absolute sites verify exactly.",
    },
    SnapPackage {
        package: "markdown-it-py-3.0.0",
        entities: 129,
        occurrences: 41,
        cross_file_entities: 0,
        cross_file_occurrences: 0,
        links: 31,
        link_occurrences: 41,
        link_occurrences_with_source: 41,
        note: "Python lane: only call-site occurrences exist (FunctionCall/MethodCall, python.rs:1567-1690); the extractor records every call site (frontends/python/src/legacy/mod.rs:1219-1315): bare-name and module-gated calls keep their module-keyed resolution, a self/cls receiver resolves through the enclosing class, and every other receiver stays an honestly foreign key. Entity-plane spans are name extents, so fragment RelSpans (declaration-extent basis) cannot be joined to entity spans; link-plane absolute sites verify exactly.",
    },
    SnapPackage {
        package: "PyYAML-6.0.2",
        entities: 193,
        occurrences: 448,
        cross_file_entities: 0,
        cross_file_occurrences: 0,
        links: 289,
        link_occurrences: 448,
        link_occurrences_with_source: 448,
        note: "Python lane: only call-site occurrences exist (FunctionCall/MethodCall, python.rs:1567-1690); the extractor records every call site (frontends/python/src/legacy/mod.rs:1219-1315): bare-name and module-gated calls keep their module-keyed resolution, a self/cls receiver resolves through the enclosing class, and every other receiver stays an honestly foreign key. Entity-plane spans are name extents, so fragment RelSpans (declaration-extent basis) cannot be joined to entity spans; link-plane absolute sites verify exactly.",
    },
];
static PYTHON_SYMBOLS: &[Snap] = &[
    Snap {
        package: "click-8.2.1",
        role: "field",
        symbol: "param_type_name",
        grep_total: 8,
        decl_est: 3,
        ir_local: 0,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 0,
        link_occ_src: 0,
        note: "",
    },
    Snap {
        package: "click-8.2.1",
        role: "method",
        symbol: "to_info_dict",
        grep_total: 13,
        decl_est: 5,
        ir_local: 6,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 0,
        link_occ_src: 0,
        note: "Sampled once `parent_kind` resolved parents by entity id rather than by position in the canonical order.",
    },
    Snap {
        package: "click-8.2.1",
        role: "function",
        symbol: "augment_usage_errors",
        grep_total: 3,
        decl_est: 1,
        ir_local: 2,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 2,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 2,
        link_occ_src: 2,
        note: "",
    },
    Snap {
        package: "click-8.2.1",
        role: "record",
        symbol: "Option",
        grep_total: 6,
        decl_est: 1,
        ir_local: 0,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 0,
        link_occ_src: 0,
        note: "",
    },
    Snap {
        package: "attrs-25.3.0",
        role: "field",
        symbol: "__slots__",
        grep_total: 18,
        decl_est: 5,
        ir_local: 0,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 0,
        link_occ_src: 0,
        note: "",
    },
    Snap {
        package: "attrs-25.3.0",
        role: "method",
        symbol: "__setstate__",
        grep_total: 6,
        decl_est: 3,
        ir_local: 0,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 0,
        link_occ_src: 0,
        note: "Sampled once `parent_kind` resolved parents by entity id rather than by position in the canonical order.",
    },
    Snap {
        package: "attrs-25.3.0",
        role: "function",
        symbol: "_make_order",
        grep_total: 2,
        decl_est: 1,
        ir_local: 1,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 1,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 1,
        link_occ_src: 1,
        note: "",
    },
    Snap {
        package: "attrs-25.3.0",
        role: "record",
        symbol: "Factory",
        grep_total: 9,
        decl_est: 2,
        ir_local: 2,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 2,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 2,
        link_occ_src: 2,
        note: "",
    },
    Snap {
        package: "jinja2-3.1.6",
        role: "field",
        symbol: "_finalize",
        grep_total: 5,
        decl_est: 1,
        ir_local: 0,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 0,
        link_occ_src: 0,
        note: "",
    },
    Snap {
        package: "jinja2-3.1.6",
        role: "method",
        symbol: "signature",
        grep_total: 3,
        decl_est: 1,
        ir_local: 2,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 2,
        decl_pos: 0,
        link_occ: 2,
        link_occ_src: 2,
        note: "Sampled once `parent_kind` resolved parents by entity id rather than by position in the canonical order.",
    },
    Snap {
        package: "jinja2-3.1.6",
        role: "function",
        symbol: "optimizeconst",
        grep_total: 11,
        decl_est: 1,
        ir_local: 0,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 0,
        link_occ_src: 0,
        note: "",
    },
    Snap {
        package: "jinja2-3.1.6",
        role: "record",
        symbol: "Frame",
        grep_total: 81,
        decl_est: 1,
        ir_local: 4,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 4,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 4,
        link_occ_src: 4,
        note: "",
    },
    Snap {
        package: "markdown-it-py-3.0.0",
        role: "method",
        symbol: "validateLink",
        grep_total: 2,
        decl_est: 1,
        ir_local: 1,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 1,
        decl_pos: 0,
        link_occ: 1,
        link_occ_src: 1,
        note: "Sampled once `parent_kind` resolved parents by entity id rather than by position in the canonical order.",
    },
    Snap {
        package: "markdown-it-py-3.0.0",
        role: "record",
        symbol: "MarkdownIt",
        grep_total: 15,
        decl_est: 1,
        ir_local: 0,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 0,
        link_occ_src: 0,
        note: "",
    },
    Snap {
        package: "PyYAML-6.0.2",
        role: "field",
        symbol: "ESCAPE_REPLACEMENTS",
        grep_total: 3,
        decl_est: 1,
        ir_local: 0,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 0,
        link_occ_src: 0,
        note: "",
    },
    Snap {
        package: "PyYAML-6.0.2",
        role: "method",
        symbol: "scan_plain_spaces",
        grep_total: 2,
        decl_est: 1,
        ir_local: 1,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 1,
        decl_pos: 0,
        link_occ: 1,
        link_occ_src: 1,
        note: "Sampled once `parent_kind` resolved parents by entity id rather than by position in the canonical order.",
    },
    Snap {
        package: "PyYAML-6.0.2",
        role: "record",
        symbol: "ScannerError",
        grep_total: 34,
        decl_est: 1,
        ir_local: 31,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 31,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 31,
        link_occ_src: 31,
        note: "",
    },
];

static GO_PACKAGES: &[SnapPackage] = &[
    SnapPackage {
        package: "github.com/google/uuid@v1.6.0",
        entities: 251,
        occurrences: 440,
        cross_file_entities: 0,
        cross_file_occurrences: 0,
        links: 334,
        link_occurrences: 440,
        link_occurrences_with_source: 114,
        note: "Go lane: the v4 oracle records the full go/types Uses plane — package-scope variables/constants/functions/types, methods, struct fields, and import bindings, each with the identifier token's exact byte extent (frontends/go/src/legacy/oracle/main.go extractReferences). Bounded by construction: universe/builtin names, function-local objects, and multi-name value specs are never recorded; LinkKind::Writes is never emitted (the Uses table has no lvalue distinction, so assignment targets carry the read class); self-edges are dropped. Spans attach to declarations and method owners only, so a subset of link occurrences stays source-less.",
    },
    SnapPackage {
        package: "gopkg.in/yaml.v3@v3.0.1",
        entities: 1516,
        occurrences: 5855,
        cross_file_entities: 0,
        cross_file_occurrences: 0,
        links: 3285,
        link_occurrences: 5855,
        link_occurrences_with_source: 1482,
        note: "Go lane: the v4 oracle records the full go/types Uses plane — package-scope variables/constants/functions/types, methods, struct fields, and import bindings, each with the identifier token's exact byte extent (frontends/go/src/legacy/oracle/main.go extractReferences). Bounded by construction: universe/builtin names, function-local objects, and multi-name value specs are never recorded; LinkKind::Writes is never emitted (the Uses table has no lvalue distinction, so assignment targets carry the read class); self-edges are dropped. Spans attach to declarations and method owners only, so a subset of link occurrences stays source-less.",
    },
    SnapPackage {
        package: "github.com/rs/zerolog@v1.33.0",
        entities: 1266,
        occurrences: 2751,
        cross_file_entities: 0,
        cross_file_occurrences: 0,
        links: 1792,
        link_occurrences: 2751,
        link_occurrences_with_source: 617,
        note: "Go lane: the v4 oracle records the full go/types Uses plane — package-scope variables/constants/functions/types, methods, struct fields, and import bindings, each with the identifier token's exact byte extent (frontends/go/src/legacy/oracle/main.go extractReferences). Bounded by construction: universe/builtin names, function-local objects, and multi-name value specs are never recorded; LinkKind::Writes is never emitted (the Uses table has no lvalue distinction, so assignment targets carry the read class); self-edges are dropped. Spans attach to declarations and method owners only, so a subset of link occurrences stays source-less.",
    },
    SnapPackage {
        package: "github.com/BurntSushi/toml@v1.4.0",
        entities: 799,
        occurrences: 2654,
        cross_file_entities: 0,
        cross_file_occurrences: 0,
        links: 1845,
        link_occurrences: 2654,
        link_occurrences_with_source: 814,
        note: "Go lane: the v4 oracle records the full go/types Uses plane — package-scope variables/constants/functions/types, methods, struct fields, and import bindings, each with the identifier token's exact byte extent (frontends/go/src/legacy/oracle/main.go extractReferences). Bounded by construction: universe/builtin names, function-local objects, and multi-name value specs are never recorded; LinkKind::Writes is never emitted (the Uses table has no lvalue distinction, so assignment targets carry the read class); self-edges are dropped. Spans attach to declarations and method owners only, so a subset of link occurrences stays source-less.",
    },
    SnapPackage {
        package: "github.com/go-chi/chi/v5@v5.0.12",
        entities: 382,
        occurrences: 918,
        cross_file_entities: 0,
        cross_file_occurrences: 0,
        links: 575,
        link_occurrences: 918,
        link_occurrences_with_source: 449,
        note: "Go lane: the v4 oracle records the full go/types Uses plane — package-scope variables/constants/functions/types, methods, struct fields, and import bindings, each with the identifier token's exact byte extent (frontends/go/src/legacy/oracle/main.go extractReferences). Bounded by construction: universe/builtin names, function-local objects, and multi-name value specs are never recorded; LinkKind::Writes is never emitted (the Uses table has no lvalue distinction, so assignment targets carry the read class); self-edges are dropped. Spans attach to declarations and method owners only, so a subset of link occurrences stays source-less.",
    },
];
static GO_SYMBOLS: &[Snap] = &[
    Snap {
        package: "github.com/google/uuid@v1.6.0",
        role: "variant",
        symbol: "Invalid",
        grep_total: 4,
        decl_est: 1,
        ir_local: 1,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 1,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 1,
        link_occ_src: 1,
        note: "",
    },
    Snap {
        package: "github.com/google/uuid@v1.6.0",
        role: "field",
        symbol: "len",
        grep_total: 13,
        decl_est: 0,
        ir_local: 1,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 1,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 1,
        link_occ_src: 1,
        note: "",
    },
    Snap {
        package: "github.com/google/uuid@v1.6.0",
        role: "method",
        symbol: "Error",
        grep_total: 2,
        decl_est: 1,
        ir_local: 0,
        ir_foreign: 2,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 0,
        link_occ_src: 0,
        note: "Sampled once `parent_kind` resolved parents by entity id rather than by position in the canonical order.",
    },
    Snap {
        package: "github.com/google/uuid@v1.6.0",
        role: "function",
        symbol: "MustParse",
        grep_total: 2,
        decl_est: 1,
        ir_local: 0,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 0,
        link_occ_src: 0,
        note: "",
    },
    Snap {
        package: "github.com/google/uuid@v1.6.0",
        role: "record",
        symbol: "UUID",
        grep_total: 45,
        decl_est: 1,
        ir_local: 59,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 14,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 59,
        link_occ_src: 14,
        note: "",
    },
    Snap {
        package: "gopkg.in/yaml.v3@v3.0.1",
        role: "variant",
        symbol: "yaml_KEY_TOKEN",
        grep_total: 2,
        decl_est: 0,
        ir_local: 9,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 2,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 9,
        link_occ_src: 2,
        note: "",
    },
    Snap {
        package: "gopkg.in/yaml.v3@v3.0.1",
        role: "field",
        symbol: "prefix",
        grep_total: 9,
        decl_est: 0,
        ir_local: 12,
        ir_foreign: 4,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 11,
        link_occ_src: 0,
        note: "",
    },
    Snap {
        package: "gopkg.in/yaml.v3@v3.0.1",
        role: "method",
        symbol: "document",
        grep_total: 10,
        decl_est: 0,
        ir_local: 2,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 1,
        link_occ_src: 0,
        note: "Sampled once `parent_kind` resolved parents by entity id rather than by position in the canonical order.",
    },
    Snap {
        package: "gopkg.in/yaml.v3@v3.0.1",
        role: "function",
        symbol: "yaml_parser_scan_block_scalar",
        grep_total: 2,
        decl_est: 1,
        ir_local: 1,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 1,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 1,
        link_occ_src: 1,
        note: "",
    },
    Snap {
        package: "gopkg.in/yaml.v3@v3.0.1",
        role: "record",
        symbol: "parser",
        grep_total: 1071,
        decl_est: 0,
        ir_local: 38,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 38,
        link_occ_src: 0,
        note: "",
    },
    Snap {
        package: "github.com/rs/zerolog@v1.33.0",
        role: "variant",
        symbol: "Disabled",
        grep_total: 3,
        decl_est: 0,
        ir_local: 9,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 3,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 9,
        link_occ_src: 3,
        note: "",
    },
    Snap {
        package: "github.com/rs/zerolog@v1.33.0",
        role: "field",
        symbol: "done",
        grep_total: 3,
        decl_est: 0,
        ir_local: 3,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 2,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 3,
        link_occ_src: 2,
        note: "",
    },
    Snap {
        package: "github.com/rs/zerolog@v1.33.0",
        role: "method",
        symbol: "Stringers",
        grep_total: 2,
        decl_est: 1,
        ir_local: 0,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 0,
        link_occ_src: 0,
        note: "Sampled once `parent_kind` resolved parents by entity id rather than by position in the canonical order.",
    },
    Snap {
        package: "github.com/rs/zerolog@v1.33.0",
        role: "function",
        symbol: "newEvent",
        grep_total: 2,
        decl_est: 1,
        ir_local: 19,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 1,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 19,
        link_occ_src: 1,
        note: "",
    },
    Snap {
        package: "github.com/rs/zerolog@v1.33.0",
        role: "record",
        symbol: "Array",
        grep_total: 7,
        decl_est: 1,
        ir_local: 67,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 4,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 67,
        link_occ_src: 4,
        note: "",
    },
    Snap {
        package: "github.com/BurntSushi/toml@v1.4.0",
        role: "variant",
        symbol: "itemInteger",
        grep_total: 8,
        decl_est: 1,
        ir_local: 9,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 7,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 9,
        link_occ_src: 7,
        note: "",
    },
    Snap {
        package: "github.com/BurntSushi/toml@v1.4.0",
        role: "field",
        symbol: "nprev",
        grep_total: 5,
        decl_est: 0,
        ir_local: 4,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 4,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 4,
        link_occ_src: 4,
        note: "",
    },
    Snap {
        package: "github.com/BurntSushi/toml@v1.4.0",
        role: "method",
        symbol: "accept",
        grep_total: 18,
        decl_est: 1,
        ir_local: 16,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 16,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 16,
        link_occ_src: 16,
        note: "Sampled once `parent_kind` resolved parents by entity id rather than by position in the canonical order.",
    },
    Snap {
        package: "github.com/BurntSushi/toml@v1.4.0",
        role: "function",
        symbol: "lexTableEnd",
        grep_total: 2,
        decl_est: 1,
        ir_local: 1,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 1,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 1,
        link_occ_src: 1,
        note: "",
    },
    Snap {
        package: "github.com/BurntSushi/toml@v1.4.0",
        role: "record",
        symbol: "parser",
        grep_total: 3,
        decl_est: 0,
        ir_local: 31,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 31,
        link_occ_src: 0,
        note: "",
    },
    Snap {
        package: "github.com/go-chi/chi/v5@v5.0.12",
        role: "variant",
        symbol: "ntStatic",
        grep_total: 14,
        decl_est: 1,
        ir_local: 13,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 13,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 13,
        link_occ_src: 13,
        note: "",
    },
    Snap {
        package: "github.com/go-chi/chi/v5@v5.0.12",
        role: "field",
        symbol: "routeParams",
        grep_total: 23,
        decl_est: 0,
        ir_local: 31,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 23,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 31,
        link_occ_src: 23,
        note: "",
    },
    Snap {
        package: "github.com/go-chi/chi/v5@v5.0.12",
        role: "method",
        symbol: "Route",
        grep_total: 7,
        decl_est: 1,
        ir_local: 0,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 0,
        link_occ_src: 0,
        note: "Sampled once `parent_kind` resolved parents by entity id rather than by position in the canonical order.",
    },
    Snap {
        package: "github.com/go-chi/chi/v5@v5.0.12",
        role: "function",
        symbol: "chain",
        grep_total: 3,
        decl_est: 0,
        ir_local: 4,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 4,
        link_occ_src: 0,
        note: "",
    },
    Snap {
        package: "github.com/go-chi/chi/v5@v5.0.12",
        role: "record",
        symbol: "nodes",
        grep_total: 21,
        decl_est: 1,
        ir_local: 7,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 7,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 7,
        link_occ_src: 7,
        note: "",
    },
];

static JAVA_PACKAGES: &[SnapPackage] = &[
    SnapPackage {
        package: "maven:com.google.code.gson:gson@2.10.1",
        entities: 2482,
        occurrences: 8788,
        cross_file_entities: 0,
        cross_file_occurrences: 0,
        links: 6051,
        link_occurrences: 8788,
        link_occurrences_with_source: 759,
        note: "Java lane: the javac doclet now walks the full expression plane — method invocations, new-class expressions, bare identifiers, and assignments (frontends/java/src/legacy/doclet/AuthorityImage.java visitMethodInvocation/visitNewClass/visitIdentifier/visitAssignment) — and the lowerer maps each to its proper reference kind (calls/method/type/reads; no hard-coded FunctionCall). Remaining: LinkKind::Writes is never emitted (assignments keep the read class); spans attach to declaring members only, so most link occurrences stay source-less; cross-file closure is zero (whole-package image, single primary file).",
    },
    SnapPackage {
        package: "maven:org.apache.commons:commons-csv@1.10.0",
        entities: 853,
        occurrences: 2200,
        cross_file_entities: 0,
        cross_file_occurrences: 0,
        links: 1660,
        link_occurrences: 2200,
        link_occurrences_with_source: 1137,
        note: "Java lane: the javac doclet now walks the full expression plane — method invocations, new-class expressions, bare identifiers, and assignments (frontends/java/src/legacy/doclet/AuthorityImage.java visitMethodInvocation/visitNewClass/visitIdentifier/visitAssignment) — and the lowerer maps each to its proper reference kind (calls/method/type/reads; no hard-coded FunctionCall). Remaining: LinkKind::Writes is never emitted (assignments keep the read class); spans attach to declaring members only, so most link occurrences stay source-less; cross-file closure is zero (whole-package image, single primary file).",
    },
    SnapPackage {
        package: "maven:org.opentest4j:opentest4j@1.3.0",
        entities: 143,
        occurrences: 246,
        cross_file_entities: 0,
        cross_file_occurrences: 0,
        links: 200,
        link_occurrences: 246,
        link_occurrences_with_source: 64,
        note: "Java lane: the javac doclet now walks the full expression plane — method invocations, new-class expressions, bare identifiers, and assignments (frontends/java/src/legacy/doclet/AuthorityImage.java visitMethodInvocation/visitNewClass/visitIdentifier/visitAssignment) — and the lowerer maps each to its proper reference kind (calls/method/type/reads; no hard-coded FunctionCall). Remaining: LinkKind::Writes is never emitted (assignments keep the read class); spans attach to declaring members only, so most link occurrences stay source-less; cross-file closure is zero (whole-package image, single primary file).",
    },
    SnapPackage {
        package: "maven:org.ow2.asm:asm@9.6",
        entities: 2644,
        occurrences: 10000,
        cross_file_entities: 0,
        cross_file_occurrences: 0,
        links: 5181,
        link_occurrences: 10000,
        link_occurrences_with_source: 2421,
        note: "Java lane: the javac doclet now walks the full expression plane — method invocations, new-class expressions, bare identifiers, and assignments (frontends/java/src/legacy/doclet/AuthorityImage.java visitMethodInvocation/visitNewClass/visitIdentifier/visitAssignment) — and the lowerer maps each to its proper reference kind (calls/method/type/reads; no hard-coded FunctionCall). Remaining: LinkKind::Writes is never emitted (assignments keep the read class); spans attach to declaring members only, so most link occurrences stay source-less; cross-file closure is zero (whole-package image, single primary file).",
    },
];
static JAVA_SYMBOLS: &[Snap] = &[
    Snap {
        package: "maven:com.google.code.gson:gson@2.10.1",
        role: "variant",
        symbol: "LAZILY_PARSED_NUMBER",
        grep_total: 2,
        decl_est: 0,
        ir_local: 3,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 1,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 3,
        link_occ_src: 1,
        note: "",
    },
    Snap {
        package: "maven:com.google.code.gson:gson@2.10.1",
        role: "field",
        symbol: "constructorConstructor",
        grep_total: 7,
        decl_est: 1,
        ir_local: 15,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 2,
        link_occ_src: 0,
        note: "",
    },
    Snap {
        package: "maven:com.google.code.gson:gson@2.10.1",
        role: "method",
        symbol: "serialize",
        grep_total: 8,
        decl_est: 0,
        ir_local: 2,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 1,
        link_occ_src: 0,
        note: "",
    },
    Snap {
        package: "maven:com.google.code.gson:gson@2.10.1",
        role: "record",
        symbol: "com.google.gson.reflect.TypeToken",
        grep_total: 4,
        decl_est: 0,
        ir_local: 71,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 18,
        decl_pos: 0,
        link_occ: 71,
        link_occ_src: 18,
        note: "Sites carry the simple name spelling (`TypeToken`), never the pinned qualified name, so the whole-class row counts them site_bad by construction.",
    },
    Snap {
        package: "maven:com.google.code.gson:gson@2.10.1",
        role: "enum",
        symbol: "com.google.gson.LongSerializationPolicy",
        grep_total: 0,
        decl_est: 0,
        ir_local: 8,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 5,
        decl_pos: 0,
        link_occ: 8,
        link_occ_src: 5,
        note: "Same qualified-name vs simple-name site spelling as the TypeToken row.",
    },
    Snap {
        package: "maven:org.apache.commons:commons-csv@1.10.0",
        role: "variant",
        symbol: "NONE",
        grep_total: 4,
        decl_est: 0,
        ir_local: 3,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 3,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 3,
        link_occ_src: 3,
        note: "",
    },
    Snap {
        package: "maven:org.apache.commons:commons-csv@1.10.0",
        role: "field",
        symbol: "BACKSLASH",
        grep_total: 5,
        decl_est: 0,
        ir_local: 4,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 4,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 4,
        link_occ_src: 4,
        note: "",
    },
    Snap {
        package: "maven:org.apache.commons:commons-csv@1.10.0",
        role: "method",
        symbol: "println",
        grep_total: 3,
        decl_est: 1,
        ir_local: 8,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 6,
        link_occ_src: 0,
        note: "Sampled once `parent_kind` resolved parents by entity id rather than by position in the canonical order.",
    },
    Snap {
        package: "maven:org.apache.commons:commons-csv@1.10.0",
        role: "record",
        symbol: "org.apache.commons.csv.Constants",
        grep_total: 11,
        decl_est: 0,
        ir_local: 7,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 3,
        decl_pos: 0,
        link_occ: 7,
        link_occ_src: 3,
        note: "Qualified-name vs simple-name site spelling.",
    },
    Snap {
        package: "maven:org.apache.commons:commons-csv@1.10.0",
        role: "enum",
        symbol: "org.apache.commons.csv.CSVFormat.Predefined",
        grep_total: 0,
        decl_est: 0,
        ir_local: 1,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 1,
        decl_pos: 0,
        link_occ: 1,
        link_occ_src: 1,
        note: "Qualified-name vs simple-name site spelling.",
    },
    Snap {
        package: "maven:org.opentest4j:opentest4j@1.3.0",
        role: "field",
        symbol: "stringRepresentation",
        grep_total: 16,
        decl_est: 1,
        ir_local: 4,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 4,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 4,
        link_occ_src: 4,
        note: "",
    },
    Snap {
        package: "maven:org.opentest4j:opentest4j@1.3.0",
        role: "method",
        symbol: "getIdentityHashCode",
        grep_total: 2,
        decl_est: 1,
        ir_local: 0,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 0,
        link_occ_src: 0,
        note: "Sampled once `parent_kind` resolved parents by entity id rather than by position in the canonical order.",
    },
    Snap {
        package: "maven:org.opentest4j:opentest4j@1.3.0",
        role: "record",
        symbol: "org.opentest4j.ValueWrapper",
        grep_total: 0,
        decl_est: 0,
        ir_local: 16,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 8,
        decl_pos: 0,
        link_occ: 16,
        link_occ_src: 8,
        note: "Qualified-name vs simple-name site spelling.",
    },
    Snap {
        package: "maven:org.ow2.asm:asm@9.6",
        role: "field",
        symbol: "ALOAD_3",
        grep_total: 2,
        decl_est: 0,
        ir_local: 2,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 2,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 2,
        link_occ_src: 2,
        note: "",
    },
    Snap {
        package: "maven:org.ow2.asm:asm@9.6",
        role: "method",
        symbol: "readTypeAnnotations",
        grep_total: 3,
        decl_est: 1,
        ir_local: 2,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 2,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 2,
        link_occ_src: 2,
        note: "Sampled once `parent_kind` resolved parents by entity id rather than by position in the canonical order.",
    },
    Snap {
        package: "maven:org.ow2.asm:asm@9.6",
        role: "record",
        symbol: "org.objectweb.asm.FieldWriter",
        grep_total: 0,
        decl_est: 0,
        ir_local: 8,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 8,
        link_occ_src: 0,
        note: "",
    },
];

static CSHARP_PACKAGES: &[SnapPackage] = &[
    SnapPackage {
        package: "morelinq.source.moreenumerable.distinctby",
        entities: 16,
        occurrences: 11,
        cross_file_entities: 0,
        cross_file_occurrences: 0,
        links: 10,
        link_occurrences: 11,
        link_occurrences_with_source: 11,
        note: "C# lane: Roslyn oracle over the single bound source. Bare-identifier field reads are now recorded (ReferenceTag::FieldRead, lower/csharp.rs FieldRead -> VariableUse); the write half is still unemitted (LinkKind::Writes stays 0). todelimitedstring 1.1.2, previously a per-run IndexCapacity{phase:Reference} terminal, now lowers and measures. Fragment entity lane and owned image reorder rows (same multiset, different order): the harness joins occurrences by (name, kind). Single-file staging: uses of MoreEnumerable live in sibling corpus files this row never carries, so all sampled uses stay foreign/unresolved.",
    },
    SnapPackage {
        package: "morelinq.source.moreenumerable.todelimitedstring",
        entities: 115,
        occurrences: 88,
        cross_file_entities: 0,
        cross_file_occurrences: 0,
        links: 88,
        link_occurrences: 88,
        link_occurrences_with_source: 88,
        note: "C# lane: Roslyn oracle over the single bound source. Bare-identifier field reads are now recorded (ReferenceTag::FieldRead, lower/csharp.rs FieldRead -> VariableUse); the write half is still unemitted (LinkKind::Writes stays 0). todelimitedstring 1.1.2, previously a per-run IndexCapacity{phase:Reference} terminal, now lowers and measures. Fragment entity lane and owned image reorder rows (same multiset, different order): the harness joins occurrences by (name, kind). Single-file staging: uses of MoreEnumerable live in sibling corpus files this row never carries, so all sampled uses stay foreign/unresolved.",
    },
    SnapPackage {
        package: "morelinq.source.moreenumerable.batch",
        entities: 16,
        occurrences: 21,
        cross_file_entities: 0,
        cross_file_occurrences: 0,
        links: 15,
        link_occurrences: 21,
        link_occurrences_with_source: 21,
        note: "C# lane: Roslyn oracle over the single bound source. Bare-identifier field reads are now recorded (ReferenceTag::FieldRead, lower/csharp.rs FieldRead -> VariableUse); the write half is still unemitted (LinkKind::Writes stays 0). todelimitedstring 1.1.2, previously a per-run IndexCapacity{phase:Reference} terminal, now lowers and measures. Fragment entity lane and owned image reorder rows (same multiset, different order): the harness joins occurrences by (name, kind). Single-file staging: uses of MoreEnumerable live in sibling corpus files this row never carries, so all sampled uses stay foreign/unresolved.",
    },
    SnapPackage {
        package: "morelinq.source.moreenumerable.orderedmerge",
        entities: 62,
        occurrences: 68,
        cross_file_entities: 0,
        cross_file_occurrences: 0,
        links: 29,
        link_occurrences: 68,
        link_occurrences_with_source: 68,
        note: "C# lane: Roslyn oracle over the single bound source. Bare-identifier field reads are now recorded (ReferenceTag::FieldRead, lower/csharp.rs FieldRead -> VariableUse); the write half is still unemitted (LinkKind::Writes stays 0). todelimitedstring 1.1.2, previously a per-run IndexCapacity{phase:Reference} terminal, now lowers and measures. Fragment entity lane and owned image reorder rows (same multiset, different order): the harness joins occurrences by (name, kind). Single-file staging: uses of MoreEnumerable live in sibling corpus files this row never carries, so all sampled uses stay foreign/unresolved.",
    },
    SnapPackage {
        package: "morelinq.source.moreenumerable.split",
        entities: 79,
        occurrences: 59,
        cross_file_entities: 0,
        cross_file_occurrences: 0,
        links: 42,
        link_occurrences: 59,
        link_occurrences_with_source: 59,
        note: "C# lane: Roslyn oracle over the single bound source. Bare-identifier field reads are now recorded (ReferenceTag::FieldRead, lower/csharp.rs FieldRead -> VariableUse); the write half is still unemitted (LinkKind::Writes stays 0). todelimitedstring 1.1.2, previously a per-run IndexCapacity{phase:Reference} terminal, now lowers and measures. Fragment entity lane and owned image reorder rows (same multiset, different order): the harness joins occurrences by (name, kind). Single-file staging: uses of MoreEnumerable live in sibling corpus files this row never carries, so all sampled uses stay foreign/unresolved.",
    },
];
static CSHARP_SYMBOLS: &[Snap] = &[
    Snap {
        package: "morelinq.source.moreenumerable.distinctby",
        role: "method",
        symbol: "DistinctByImpl",
        grep_total: 2,
        decl_est: 1,
        ir_local: 0,
        ir_foreign: 1,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 0,
        link_occ_src: 0,
        note: "Sampled once `parent_kind` resolved parents by entity id rather than by position in the canonical order.",
    },
    Snap {
        package: "morelinq.source.moreenumerable.distinctby",
        role: "record",
        symbol: "MoreEnumerable",
        grep_total: 1,
        decl_est: 1,
        ir_local: 0,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 0,
        link_occ_src: 0,
        note: "The partial MoreEnumerable class declares the method; all uses live in sibling corpus files that this single-file staging never carries.",
    },
    Snap {
        package: "morelinq.source.moreenumerable.todelimitedstring",
        role: "field",
        symbol: "Int16",
        grep_total: 2,
        decl_est: 1,
        ir_local: 1,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 1,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 1,
        link_occ_src: 1,
        note: "",
    },
    Snap {
        package: "morelinq.source.moreenumerable.todelimitedstring",
        role: "method",
        symbol: "ToDelimitedString",
        grep_total: 42,
        decl_est: 28,
        ir_local: 14,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 1,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 1,
        link_occ_src: 1,
        note: "Sampled once `parent_kind` resolved parents by entity id rather than by position in the canonical order.",
    },
    Snap {
        package: "morelinq.source.moreenumerable.todelimitedstring",
        role: "record",
        symbol: "MoreEnumerable",
        grep_total: 2,
        decl_est: 1,
        ir_local: 0,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 0,
        link_occ_src: 0,
        note: "",
    },
    Snap {
        package: "morelinq.source.moreenumerable.batch",
        role: "method",
        symbol: "BatchImpl",
        grep_total: 2,
        decl_est: 1,
        ir_local: 0,
        ir_foreign: 1,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 0,
        link_occ_src: 0,
        note: "Sampled once `parent_kind` resolved parents by entity id rather than by position in the canonical order.",
    },
    Snap {
        package: "morelinq.source.moreenumerable.batch",
        role: "record",
        symbol: "MoreEnumerable",
        grep_total: 1,
        decl_est: 1,
        ir_local: 0,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 0,
        link_occ_src: 0,
        note: "",
    },
    Snap {
        package: "morelinq.source.moreenumerable.orderedmerge",
        role: "method",
        symbol: "OrderedMerge",
        grep_total: 13,
        decl_est: 7,
        ir_local: 0,
        ir_foreign: 6,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 0,
        link_occ_src: 0,
        note: "Sampled once `parent_kind` resolved parents by entity id rather than by position in the canonical order.",
    },
    Snap {
        package: "morelinq.source.moreenumerable.orderedmerge",
        role: "record",
        symbol: "MoreEnumerable",
        grep_total: 1,
        decl_est: 1,
        ir_local: 0,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 0,
        link_occ_src: 0,
        note: "",
    },
    Snap {
        package: "morelinq.source.moreenumerable.split",
        role: "method",
        symbol: "SplitImpl",
        grep_total: 4,
        decl_est: 2,
        ir_local: 0,
        ir_foreign: 2,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 0,
        link_occ_src: 0,
        note: "Sampled once `parent_kind` resolved parents by entity id rather than by position in the canonical order.",
    },
    Snap {
        package: "morelinq.source.moreenumerable.split",
        role: "record",
        symbol: "MoreEnumerable",
        grep_total: 1,
        decl_est: 1,
        ir_local: 0,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 0,
        link_occ_src: 0,
        note: "",
    },
];

// The pinned Nix Linux libclang and the macOS Clang authority produce
// different, repeatable reference planes over the same corpus sources. Linux
// values below were identical in independent PR builds 2195 and 2205; keep
// both baselines exact instead of weakening the assertion with a range.
const CLANG_LINUX: bool = cfg!(target_os = "linux");
static CLANG_PACKAGES: &[SnapPackage] = &[
    SnapPackage {
        package: "pugixml",
        entities: 2585,
        occurrences: if CLANG_LINUX { 3511 } else { 3509 },
        cross_file_entities: 0,
        cross_file_occurrences: 0,
        links: if CLANG_LINUX { 2144 } else { 2142 },
        link_occurrences: if CLANG_LINUX { 3511 } else { 3509 },
        link_occurrences_with_source: if CLANG_LINUX { 3511 } else { 3509 },
        note: "Clang lane: name-token extents replaced whole-CallExpr extents, so most sites verify on the identifier; includes now surface as Import link occurrences and cross-file references travel as Stable targets instead of being dropped. Remaining: header-hosted declarations stay out of the main-file image (no cross-file closure — cross_file_* stay 0), macro-generated sites keep expression extents, and the pugixml row's occurrence/link totals sit slightly under the prior pin (-30/-19) because include-file fact rows moved out of the main-file planes while the new import/override planes added less back.",
    },
    SnapPackage {
        package: "json-c",
        entities: 408,
        occurrences: if CLANG_LINUX { 717 } else { 713 },
        cross_file_entities: 0,
        cross_file_occurrences: 0,
        links: if CLANG_LINUX { 505 } else { 512 },
        link_occurrences: if CLANG_LINUX { 717 } else { 713 },
        link_occurrences_with_source: if CLANG_LINUX { 717 } else { 713 },
        note: "Clang lane: name-token extents replaced whole-CallExpr extents, so most sites verify on the identifier; includes now surface as Import link occurrences and cross-file references travel as Stable targets instead of being dropped. Remaining: header-hosted declarations stay out of the main-file image (no cross-file closure — cross_file_* stay 0) and macro-generated sites keep expression extents.",
    },
    SnapPackage {
        package: "inih",
        entities: 51,
        occurrences: if CLANG_LINUX { 17 } else { 16 },
        cross_file_entities: 0,
        cross_file_occurrences: 0,
        links: if CLANG_LINUX { 14 } else { 13 },
        link_occurrences: if CLANG_LINUX { 17 } else { 16 },
        link_occurrences_with_source: if CLANG_LINUX { 17 } else { 16 },
        note: "Clang lane: name-token extents replaced whole-CallExpr extents, so most sites verify on the identifier; includes now surface as Import link occurrences and cross-file references travel as Stable targets instead of being dropped. Remaining: header-hosted declarations stay out of the main-file image (no cross-file closure — cross_file_* stay 0) and macro-generated sites keep expression extents.",
    },
    SnapPackage {
        package: "cJSON",
        entities: 456,
        occurrences: 591,
        cross_file_entities: 0,
        cross_file_occurrences: 0,
        links: 249,
        link_occurrences: 591,
        link_occurrences_with_source: 591,
        note: "Clang lane: name-token extents replaced whole-CallExpr extents, so most sites verify on the identifier; includes now surface as Import link occurrences and cross-file references travel as Stable targets instead of being dropped. Remaining: header-hosted declarations stay out of the main-file image (no cross-file closure — cross_file_* stay 0) and macro-generated sites keep expression extents (the `content` row's site_bad is the macro-expanded `buffer_at_offset(...)` spelling class).",
    },
    SnapPackage {
        package: "lua",
        entities: 218,
        occurrences: 145,
        cross_file_entities: 0,
        cross_file_occurrences: 0,
        links: 102,
        link_occurrences: 145,
        link_occurrences_with_source: 145,
        note: "Clang lane: name-token extents replaced whole-CallExpr extents, so most sites verify on the identifier; includes now surface as Import link occurrences and cross-file references travel as Stable targets instead of being dropped. Remaining: header-hosted declarations stay out of the main-file image (no cross-file closure — cross_file_* stay 0) and macro-generated sites keep expression extents.",
    },
];
static CLANG_SYMBOLS: &[Snap] = &[
    Snap {
        package: "pugixml",
        role: "variant",
        symbol: "ctx_start_symbol",
        grep_total: 3,
        decl_est: 1,
        ir_local: 0,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 0,
        link_occ_src: 0,
        note: "",
    },
    Snap {
        package: "pugixml",
        role: "field",
        symbol: "data",
        grep_total: 186,
        decl_est: 23,
        ir_local: 5,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 0,
        link_occ_src: 0,
        note: "",
    },
    Snap {
        package: "pugixml",
        role: "method",
        symbol: "parse_function",
        grep_total: 2,
        decl_est: 1,
        ir_local: 0,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 0,
        link_occ_src: 0,
        note: "Sampled once `parent_kind` resolved parents by entity id rather than by position in the canonical order.",
    },
    Snap {
        package: "pugixml",
        role: "function",
        symbol: "name",
        grep_total: 226,
        decl_est: 27,
        ir_local: 6,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 2,
        site_bad: 2,
        decl_pos: 0,
        link_occ: 4,
        link_occ_src: 4,
        note: "The two site_bad rows are member-access receiver spellings (`_wrap.name()`, `node.node().name()`) whose recorded extent covers the call expression, not the bare callee token.",
    },
    Snap {
        package: "pugixml",
        role: "record",
        symbol: "strconv_attribute_impl",
        grep_total: 17,
        decl_est: 1,
        ir_local: 0,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 0,
        link_occ_src: 0,
        note: "",
    },
    Snap {
        package: "pugixml",
        role: "enum",
        symbol: "axis_t",
        grep_total: 13,
        decl_est: 1,
        ir_local: 0,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 0,
        link_occ_src: 0,
        note: "",
    },
    Snap {
        package: "json-c",
        role: "function",
        symbol: "json_object_new_array",
        grep_total: 2,
        decl_est: 1,
        ir_local: 2,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 2,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 2,
        link_occ_src: 2,
        note: "",
    },
    Snap {
        package: "inih",
        role: "field",
        symbol: "num_left",
        grep_total: 4,
        decl_est: 1,
        ir_local: 0,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 0,
        link_occ_src: 0,
        note: "",
    },
    Snap {
        package: "inih",
        role: "function",
        symbol: "ini_reader_string",
        grep_total: 2,
        decl_est: 1,
        ir_local: 0,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 0,
        link_occ_src: 0,
        note: "",
    },
    Snap {
        package: "cJSON",
        role: "field",
        symbol: "content",
        grep_total: 12,
        decl_est: 1,
        ir_local: 14,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 3,
        site_bad: 11,
        decl_pos: 0,
        link_occ: 14,
        link_occ_src: 14,
        note: "site_bad rows are macro-expanded accesses (`buffer_at_offset(buffer)` spellings) whose recorded extent is the whole expanded expression.",
    },
    Snap {
        package: "cJSON",
        role: "function",
        symbol: "cJSON_ReplaceItemViaPointer",
        grep_total: 3,
        decl_est: 1,
        ir_local: 4,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 4,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 4,
        link_occ_src: 4,
        note: "",
    },
    Snap {
        package: "cJSON",
        role: "record",
        symbol: "internal_hooks",
        grep_total: 10,
        decl_est: 1,
        ir_local: 0,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 0,
        link_occ_src: 0,
        note: "",
    },
    Snap {
        package: "lua",
        role: "function",
        symbol: "LEintfloat",
        grep_total: 2,
        decl_est: 1,
        ir_local: 0,
        ir_foreign: 0,
        ir_stable: 0,
        site_ok: 0,
        site_bad: 0,
        decl_pos: 0,
        link_occ: 0,
        link_occ_src: 0,
        note: "",
    },
];

// ---------------------------------------------------------------------------
// Lane selection tables (the audit-selected packages and files).
// ---------------------------------------------------------------------------

const RUST_CRATES: &[&str] = &[
    "log-0.4.22",
    "once_cell-1.20.2",
    "httpdate-1.0.3",
    "aho-corasick-1.1.3",
    "memchr-2.7.4",
];
// Withdrawn selections: bitflags-2.6.0 (its synthetic-fixture lowering flips
// run to run — 7 vs 11 entities for identical bytes, rust-analyzer
// macro-expansion nondeterminism on `bitflags!` output; finding F15) and
// hashbrown-0.14.5 (largest file terminates with RustGenericParameter).

const TYPESCRIPT_PACKAGES_SEL: &[&str] = &[
    "tslib-2.8.1",
    "chalk-5.3.0",
    "uuid-11.0.3",
    "is-stream-3.0.0",
    "hono-4.6.12",
];

const PYTHON_PACKAGES_SEL: &[&str] = &[
    "click-8.2.1",
    "attrs-25.3.0",
    "jinja2-3.1.6",
    "markdown-it-py-3.0.0",
    "PyYAML-6.0.2",
];

const GO_MODULES: &[&str] = &[
    "github.com/google/uuid@v1.6.0",
    "gopkg.in/yaml.v3@v3.0.1",
    "github.com/rs/zerolog@v1.33.0",
    "github.com/BurntSushi/toml@v1.4.0",
    "github.com/go-chi/chi/v5@v5.0.12",
];

/// Maven coordinates; corpus root layout `group/…/artifact/version`.
const JAVA_COORDINATES: &[&str] = &[
    "maven:com.google.code.gson:gson@2.10.1",
    "maven:org.apache.commons:commons-csv@1.10.0",
    "maven:org.opentest4j:opentest4j@1.3.0",
    "maven:org.ow2.asm:asm@9.6",
];

/// C# corpus rows: package, version, archive-relative source path.
///
/// `groupadjacent` 1.0.1 was measured and then withdrawn from this table: it
/// lowers cleanly on a solo lane but flips to `DuplicateDeclarationIdentity`
/// under the full parallel suite — the same arity pair
/// `Grouping`/`Grouping<TKey, TElement>` that
/// `csharp_identity_regressions.rs` pins, exposed to run-to-run authority
/// image ordering. The row's instability is finding F14 in the audit report;
/// the snapshot pins only rows whose authority output is run-stable.
const CSHARP_ROWS: &[(&str, &str, &str)] = &[
    (
        "morelinq.source.moreenumerable.distinctby",
        "1.0.2",
        "content/net35/MoreLinq/MoreEnumerable.DistinctBy.cs",
    ),
    (
        "morelinq.source.moreenumerable.todelimitedstring",
        "1.1.2",
        "content/net20/MoreLinq/MoreEnumerable.ToDelimitedString.g.cs",
    ),
    (
        "morelinq.source.moreenumerable.batch",
        "1.0.2",
        "content/net20/MoreLinq/MoreEnumerable.Batch.cs",
    ),
    (
        "morelinq.source.moreenumerable.orderedmerge",
        "1.0.1",
        "content/net20/MoreLinq/MoreEnumerable.OrderedMerge.cs",
    ),
    (
        "morelinq.source.moreenumerable.split",
        "1.0.2",
        "content/net20/MoreLinq/MoreEnumerable.Split.cs",
    ),
];

/// Clang rows: package, entry file relative to the package root, C++ flag.
const CLANG_ROWS: &[(&str, &str, bool)] = &[
    ("pugixml", "src/pugixml.cpp", true),
    ("json-c", "json_object.c", false),
    ("inih", "ini.c", false),
    ("cJSON", "cJSON.c", false),
    ("lua", "onelua.c", false),
];

// ---------------------------------------------------------------------------
// Outcome records (worker -> parent transport).
// ---------------------------------------------------------------------------

#[derive(Default)]
struct PackageOutcome {
    package: String,
    entities: usize,
    occurrences: usize,
    cross_file_entities: usize,
    cross_file_occurrences: usize,
    links: usize,
    link_occurrences: usize,
    link_occurrences_with_source: usize,
    notes: Vec<String>,
    symbols: Vec<SymbolOutcome>,
}

struct SymbolOutcome {
    role: String,
    name: String,
    kind: EntityKind,
    grep_total: usize,
    decl_est: usize,
    ir_local: usize,
    ir_foreign: usize,
    ir_stable: usize,
    site_ok: usize,
    site_bad: usize,
    decl_pos: usize,
    link_occ: usize,
    link_occ_src: usize,
    notes: Vec<String>,
}

impl PackageOutcome {
    fn write(&self, out: &mut String) {
        out.push_str(&format!(
            "P {} {} {} {} {} {} {} {}\n",
            self.package,
            self.entities,
            self.occurrences,
            self.cross_file_entities,
            self.cross_file_occurrences,
            self.links,
            self.link_occurrences,
            self.link_occurrences_with_source,
        ));
        for note in &self.notes {
            out.push_str(&format!("PN {}\n", escape_line(note)));
        }
        for symbol in &self.symbols {
            out.push_str(&format!(
                "S {} {} {} {:?} {} {} {} {} {} {} {} {} {} {}\n",
                self.package,
                symbol.role,
                symbol.name,
                symbol.kind,
                symbol.grep_total,
                symbol.decl_est,
                symbol.ir_local,
                symbol.ir_foreign,
                symbol.ir_stable,
                symbol.site_ok,
                symbol.site_bad,
                symbol.decl_pos,
                symbol.link_occ,
                symbol.link_occ_src,
            ));
            for note in &symbol.notes {
                out.push_str(&format!("SN {}\n", escape_line(note)));
            }
        }
    }
}

fn escape_line(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            other => escaped.push(other),
        }
    }
    escaped
}

fn unescape_line(text: &str) -> String {
    let mut unescaped = String::new();
    let mut characters = text.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            unescaped.push(character);
            continue;
        }
        match characters.next() {
            Some('n') => unescaped.push('\n'),
            Some('r') => unescaped.push('\r'),
            Some('\\') => unescaped.push('\\'),
            Some(other) => {
                unescaped.push('\\');
                unescaped.push(other);
            }
            None => unescaped.push('\\'),
        }
    }
    unescaped
}

fn outcome_kind(text: &str) -> Option<EntityKind> {
    EntityKind::ALL
        .iter()
        .find(|kind| format!("{kind:?}") == text)
        .copied()
}

fn parse_outcomes(text: &str) -> (Vec<PackageOutcome>, Vec<String>) {
    let mut packages: Vec<PackageOutcome> = Vec::new();
    let mut errors = Vec::new();
    for line in text.lines() {
        let Some((tag, rest)) = line.split_once(' ') else {
            continue;
        };
        match tag {
            "P" => {
                let fields: Vec<&str> = rest.split_whitespace().collect();
                if fields.len() != 8 {
                    errors.push(format!("short package record: {line}"));
                    continue;
                }
                let values: Vec<usize> =
                    fields[1..8].iter().filter_map(|f| f.parse().ok()).collect();
                if values.len() != 7 {
                    errors.push(format!("bad package record: {line}"));
                    continue;
                }
                packages.push(PackageOutcome {
                    package: fields[0].to_owned(),
                    entities: values[0],
                    occurrences: values[1],
                    cross_file_entities: values[2],
                    cross_file_occurrences: values[3],
                    links: values[4],
                    link_occurrences: values[5],
                    link_occurrences_with_source: values[6],
                    notes: Vec::new(),
                    symbols: Vec::new(),
                });
            }
            "PN" => {
                if let Some(last) = packages.last_mut() {
                    last.notes.push(unescape_line(rest));
                }
            }
            "S" => {
                let fields: Vec<&str> = rest.split_whitespace().collect();
                if fields.len() != 14 {
                    errors.push(format!("short symbol record: {line}"));
                    continue;
                }
                let Some(kind) = outcome_kind(fields[3]) else {
                    errors.push(format!("bad kind in: {line}"));
                    continue;
                };
                let values: Vec<usize> = fields[4..14]
                    .iter()
                    .filter_map(|f| f.parse().ok())
                    .collect();
                if values.len() != 10 {
                    errors.push(format!("bad symbol record: {line}"));
                    continue;
                }
                if let Some(last) = packages.last_mut() {
                    last.symbols.push(SymbolOutcome {
                        role: fields[1].to_owned(),
                        name: fields[2].to_owned(),
                        kind,
                        grep_total: values[0],
                        decl_est: values[1],
                        ir_local: values[2],
                        ir_foreign: values[3],
                        ir_stable: values[4],
                        site_ok: values[5],
                        site_bad: values[6],
                        decl_pos: values[7],
                        link_occ: values[8],
                        link_occ_src: values[9],
                        notes: Vec::new(),
                    });
                }
            }
            "SN" => {
                if let Some(last) = packages.last_mut() {
                    if let Some(symbol) = last.symbols.last_mut() {
                        symbol.notes.push(unescape_line(rest));
                    }
                }
            }
            "ERR" => errors.push(unescape_line(rest)),
            _ => {}
        }
    }
    (packages, errors)
}

// ---------------------------------------------------------------------------
// Shared measurement core.
// ---------------------------------------------------------------------------

/// The compile deadline for one package row.
const PACKAGE_DEADLINE: Duration = Duration::from_secs(240);
/// Fragment output budget for one package row.
const FRAGMENT_BUDGET: usize = 64 * 1024 * 1024;

/// One prepared authority ready to enter `compile_semantic`.
enum Authority {
    Rust {
        project: backend_frontend_rust::legacy::RustProject,
        source_limit: u32,
    },
    Go {
        image: Vec<u8>,
    },
    TypeScript {
        report: backend_frontend_typescript::legacy::Report,
    },
    Python {
        report: backend_frontend_python::legacy::CheckerReport,
    },
    PythonAstOnly,
    Java {
        image: Vec<u8>,
    },
    CSharp {
        image: Vec<u8>,
    },
    ClangProject {
        project: backend_frontend_clang::ClangProject,
    },
}

struct Prepared {
    profile: LanguageProfile,
    source: Vec<u8>,
    toolchain: ResolvedToolchain<'static>,
    authority: Authority,
    work: PathBuf,
}

fn fresh_dir(label: &str) -> PathBuf {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!(
        "nudox-refs-{label}-{}-{nonce}-{sequence}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).expect("fixture directory");
    path
}

fn env_tool(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from)
}

fn path_tool(program: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

fn command_version(program: &Path, argument: &str) -> Vec<u8> {
    let Ok(output) = Command::new(program).arg(argument).output() else {
        return Vec::new();
    };
    if output.stdout.is_empty() {
        output.stderr
    } else {
        output.stdout
    }
}

fn leaked_version(bytes: &[u8]) -> &'static [u8] {
    Box::leak(bytes.to_vec().into_boxed_slice())
}

fn leaked_path(path: PathBuf) -> &'static Path {
    Box::leak(path.into_boxed_path())
}

/// Compiles one prepared row through the real authority and measures the
/// reopened fragment plus the owned semantic image.
fn measure(label: &str, prepared: Prepared) -> Result<PackageOutcome, String> {
    let Prepared {
        profile,
        source,
        toolchain,
        authority,
        work,
    } = prepared;
    fs::create_dir_all(&work).map_err(|error| format!("work dir: {error}"))?;
    let mut diagnostic = vec![0_u8; 64 * 1024];
    let mut output = vec![0xa5_u8; FRAGMENT_BUDGET];
    let cancelled = AtomicBool::new(false);
    let authority_input = match &authority {
        Authority::Rust {
            project,
            source_limit,
        } => SemanticAuthorityInput::Rust {
            project,
            maximum_source_bytes: backend_frontend_rust::legacy::SourceByteLimit(*source_limit),
            features: backend_frontend_rust::legacy::RustFeatureControl::default(),
        },
        Authority::Go { image } => SemanticAuthorityInput::Go { image },
        Authority::TypeScript { report } => SemanticAuthorityInput::TypeScript { report },
        Authority::Python { report } => SemanticAuthorityInput::Python { report },
        Authority::PythonAstOnly => SemanticAuthorityInput::None,
        Authority::Java { image } => SemanticAuthorityInput::Java { image },
        Authority::CSharp { image } => SemanticAuthorityInput::CSharp { image },
        Authority::ClangProject { project } => SemanticAuthorityInput::Clang { project },
    };
    let (written, ir) = {
        let compiled = compile_semantic(
            CompileRequest {
                profile,
                stage: Stage::LowerIr,
                source: &source,
                declaration_scope: DeclarationScope::fixture(),
                toolchain: ToolchainSelection::ResolvedNative(toolchain),
                authority: authority_input,
                control: CompileControl {
                    deadline: Instant::now() + PACKAGE_DEADLINE,
                    cancelled: &cancelled,
                },
            },
            CompileScratch {
                diagnostic_output: &mut diagnostic,
                native_work: &work,
            },
            CompileOutput {
                fragment_output: &mut output,
            },
        )
        .map_err(|failure| format!("compile failed: {failure:?}"))?;
        (compiled.artifact.fragment.as_ref().len(), compiled.ir)
    };
    let _ = fs::remove_dir_all(&work);
    // Reopen the fragment from its written bytes: the measured surface is
    // exactly what a durable reader would see.
    let view = FragmentView::validate(&output[..written])
        .map_err(|cause| format!("fragment validation failed: {cause:?}"))?;
    let mut outcome = measure_fragment(&view, ir, &source);
    outcome.package = label.to_owned();
    Ok(outcome)
}

struct EntityInfo {
    id: EntityId,
    name: Vec<u8>,
    kind: EntityKind,
    parent: Option<EntityId>,
    span: Option<SpanInfo>,
}

struct SpanInfo {
    file: Vec<u8>,
    start: u32,
    end: u32,
}

/// One absolute link-plane site: the owned image's authority-observed use
/// position, already resolved against the owner's provenance span.
struct SiteInfo {
    file: Vec<u8>,
    start: usize,
    end: usize,
}

struct Sampled {
    role: &'static str,
    name: Vec<u8>,
    kind: EntityKind,
    id: EntityId,
}

/// All measured facts of one reopened fragment and its owned image.
fn measure_fragment<'fragment>(
    view: &FragmentView<'fragment>,
    ir: Ir,
    source: &[u8],
) -> PackageOutcome {
    let mut outcome = PackageOutcome::default();
    // Entity inventory with names, parents, and source spans.
    let mut entities: Vec<EntityInfo> = Vec::new();
    for row in ir.canonical_entities() {
        let name = ir.atom(row.name).unwrap_or(b"?").to_vec();
        let span = row.source.map(|span| SpanInfo {
            file: ir.atom(span.file()).unwrap_or(b"?").to_vec(),
            start: span.start(),
            end: span.end(),
        });
        entities.push(EntityInfo {
            id: row.id,
            name,
            kind: row.kind,
            parent: row.parent,
            span,
        });
    }
    outcome.entities = entities.len();
    // Cross-check: the fragment entity lane and the owned image must agree
    // row-for-row, or entity-id joins across the two planes are meaningless.
    let mut fragment_entities: Vec<(Vec<u8>, EntityKind)> = Vec::new();
    for entity in view.entities() {
        let ordinal = usize::try_from(entity.name.raw).unwrap_or(usize::MAX);
        let name = view
            .atoms()
            .nth(ordinal)
            .map(|atom| atom.bytes.to_vec())
            .unwrap_or_default();
        fragment_entities.push((name, entity.kind));
    }
    if fragment_entities.len() != entities.len() {
        outcome.notes.push(format!(
            "entity plane divergence: fragment carries {} rows, owned image carries {}",
            fragment_entities.len(),
            entities.len()
        ));
    } else {
        let disagreements = fragment_entities
            .iter()
            .zip(entities.iter())
            .filter(|((fragment_name, fragment_kind), owned)| {
                *fragment_kind != owned.kind || fragment_name.as_slice() != owned.name.as_slice()
            })
            .count();
        if disagreements > 0 {
            outcome.notes.push(format!(
                "entity plane divergence: {disagreements} rows disagree in name/kind between fragment and owned image"
            ));
            let mut shown = 0;
            for (ordinal, ((fragment_name, fragment_kind), owned)) in
                fragment_entities.iter().zip(entities.iter()).enumerate()
            {
                if *fragment_kind == owned.kind && fragment_name.as_slice() == owned.name.as_slice()
                {
                    continue;
                }
                outcome.notes.push(format!(
                    "divergence at {ordinal}: fragment {:?}/{fragment_kind:?} vs owned {:?}/{owned_kind:?}",
                    String::from_utf8_lossy(fragment_name),
                    String::from_utf8_lossy(&owned.name),
                    fragment_kind = fragment_kind,
                    owned_kind = owned.kind,
                ));
                shown += 1;
                if shown >= 3 {
                    break;
                }
            }
        }
    }
    // Fragment occurrence plane.
    let mut occurrences: Vec<DecodedOccurrence<'fragment>> = Vec::new();
    if let Some(cursor) = view.occurrences() {
        for row in cursor {
            match row {
                Ok(row) => occurrences.push(row),
                Err(cause) => outcome
                    .notes
                    .push(format!("occurrence decode fault: {cause:?}")),
            }
        }
    }
    outcome.occurrences = occurrences.len();
    // Link plane: kind histogram, source-carrying share, per-entity counts.
    let mut link_kinds = [0_usize; 11];
    let mut links_total = 0_usize;
    for (_, link) in ir.canonical_links() {
        let slot = match link.kind {
            LinkKind::Calls => 0,
            LinkKind::MethodCall => 1,
            LinkKind::TypeReference => 2,
            LinkKind::Reads => 3,
            LinkKind::Writes => 4,
            LinkKind::Imports => 5,
            LinkKind::Implements => 6,
            LinkKind::Overrides => 7,
            LinkKind::Reexports => 8,
            LinkKind::Inherits => 9,
            LinkKind::Documents => 10,
        };
        link_kinds[slot] += 1;
        links_total += 1;
    }
    let mut link_occ_by_target: Vec<(EntityId, bool)> = Vec::new();
    let mut link_occ_sites: Vec<(EntityId, SiteInfo)> = Vec::new();
    let mut link_occurrences = 0_usize;
    let mut link_occurrences_with_source = 0_usize;
    for (_, occurrence) in ir.link_occurrences() {
        link_occurrences += 1;
        if occurrence.source.is_some() {
            link_occurrences_with_source += 1;
        }
        if let Some(link) = ir.link(occurrence.link) {
            if let LinkTarget::Local(target) = link.target {
                link_occ_by_target.push((target, occurrence.source.is_some()));
                if let Some(source) = occurrence.source {
                    link_occ_sites.push((
                        target,
                        SiteInfo {
                            file: ir.atom(source.file()).unwrap_or(b"?").to_vec(),
                            start: usize::try_from(source.start()).unwrap_or(usize::MAX),
                            end: usize::try_from(source.end()).unwrap_or(usize::MAX),
                        },
                    ));
                }
            }
        }
    }
    outcome.links = links_total;
    outcome.link_occurrences = link_occurrences;
    outcome.link_occurrences_with_source = link_occurrences_with_source;
    outcome.notes.push(format!(
        "link kinds: calls={} method={} type={} reads={} writes={} imports={} implements={} overrides={} reexports={} inherits={} documents={}",
        link_kinds[0], link_kinds[1], link_kinds[2], link_kinds[3],
        link_kinds[4], link_kinds[5], link_kinds[6], link_kinds[7],
        link_kinds[8], link_kinds[9], link_kinds[10],
    ));
    // Primary file: the file atom carrying the most entity spans. Lanes that
    // never attach spans (Rust, TypeScript, Go, Java) have no file facts at
    // all, which is itself the measurement result.
    let mut file_votes: Vec<(&[u8], usize)> = Vec::new();
    for entity in &entities {
        let Some(span) = &entity.span else {
            continue;
        };
        match file_votes
            .iter_mut()
            .find(|(file, _)| *file == span.file.as_slice())
        {
            Some((_, votes)) => *votes += 1,
            None => file_votes.push((span.file.as_slice(), 1)),
        }
    }
    file_votes.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(right.0)));
    let primary_file = file_votes.first().map(|(file, _)| *file);
    if primary_file.is_none() {
        outcome.notes.push(
            "no entity carries a source span: occurrence sites cannot be placed in source \
             (links are emitted source-less)"
                .to_owned(),
        );
    }
    outcome.cross_file_entities = entities
        .iter()
        .filter(|entity| {
            entity.span.as_ref().is_some_and(|span| {
                primary_file.is_some_and(|primary| span.file.as_slice() != primary)
            })
        })
        .count();
    if outcome.cross_file_entities > 0 {
        outcome
            .notes
            .push("fragment carries cross-file declaration spans".to_owned());
    }
    // Cross-file occurrences: owners whose span file differs from the
    // primary. For lanes without spans this is unobservable.
    outcome.cross_file_occurrences = occurrences
        .iter()
        .filter(|row| {
            entities
                .get(usize::try_from(row.owner.raw).unwrap_or(usize::MAX))
                .and_then(|owner| owner.span.as_ref())
                .is_some_and(|span| {
                    primary_file.is_some_and(|primary| span.file.as_slice() != primary)
                })
        })
        .count();
    // Confidence / kind / target mix over the whole lane.
    let mut kind_votes = [0_usize; 8];
    let mut confidence_votes = [0_usize; 5];
    let mut target_votes = [0_usize; 3];
    for row in &occurrences {
        kind_votes[usize::from(u8::from(row.occurrence.kind))] += 1;
        confidence_votes[usize::from(u8::from(row.occurrence.confidence))] += 1;
        match row.occurrence.target {
            OccurrenceTarget::Local(_) => target_votes[0] += 1,
            OccurrenceTarget::Foreign(_) => target_votes[1] += 1,
            OccurrenceTarget::Stable(_) => target_votes[2] += 1,
        }
    }
    outcome.notes.push(format!(
        "targets: local={} foreign={} stable={}",
        target_votes[0], target_votes[1], target_votes[2]
    ));
    outcome.notes.push(format!(
        "kinds: call={} method={} type={} variable={} macro={} field={} import={} overrides={}",
        kind_votes[0],
        kind_votes[1],
        kind_votes[2],
        kind_votes[3],
        kind_votes[4],
        kind_votes[5],
        kind_votes[6],
        kind_votes[7],
    ));
    outcome.notes.push(format!(
        "confidence: syntactic={} suffix={} index={} import={} oracle={}",
        confidence_votes[0],
        confidence_votes[1],
        confidence_votes[2],
        confidence_votes[3],
        confidence_votes[4],
    ));
    // Sampled symbols. Roles (method vs function) and declaration spans are
    // owned-image facts; occurrence targets live in the fragment's entity
    // space and link occurrences in the owned image's entity space. The two
    // planes reorder rows (measured above), so this harness joins
    // occurrences by (name, kind) in fragment space and links by id in
    // owned space, and takes site truth from the link plane's absolute
    // source spans rather than re-deriving positions from RelSpans.
    for sampled in sample_symbols(&entities, source) {
        let mut symbol = SymbolOutcome {
            role: sampled.role.to_owned(),
            name: String::from_utf8_lossy(&sampled.name).into_owned(),
            kind: sampled.kind,
            grep_total: 0,
            decl_est: 0,
            ir_local: 0,
            ir_foreign: 0,
            ir_stable: 0,
            site_ok: 0,
            site_bad: 0,
            decl_pos: 0,
            link_occ: 0,
            link_occ_src: 0,
            notes: Vec::new(),
        };
        let matches = identifier_matches(source, &sampled.name);
        symbol.grep_total = matches.len();
        // Declaration-name tokens: the first in-span match of every entity
        // carrying the same name.
        let mut decl_positions: Vec<usize> = Vec::new();
        for entity in &entities {
            let Some(span) = &entity.span else {
                continue;
            };
            if entity.name != sampled.name {
                continue;
            }
            let span_start = usize::try_from(span.start).unwrap_or(usize::MAX);
            let span_end = usize::try_from(span.end).unwrap_or(usize::MAX);
            if let Some(first) = matches
                .iter()
                .find(|position| **position >= span_start && **position < span_end)
            {
                decl_positions.push(*first);
            }
        }
        decl_positions.sort_unstable();
        decl_positions.dedup();
        symbol.decl_est = decl_positions.len();
        // Fragment-space occurrence counts joined by (name, kind).
        let fragment_rows: Vec<usize> = fragment_entities
            .iter()
            .enumerate()
            .filter(|(_, (name, kind))| {
                *kind == sampled.kind && name.as_slice() == sampled.name.as_slice()
            })
            .map(|(ordinal, _)| ordinal)
            .collect();
        if fragment_rows.is_empty() {
            symbol.notes.push(
                "no fragment entity row carries this (name, kind): the owned image row has no fragment counterpart".to_owned(),
            );
        }
        for row in &occurrences {
            match row.occurrence.target {
                OccurrenceTarget::Local(target) => {
                    if fragment_rows.contains(&(usize::try_from(target.raw).unwrap_or(usize::MAX)))
                    {
                        symbol.ir_local += 1;
                    }
                }
                OccurrenceTarget::Foreign(key)
                    if foreign_key_names_symbol(key.path, key.display, &sampled.name) =>
                {
                    symbol.ir_foreign += 1;
                }
                _ => {}
            }
        }
        // Owned-space link counts and absolute site verification.
        for (target, with_source) in &link_occ_by_target {
            if *target != sampled.id {
                continue;
            }
            symbol.link_occ += 1;
            if *with_source {
                symbol.link_occ_src += 1;
            }
        }
        for (target, site) in &link_occ_sites {
            if *target != sampled.id {
                continue;
            }
            let in_entry = primary_file.is_some_and(|primary| site.file.as_slice() == primary);
            if !in_entry {
                symbol.site_bad += 1;
                if symbol.notes.len() < 6 {
                    symbol.notes.push(format!(
                        "link site in non-entry file {:?} at {}..{} (not verified against entry bytes)",
                        String::from_utf8_lossy(&site.file),
                        site.start,
                        site.end,
                    ));
                }
                continue;
            }
            let Some(bytes) = source.get(site.start..site.end) else {
                symbol.site_bad += 1;
                if symbol.notes.len() < 6 {
                    symbol.notes.push(format!(
                        "link site {}..{} escapes the entry source of {} bytes",
                        site.start,
                        site.end,
                        source.len()
                    ));
                }
                continue;
            };
            if bytes == symbol.name.as_bytes() {
                symbol.site_ok += 1;
                if decl_positions.contains(&site.start) {
                    symbol.decl_pos += 1;
                }
            } else {
                symbol.site_bad += 1;
                if symbol.notes.len() < 6 {
                    let line = source[..site.start.min(source.len())]
                        .iter()
                        .filter(|byte| **byte == b'\n')
                        .count()
                        + 1;
                    let window_start = site.start.saturating_sub(24).min(source.len());
                    let window_end =
                        (site.start.min(source.len()).saturating_add(24)).min(source.len());
                    symbol.notes.push(format!(
                        "link site at byte {} (line {line}) reads {:?} instead of {:?} [window {:?}]",
                        site.start,
                        String::from_utf8_lossy(bytes),
                        symbol.name,
                        String::from_utf8_lossy(&source[window_start..window_end]),
                    ));
                }
            }
        }
        if symbol.grep_total > symbol.decl_est && symbol.ir_local == 0 && symbol.ir_foreign == 0 {
            symbol
                .notes
                .push("no occurrence resolves to this entity despite in-file uses".to_owned());
        }
        outcome.symbols.push(symbol);
    }
    outcome
}

/// The declaring parent's kind, when the parent row exists.
/// The kind of an entity's parent, found by the parent's entity id.
///
/// `entities` is in canonical order while `parent` is a dense entity id, so
/// indexing the vector by the id misread a parent whenever lowering changed
/// the entity order: every aho-corasick method stopped being classed as a
/// method after c65e12730 although the image still parents all 56 of them to
/// their `impl` blocks.
fn parent_kind(entity: &EntityInfo, entities: &[EntityInfo]) -> Option<EntityKind> {
    let parent = entity.parent?;
    entities
        .iter()
        .find(|candidate| candidate.id == parent)
        .map(|parent| parent.kind)
}

fn kind_matches(entity: &EntityInfo, entities: &[EntityInfo], role: &str) -> bool {
    match role {
        "variant" => matches!(entity.kind, EntityKind::Variant | EntityKind::Constant),
        "field" => entity.kind == EntityKind::Field,
        "method" => {
            entity.kind == EntityKind::Function
                && parent_kind(entity, entities).is_some_and(|kind| {
                    matches!(
                        kind,
                        EntityKind::Record
                            | EntityKind::Enum
                            | EntityKind::Trait
                            | EntityKind::Implementation
                    )
                })
        }
        "function" => {
            entity.kind == EntityKind::Function
                && parent_kind(entity, entities).is_none_or(|kind| kind == EntityKind::Module)
        }
        "record" => entity.kind == EntityKind::Record,
        "enum" => entity.kind == EntityKind::Enum,
        _ => false,
    }
}

/// Deterministic symbol sampling: first entity per required role whose name
/// has at least two in-file matches (a use is plausible), else the first with
/// one. Roles: variant (fallback constant), field, method, function, record,
/// enum.
fn sample_symbols(entities: &[EntityInfo], source: &[u8]) -> Vec<Sampled> {
    let mut sampled: Vec<Sampled> = Vec::new();
    let mut seen: Vec<Vec<u8>> = Vec::new();
    for role in ["variant", "field", "method", "function", "record", "enum"] {
        let mut candidates = entities
            .iter()
            .filter(|entity| kind_matches(entity, entities, role));
        let best = candidates
            .clone()
            .find(|entity| identifier_matches(source, &entity.name).len() >= 2)
            .or_else(|| candidates.next());
        if let Some(entity) = best {
            if !seen.contains(&entity.name) {
                seen.push(entity.name.clone());
                sampled.push(Sampled {
                    role,
                    name: entity.name.clone(),
                    kind: entity.kind,
                    id: entity.id,
                });
            }
        }
    }
    sampled
}

/// Every word-boundary match position of `name` in `source`.
fn identifier_matches(source: &[u8], name: &[u8]) -> Vec<usize> {
    if name.is_empty() || source.len() < name.len() {
        return Vec::new();
    }
    let is_word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    let mut positions = Vec::new();
    let mut at = 0_usize;
    while let Some(found) = source[at..]
        .windows(name.len())
        .position(|window| window == name)
    {
        let position = at + found;
        let before_ok = position == 0 || !is_word(source[position - 1]);
        let after = position + name.len();
        let after_ok = after >= source.len() || !is_word(source[after]);
        if before_ok && after_ok {
            positions.push(position);
        }
        at = position + 1;
    }
    positions
}

/// True when a foreign key names `symbol`: the key path's last path-ish
/// segment or its display spelling equals the symbol.
fn foreign_key_names_symbol(path: &str, display: &str, symbol: &[u8]) -> bool {
    let symbol = String::from_utf8_lossy(symbol);
    if display == symbol {
        return true;
    }
    path.rsplit(['.', ':', '/', '<', '>', '(', ')', ' '])
        .next()
        .is_some_and(|segment| segment == symbol)
}

// ---------------------------------------------------------------------------
// Lane drivers: staging exactly as the flow audit does per lane.
// ---------------------------------------------------------------------------

fn corpus_root(var: &str) -> Option<PathBuf> {
    let root = env_tool(var)?;
    root.is_dir().then_some(root)
}

/// The audit's deterministic file pick: largest file with the given
/// extension filter, ties broken by the smaller path.
fn largest_file(root: &Path, accept: &dyn Fn(&Path) -> bool) -> Option<PathBuf> {
    let mut best: Option<(PathBuf, u64)> = None;
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let entries = fs::read_dir(&directory).ok()?;
        let mut children: Vec<_> = entries.flatten().map(|entry| entry.path()).collect();
        children.sort();
        for path in children {
            let Ok(meta) = fs::metadata(&path) else {
                continue;
            };
            if meta.is_dir() {
                stack.push(path);
            } else if meta.is_file() && accept(&path) {
                let replace = match &best {
                    None => true,
                    Some((known, known_len)) => {
                        meta.len() > *known_len || (meta.len() == *known_len && path < *known)
                    }
                };
                if replace {
                    best = Some((path, meta.len()));
                }
            }
        }
    }
    best.map(|(path, _)| path)
}

// ----- Rust lane -----------------------------------------------------------

fn rust_tool() -> Option<PathBuf> {
    if let Some(tool) = env_tool("RUSTC").or_else(|| env_tool("NUDOX_RUSTC")) {
        if tool.is_absolute() {
            return Some(tool);
        }
    }
    path_tool("rustc").and_then(|candidate| candidate.canonicalize().ok())
}

fn rust_prepare(root: &Path, crate_name: &str) -> Result<Prepared, String> {
    let Some(tool) = rust_tool() else {
        return Err("no rustc".to_owned());
    };
    let directory = root.join(crate_name);
    let Some(selected) = largest_file(&directory, &|path| {
        path.extension().and_then(|ext| ext.to_str()) == Some("rs")
    }) else {
        return Err(format!("{crate_name}: no .rs source"));
    };
    let source = fs::read(&selected).map_err(|error| error.to_string())?;
    let toolchain = backend_frontend_rust::legacy::RustToolchain::discover(&tool)
        .map_err(|error| format!("rust toolchain: {error:?}"))?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let fixture =
        std::env::temp_dir().join(format!("nudox-refs-rust-{nonce}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&fixture);
    fs::create_dir_all(fixture.join("src")).map_err(|error| error.to_string())?;
    fs::write(
        fixture.join("Cargo.toml"),
        b"[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .map_err(|error| error.to_string())?;
    let source_path = fixture.join("src/lib.rs");
    fs::write(&source_path, &source).map_err(|error| error.to_string())?;
    let project = backend_frontend_rust::legacy::RustProject::open_with_source(
        &fixture,
        &source_path,
        &toolchain,
        RustEdition::Rust2024,
    )
    .map_err(|error| format!("project: {error:?}"))?;
    let version = command_version(&tool, "--version");
    let resolved = ResolvedToolchain::from_version(
        NativeTool::Rustc,
        leaked_path(tool),
        leaked_version(&version),
    )
    .map_err(|cause| format!("resolved rust toolchain: {cause:?}"))?;
    let source_limit = u32::try_from(source.len()).unwrap_or(u32::MAX);
    Ok(Prepared {
        profile: LanguageProfile::Rust(RustEdition::Rust2024),
        source,
        toolchain: resolved,
        authority: Authority::Rust {
            project,
            source_limit,
        },
        work: fixture.join("work"),
    })
}

// ----- TypeScript lane -----------------------------------------------------

fn typescript_tool() -> Option<PathBuf> {
    env_tool("COMPILER_TYPESCRIPT_COMPILER")
        .or_else(|| env_tool("NUDOX_TSC"))
        .filter(|path| path.is_file())
        .or_else(|| path_tool("tsc"))
}

fn typescript_prepare(root: &Path, package: &str) -> Result<Prepared, String> {
    let Some(tool) = typescript_tool() else {
        return Err("no tsc".to_owned());
    };
    let package_root = root.join(package);
    let accepts = |path: &Path| {
        let extension = path.extension().and_then(|ext| ext.to_str());
        if matches!(extension, Some("ts") | Some("tsx")) {
            return true;
        }
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                name.ends_with(".d.ts") || name.ends_with(".d.mts") || name.ends_with(".d.cts")
            })
    };
    let Some(selected) = largest_file(&package_root, &accepts) else {
        return Err(format!("{package}: no .ts source"));
    };
    let source = fs::read(&selected).map_err(|error| error.to_string())?;
    let report = backend_frontend_typescript::legacy::Checker::default()
        .run_in_package(TypeScriptSource::TypeScript, &source, &package_root)
        .map_err(|error| format!("checker: {error:?}"))?;
    let version = command_version(&tool, "--version");
    let canonical = tool.canonicalize().unwrap_or(tool);
    let resolved = ResolvedToolchain::from_version(
        NativeTool::TypeScriptCompiler,
        leaked_path(canonical),
        leaked_version(&version),
    )
    .map_err(|cause| format!("resolved ts toolchain: {cause:?}"))?;
    Ok(Prepared {
        profile: LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
        source,
        toolchain: resolved,
        authority: Authority::TypeScript { report },
        work: fresh_dir("ts"),
    })
}

// ----- Python lane ---------------------------------------------------------

fn python_prepare(root: &Path, package: &str) -> Result<Prepared, String> {
    let directory = root.join(package);
    let Some(selected) = largest_file(&directory, &|path| {
        path.extension().and_then(|ext| ext.to_str()) == Some("py")
    }) else {
        return Err(format!("{package}: no .py source"));
    };
    let source = fs::read(&selected).map_err(|error| error.to_string())?;
    let profile = PythonVersion::Python314;
    let authority = if backend_frontend_python::legacy::Pyrefly::from_env().is_available() {
        let facts = backend_frontend_python::legacy::extract(&source, profile)
            .map_err(|error| format!("extract: {error:?}"))?;
        let report = backend_frontend_python::legacy::Pyrefly::from_env()
            .with_timeout(Duration::from_secs(180))
            .analyze(&source, profile, &facts)
            .map_err(|error| format!("pyrefly: {error:?}"))?;
        Authority::Python { report }
    } else {
        Authority::PythonAstOnly
    };
    // Python lowering runs in-process; the native toolchain slot only feeds
    // the compile recipe identity.
    let tool = env_tool("NUDOX_PYTHON").unwrap_or_else(|| PathBuf::from("/usr/bin/python3"));
    let resolved = ResolvedToolchain::from_version(
        NativeTool::Python,
        leaked_path(tool),
        leaked_version(b"references-coverage-python"),
    )
    .map_err(|cause| format!("resolved python toolchain: {cause:?}"))?;
    Ok(Prepared {
        profile: LanguageProfile::Python(profile),
        source,
        toolchain: resolved,
        authority,
        work: fresh_dir("python"),
    })
}

// ----- Go lane -------------------------------------------------------------

fn go_prepare(root: &Path, module: &str) -> Result<Prepared, String> {
    let Some(tool) = env_tool("NUDOX_GO").or_else(|| path_tool("go")) else {
        return Err("no go".to_owned());
    };
    let module_dir = root.join(module);
    let is_test = |path: &Path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with("_test.go"))
    };
    let mut files: Vec<(PathBuf, u64)> = Vec::new();
    let mut stack = vec![module_dir];
    while let Some(directory) = stack.pop() {
        for entry in fs::read_dir(&directory)
            .map_err(|error| error.to_string())?
            .flatten()
        {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("go") {
                let len = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
                files.push((path, len));
            }
        }
    }
    files.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    let selected = files
        .iter()
        .find(|(path, _)| !is_test(path))
        .or_else(|| files.first())
        .map(|(path, _)| path.clone())
        .ok_or_else(|| format!("{module}: no .go source"))?;
    let source = fs::read(&selected).map_err(|error| error.to_string())?;
    let fixture = fresh_dir("go");
    let staged = backend_frontend_go::legacy::stage_module(&fixture, &selected, &source)
        .map_err(|error| format!("stage: {error}"))?;
    let oracle = backend_frontend_go::legacy::GoOracle {
        output_limit: 32 * 1024 * 1024,
        timeout: Duration::from_secs(120),
    };
    let image = oracle
        .authority_image_for_package(&staged.source, &staged.root)
        .map_err(|error| format!("oracle: {error:?}"))?;
    let version = command_version(&tool, "version");
    let canonical = tool.canonicalize().unwrap_or(tool);
    let resolved = ResolvedToolchain::from_version(
        NativeTool::GoCompiler,
        leaked_path(canonical),
        leaked_version(&version),
    )
    .map_err(|cause| format!("resolved go toolchain: {cause:?}"))?;
    Ok(Prepared {
        profile: LanguageProfile::Go(GoVersion::Go125),
        source,
        toolchain: resolved,
        authority: Authority::Go { image },
        work: fixture.join("work"),
    })
}

// ----- Java lane -----------------------------------------------------------

fn java_prepare(
    harness: &mut backend_frontend_java::legacy::harness::Harness,
    tool: &backend_frontend_java::legacy::harness::JdkToolchain<'static>,
    root: &Path,
    coordinate: &str,
) -> Result<Prepared, String> {
    use backend_frontend_java::legacy::harness::{HarnessRequest, JavaSource};
    let raw = coordinate
        .strip_prefix("maven:")
        .ok_or_else(|| format!("{coordinate}: bad coordinate"))?;
    let (name, version) = raw
        .rsplit_once('@')
        .ok_or_else(|| format!("{coordinate}: no version"))?;
    let (group, artifact) = name
        .split_once(':')
        .ok_or_else(|| format!("{coordinate}: no artifact"))?;
    let mut package_root = root.to_path_buf();
    for component in group.split('.') {
        package_root.push(component);
    }
    package_root.push(artifact);
    package_root.push(version);
    if !package_root.is_dir() {
        return Err(format!("{coordinate}: package missing"));
    }
    let mut files = Vec::new();
    let mut stack = vec![package_root.clone()];
    while let Some(directory) = stack.pop() {
        for entry in fs::read_dir(&directory)
            .map_err(|error| error.to_string())?
            .flatten()
        {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("java") {
                files.push(path);
            }
        }
    }
    files.sort();
    let selected = files
        .iter()
        .cloned()
        .max_by_key(|path| {
            (
                fs::metadata(path).map(|meta| meta.len()).unwrap_or(0),
                path.clone(),
            )
        })
        .ok_or_else(|| format!("{coordinate}: no .java files"))?;
    let selected_relative = selected
        .strip_prefix(&package_root)
        .map_err(|error| error.to_string())?;
    let source = fs::read(&selected).map_err(|error| error.to_string())?;
    let mut sibling_sources: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    for file in &files {
        if file == &selected {
            continue;
        }
        let relative = file
            .strip_prefix(&package_root)
            .map_err(|error| error.to_string())?;
        let bytes = fs::read(file).map_err(|error| error.to_string())?;
        sibling_sources.push((relative.to_path_buf(), bytes));
    }
    let mut sources: Vec<JavaSource<'_>> = Vec::with_capacity(sibling_sources.len() + 1);
    sources.push(JavaSource {
        name: selected_relative,
        bytes: source.as_slice(),
    });
    for (relative, bytes) in &sibling_sources {
        sources.push(JavaSource {
            name: relative,
            bytes,
        });
    }
    let request = HarnessRequest {
        sources: &sources,
        classpath: &[],
        release: backend_frontend_java::legacy::JavaRelease::Java21,
    };
    // Sourcepath roots: the package plus its sibling artifact versions, the
    // same closure the flow audit stages.
    let mut roots: Vec<PathBuf> = vec![package_root.clone()];
    if let Some(group_dir) = package_root.parent().and_then(Path::parent) {
        if let Ok(artifacts) = fs::read_dir(group_dir) {
            for artifact in artifacts.flatten() {
                let artifact_path = artifact.path();
                if !artifact_path.is_dir() {
                    continue;
                }
                if let Ok(versions) = fs::read_dir(&artifact_path) {
                    for candidate in versions.flatten() {
                        let candidate = candidate.path();
                        if candidate == package_root || !candidate.is_dir() {
                            continue;
                        }
                        if candidate.join("module-info.java").is_file() {
                            continue;
                        }
                        roots.push(candidate);
                    }
                }
            }
        }
    }
    roots.sort();
    roots.dedup();
    let root_refs: Vec<&Path> = roots.iter().map(PathBuf::as_path).collect();
    let mut image = Vec::with_capacity(64 * 1024);
    harness
        .image_with_sourcepath(tool, request, &root_refs, &mut image)
        .map_err(|error| format!("{coordinate}: image: {error}"))?;
    let javac = root.join("bin").join("javac");
    let resolved = ResolvedToolchain::from_version(
        NativeTool::JavaCompiler,
        leaked_path(javac),
        leaked_version(b"references-coverage-java"),
    )
    .map_err(|cause| format!("resolved java toolchain: {cause:?}"))?;
    Ok(Prepared {
        profile: LanguageProfile::Java(JavaRelease::Java21),
        source,
        toolchain: resolved,
        authority: Authority::Java { image },
        work: fresh_dir("java"),
    })
}

// ----- C# lane -------------------------------------------------------------

fn csharp_dotnet() -> Option<PathBuf> {
    env_tool("COMPILER_CSHARP_COMPILER")
        .or_else(|| env_tool("NUDOX_DOTNET"))
        .filter(|path| path.is_file())
        .or_else(|| path_tool("dotnet"))
}

/// Publishes the vendored Roslyn oracle once per process.
///
/// The build is the flow harness's locked-restore flow
/// (`compiler_corpus/authority.rs` `CSharpAuthorityProvider::new`): a locked
/// restore pins the Roslyn closure from `packages.lock.json`, and the
/// no-restore publish reuses exactly those assets. The committed `obj/`
/// build assets are not part of the source tree, so a no-restore publish
/// without the prior restore has nothing to consume.
fn csharp_oracle(dotnet: &Path) -> Result<PathBuf, String> {
    static ORACLE: std::sync::OnceLock<Result<PathBuf, String>> = std::sync::OnceLock::new();
    ORACLE
        .get_or_init(|| {
            let helper = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../frontends/csharp/src/legacy/helper");
            let root = fresh_dir("oracle-publish");
            let publish = root.join("publish");
            if let Err(error) = fs::create_dir_all(&publish) {
                return Err(error.to_string());
            }
            // Isolate this process's intermediate AND build-output state:
            // every concurrently running C# test builds the same checked-in
            // helper project, and `dotnet restore`/`publish` write `obj/`
            // (`BaseIntermediateOutputPath`) and build into `bin/`
            // (`BaseOutputPath`) under the project directory by default —
            // `-o` only redirects the final publish copy, not that
            // intermediate build step.
            let intermediate = publish.join("obj");
            let mut intermediate_arg = std::ffi::OsString::from("-p:BaseIntermediateOutputPath=");
            intermediate_arg.push(&intermediate);
            intermediate_arg.push(std::path::MAIN_SEPARATOR.to_string());
            let output_base = publish.join("bin");
            let mut output_base_arg = std::ffi::OsString::from("-p:BaseOutputPath=");
            output_base_arg.push(&output_base);
            output_base_arg.push(std::path::MAIN_SEPARATOR.to_string());
            let restored = Command::new(dotnet)
                .args(["restore", "oracle.csproj", "--locked-mode", "--nologo"])
                .arg(&intermediate_arg)
                .current_dir(&helper)
                .status();
            match restored {
                Ok(status) if status.success() => {}
                Ok(status) => return Err(format!("oracle locked restore failed: {status}")),
                Err(error) => return Err(error.to_string()),
            }
            // `UseSharedCompilation=false`: see csharp_packaging.rs's build
            // invocation for why every concurrent build of this shared
            // checked-in project must not share MSBuild's ambient
            // VBCSCompiler node.
            let status = Command::new(dotnet)
                .args([
                    "publish",
                    "oracle.csproj",
                    "-c",
                    "Release",
                    "--nologo",
                    "--no-restore",
                    "-p:UseSharedCompilation=false",
                ])
                .arg(&intermediate_arg)
                .arg(&output_base_arg)
                .arg("-o")
                .arg(&publish)
                .current_dir(&helper)
                .status();
            match status {
                Ok(status) if publish.join("oracle.dll").is_file() && status.success() => {
                    Ok(publish.join("oracle.dll"))
                }
                Ok(status) => Err(format!("oracle publish failed: {status}")),
                Err(error) => Err(error.to_string()),
            }
        })
        .clone()
}

fn csharp_prepare(root: &Path, row: &(&str, &str, &str)) -> Result<Prepared, String> {
    let (package, version, relative) = *row;
    let Some(dotnet) = csharp_dotnet() else {
        return Err("no dotnet".to_owned());
    };
    let source_path = root.join(package).join(version).join(relative);
    let source = fs::read(&source_path)
        .map_err(|_| format!("{package}: corpus source {} missing", source_path.display()))?;
    let fixture = fresh_dir("csharp");
    let source_root = fixture.join("root");
    fs::create_dir_all(&source_root).map_err(|error| error.to_string())?;
    let binding = source_root.join("Package.cs");
    fs::write(&binding, &source).map_err(|error| error.to_string())?;
    let image_path = fixture.join("authority.image");
    let oracle = csharp_oracle(&dotnet)?;
    let status = Command::new(&dotnet)
        .arg("exec")
        .arg(&oracle)
        .arg("--mode")
        .arg("source")
        .arg("--assembly-name")
        .arg("ReferencesCoverage")
        .arg("--authority-image")
        .arg("--source-binding")
        .arg(&binding)
        .arg("--out")
        .arg(&image_path)
        .arg("--root")
        .arg(&source_root)
        .status()
        .map_err(|error| error.to_string())?;
    if !status.success() {
        return Err(format!("{package}: roslyn oracle rejected the row"));
    }
    let image = fs::read(&image_path).map_err(|error| error.to_string())?;
    let resolved = ResolvedToolchain::from_version(
        NativeTool::CSharpCompiler,
        leaked_path(dotnet),
        leaked_version(b"references-coverage-csharp"),
    )
    .map_err(|cause| format!("resolved csharp toolchain: {cause:?}"))?;
    Ok(Prepared {
        profile: LanguageProfile::CSharp(CSharpVersion::CSharp14),
        source,
        toolchain: resolved,
        authority: Authority::CSharp { image },
        work: fixture.join("work"),
    })
}

// ----- Clang lane ----------------------------------------------------------

fn clang_prepare(root: &Path, row: &(&str, &str, bool)) -> Result<Prepared, String> {
    let (package, entry_relative, cxx) = *row;
    let package_root = root.join(package);
    let entry = package_root.join(entry_relative);
    if !entry.is_file() {
        // Deterministic fallback: the largest source file of the package.
        let accept = move |path: &Path| {
            let extension = path.extension().and_then(|ext| ext.to_str());
            if cxx {
                matches!(extension, Some("cc") | Some("cpp") | Some("cxx"))
            } else {
                extension == Some("c")
            }
        };
        let fallback = largest_file(&package_root, &accept)
            .ok_or_else(|| format!("{package}: no entry source"))?;
        return clang_prepare_entry(&package_root, &fallback, cxx);
    }
    clang_prepare_entry(&package_root, &entry, cxx)
}

fn clang_prepare_entry(package_root: &Path, entry: &Path, cxx: bool) -> Result<Prepared, String> {
    let project = backend_frontend_clang::ClangProject::open(package_root, entry)
        .map_err(|cause| format!("project: {cause:?}"))?;
    let source = fs::read(entry).map_err(|error| error.to_string())?;
    let resolved = ResolvedToolchain::from_identity(
        NativeTool::Clang,
        Path::new("/usr/bin/clang"),
        backend_version::ContentId::from_canonical_bytes(b"references-coverage-clang"),
    )
    .map_err(|cause| format!("resolved clang toolchain: {cause:?}"))?;
    let profile = if cxx {
        LanguageProfile::Cxx(CxxStandard::Cxx23)
    } else {
        LanguageProfile::C(CStandard::C23)
    };
    Ok(Prepared {
        profile,
        source,
        toolchain: resolved,
        authority: Authority::ClangProject { project },
        work: fresh_dir("clang"),
    })
}

// ---------------------------------------------------------------------------
// Lane workers and snapshot assertions.
// ---------------------------------------------------------------------------

/// The per-lane worker entry: prepares and measures every selected package,
/// writing typed outcome records to the lane's outcome file.
fn run_lane(lane: &str, outcome_path: &Path) -> Result<(), String> {
    let mut text = String::new();
    fn record(result: Result<PackageOutcome, String>, label: &str, text: &mut String) {
        match result {
            Ok(outcome) => outcome.write(text),
            Err(error) => text.push_str(&format!("ERR {label}: {error}\n")),
        }
    }
    match lane {
        "rust" => {
            let Some(root) = corpus_root("NUDOX_RUST_CORPUS_DIR") else {
                writeln_lane(outcome_path, "SKIP NUDOX_RUST_CORPUS_DIR unset")?;
                return Ok(());
            };
            for crate_name in RUST_CRATES {
                record(
                    rust_prepare(&root, crate_name)
                        .and_then(|prepared| measure(crate_name, prepared)),
                    crate_name,
                    &mut text,
                );
            }
        }
        "typescript" => {
            let Some(root) = corpus_root("NUDOX_TYPESCRIPT_CORPUS_DIR") else {
                writeln_lane(outcome_path, "SKIP NUDOX_TYPESCRIPT_CORPUS_DIR unset")?;
                return Ok(());
            };
            for package in TYPESCRIPT_PACKAGES_SEL {
                record(
                    typescript_prepare(&root, package)
                        .and_then(|prepared| measure(package, prepared)),
                    package,
                    &mut text,
                );
            }
        }
        "python" => {
            let Some(root) = corpus_root("NUDOX_PYTHON_CORPUS_DIR") else {
                writeln_lane(outcome_path, "SKIP NUDOX_PYTHON_CORPUS_DIR unset")?;
                return Ok(());
            };
            for package in PYTHON_PACKAGES_SEL {
                record(
                    python_prepare(&root, package).and_then(|prepared| measure(package, prepared)),
                    package,
                    &mut text,
                );
            }
        }
        "go" => {
            let Some(root) = corpus_root("NUDOX_GO_CORPUS_DIR") else {
                writeln_lane(outcome_path, "SKIP NUDOX_GO_CORPUS_DIR unset")?;
                return Ok(());
            };
            for module in GO_MODULES {
                record(
                    go_prepare(&root, module).and_then(|prepared| measure(module, prepared)),
                    module,
                    &mut text,
                );
            }
        }
        "java" => {
            let Some(root) = corpus_root("NUDOX_JAVA_CORPUS_DIR") else {
                writeln_lane(outcome_path, "SKIP NUDOX_JAVA_CORPUS_DIR unset")?;
                return Ok(());
            };
            let Some(jdk_root) = env_tool("NUDOX_JDK") else {
                writeln_lane(outcome_path, "SKIP NUDOX_JDK unset")?;
                return Ok(());
            };
            let jdk =
                backend_frontend_java::legacy::harness::JdkToolchain::from_owned_root(jdk_root)
                    .map_err(|error| error.to_string())?;
            let mut harness = backend_frontend_java::legacy::harness::Harness::new()
                .map_err(|error| error.to_string())?;
            harness.prepare(&jdk).map_err(|error| error.to_string())?;
            for coordinate in JAVA_COORDINATES {
                record(
                    java_prepare(&mut harness, &jdk, &root, coordinate)
                        .and_then(|prepared| measure(coordinate, prepared)),
                    coordinate,
                    &mut text,
                );
            }
        }
        "csharp" => {
            let Some(root) = corpus_root("NUDOX_CSHARP_CORPUS_DIR") else {
                writeln_lane(outcome_path, "SKIP NUDOX_CSHARP_CORPUS_DIR unset")?;
                return Ok(());
            };
            for row in CSHARP_ROWS {
                record(
                    csharp_prepare(&root, row).and_then(|prepared| measure(row.0, prepared)),
                    row.0,
                    &mut text,
                );
            }
        }
        "clang" => {
            let Some(root) = corpus_root("NUDOX_CLANG_CORPUS_DIR") else {
                writeln_lane(outcome_path, "SKIP NUDOX_CLANG_CORPUS_DIR unset")?;
                return Ok(());
            };
            for row in CLANG_ROWS {
                record(
                    clang_prepare(&root, row).and_then(|prepared| measure(row.0, prepared)),
                    row.0,
                    &mut text,
                );
            }
        }
        other => return Err(format!("unknown lane {other}")),
    }
    writeln_lane(outcome_path, &text)
}

fn writeln_lane(outcome_path: &Path, text: &str) -> Result<(), String> {
    let mut file = fs::File::create(outcome_path).map_err(|error| error.to_string())?;
    file.write_all(text.as_bytes())
        .map_err(|error| error.to_string())
}

/// Parent-side lane runner: spawns the worker, parses its typed outcomes,
/// prints the coverage table, and asserts the pinned snapshot.
fn drive_lane(lane: &'static str, packages: &[SnapPackage], symbols: &[Snap], baseline: bool) {
    let worker_test = format!("references_coverage_{}_lane", lane);
    let dir = fresh_dir(&format!("refs-lane-{lane}"));
    let outcome_path = dir.join("outcomes.txt");
    let log_path = dir.join("worker.log");
    let exe = std::env::current_exe().expect("current test executable");
    let mut command = Command::new(exe);
    command.args([&worker_test, "--exact", "--nocapture"]);
    command.env("NUDOX_REFS_LANE_WORKER", lane);
    command.env("NUDOX_REFS_LANE_OUTCOME", &outcome_path);
    let log = fs::File::create(&log_path).expect("worker log");
    let log_duplicate = log.try_clone().expect("worker log duplicate");
    command.stdout(log);
    command.stderr(log_duplicate);
    let started = Instant::now();
    let status = command.status();
    let outcome_text = fs::read_to_string(&outcome_path).unwrap_or_default();
    let (measured, errors) = parse_outcomes(&outcome_text);
    let crashed = !matches!(&status, Ok(status) if status.success());
    if crashed {
        let tail = fs::read_to_string(&log_path).unwrap_or_default();
        let cut = tail.len().saturating_sub(4000);
        panic!(
            "{lane} lane worker did not produce its typed outcome (status {status:?}); log tail:\n{}",
            &tail[cut..]
        );
    }
    let _ = fs::remove_dir_all(&dir);
    eprintln!(
        "\n=== {lane} lane ({} packages, {:?}) ===",
        measured.len(),
        started.elapsed()
    );
    let mut mismatch = String::new();
    for package in &measured {
        eprintln!(
            "{}: entities={} occurrences={} cross_file_entities={} cross_file_occurrences={} links={} link_occ={} link_occ_src={}",
            package.package,
            package.entities,
            package.occurrences,
            package.cross_file_entities,
            package.cross_file_occurrences,
            package.links,
            package.link_occurrences,
            package.link_occurrences_with_source,
        );
        for note in &package.notes {
            eprintln!("    note: {note}");
        }
        for symbol in &package.symbols {
            let uses = symbol.grep_total.saturating_sub(symbol.decl_est);
            let coverage = if uses == 0 {
                100.0_f64
            } else {
                (f64::from(u32::try_from(symbol.ir_local).unwrap_or(u32::MAX))
                    / f64::from(u32::try_from(uses).unwrap_or(u32::MAX))
                    * 100.0)
                    .min(999.0)
            };
            eprintln!(
                "    {:<9} {:<28} kind={:<20?} grep={} uses={} ir_local={} ir_foreign={} stable={} site_ok={} site_bad={} decl_pos={} link_occ={}/{} => {:.0}%",
                symbol.role,
                symbol.name,
                symbol.kind,
                symbol.grep_total,
                uses,
                symbol.ir_local,
                symbol.ir_foreign,
                symbol.ir_stable,
                symbol.site_ok,
                symbol.site_bad,
                symbol.decl_pos,
                symbol.link_occ,
                symbol.link_occ_src,
                coverage,
            );
            for note in &symbol.notes {
                eprintln!("        note: {note}");
            }
        }
        if let Some(pinned) = packages.iter().find(|snap| snap.package == package.package) {
            compare_package(package, pinned, &mut mismatch);
        }
        for symbol in &package.symbols {
            let Some(pinned) = symbols.iter().find(|snap| {
                snap.package == package.package
                    && snap.symbol == symbol.name
                    && snap.role == symbol.role
            }) else {
                continue;
            };
            compare_symbol(symbol, pinned, &mut mismatch);
        }
    }
    for package in packages {
        if !measured
            .iter()
            .any(|measured_package| measured_package.package == package.package)
        {
            mismatch.push_str(&format!(
                "{}: pinned package {} produced no measurement\n",
                lane, package.package
            ));
        }
    }
    for symbol in symbols {
        let present = measured.iter().any(|measured_package| {
            measured_package.package == symbol.package
                && measured_package.symbols.iter().any(|measured_symbol| {
                    measured_symbol.name == symbol.symbol && measured_symbol.role == symbol.role
                })
        });
        if !present {
            mismatch.push_str(&format!(
                "{}: pinned symbol {}::{} ({}) produced no measurement\n",
                lane, symbol.package, symbol.symbol, symbol.role
            ));
        }
    }
    for error in &errors {
        eprintln!("{lane} worker error: {error}");
    }
    if baseline {
        print_baseline(lane, &measured);
        return;
    }
    assert!(mismatch.is_empty(), "{lane} snapshot drift:\n{mismatch}");
}

fn compare_package(package: &PackageOutcome, pinned: &SnapPackage, mismatch: &mut String) {
    let observed = (
        package.entities,
        package.occurrences,
        package.cross_file_entities,
        package.cross_file_occurrences,
        package.links,
        package.link_occurrences,
        package.link_occurrences_with_source,
    );
    let expected = (
        pinned.entities,
        pinned.occurrences,
        pinned.cross_file_entities,
        pinned.cross_file_occurrences,
        pinned.links,
        pinned.link_occurrences,
        pinned.link_occurrences_with_source,
    );
    if observed != expected {
        mismatch.push_str(&format!(
            "{} package observed {observed:?} pinned {expected:?} — {}\n",
            package.package, pinned.note,
        ));
    }
}

fn compare_symbol(symbol: &SymbolOutcome, pinned: &Snap, mismatch: &mut String) {
    let observed = (
        symbol.grep_total,
        symbol.decl_est,
        symbol.ir_local,
        symbol.ir_foreign,
        symbol.ir_stable,
        symbol.site_ok,
        symbol.site_bad,
        symbol.decl_pos,
        symbol.link_occ,
        symbol.link_occ_src,
    );
    let expected = (
        pinned.grep_total,
        pinned.decl_est,
        pinned.ir_local,
        pinned.ir_foreign,
        pinned.ir_stable,
        pinned.site_ok,
        pinned.site_bad,
        pinned.decl_pos,
        pinned.link_occ,
        pinned.link_occ_src,
    );
    if observed != expected {
        mismatch.push_str(&format!(
            "{}::{} ({}, {}): observed {observed:?} pinned {expected:?} — {}\n",
            pinned.package,
            pinned.symbol,
            pinned.role,
            format!("{:?}", symbol.kind),
            pinned.note,
        ));
    }
}

/// Prints the measured table as compilable snapshot literals for re-pinning.
fn print_baseline(lane: &str, measured: &[PackageOutcome]) {
    let upper = lane.to_ascii_uppercase();
    eprintln!("\n// ----- {lane} snapshot literal -----");
    eprintln!("static {upper}_PACKAGES: &[SnapPackage] = &[");
    for package in measured {
        eprintln!(
            "    SnapPackage {{ package: {:?}, entities: {}, occurrences: {}, cross_file_entities: {}, cross_file_occurrences: {}, links: {}, link_occurrences: {}, link_occurrences_with_source: {}, note: \"\" }},",
            package.package,
            package.entities,
            package.occurrences,
            package.cross_file_entities,
            package.cross_file_occurrences,
            package.links,
            package.link_occurrences,
            package.link_occurrences_with_source,
        );
    }
    eprintln!("];");
    eprintln!("static {upper}_SYMBOLS: &[Snap] = &[");
    for package in measured {
        for symbol in &package.symbols {
            eprintln!(
                "    Snap {{ package: {:?}, role: {:?}, symbol: {:?}, grep_total: {}, decl_est: {}, ir_local: {}, ir_foreign: {}, ir_stable: {}, site_ok: {}, site_bad: {}, decl_pos: {}, link_occ: {}, link_occ_src: {}, note: \"\" }},",
                package.package,
                symbol.role,
                symbol.name,
                symbol.grep_total,
                symbol.decl_est,
                symbol.ir_local,
                symbol.ir_foreign,
                symbol.ir_stable,
                symbol.site_ok,
                symbol.site_bad,
                symbol.decl_pos,
                symbol.link_occ,
                symbol.link_occ_src,
            );
        }
    }
    eprintln!("];");
}

/// Stack budget for one lane worker's drive thread. The semantic engine's
/// compile recursion is proportional to declaration nesting, and real
/// sources overflow libtest's default 8 MiB test-thread stack (the same
/// failure `compiler_corpus/real.rs` isolates); the worker therefore drives
/// its lane on a dedicated thread with this documented budget.
const WORKER_STACK_BYTES: usize = 512 * 1024 * 1024;

macro_rules! lane_test {
    ($fn_name:ident, $lane:literal) => {
        #[test]
        fn $fn_name() {
            let lane: &str = $lane;
            if std::env::var("NUDOX_REFS_LANE_WORKER").as_deref() != Ok(lane) {
                return;
            }
            let Some(outcome) = std::env::var_os("NUDOX_REFS_LANE_OUTCOME") else {
                return;
            };
            let outcome_path = PathBuf::from(&outcome);
            let drive_path = outcome_path.clone();
            let drive = std::thread::Builder::new()
                .name(format!("refs-{lane}-drive"))
                .stack_size(WORKER_STACK_BYTES)
                .spawn(move || {
                    if let Err(error) = run_lane(lane, &drive_path) {
                        let _ = writeln_lane(&drive_path, &format!("ERR lane failed: {error}\n"));
                        std::process::exit(101);
                    }
                });
            if let Ok(handle) = drive {
                if handle.join().is_err() {
                    let _ = writeln_lane(
                        &outcome_path,
                        "ERR lane drive thread panicked before writing its outcome\n",
                    );
                    std::process::exit(101);
                }
            }
        }
    };
}

// The seven worker entry points. In a normal test run these return
// immediately; the parent tests below re-execute the binary against them.
lane_test!(references_coverage_rust_lane, "rust");
lane_test!(references_coverage_typescript_lane, "typescript");
lane_test!(references_coverage_python_lane, "python");
lane_test!(references_coverage_go_lane, "go");
lane_test!(references_coverage_java_lane, "java");
lane_test!(references_coverage_csharp_lane, "csharp");
lane_test!(references_coverage_clang_lane, "clang");

/// Parent-side snapshot assertions, one per lane, each in its own test so a
/// single lane's drift names itself.
#[test]
fn rust_references_snapshot() {
    drive_lane("rust", RUST_PACKAGES, RUST_SYMBOLS, baseline_mode());
}

#[test]
fn typescript_references_snapshot() {
    drive_lane(
        "typescript",
        TYPESCRIPT_PACKAGES,
        TYPESCRIPT_SYMBOLS,
        baseline_mode(),
    );
}

#[test]
fn python_references_snapshot() {
    drive_lane("python", PYTHON_PACKAGES, PYTHON_SYMBOLS, baseline_mode());
}

#[test]
fn go_references_snapshot() {
    drive_lane("go", GO_PACKAGES, GO_SYMBOLS, baseline_mode());
}

#[test]
fn java_references_snapshot() {
    drive_lane("java", JAVA_PACKAGES, JAVA_SYMBOLS, baseline_mode());
}

#[test]
fn csharp_references_snapshot() {
    drive_lane("csharp", CSHARP_PACKAGES, CSHARP_SYMBOLS, baseline_mode());
}

#[test]
fn clang_references_snapshot() {
    assert!(
        cfg!(any(target_os = "linux", target_os = "macos")),
        "Clang reference snapshot needs a measured baseline for this platform"
    );
    drive_lane("clang", CLANG_PACKAGES, CLANG_SYMBOLS, baseline_mode());
}

fn baseline_mode() -> bool {
    std::env::var_os("NUDOX_REFS_BASELINE").is_some()
}

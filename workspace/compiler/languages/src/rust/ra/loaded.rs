//! Workspace load via `ra_ap_load_cargo`.
//!
//! Ported almost verbatim from
//! `workspace/compiler/compile/rust/ra/load.rs`.  The only structural change is
//! that the output carries `document_private` so that `lower_workspace` can
//! pass it to `LowerCtx` without threading it through another parameter.

use std::{path::Path, thread, time::Instant};

use ra_ap_ide_db::RootDatabase;
use ra_ap_load_cargo::{LoadCargoConfig, ProcMacroServerChoice, load_workspace};
use ra_ap_paths::AbsPathBuf;
use ra_ap_proc_macro_api::ProcMacroClient;
use ra_ap_project_model::{
    CargoConfig, CargoFeatures, CargoWorkspace, ProjectManifest, ProjectWorkspace,
    ProjectWorkspaceKind, RustLibSource, TargetKind,
};
use ra_ap_toolchain::Tool;
use ra_ap_vfs::Vfs;
use tracing::{debug, warn};

use crate::rust::error::Error;

// ── Policy types ──────────────────────────────────────────────────────────────

/// Configuration for a single workspace load.
#[derive(Debug, Clone)]
pub struct ExtractConfig {
    pub offline: bool,
    pub run_build_scripts: bool,
    pub num_threads: usize,
}

/// Forces `cargo metadata` to run offline when set to a true value.
///
/// `1`, `true` or `yes` (any case) mean offline; `0`, `false`, `no` and unset
/// mean cargo may use the network. Anything else is treated as unset, because a
/// typo in a variable should not silently switch a producer into a mode where
/// most packages fail to resolve.
pub const CARGO_OFFLINE_ENV: &str = "NUDOX_CARGO_OFFLINE";

/// Whether `cargo metadata` should be run offline, given the raw variable.
///
/// A pure function so the decision table is testable without touching the
/// process environment — the same split `nudox` uses wherever an env var
/// changes behaviour.
fn offline_from_env(raw: Option<&str>) -> bool {
    match raw.map(str::trim) {
        Some(v) if v.eq_ignore_ascii_case("1")
            || v.eq_ignore_ascii_case("true")
            || v.eq_ignore_ascii_case("yes") => true,
        _ => false,
    }
}

impl ExtractConfig {
    /// Defaults matched to the rustdoc path (build scripts on), with the
    /// network decision taken from [`CARGO_OFFLINE_ENV`].
    ///
    /// # Why this is no longer unconditionally offline
    ///
    /// It used to be `offline: true`, always. That is correct for a sealed
    /// build and wrong for the product: `cargo metadata --offline` needs every
    /// transitive dependency to already be in the local registry cache, and
    /// when one is missing rust-analyzer falls back to `--no-deps`, which
    /// yields an empty dependency graph in which every Cargo feature —
    /// `default` included — evaluates false. The producer then (rightly)
    /// refuses to lower the package rather than silently omit every
    /// `cfg(feature = ...)` item, so the reader gets a failed package.
    ///
    /// Opening `serde` on a machine that had not already cached `quote` failed
    /// exactly that way. For an application whose central action is "open a
    /// package and read it", that is the common case, not the edge.
    ///
    /// Offline is still one variable away, and a sandboxed or sealed build
    /// should set it: cargo consults the local cache first regardless, so the
    /// network is touched only for what is genuinely missing.
    pub fn for_extract() -> Self {
        Self {
            offline: offline_from_env(std::env::var(CARGO_OFFLINE_ENV).ok().as_deref()),
            run_build_scripts: true,
            num_threads: thread::available_parallelism()
                .map(|n| n.get().min(8))
                .unwrap_or(1),
        }
    }
}

#[cfg(test)]
mod offline_env_tests {
    use super::offline_from_env;

    #[test]
    fn unset_allows_the_network() {
        assert!(!offline_from_env(None));
    }

    #[test]
    fn truthy_values_force_offline() {
        for v in ["1", "true", "TRUE", "Yes", " yes "] {
            assert!(offline_from_env(Some(v)), "{v:?} should mean offline");
        }
    }

    #[test]
    fn falsey_values_allow_the_network() {
        for v in ["0", "false", "no", ""] {
            assert!(!offline_from_env(Some(v)), "{v:?} should allow the network");
        }
    }

    #[test]
    fn a_typo_does_not_silently_force_offline() {
        // The failure mode this guards: `NUDOX_CARGO_OFFLINE=ture` switching a
        // producer into the mode where most packages fail to resolve, with
        // nothing anywhere saying why.
        assert!(!offline_from_env(Some("ture")));
        assert!(!offline_from_env(Some("offline")));
    }
}

// ── Dependency resolution signal ──────────────────────────────────────────────

/// The `cargo metadata` failure that rust-analyzer swallowed when it fell back
/// to `--no-deps`.
///
/// It exists as its own type so that the failure can sit in a `#[source]` slot
/// (see [`crate::rust::error::Error::DependenciesUnresolved`]) and be reached by
/// walking `std::error::Error::source` — the diagnosis path AGENTS-DOCTRINE §8
/// prescribes — instead of being flattened into a message the moment it crosses
/// a layer.
#[derive(Debug, Clone, thiserror::Error)]
#[error(
    "`cargo metadata` failed and rust-analyzer substituted `--no-deps` metadata: {cargo_diagnostic}"
)]
pub struct NoDepsFallback {
    cargo_diagnostic: String,
}

impl NoDepsFallback {
    /// The `cargo metadata` failure chain, rendered.
    ///
    /// A `String` rather than a live error object on purpose: upstream stores
    /// the cause as `Arc<anyhow::Error>` in `ProjectWorkspaceKind::Cargo::error`,
    /// and `anyhow::Error` deliberately does not implement `std::error::Error`,
    /// so it cannot occupy a `#[source]` slot — nor can it be moved out of an
    /// `Arc` we only borrow. `{:#}` renders the whole `Caused by` chain, which
    /// is the part a reader actually needs. Keeping the live object instead
    /// would mean taking `anyhow` on as a dependency of this crate.
    pub fn cargo_diagnostic(&self) -> &str {
        &self.cargo_diagnostic
    }
}

/// How completely this workspace's dependency graph resolved.
///
/// # Why this is a type and not a log line
///
/// `ProjectWorkspace::load` returns `Ok` in two materially different
/// situations. In one, `cargo metadata` resolved the whole graph. In the other
/// it failed, and `ra_ap_project_model` silently retried with `--no-deps` and
/// returned *that* — recording the original failure in
/// `ProjectWorkspaceKind::Cargo::error`, an `Option` field no caller is obliged
/// to read (`cargo_workspace.rs:806-823`, 0.0.341). `--no-deps` metadata has no
/// `resolve` section, so every dependency is absent from the crate graph and
/// every Cargo feature — `default` included — evaluates false. The lowering
/// that comes out is not "slightly smaller"; it is missing every
/// `cfg(feature = "…")` item in the package, and it is structurally
/// indistinguishable from a complete one.
///
/// Making that difference a value on [`LoadedWorkspace`], and refusing to lower
/// a `NoDeps` workspace unless the caller has said so out loud
/// ([`LoadedWorkspace::accept_degraded_dependencies`]), is what stops the
/// degraded case from travelling as if it were the full one.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum DependencyResolution {
    /// Full `cargo metadata` succeeded: the `resolve` section is present, every
    /// dependency is in the crate graph, and feature activation is real.
    Full,

    /// `cargo metadata` failed and upstream substituted `--no-deps` metadata.
    ///
    /// Carries the failure so a caller can act on it (fetch the missing
    /// package, vendor it, drop `--offline`) rather than merely notice it.
    NoDeps(NoDepsFallback),
}

impl DependencyResolution {
    /// The `--no-deps` fallback, when that is why the graph is incomplete.
    ///
    /// This exists so that callers outside this crate can ask the question they
    /// actually have — "is this incomplete, and if so why?" — *totally*, without
    /// a wildcard match arm. The enum is `#[non_exhaustive]` because a future
    /// loader could report a third kind of incompleteness (a sysroot that failed
    /// to load, say); making every call site write `_ => {}` today would mean
    /// that future variant is silently absorbed everywhere instead of forcing a
    /// decision. Routing the question through this projection moves the decision
    /// to one place: whoever adds the variant must classify it here and in
    /// [`Self::is_degraded`], so the gap the compiler cannot catch is a single
    /// reviewable function rather than a scattering of `_` arms.
    pub fn no_deps(&self) -> Option<&NoDepsFallback> {
        match self {
            Self::Full => None,
            Self::NoDeps(fallback) => Some(fallback),
        }
    }

    /// Whether the crate graph is missing its dependencies.
    ///
    /// The predicate half of [`Self::no_deps`], for callers that only branch and
    /// never report.
    pub fn is_degraded(&self) -> bool {
        match self {
            Self::Full => false,
            Self::NoDeps(_) => true,
        }
    }
}

// ── Build-script execution signal ─────────────────────────────────────────────

/// Why a package's `build.rs` output never reached the crate graph.
///
/// It exists as its own type, exactly like [`NoDepsFallback`], so the failure
/// can sit in a `#[source]` slot (see
/// [`crate::rust::error::Error::BuildScriptsFailed`]) and be reached by walking
/// `std::error::Error::source` instead of being flattened into a message the
/// moment it crosses a layer (AGENTS-DOCTRINE §8).
///
/// # Why the cause is rendered text on every arm
///
/// Because on the arm that actually fires it is *all that exists*. Upstream
/// accumulates cargo's diagnostics into a `String` inside
/// `ra_ap_project_model::build_dependencies::WorkspaceBuildScripts::run_command`
/// (0.0.341) and exposes them only as `WorkspaceBuildScripts::error() ->
/// Option<&str>`; there is no error object left to preserve. The other arm's
/// cause is an `anyhow::Error`, which — as [`NoDepsFallback`] already records —
/// deliberately does not implement `std::error::Error` and so cannot occupy a
/// `#[source]` slot either. `{:#}` renders its whole `Caused by` chain, which is
/// the part a reader needs. Keeping one arm typed would buy a downcast nobody
/// performs, at the cost of a shape every reader has to special-case.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum BuildScriptFailure {
    /// rust-analyzer could not run the build-script `cargo check` at all — the
    /// cargo binary is missing, the working directory is gone, the process
    /// could not be spawned.
    ///
    /// Distinct from [`Self::CargoRefusedTheWorkspace`] because the recovery is
    /// different in kind: this one is about the machine, that one is about the
    /// package.
    #[error("rust-analyzer could not run the build-script `cargo check`: {diagnostic}")]
    NotRun {
        /// The invocation failure with its whole `Caused by` chain.
        diagnostic: String,
    },

    /// The command ran and cargo exited non-zero, so no `cargo:rustc-cfg` line
    /// reached the crate graph.
    ///
    /// The observed cause on real crates.io tarballs is *target resolution*,
    /// not compilation: `cargo package` honours `exclude`, so a published
    /// tarball can omit `tests/` or `benches/` while its own generated
    /// `Cargo.toml` still declares `[[test]]`/`[[bench]]` entries pointing at
    /// them. See docs/LIMITATIONS.md L50.
    #[error("the build-script `cargo check` exited non-zero: {diagnostic}")]
    CargoRefusedTheWorkspace {
        /// Everything cargo wrote, as upstream accumulated it.
        diagnostic: String,
    },

    /// The workspace declares a build script and [`ExtractConfig`] asked for
    /// build scripts not to run.
    ///
    /// A configuration choice, not an environment failure — but with exactly
    /// the same consequence for the table, so it is refused through the same
    /// gate rather than being the one way to get the old silent behaviour back.
    #[error(
        "build scripts are disabled by `ExtractConfig::run_build_scripts`, but this workspace \
         declares one, so every `cargo:rustc-cfg` it would emit evaluates false"
    )]
    DisabledByConfiguration,
}

impl BuildScriptFailure {
    /// The underlying cargo/invocation text, when there is one.
    ///
    /// A projection rather than a public field so that
    /// [`Self::DisabledByConfiguration`] — which has no external cause, because
    /// nothing external happened — is answered `None` instead of being handed a
    /// fabricated string. Callers outside this crate can therefore ask "what did
    /// cargo say?" totally, without a wildcard arm over a `#[non_exhaustive]`
    /// enum.
    pub fn diagnostic(&self) -> Option<&str> {
        match self {
            Self::NotRun { diagnostic } | Self::CargoRefusedTheWorkspace { diagnostic } => {
                Some(diagnostic)
            }
            Self::DisabledByConfiguration => None,
        }
    }
}

/// Whether this workspace's build scripts delivered the `cargo:rustc-cfg` and
/// `OUT_DIR` output the lowering depends on.
///
/// # Why this is a type and not a log line
///
/// This is [`DependencyResolution`]'s defect one layer down, and it is worse.
/// `ProjectWorkspace::run_build_scripts` reports a failed `cargo check` as
/// `Ok(scripts)` with the diagnostics parked in `scripts.error()`, an
/// `Option<&str>` no caller is obliged to read — the same shape as
/// `ProjectWorkspaceKind::Cargo::error`. Until this type existed, `load`
/// answered it with `warn!`, nothing in this crate or its tests installs a
/// `tracing` subscriber, and the warning went nowhere.
///
/// What is lost is not "slightly less metadata". Every `cargo:rustc-cfg` the
/// script would have emitted evaluates **false**, so every `#[cfg(atomic_cas)]`
/// item is pruned before the walk sees it, and — because `cfg` predicates come
/// in pairs — the `#[cfg(not(...))]` fallback shim on the other side is lowered
/// in its place, as though it were the package's public structure. The table
/// that comes out describes a crate that does not exist on this target, and it
/// is structurally indistinguishable from a correct one. `log 0.4.17` lost
/// `set_logger` and `set_boxed_logger` this way; `nom 5.1.3` lost eight public
/// parsers. See docs/LIMITATIONS.md L50.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum BuildScriptExecution {
    /// No local/member package declares a build script, so there is nothing to
    /// run and nothing to lose.
    ///
    /// Not a degradation, and deliberately distinct from [`Self::Ran`]: "we
    /// skipped it because there was none" and "we ran it and it worked" are the
    /// same *outcome* and different *evidence*, and conflating them is how the
    /// 74 ms `build_scripts` phase in L50 read as "fast" rather than as "did not
    /// happen".
    NotDeclared,

    /// The build-script `cargo check` ran and cargo exited 0: every
    /// `cargo:rustc-cfg` and `OUT_DIR` the workspace's own build scripts emit is
    /// in the crate graph.
    Ran,

    /// A build script is declared and its output did not reach the crate graph.
    ///
    /// Carries the failure so a caller can act on it rather than merely notice
    /// it.
    Failed(BuildScriptFailure),
}

impl BuildScriptExecution {
    /// The build-script failure, when that is why the crate graph is missing
    /// cfgs.
    ///
    /// The same projection [`DependencyResolution::no_deps`] is, for the same
    /// reason: it lets a caller outside this crate ask its real question
    /// totally, without a `_` arm that would silently absorb a future variant.
    pub fn failure(&self) -> Option<&BuildScriptFailure> {
        match self {
            Self::NotDeclared | Self::Ran => None,
            Self::Failed(failure) => Some(failure),
        }
    }

    /// Whether `cfg`-gated items are missing from this load.
    pub fn is_degraded(&self) -> bool {
        match self {
            Self::NotDeclared | Self::Ran => false,
            Self::Failed(_) => true,
        }
    }
}

// ── The single completeness contract ──────────────────────────────────────────

/// How complete this load is, on every axis that can leave an `Ok` describing a
/// package that does not exist.
///
/// # Why one type with two axes, rather than one enum
///
/// The obvious unification — a single `LoadCompleteness::{Full, NoDeps,
/// BuildScriptsFailed}` enum with [`DependencyResolution`] folded into it — was
/// considered and rejected, because **the two axes are not mutually exclusive
/// and a sum type would force us to drop one of them.** A package can perfectly
/// well fail `cargo metadata` *and* fail its build-script `cargo check`; in
/// fact one causes the other, since upstream's `run_build_scripts` returns
/// `Ok(WorkspaceBuildScripts::default())` without running anything at all when
/// the workspace was loaded from `--no-deps` metadata. An enum would have made
/// that pair unrepresentable and the load would have reported whichever
/// degradation the code happened to check first, which is precisely the class of
/// under-reporting this file exists to stop.
///
/// What *is* unified, and what the design constraint actually asked for, is the
/// **choke point**: [`Self::require_complete`] is the only function that turns
/// "degraded but unaccepted" into a typed error, on either axis, and it is the
/// only reader of the caller's acceptances. Adding a third axis means adding a
/// field here and an arm there, and nothing else in the crate can be reached
/// without passing through it.
///
/// The axes stay separately *acceptable* for the same reason they stay
/// separately representable: `accept_degraded_dependencies` and
/// `accept_missing_build_script_cfgs` license different lies about the output,
/// and a caller who has reasoned about one has not thereby reasoned about the
/// other. One combined "accept everything" switch would let a future third axis
/// be waved through by a call site written before it existed.
#[derive(Debug, Clone)]
pub struct LoadCompleteness {
    /// How `cargo metadata` resolved the dependency graph.
    dependencies: DependencyResolution,
    /// Whether the build scripts' `cargo:rustc-cfg` output reached the graph.
    build_scripts: BuildScriptExecution,
}

impl LoadCompleteness {
    /// How `cargo metadata` resolved this workspace's dependency graph.
    pub fn dependencies(&self) -> &DependencyResolution {
        &self.dependencies
    }

    /// Whether this workspace's build-script `cargo:rustc-cfg` output arrived.
    pub fn build_scripts(&self) -> &BuildScriptExecution {
        &self.build_scripts
    }

    /// Whether anything about this load makes its table unsafe to publish.
    pub fn is_degraded(&self) -> bool {
        self.dependencies.is_degraded() || self.build_scripts.is_degraded()
    }

    /// The one place a degraded-but-unaccepted load becomes a typed error.
    ///
    /// The order is causal, not arbitrary. A `--no-deps` load makes
    /// `ProjectWorkspace::run_build_scripts` a silent no-op upstream — it
    /// returns an empty `WorkspaceBuildScripts` whose `error()` is `None`, so
    /// this load would classify its build scripts as [`BuildScriptExecution::Ran`]
    /// having run nothing. Reporting the resolution failure first therefore
    /// names the cause rather than the symptom. A caller who accepts the
    /// dependency degradation has, by the same token, accepted that the
    /// build-script axis is not measurable for that load.
    fn require_complete(
        &self,
        package: &str,
        accepted: AcceptedDegradations,
    ) -> Result<(), Error> {
        if !accepted.dependencies
            && let Some(fallback) = self.dependencies.no_deps()
        {
            return Err(Error::DependenciesUnresolved {
                package: package.to_owned(),
                cause: fallback.clone(),
            });
        }
        if !accepted.build_scripts
            && let Some(failure) = self.build_scripts.failure()
        {
            return Err(Error::BuildScriptsFailed {
                package: package.to_owned(),
                cause: failure.clone(),
            });
        }
        Ok(())
    }
}

/// The degradations a caller has explicitly agreed to lower anyway.
///
/// Private, `Default`-to-nothing-accepted, and settable only through the two
/// consuming `#[must_use]` methods on [`LoadedWorkspace`]. That is the whole
/// mechanism behind "a caller that does nothing gets the error, not the silence":
/// there is no constructor, no field, and no config knob that produces an
/// accepted degradation by accident.
#[derive(Debug, Clone, Copy, Default)]
struct AcceptedDegradations {
    /// Set only by `LoadedWorkspace::accept_degraded_dependencies`.
    dependencies: bool,
    /// Set only by `LoadedWorkspace::accept_missing_build_script_cfgs`.
    build_scripts: bool,
}

// ── LoadedWorkspace ───────────────────────────────────────────────────────────

/// One loaded analysis session.
///
/// Owns the salsa `RootDatabase`, the `Vfs`, the `ProjectWorkspace` (for
/// package/target metadata), and the proc-macro client handle.
///
/// # Oracle lifetime contract
///
/// `crate::produce` keeps this value alive for the entire duration of
/// `Producer::lower`, so it is safe for `lower` to borrow HIR out of `db`.
/// The producer does **not** need a self-referential struct here because the
/// borrow occurs inside the `lower()` call, not stored anywhere.
///
/// # Completeness contract
///
/// A `LoadedWorkspace` records how complete it is ([`Self::completeness`]) on
/// both axes that can silently shrink a table: whether `cargo metadata`
/// resolved the dependency graph, and whether the build scripts'
/// `cargo:rustc-cfg` output reached the crate graph. When either is degraded,
/// `lower_workspace` **refuses** to walk it and returns
/// [`crate::rust::error::Error::DependenciesUnresolved`] or
/// [`crate::rust::error::Error::BuildScriptsFailed`]. A caller that genuinely wants
/// the degraded lowering says so with [`Self::accept_degraded_dependencies`] or
/// [`Self::accept_missing_build_script_cfgs`]. There is no path from a degraded
/// load to a lowering that does not pass through
/// [`LoadCompleteness::require_complete`].
pub struct LoadedWorkspace {
    pub db: RootDatabase,
    pub vfs: Vfs,
    /// Absolute root of the package this workspace was loaded for.
    ///
    /// Kept so that source locations can be reported *relative* to it. An IR
    /// that embedded `/Users/…/result/memchr-2.8.3/src/lib.rs` would make
    /// every entry's identity a property of the build machine; relative to this
    /// root the same declaration is `src/lib.rs` everywhere.
    pub root: AbsPathBuf,
    /// Kept for package/target metadata.
    pub ws: ProjectWorkspace,
    /// Whether private items should be lowered.
    pub document_private: bool,
    /// The name of the package this workspace was loaded for.
    ///
    /// Carried here rather than on the producer value because `Producer::lower`
    /// receives only the oracle — see the note on [`super::load`].
    pub package_name: String,
    /// How complete this load is, on every axis that can silently shrink the
    /// table.
    ///
    /// Private, unlike its neighbours, because it is a *claim about
    /// completeness* rather than a piece of workspace data: exposing it as a
    /// field would let a caller pattern-match past it, which is precisely the
    /// mistake `ProjectWorkspaceKind::Cargo::error` and
    /// `WorkspaceBuildScripts::error()` both invite upstream.
    completeness: LoadCompleteness,
    /// Which degradations the caller has explicitly accepted.
    ///
    /// Only [`LoadedWorkspace::accept_degraded_dependencies`] and
    /// [`LoadedWorkspace::accept_missing_build_script_cfgs`] can set a field on
    /// it, and only [`LoadCompleteness::require_complete`] reads it.
    accepted: AcceptedDegradations,
    _proc_macro: Option<ProcMacroClient>,
}

impl LoadedWorkspace {
    /// How complete this load is, on both axes.
    ///
    /// Every consumer of a lowering needs this to know what the numbers it is
    /// about to record actually measure — a corpus entry taken under
    /// [`DependencyResolution::NoDeps`] or
    /// [`BuildScriptExecution::Failed`] is not comparable with one taken under
    /// a complete load.
    pub fn completeness(&self) -> &LoadCompleteness {
        &self.completeness
    }

    /// How `cargo metadata` resolved this workspace's dependency graph.
    pub fn dependency_resolution(&self) -> &DependencyResolution {
        self.completeness.dependencies()
    }

    /// Whether this workspace's build scripts delivered their `cargo:rustc-cfg`
    /// output.
    pub fn build_script_execution(&self) -> &BuildScriptExecution {
        self.completeness.build_scripts()
    }

    /// Proceed with a lowering whose dependency graph is known to be empty.
    ///
    /// This is the *only* way to obtain a lowering from a dependency-degraded
    /// load. It is deliberately a consuming, `#[must_use]` call rather than a
    /// config flag or an environment variable so that accepting a
    /// feature-and-dependency-free lowering is a visible statement at the call
    /// site, attributable in review, rather than a default nobody chose.
    #[must_use = "accepting a degraded dependency graph returns a new workspace; \
                  discarding it leaves the original still refusing to lower"]
    pub fn accept_degraded_dependencies(mut self) -> Self {
        self.accepted.dependencies = true;
        self
    }

    /// Proceed with a lowering whose `cfg`-gated public API is known to be
    /// missing.
    ///
    /// The build-script twin of [`Self::accept_degraded_dependencies`], and
    /// deliberately a *separate* statement: the two accept different lies. The
    /// dependency one says "I know this table has no features and no
    /// dependencies"; this one says "I know every item behind a
    /// `build.rs`-emitted `cfg` is absent and its `not(...)` counterpart is
    /// present in its place". Nothing in this crate calls it — it exists so the
    /// refusal has a documented door rather than being routed around.
    #[must_use = "accepting a load with no build-script cfgs returns a new workspace; \
                  discarding it leaves the original still refusing to lower"]
    pub fn accept_missing_build_script_cfgs(mut self) -> Self {
        self.accepted.build_scripts = true;
        self
    }

    /// Gate consulted by `lower_workspace` before any walking happens.
    ///
    /// Returns the first unaccepted degradation as an error — cause chain
    /// intact — unless the load is complete or the caller opted in. Delegates to
    /// [`LoadCompleteness::require_complete`], which is the single place in the
    /// crate where a degraded load becomes a typed failure.
    pub(crate) fn require_complete_load(&self) -> Result<(), Error> {
        self.completeness
            .require_complete(&self.package_name, self.accepted)
    }
}

// ── Public load function ──────────────────────────────────────────────────────

/// Load the Cargo workspace at `root` into HIR.
///
/// - Discovers the manifest (or workspace root).
/// - Runs build scripts for `OUT_DIR` items, but only when the workspace
///   actually declares one (see `workspace_has_build_script`).
/// - Does *not* eagerly prime salsa caches; the caller's HIR walk fills them
///   on demand, scoped to what it actually visits (see the comment at the end
///   of this function).
pub(crate) fn load(
    root: &Path,
    package_name: &str,
    document_private: bool,
) -> Result<LoadedWorkspace, Error> {
    let cfg = ExtractConfig::for_extract();

    let mut cargo_config = build_cargo_config(&cfg, CargoFeatures::All);
    let abs = abs_path(root)?;

    let manifest = time_phase("manifest_discover", || {
        ProjectManifest::discover_single(abs.as_ref())
    })
    .map_err(|e| Error::Load(e.into()))?;

    let mut ws = time_phase("workspace_load", || {
        ProjectWorkspace::load(manifest.clone(), &cargo_config, &|msg| {
            debug!(target: "ra_load", "{msg}");
        })
    })
    .map_err(|e| Error::Load(e.into()))?;

    // ── Drop std-integration features, then load again ────────────────────────
    //
    // `CargoFeatures::All` is right in general (see `build_cargo_config`), but a
    // small family of features exists solely so a crates.io crate can be built
    // *inside* `rust-lang/rust`'s standard-library workspace, and their whole
    // purpose is to replace `core` with a stub. Activating one in a
    // documentation build is never what anybody wants, and the damage is not
    // subtle: memchr 2.7.6's `rustc-dep-of-std` pulls in
    // `rustc-std-workspace-core 1.0.0`, whose `src/lib.rs` is *zero bytes*, and
    // Cargo installs it into the crate graph under the name `core`, shadowing
    // the real `core` for that package. Every `core::` path stops resolving,
    // every `#[derive]` stops expanding, and the lowering silently loses 442 of
    // its 1 344 entries. (2.8.x pins 1.0.1 of the same shim, which does
    // `pub use the_core::*`, so the identical configuration is harmless there —
    // which is exactly why this went unnoticed.) docs.rs does not enable these
    // features either.
    //
    // The rule is deliberately structural, not a name blocklist: a feature is
    // excluded when activating it would enable a dependency whose *resolved
    // package name* is a `rustc-std-workspace-*` shim. `rustc-dep-of-std` is
    // merely the conventional name; matching on the name would miss the crate
    // that spells it differently and would break the moment someone renames it.
    //
    // Detecting this needs the resolved graph (the rename `core` →
    // `rustc-std-workspace-core` only exists in `cargo metadata`'s output), so
    // it costs a second `cargo metadata` — but only for the rare package that
    // actually has such a feature, and only the metadata call, not the crate
    // graph build.
    //
    // Untested edge: a *virtual* workspace (no root package) that also vendors a
    // `rustc-std-workspace-*` shim. `--no-default-features` is rejected at a
    // virtual root by some Cargo versions, so the reload would fail — loudly, as
    // a `Error::Load`, not silently. No fixture exercises it; if it
    // ever fires, the fix is to scope the flags with `-p` rather than to widen
    // the exclusion.
    let selection = select_features_excluding_std_integration(&ws);
    if !selection.excluded.is_empty() {
        warn!(
            excluded = ?selection.excluded,
            "excluding std-integration features: they would substitute a \
             `rustc-std-workspace-*` shim for `core` and silently break name \
             resolution for the package being documented"
        );
        eprintln!(
            "ra_load std_integration_features_excluded=[{}] package={package_name}",
            selection.excluded.join(",")
        );
        cargo_config = build_cargo_config(
            &cfg,
            CargoFeatures::Selected {
                features: selection.selected,
                // Every feature we still want is named explicitly above,
                // `default` included when it is not itself excluded, so turning
                // the implicit default set off is what makes the exclusion
                // actually take effect: a `default` that transitively enables
                // the shim must not sneak back in.
                no_default_features: true,
            },
        );
        ws = time_phase("workspace_reload_without_std_integration", || {
            ProjectWorkspace::load(manifest, &cargo_config, &|msg| {
                debug!(target: "ra_load", "{msg}");
            })
        })
        .map_err(|e| Error::Load(e.into()))?;
    }

    // `ProjectWorkspace::load` can "succeed" while quietly documenting a much
    // smaller workspace than the one on disk: `ra_ap_project_model` retries a
    // failed full `cargo metadata` with `--no-deps` and returns that instead,
    // recording the original failure only in `ProjectWorkspaceKind::Cargo::error`
    // (see its doc comment: "only populated if retried fetching via `--no-deps`
    // succeeded"). `--no-deps` metadata carries no `resolve` section at all, so
    // every package's `active_features` — not just non-default ones, *all* of
    // them, including `default` itself — comes back empty, and every dependency
    // crate is simply absent from the graph.
    //
    // The `warn!`/`eprintln!` below are *reporting*, not the fix: upstream also
    // warns, and its warning is silent here because nothing in this crate or
    // its tests installs a `tracing` subscriber. The fix is the
    // `DependencyResolution` value this block produces — see that type's docs
    // and `LoadedWorkspace::require_resolved_dependencies`. The `eprintln!` is
    // kept because the corpus tooling greps for the literal
    // `ra_load metadata_degraded=true` line.
    let resolution = match &ws.kind {
        ProjectWorkspaceKind::Cargo {
            error: Some(metadata_error),
            ..
        }
        | ProjectWorkspaceKind::DetachedFile {
            cargo: Some((_, _, Some(metadata_error))),
            ..
        } => {
            warn!(
                error = %metadata_error,
                "full `cargo metadata` failed and rust-analyzer fell back to `--no-deps`; \
                 this package's dependency graph is empty and every Cargo feature — \
                 default included — is inactive, so every `cfg(feature = ...)` item is \
                 pruned before the walk ever sees it"
            );
            eprintln!(
                "ra_load metadata_degraded=true package={package_name} error={metadata_error}"
            );
            // `{:#}` is anyhow's alternate Display: the whole `Caused by` chain
            // on one line. `{}` alone would keep only the outermost message,
            // which for a resolution failure is the least informative link.
            DependencyResolution::NoDeps(NoDepsFallback {
                cargo_diagnostic: format!("{metadata_error:#}"),
            })
        }
        ProjectWorkspaceKind::Cargo { error: None, .. }
        | ProjectWorkspaceKind::DetachedFile { .. }
        | ProjectWorkspaceKind::Json(_) => DependencyResolution::Full,
    };

    // `run_build_scripts` spawns `cargo check --message-format=json` and waits
    // on it, purely to discover `OUT_DIR` paths. That is real work only for a
    // package whose build actually runs a `build.rs`. `cargo metadata` — which
    // `ProjectWorkspace::load` above has already collected — has *already*
    // resolved that, implicit `build.rs` detection included, into a
    // `TargetKind::BuildScript` target per package. Asking it is exact where
    // re-deriving the answer from the manifest ourselves would be a second,
    // divergent implementation of Cargo's own detection.
    let declares_build_script = workspace_has_build_script(&ws);
    let run_build_scripts = cfg.run_build_scripts && declares_build_script;

    // Every arm of this produces a `BuildScriptExecution`, so there is no way to
    // leave this block without having classified what the crate graph is about
    // to be missing. The `warn!`s that used to be here were reporting, and they
    // reported into a void — nothing in this crate or its tests installs a
    // `tracing` subscriber (AGENTS-DOCTRINE §8: "A `warn!` does not do that;
    // nothing in a log line stops the next caller").
    let build_scripts = if !declares_build_script {
        debug!(
            run_build_scripts_configured = cfg.run_build_scripts,
            "no build script declared by any local/member package; skipping run_build_scripts"
        );
        BuildScriptExecution::NotDeclared
    } else if !cfg.run_build_scripts {
        BuildScriptExecution::Failed(BuildScriptFailure::DisabledByConfiguration)
    } else {
        // `narrowed_build_script_config` is what makes the two L50 packages
        // load correctly rather than merely fail loudly; see its doc comment.
        let build_script_config = narrowed_build_script_config(&ws, &cargo_config);
        time_phase("build_scripts", || {
            match ws.run_build_scripts(&build_script_config, &|msg| {
                debug!(target: "ra_load", "{msg}");
            }) {
                Ok(scripts) => {
                    let outcome = match scripts.error() {
                        None => BuildScriptExecution::Ran,
                        // Upstream returns `Ok` here: a `cargo check` that
                        // exited non-zero is reported out-of-band in
                        // `WorkspaceBuildScripts::error()`, an `Option<&str>` no
                        // caller is obliged to read. This is the arm L50 fires
                        // on, and the arm its own text mis-attributed to the
                        // `Err` branch below.
                        Some(diagnostic) => BuildScriptExecution::Failed(
                            BuildScriptFailure::CargoRefusedTheWorkspace {
                                diagnostic: diagnostic.to_owned(),
                            },
                        ),
                    };
                    // Installed even when cargo refused, because whatever *did*
                    // get emitted before the failure is still better than
                    // nothing for a caller who goes on to call
                    // `accept_missing_build_script_cfgs`. It cannot make the
                    // load look complete: `build_scripts` above already says it
                    // is not.
                    ws.set_build_scripts(scripts);
                    outcome
                }
                // `{:#}` is anyhow's alternate Display: the whole `Caused by`
                // chain on one line. `{}` alone would keep only the outermost
                // message, which here is upstream's own `with_context` wrapper
                // and says nothing a reader can act on.
                Err(e) => BuildScriptExecution::Failed(BuildScriptFailure::NotRun {
                    diagnostic: format!("{e:#}"),
                }),
            }
        })
    };

    let load_config = build_load_config(&cfg, run_build_scripts);

    // Clone so we keep `ws` for package metadata after `load_workspace` consumes a copy.
    let (db, vfs, proc_macro) = time_phase("load_workspace", || {
        load_workspace(ws.clone(), &cargo_config.extra_env, &load_config)
    })
    .map_err(|e| Error::Load(e.into()))?;

    if proc_macro.is_none() {
        warn!("proc-macro server unavailable; macro-generated items will be missing");
    }

    // No explicit `parallel_prime_caches` call here — deliberately. It used to
    // eagerly prime salsa caches for *every* crate in the resolved graph, not
    // just the documented package(s) `lower_workspace` is actually about to
    // walk: speculative work for the rest of the dependency tree (including
    // the sysroot) that a single-package walk mostly throws away. Measured
    // head-to-head, priming first was slower, not faster, in every paired run
    // across three real crates of very different sizes (`itoa` 153 entries,
    // `log` 419, `memchr` 11 329) — see `docs/LIMITATIONS.md` L1 for the numbers.
    // Salsa is demand-driven and memoized regardless, so removing this costs
    // nothing but the eager, over-broad head start; `lower_workspace`'s walk
    // (`attach_db` + `lower_all_packages_into` in `super`) computes exactly
    // the same queries lazily, scoped to what it actually touches.
    //
    // Cancellation is unaffected: the only salsa cancellation point this
    // function used to guard, `Cancelled::catch`, is still covered — real
    // cancellation now surfaces from the walk itself, where the per-crate
    // `catch_unwind` in `lower_all_packages_into` already downcasts the panic
    // payload to `Cancelled` and returns `Error::Cancelled` for it
    // (see the comment on `lower_workspace_inner` in `super`).

    Ok(LoadedWorkspace {
        db,
        vfs,
        // Lexically normalised (`a/b/../c` → `a/c`), because the paths the VFS
        // reports come back through `cargo metadata` already normalised, and a
        // root still carrying `..` segments would fail to prefix-match every
        // one of them — reporting a whole package as "outside itself".
        // `normalize` and not `canonicalize`: `AbsPath::canonicalize` is a
        // deliberate `panic!` upstream (paths#14430), and resolving symlinks
        // would introduce the *opposite* mismatch on any machine whose checkout
        // sits behind one.
        root: abs.normalize(),
        ws,
        document_private,
        package_name: package_name.to_owned(),
        completeness: LoadCompleteness {
            dependencies: resolution,
            build_scripts,
        },
        accepted: AcceptedDegradations::default(),
        _proc_macro: proc_macro,
    })
}

// ── The narrowed build-script invocation ──────────────────────────────────────

/// A copy of `cargo_config` whose build-script `cargo check` does **not** pass
/// `--all-targets`.
///
/// # Why this override exists at all
///
/// `ra_ap_project_model` 0.0.341 adds `--all-targets` to the build-script
/// `cargo check` *unconditionally* whenever the toolchain is new enough for
/// `--compile-time-deps` (`build_dependencies.rs:530-535`), ignoring
/// `CargoConfig::all_targets`, which this crate has always set to `false`. Its
/// stated reason — "we won't actually build the binaries, and as such, this will
/// succeed even on targets without libtest" — is true of *compilation* and false
/// of *target resolution*, which happens first and is fatal.
///
/// That is the whole of docs/LIMITATIONS.md L50. `cargo package` honours `exclude`,
/// so a published tarball routinely omits `tests/` or `benches/` while the
/// `Cargo.toml` cargo generated for it still declares `[[test]]`/`[[bench]]`
/// entries pointing into them. `--all-targets` makes cargo resolve those
/// targets, cargo cannot find the files, and it exits before running a single
/// build script. Measured directly in the checkouts:
///
/// ```text
/// $ cargo check --all-targets            # result/log-0.4.17
/// error: can't find integration-test `filters` at path `…/tests/filters.rs`
/// error: could not compile due to 2 previous target resolution errors
/// ```
///
/// Nothing this engine does needs those targets. A build script's output is a
/// property of the *package*, not of which of its targets you asked cargo to
/// check, and the only reason to widen the set would be to pick up build
/// scripts belonging to dev-dependencies — whose `OUT_DIR` and cfgs are
/// irrelevant to documenting the package's public API. Dropping the flag is
/// therefore narrowing the request to what was always wanted, not weakening a
/// check. Verified on the two L50 packages and on the `libc`/`log 0.4.33`
/// controls: the identical command minus `--all-targets` exits 0 and delivers
/// `atomic_cas`/`has_atomics` (log 0.4.17), `stable_i128` (nom 5.1.3) and
/// libc's fifteen.
///
/// # Why it is safe to hand-build the argv
///
/// `run_build_script_command` is rust-analyzer's own supported extension point
/// for this (`rust-analyzer.cargo.buildScripts.overrideCommand`), and taking it
/// means mirroring the flags upstream would otherwise assemble —
/// `build_dependencies.rs:447-557`, pinned at `=0.0.341` alongside every other
/// `ra_ap_*` crate. A mirror can drift, and this one is allowed to, because the
/// failure is *bounded by the type above it*: any command that does not deliver
/// the cfgs makes cargo exit non-zero or the invocation fail, and
/// [`BuildScriptExecution::Failed`] then refuses the whole load. There is no
/// arrangement of these flags that produces a quietly-wrong table, which is
/// exactly the property the old code lacked.
///
/// Two upstream behaviours are deliberately not mirrored, both no-ops here:
/// `--target-dir` (this crate leaves `TargetDirectoryConfig::None`, under which
/// upstream passes no `--target-dir` either) and the temporary `Cargo.lock`
/// copy (`make_lockfile_copy`), which exists to keep an editor from touching a
/// user's lockfile; these checkouts are disposable fixtures and the same
/// `cargo metadata` has already resolved against that lockfile.
fn narrowed_build_script_config(ws: &ProjectWorkspace, cargo_config: &CargoConfig) -> CargoConfig {
    let mut narrowed = cargo_config.clone();
    let ProjectWorkspaceKind::Cargo { cargo, .. } = &ws.kind else {
        // Non-Cargo workspace kinds never reach `run_for_workspace`'s
        // command-building path, so there is nothing to narrow.
        return narrowed;
    };

    narrowed.run_build_script_command = Some(narrowed_build_script_argv(cargo, cargo_config));
    // `--compile-time-deps` and the `-Z` flag it rides on are nightly-gated;
    // upstream sets this same override for the same reason
    // (`build_dependencies.rs:554-556`). Set on the clone rather than on the
    // caller's config so it cannot leak into the `cargo metadata` invocations,
    // which have already run by this point and must stay on the real channel.
    narrowed.extra_env.insert(
        "__CARGO_TEST_CHANNEL_OVERRIDE_DO_NOT_USE_THIS".to_owned(),
        Some("nightly".to_owned()),
    );
    narrowed
}

/// The build-script `cargo check` argv, mirroring
/// `WorkspaceBuildScripts::build_command` minus `--all-targets`.
///
/// Split out from [`narrowed_build_script_config`] so it can be asserted on
/// without running cargo: `--all-targets` being absent is the entire point of
/// this file's change, and a test that had to boot rust-analyzer to see it would
/// never be run.
fn narrowed_build_script_argv(cargo: &CargoWorkspace, cargo_config: &CargoConfig) -> Vec<String> {
    // Program resolution goes through the same `ra_ap_toolchain` lookup upstream
    // uses for `Tool::Cargo` (`Sysroot::tool` -> `Tool::prefer_proxy`), so an
    // override does not quietly switch which cargo runs.
    let mut argv = vec![
        Tool::Cargo.prefer_proxy().to_string(),
        "check".to_owned(),
        "--quiet".to_owned(),
        "--workspace".to_owned(),
        "--message-format=json".to_owned(),
    ];
    argv.extend(cargo_config.extra_args.iter().cloned());
    argv.push("--manifest-path".to_owned());
    argv.push(cargo.manifest_path().to_string());

    match &cargo_config.features {
        CargoFeatures::All => argv.push("--all-features".to_owned()),
        CargoFeatures::Selected {
            features,
            no_default_features,
        } => {
            if *no_default_features {
                argv.push("--no-default-features".to_owned());
            }
            // Upstream filters the list against the workspace's own feature
            // names before passing it; a name cargo does not know is a hard
            // error, and `select_features_excluding_std_integration` collects
            // across every local/member package.
            let allowed = cargo.workspace_features();
            let selected = features
                .iter()
                .filter(|feature| allowed.contains(*feature))
                .cloned()
                .collect::<Vec<_>>()
                .join(",");
            if !selected.is_empty() {
                argv.push("--features".to_owned());
                argv.push(selected);
            }
        }
    }

    // `--keep-going` so one unbuildable dependency does not hide every build
    // script behind it; `--compile-time-deps` so cargo builds *only* build
    // scripts and proc macros, which is both the fast path and the reason a
    // package like `log 0.4.17` succeeds at all — its `value-bag 1.0.0-alpha.9`
    // dependency no longer compiles on a modern rustc, and a full `cargo check`
    // would fail on that instead.
    argv.push("--keep-going".to_owned());
    argv.push("--compile-time-deps".to_owned());
    argv.push("-Zunstable-options".to_owned());
    argv
}

// ── Phase instrumentation ─────────────────────────────────────────────────────

/// Run `f` inside a `tracing` span named `phase`.
///
/// The span is the real instrumentation — anything that installs a `tracing`
/// subscriber (the daemon, `RUST_LOG`-driven tooling) gets per-phase timing for
/// free, in release builds too. The debug-only `eprintln!` exists because
/// nothing in this workspace's *test* binaries installs a subscriber, so
/// without it the phase breakdown this function exists to produce would be
/// invisible under `cargo test -- --nocapture` — which is how this file's L1
/// numbers were actually measured. It is compiled out of release builds
/// (`cfg!(debug_assertions)`) so a production load does not gain four
/// unconditional stderr lines per package it did not have before.
fn time_phase<R>(phase: &'static str, f: impl FnOnce() -> R) -> R {
    let span = tracing::info_span!("ra_load_phase", phase);
    let _guard = span.enter();
    let started = Instant::now();
    let out = f();
    let elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0;
    tracing::info!(phase, elapsed_ms, "ra_load phase complete");
    if cfg!(debug_assertions) {
        eprintln!("ra_load phase={phase} elapsed_ms={elapsed_ms:.1}");
    }
    out
}

/// Whether any local/member package in `ws` declares a build script (explicit
/// `build = "…"` or an auto-detected `build.rs`).
///
/// Deliberately scoped to local/member packages, not the whole resolved
/// dependency graph: a build script belonging to an external dependency sets
/// `cfg`/`OUT_DIR` state for *that dependency's own compilation*, not for the
/// package(s) this call is about to document. Restricting to local/member
/// packages is a superset of "the root package" so that binary-only,
/// direct-repo workspaces (whose documented set is BFS'd from local member
/// libraries in `documented_package_names`) are not under-covered.
fn workspace_has_build_script(ws: &ProjectWorkspace) -> bool {
    let ProjectWorkspaceKind::Cargo { cargo, .. } = &ws.kind else {
        // Non-Cargo workspace kinds (detached file, rust-project.json) have no
        // build scripts to run in the first place.
        return false;
    };
    cargo.packages().any(|pkg| {
        let data = &cargo[pkg];
        (data.is_local || data.is_member)
            && data
                .targets
                .iter()
                .any(|&t| matches!(cargo[t].kind, TargetKind::BuildScript))
    })
}

// ── std-integration feature detection ─────────────────────────────────────────

/// Package-name prefix of the shim crates that exist only to let a crates.io
/// crate compile inside `rust-lang/rust`'s std workspace.
///
/// `rustc-std-workspace-core`, `-alloc` and `-std` are all published stubs whose
/// entire job is to stand in for the real sysroot crate of the same name. When
/// Cargo puts one in the graph under the name `core`, it *shadows* the sysroot
/// `core` for that package.
const STD_SHIM_PACKAGE_PREFIX: &str = "rustc-std-workspace-";

/// The `--features` list to load with, and what was dropped from it.
///
/// Both lists hold **bare** feature names, deduplicated across the workspace's
/// local/member packages. The `package/feature` form is *not* usable here:
/// Cargo resolves the left-hand side against the package's **dependencies**, so
/// for the single-package manifests this corpus is made of it fails with
/// ``package `libc` does not have a dependency named `libc` `` — verified
/// against the CLI on both `libc` and `memchr`.
///
/// The cost of bare names is that a multi-member workspace whose members do not
/// all declare the same feature would make Cargo reject the list. That path is
/// only ever reached by a workspace that *also* vendors a
/// `rustc-std-workspace-*` shim, no fixture exercises it, and it fails loudly as
/// a `Error::Load` rather than silently mis-lowering.
struct FeatureSelection {
    /// Feature names to pass to `--features`.
    selected: Vec<String>,
    /// Feature names withheld, for the operator-facing log line.
    ///
    /// Empty means "nothing to do" — the caller keeps [`CargoFeatures::All`]
    /// and never pays for a second `cargo metadata`.
    excluded: Vec<String>,
}

/// Split every local/member package's features into "safe to activate" and
/// "would swap a `rustc-std-workspace-*` shim in for `core`".
fn select_features_excluding_std_integration(ws: &ProjectWorkspace) -> FeatureSelection {
    let mut selected = Vec::new();
    let mut excluded = Vec::new();

    let ProjectWorkspaceKind::Cargo { cargo, .. } = &ws.kind else {
        return FeatureSelection { selected, excluded };
    };

    for pkg in cargo.packages() {
        let data = &cargo[pkg];
        // Scoped to local/member packages for the same reason
        // `workspace_has_build_script` is: `--features` on a `cargo metadata`
        // invocation applies to the workspace's own packages, and a dependency's
        // features are chosen by whoever depends on it, not by us.
        if !(data.is_local || data.is_member) {
            continue;
        }

        // Every spelling a `[features]` entry could use for a std-shim
        // dependency, because a feature value names the dependency as the
        // *manifest* does — the rename when there is one, the package name when
        // there is not — and neither is reliably what `PackageDependency::name`
        // holds.
        //
        // `PackageDependency::name` comes from `cargo metadata`'s
        // `resolve.nodes[].deps[].name`, which is the **lib** name: for a
        // renamed dependency that is the rename (memchr's
        // `rustc-std-workspace-core` renamed to `core` → `"core"`, matching its
        // `rustc-dep-of-std = ["core"]`), but for an un-renamed one it is the
        // package name with hyphens turned into underscores (libc's →
        // `"rustc_std_workspace_core"`, while its feature table says
        // `rustc-dep-of-std = ["align", "rustc-std-workspace-core"]`). Matching
        // on only one of the two silently misses half the cases — it missed
        // `libc` on the first corpus pass.
        let shim_aliases: Vec<&str> = data
            .dependencies
            .iter()
            .filter(|dep| cargo[dep.pkg].name.starts_with(STD_SHIM_PACKAGE_PREFIX))
            .flat_map(|dep| [dep.name.as_str(), cargo[dep.pkg].name.as_str()])
            .collect();

        for feature in data.features.keys() {
            if !shim_aliases.is_empty()
                && feature_activates_any(&data.features, feature, &shim_aliases)
            {
                excluded.push(feature.clone());
            } else {
                selected.push(feature.clone());
            }
        }
    }

    selected.sort();
    selected.dedup();
    excluded.sort();
    excluded.dedup();
    // A name excluded by one package must not be re-enabled by another that
    // happens to declare a feature of the same name: the exclusion is the whole
    // point of the second load.
    selected.retain(|f| !excluded.contains(f));
    FeatureSelection { selected, excluded }
}

/// Whether activating `feature` reaches any dependency in `targets`.
///
/// Walks the package's own feature graph transitively, because a feature that
/// looks innocuous (`std`) can enable one that is not (`rustc-dep-of-std`).
/// Entries take the forms Cargo allows in a `[features]` value: a bare feature
/// name, `dep:name`, `name/feat`, and `name?/feat` — all of which are reduced to
/// the leading token before comparison.
fn feature_activates_any(
    features: &rustc_hash::FxHashMap<String, Vec<String>>,
    feature: &str,
    targets: &[&str],
) -> bool {
    let mut seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    let mut queue = vec![feature];

    while let Some(current) = queue.pop() {
        if !seen.insert(current) {
            continue;
        }
        let Some(entries) = features.get(current) else {
            continue;
        };
        for entry in entries {
            let token = entry
                .strip_prefix("dep:")
                .unwrap_or(entry)
                .split(['/', '?'])
                .next()
                .unwrap_or("");
            if targets.contains(&token) {
                return true;
            }
            // A token that is also a feature name is followed; one that is only
            // a dependency name is not (its own features are that dependency's
            // business, and it is not a shim or we would have returned above).
            if features.contains_key(token) {
                // `features` outlives this call, so re-borrow the key from the
                // map rather than from the `String` we just sliced.
                if let Some((key, _)) = features.get_key_value(token) {
                    queue.push(key.as_str());
                }
            }
        }
    }
    false
}

/// Build the `CargoConfig` for one `cargo metadata` invocation.
///
/// `features` is a parameter rather than a constant because `load` may have to
/// run twice: once with [`CargoFeatures::All`], and — only if that reveals a
/// feature that would substitute a `rustc-std-workspace-*` shim for `core` —
/// again with an explicit list that omits it. See the comment at that call site
/// for why the detection cannot happen before the first load.
fn build_cargo_config(cfg: &ExtractConfig, features: CargoFeatures) -> CargoConfig {
    let mut cargo = CargoConfig {
        sysroot: Some(RustLibSource::Discover),
        all_targets: false,
        set_test: false,
        no_deps: false,
        // `CargoFeatures::default()` — what this field held implicitly before
        // this change — is `Selected { features: [], no_default_features:
        // false }`: only the crate's `default` feature set. Every item behind a
        // non-default `#[cfg(feature = "…")]` (memchr's `alloc`, serde's `rc`
        // and `derive`, log's `kv`, …) was pruned by rust-analyzer before
        // `lower_workspace` ever walked it — see docs/LIMITATIONS.md L14.
        //
        // `CargoFeatures::All` is the right default here, not
        // `Selected`-with-an-explicit-list:
        //   - We are a *documentation* engine, not a build. We are not trying to
        //     reproduce "what a consumer's `Cargo.toml` would activate" (that is
        //     an infinite family, one per consumer); we are trying to reproduce
        //     "what public API exists", which is the union over every feature
        //     the author gated something behind. `docs.rs` made the identical
        //     call for the identical reason, and is the bar this product is
        //     measured against (see docs/AGENTS-DOCTRINE.md §0).
        //   - The stated risk — enabling mutually-exclusive features together,
        //     or cfg combinations that never occur in a real build — is real,
        //     but it is a *lowering-time* risk (duplicate/contradictory items;
        //     the same class as L4), not a *resolution-time* one: `cargo
        //     metadata` already has to resolve every optional dependency's
        //     version to build a valid `Cargo.lock`, regardless of which
        //     features are later activated for compilation. `All` does not make
        //     an unresolvable offline dependency any more or less resolvable
        //     than the previous default did — confirmed empirically: a plain
        //     `cargo metadata --offline` with *no* `--features` flags at all
        //     already fails identically to `--all-features` for most
        //     multi-dependency fixtures under `result/` (checked directly
        //     with the `cargo metadata` CLI; see the L14 report for the list).
        //     So `All` costs nothing extra offline that `Selected` was not
        //     already paying, and it is strictly more correct once resolution
        //     succeeds.
        //   - A reader who lands on a feature-gated item and finds it inert or
        //     mutually-exclusive with another isn't worse off than not being
        //     shown the item existed at all; `Symbol::cfg` (docs/LIMITATIONS.md L3)
        //     already carries the gating predicate end-to-end to a GUI chip, so
        //     the reader is told *which* feature they would need — the same
        //     disclosure docs.rs makes.
        //
        // The one carve-out is std-integration features: `All` would activate
        // them too, and they are the opposite of "more API" — they swap `core`
        // itself for a stub. `load` strips exactly those and nothing else; see
        // `select_features_excluding_std_integration`.
        features,
        ..CargoConfig::default()
    };
    if cfg.offline {
        cargo
            .extra_env
            .insert("CARGO_NET_OFFLINE".into(), Some("true".into()));
        cargo.extra_args.push("--offline".into());
    }
    cargo
}

fn build_load_config(cfg: &ExtractConfig, run_build_scripts: bool) -> LoadCargoConfig {
    LoadCargoConfig {
        load_out_dirs_from_check: run_build_scripts,
        with_proc_macro_server: ProcMacroServerChoice::Sysroot,
        prefill_caches: false,
        num_worker_threads: cfg.num_threads.max(1),
        proc_macro_processes: 1,
    }
}

fn abs_path(root: &Path) -> Result<AbsPathBuf, Error> {
    let path = if root.is_absolute() {
        root.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| Error::Load(e.into()))?
            .join(root)
    };
    Ok(AbsPathBuf::assert_utf8(path))
}

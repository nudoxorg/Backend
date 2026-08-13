//! The **Linux** compile strategy: produce IR for a package inside an
//! **ephemeral** SmolvmCage (SMOLVM-PLAN §3).
//!
//! This module owns the microVM half of the compile phase. The long-lived
//! compiler daemon the driver used to POST to is gone (there is no
//! `CompileRequest`/`CompileResponse` wire protocol any more); IR is produced by
//! one microVM per job, forked-from-golden for a warm ~250 ms start and torn
//! down after (one-VM-per-job ephemerality).
//!
//! The cage RUNTIME requires Linux KVM (libkrun / the smolvm backend), so this
//! whole module is gated to `target_os = "linux"` by its `mod` declaration in
//! [`super`]. On non-Linux hosts the in-process strategy
//! (`compile_inprocess`, owned by a sibling) takes the compile phase instead;
//! see the `#[cfg]` dispatch in [`super::indexing::Indexer::execute_compile_phase`].
//!
//! The single entry point is [`produce_ir`]: it materializes the staged
//! sanitized sources onto a scratch tree, resolves the toolchain golden for the
//! package's language, forks a warm cage (falling back to a cold boot when live
//! fork is unavailable), runs the language producer inside it, and returns the
//! producer's stdout — a postcard-framed NdIrF1 stream that
//! [`super::indexing::ingest_ir_bytes`] then decodes.

use crate::ecosystem::Language;
use crate::server::error::{InternalError, ServerError, ServerResult};
use crate::server::registry::blob::creation::BlobBuilder;

/// Guest mount point (via the virtiofs tag `ro0`) for the materialized source
/// tree. The cage projects `FsGrant.read_only[0]` to `/mnt/ro0` inside the VM
/// (see `sandbox::smolvm_backend::translate_mounts`).
const GUEST_SOURCE_MOUNT: &str = "/mnt/ro0";

/// Produce IR for `package` inside an ephemeral SmolvmCage and return the raw
/// NdIrF1 stream bytes (the producer's stdout).
///
/// Lifecycle: materialize the staged sources onto a scratch tree (dropped at
/// scope end) → resolve the toolchain-image golden for the package's language
/// (real `ImageDigest` when a [`sandbox::ToolchainImageStore`] is wired, else a
/// deterministic per-language placeholder) → drive the blocking cage boundary
/// off the async reactor via `spawn_blocking` so heartbeats/other jobs keep
/// flowing. The decode of the returned bytes happens in the caller
/// ([`super::indexing::ingest_ir_bytes`]).
pub(crate) async fn produce_ir(
    toolchain_images: Option<&sandbox::ToolchainImageStore>,
    package: &str,
    language: Language,
    builder: &BlobBuilder,
) -> ServerResult<Vec<u8>> {
    // ── Materialize the staged sources onto a scratch tree ────────────────────
    // `execute_extract_phase` already staged the sanitized source files in the
    // builder; write them out once so the cage can mount the tree RO (no second
    // archive pass). Both dirs live under one TempDir that is dropped (deleted)
    // when this scope ends — nothing survives the job on the host.
    let workspace = tempfile::Builder::new()
        .prefix("nudox-compile-")
        .tempdir()
        .map_err(|source| ServerError::Internal(InternalError::MaterializeForCompile { source }))?;
    let source_root = workspace.path().join("src");
    let scratch_root = workspace.path().join("scratch");
    materialize_sources(builder, &source_root, &scratch_root)
        .map_err(|source| ServerError::Internal(InternalError::MaterializeForCompile { source }))?;

    // ── Resolve the toolchain golden + drive the cage (blocking) ──────────────
    let profile = producer_profile(language);
    let image = toolchain_images
        .and_then(|store| store.lookup(profile))
        .map(|img| img.config_digest)
        .unwrap_or_else(|| toolchain_image_digest_placeholder(language));

    let cage_name = package.to_owned();
    let ir_bytes = tokio::task::spawn_blocking(move || {
        run_producer_in_cage(
            &cage_name,
            language,
            profile,
            image,
            &source_root,
            &scratch_root,
        )
    })
    .await
    .map_err(|join| {
        ServerError::Internal(InternalError::CageCompile {
            package: package.to_owned(),
            reason: format!("cage task panicked or was cancelled: {join}"),
        })
    })??;

    Ok(ir_bytes)
}

/// The per-language producer invocation contract.
///
/// This is the **code-level** specification of HOW each producer is invoked.
/// The golden toolchain OCI images (built by the Buck2 compiler tree) fulfill
/// this contract by shipping the producer binary at `entrypoint` and honouring
/// the `args` convention. The cage on the host side merely execs these bytes;
/// the content of the image is a deployment concern, not a code concern.
///
/// # Convention
///
/// Every producer binary follows the same interface:
/// - `--source <path>`  : the RO-mounted source tree inside the guest VM
///                        (always [`GUEST_SOURCE_MOUNT`]).
/// - `--emit ndirf1`    : emit IR on stdout in the NdIrF1 / `ir-stream`
///                        postcard-framed protocol (SMOLVM-PLAN §6.1).
///
/// Language-specific additional flags (e.g. `--edition 2021` for Rust,
/// `--target-jdk 21` for Java) follow after `--emit ndirf1`; they are
/// listed per-language below.
///
/// # Guest paths
///
/// All binary paths are absolute guest paths within the language toolchain
/// image (e.g. `/opt/nudox/rust/bin/nudox-rust-producer`). The host never
/// resolves these paths; the image's `PATH` is irrelevant because the cage
/// execs the binary directly.
struct ProducerInvocation {
    /// Absolute guest path to the producer binary.
    entrypoint: &'static str,
    /// Argv (excluding argv0). Passed verbatim to the cage exec.
    args: &'static [&'static str],
}

/// Return the producer invocation contract for `language`.
///
/// Each arm names the guest entrypoint + the fixed argv the cage passes.
/// The golden toolchain image for that language must ship the binary at this
/// path and must accept this argv without modification.
fn producer_command(language: Language) -> ProducerInvocation {
    match language {
        // Rust: nudox-rust-producer (ra_ap-backed) — reads Cargo sources at
        // GUEST_SOURCE_MOUNT, emits NdIrF1 IR on stdout. `--edition 2021` is
        // the default; the producer overrides it per-crate from Cargo.toml.
        Language::Rust => ProducerInvocation {
            entrypoint: "/opt/nudox/rust/bin/nudox-rust-producer",
            args: &[
                "--source",
                GUEST_SOURCE_MOUNT,
                "--emit",
                "ndirf1",
                "--edition",
                "2021",
            ],
        },

        // Java: nudox-java-producer (Doclet/javac-oracle-backed) — reads
        // Maven/Gradle sources at GUEST_SOURCE_MOUNT, emits NdIrF1.
        // `--target-jdk 21` is the baseline; the producer auto-detects from
        // pom.xml/build.gradle when available.
        Language::Java => ProducerInvocation {
            entrypoint: "/opt/nudox/java/bin/nudox-java-producer",
            args: &[
                "--source",
                GUEST_SOURCE_MOUNT,
                "--emit",
                "ndirf1",
                "--target-jdk",
                "21",
            ],
        },

        // Go: nudox-go-producer (go/types-backed) — reads Go module sources.
        // `--module-root` instructs the producer to treat GUEST_SOURCE_MOUNT
        // as the module root (go.mod must be present at that path).
        Language::Go => ProducerInvocation {
            entrypoint: "/opt/nudox/go/bin/nudox-go-producer",
            args: &[
                "--source",
                GUEST_SOURCE_MOUNT,
                "--emit",
                "ndirf1",
                "--module-root",
                GUEST_SOURCE_MOUNT,
            ],
        },

        // C#: nudox-csharp-producer (Roslyn-backed) — reads .NET sources.
        // `--target-tfm net9.0` is the baseline target framework moniker; the
        // producer overrides it from the .csproj when available.
        Language::CSharp => ProducerInvocation {
            entrypoint: "/opt/nudox/dotnet/bin/nudox-csharp-producer",
            args: &[
                "--source",
                GUEST_SOURCE_MOUNT,
                "--emit",
                "ndirf1",
                "--target-tfm",
                "net9.0",
            ],
        },

        // Nix: nudox-nix-producer (snix-eval-backed) — evaluates the Nix
        // flake at GUEST_SOURCE_MOUNT and emits IR for derivation symbols.
        // `--flake` instructs the producer to look for a `flake.nix` at the
        // source root and evaluate the default package set.
        Language::Nix => ProducerInvocation {
            entrypoint: "/opt/nudox/nix/bin/nudox-nix-producer",
            args: &[
                "--source",
                GUEST_SOURCE_MOUNT,
                "--emit",
                "ndirf1",
                "--flake",
            ],
        },

        // TypeScript: nudox-ts-producer (OXC-backed static parser, LOW tier).
        // OXC operates on the source tree directly; no separate oracle binary.
        Language::Typescript => ProducerInvocation {
            entrypoint: "/opt/nudox/ts/bin/nudox-ts-producer",
            args: &["--source", GUEST_SOURCE_MOUNT, "--emit", "ndirf1"],
        },

        // Python: nudox-python-producer (pyrefly/static-parse-backed, LOW tier).
        Language::Python => ProducerInvocation {
            entrypoint: "/opt/nudox/python/bin/nudox-python-producer",
            args: &["--source", GUEST_SOURCE_MOUNT, "--emit", "ndirf1"],
        },

        // C/C++: nudox-cpp-producer (tree-sitter static parse, LOW tier).
        // No compiler oracle yet; source-only, git-checkout plane (RL-14 §7.4).
        Language::Cpp => ProducerInvocation {
            entrypoint: "/opt/nudox/cpp/bin/nudox-cpp-producer",
            args: &["--source", GUEST_SOURCE_MOUNT, "--emit", "ndirf1"],
        },
    }
}

/// Map a package language onto the sandbox [`sandbox::ProducerProfile`] whose
/// resource ceilings + threat tier the compile runs under.
fn producer_profile(language: Language) -> sandbox::ProducerProfile {
    use sandbox::ProducerProfile;
    match language {
        Language::Rust => ProducerProfile::Rust,
        Language::Java => ProducerProfile::Java,
        Language::Go => ProducerProfile::Go,
        Language::CSharp => ProducerProfile::CSharp,
        Language::Nix => ProducerProfile::Nix,
        // deno_doc (TS) / pyrefly (Python) are LOW static parsers; C/C++ has no
        // dedicated profile yet and reads as a static parse over source.
        Language::Typescript | Language::Python | Language::Cpp => ProducerProfile::StaticParser,
    }
}

/// Fallback toolchain-image digest used when no [`sandbox::ToolchainImageStore`]
/// is wired into the [`super::indexing::Indexer`] (e.g. when images are not yet
/// provisioned on this forge node, or in development without a full OCI image
/// store).
///
/// Returns a deterministic, collision-free-per-language placeholder so
/// `prepare_golden`/`fork_golden` are exercised for real (distinct languages
/// get distinct goldens) without inventing image content. Byte 0 is the first
/// byte of the language token, rest zero.
///
/// The `Indexer::with_toolchain_images` call wires in the real
/// `ToolchainImageStore`; once images are provisioned the store lookup in
/// [`produce_ir`] supersedes this function for every language whose image is
/// registered. This fallback fires only for the not-found case.
fn toolchain_image_digest_placeholder(language: Language) -> sandbox::ImageDigest {
    let mut bytes = [0u8; 32];
    if let Some(&b0) = language.as_token().as_bytes().first() {
        bytes[0] = b0;
    }
    sandbox::ImageDigest::from_bytes(bytes)
}

/// Write the builder's staged source files onto `source_root` (creating parent
/// dirs) and create an empty `scratch_root`. Path traversal is already excluded
/// by ingest sanitization; we defensively skip any absolute / `..` component.
fn materialize_sources(
    builder: &BlobBuilder,
    source_root: &std::path::Path,
    scratch_root: &std::path::Path,
) -> std::io::Result<()> {
    std::fs::create_dir_all(source_root)?;
    std::fs::create_dir_all(scratch_root)?;
    for (rel, bytes) in builder.source_files() {
        let rel_path = std::path::Path::new(rel.as_str());
        // Defensive: never escape the source root.
        if rel_path.is_absolute()
            || rel_path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            continue;
        }
        let dest = source_root.join(rel_path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dest, bytes)?;
    }
    Ok(())
}

/// Fork a warm cage from the language golden (falling back to a fresh cage) and
/// run the sealed producer command inside it, returning the produced IR bytes
/// (the producer's stdout). Synchronous: intended for `spawn_blocking`.
///
/// This is the real cage lifecycle: `prepare_golden` (idempotent) → `fork_golden`
/// → `Cage::run` → teardown (the clone's `kill` runs inside `run`). The producer
/// argv is resolved by [`producer_command`], which specifies the per-language
/// invocation contract that the golden toolchain images fulfil.
fn run_producer_in_cage(
    package: &str,
    language: Language,
    profile: sandbox::ProducerProfile,
    image: sandbox::ImageDigest,
    source_root: &std::path::Path,
    scratch_root: &std::path::Path,
) -> ServerResult<Vec<u8>> {
    use sandbox::vm::{VmError, VmHandle, VmRuntime};
    use sandbox::{
        Cage, CancelToken, CapabilityBudget, Env, FsGrant, NetGrant, RootfsStore, SealedCommand,
        SmolvmCage, SmolvmRuntime,
    };

    let cage_err = |reason: String| {
        ServerError::Internal(InternalError::CageCompile {
            package: package.to_owned(),
            reason,
        })
    };

    // Bootstrap the rootfs store + runtime (NUDOX_GUEST_ROOTFS on a forge node).
    let store = RootfsStore::from_env().map_err(|e| cage_err(e.to_string()))?;
    let runtime = SmolvmRuntime::new(store);

    // ── Golden: prepare (idempotent) then fork a warm clone ───────────────────
    // Falls back to a fresh cold-boot cage when live fork is unavailable on this
    // host (or the golden could not be parked) — the no-silent-degrade rule
    // still holds: only *unsupported fork* falls back; a hard cage failure
    // propagates.
    let handle = match runtime
        .prepare_golden(&image)
        .and_then(|golden| runtime.fork_golden(&golden))
    {
        Ok(handle) => Some(handle),
        Err(VmError::Unsupported { reason }) => {
            tracing::info!(
                %package,
                reason,
                "golden fork unavailable on this host; cold-booting a fresh cage"
            );
            None
        }
        Err(other) => return Err(cage_err(format!("golden prepare/fork: {other}"))),
    };

    // ── Budget: RO source root, ephemeral scratch overlay, network OFF ────────
    // By design (sealed-image plane): the toolchain roots (rustup/cargo/GOROOT/…)
    // live INSIDE the golden guest image, so there are NO host-side RO toolchain
    // binds — only the package source is bound RO. A host-bind toolchain plane
    // would extend this `FsGrant`; the golden-image plane does not need it.
    let fs = FsGrant::scratch(scratch_root).ro(source_root);
    let budget = CapabilityBudget::new(fs, NetGrant::Off, Env::empty(), profile.limits());

    // ── The producer invocation ───────────────────────────────────────────────
    // `producer_command` specifies the per-language invocation contract (guest
    // binary path + argv) that the golden toolchain OCI image fulfils. The cage
    // execs this verbatim; the binary inside the image reads the RO source tree
    // at GUEST_SOURCE_MOUNT and streams NdIrF1 IR frames on its stdout.
    let inv = producer_command(language);
    let command = SealedCommand::new(inv.entrypoint, inv.args.iter().copied(), budget);

    let cancel = CancelToken::never();
    let output = match handle {
        // Warm fork clone: run directly against the live handle, then tear down.
        Some(mut handle) => {
            let spec = sandbox::project_run_spec(&command);
            let out = handle.exec(&spec);
            handle.kill(); // one-VM-per-job: the clone dies at end of job
            out.map_err(|e| cage_err(format!("producer exec (forked clone): {e}")))?
        }
        // Cold path: the cage's own `run` boots a fresh VM, execs, and tears
        // it down (ephemeral overlay dies with it).
        None => {
            let cage = SmolvmCage::with_runtime(runtime);
            Cage::run(&cage, command, &cancel)
                .map_err(|e| cage_err(format!("cage run (cold boot): {e}")))?
        }
    };

    if !output.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ServerError::Internal(InternalError::ProducerFailed {
            package: package.to_owned(),
            reason: format!(
                "exit {:?}; stderr: {}",
                output.status(),
                stderr.chars().take(2000).collect::<String>()
            ),
        }));
    }

    Ok(output.stdout)
}

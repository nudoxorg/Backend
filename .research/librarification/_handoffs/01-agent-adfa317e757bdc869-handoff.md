# Handoff: Audit compiler crate exhaustively
- Transcript: `/Users/philocalyst/.claude/projects/-Users-philocalyst-Projects-Backend/befc9920-dc9d-40dc-b94e-b3a2d2ed39ad/subagents/agent-adfa317e757bdc869.jsonl`
- Agent id: `agent-adfa317e757bdc869`
- Deliverable: `/Users/philocalyst/Projects/Backend/.research/librarification/01-compiler-audit.md`
- Existing report: True (757 lines)
- Tools used: 64 ({'Bash': 10, 'Read': 53, 'Write': 1})
- Files/greps touched: 53
- Web queries: 0

## Web queries performed

## Files read / grepped (sample)
- /Users/philocalyst/Projects/Backend/workspace/compiler/BUCK
- /Users/philocalyst/Projects/Backend/workspace/compiler/README.md
- /Users/philocalyst/Projects/Backend/workspace/compiler/lib.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/error.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/protocol.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/compile/mod.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/compile/producer/mod.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/compile/producer/runtime.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/compile/isolate.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/generate
- /Users/philocalyst/Projects/Backend/workspace/compiler/generate/mod.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/generate/surface.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/generate/blob_info.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/generate/cst.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/generate/source_archive.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/generate/linked_data/mod.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/generate/linked_data/emit.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/graph
- /Users/philocalyst/Projects/Backend/workspace/compiler/graph/mod.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/graph/model.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/graph/from_ir.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/graph/link.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/daemon
- /Users/philocalyst/Projects/Backend/workspace/compiler/daemon/mod.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/bin/compiler_daemon.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/daemon/forge.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/compile/rust/producer.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/compile/rust/mod.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/compile/go/producer.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/compile/typescript/oxc/mod.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/compile/python/producer.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/compile/nix/producer.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/compile/csharp/producer.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/compile/java/producer.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/compile/typescript/mod.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/compile/typescript/producer.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/treesitter/mod.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/sandbox/lib.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/sandbox/cage.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/sandbox/worker.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/sandbox/probe.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/generate/parse_cache.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/generate/occurrences.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/sandbox/toolchains.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/compile/producer/scratch.rs
- /Users/philocalyst/Projects/Backend/workspace/heart/connection.rs
- /Users/philocalyst/Projects/Backend/workspace/heart/lib.rs
- /Users/philocalyst/Projects/Backend/workspace/heart/content.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/compile/producer/wire.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/sandbox/budget.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/compile/rust/ra/load.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/compile/go/oracle.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/compile/java/oracle.rs

## Assistant synthesis fragments (inert history — verify)

### fragment

I'll conduct an exhaustive audit of the workspace/compiler and related modules. Let me start by exploring the codebase structure broadly.

### fragment

Now I have enough information to write the comprehensive audit report. Let me check a few more things quickly:

## ORIGINAL PROMPT (complete this mission)

You are a research subagent producing an exhaustive codebase audit that feeds a major architecture plan.

CONTEXT — the nudox project (repo: /Users/philocalyst/Projects/Backend):
nudox is a multi-language code-intelligence / documentation platform. Current pipeline: package source is acquired, a per-language "producer" (oracle compilers: rust-analyzer/rustdoc, OXC for TypeScript, Pyrefly for Python, go/types, a javadoc doclet, Roslyn for C#, snix for Nix) lowers it to a shared IR (workspace/ir), emitted as blobs into a content-addressed store, then fanned out to derived stores: Tantivy (text search), Qdrant (vectors), TerminusDB (graph), coordinated by Postgres (global index + job queue + outbox) behind an axum server (workspace/server) with service logic in workspace/registry. A GPUI desktop app (workspace/gui) is the developer-facing product.

THE PLANNED RESTRUCTURE ("librarification"):
1. `compiler` becomes dual-form. As a BINARY it is a stateless, sandboxed, self-contained server: takes sealed inputs, produces IR + blobs, and holds as little state as possible — deployed as a Kubernetes fleet for UNTRUSTED packages (third-party deps). As a LIBRARY it embeds in a client and compiles only TRUSTED code: the developer's own project, its path dependencies, its immediate workspace.
2. `server` becomes `client` — a library the GUI consumes, abstracting remote-vs-local compute behind traits/typestates (precedent: heart::Connect with Cold/Live typestates).
3. Remote side = INDEX (sqlite + S3, k8s-orchestrated); local side = REGISTRY (sqlite + disk blobs + embedded search/vector). Both store IR + resolved tree-sitter trees + source efficiently.
4. Postgres dropped entirely; queue → k8s primitives; global index → sqlite on remote.
5. TerminusDB only for hottest packages (leaky-bucket admission); everything else does graph ops over IR loaded from blobs.
6. Whole pipeline becomes deeply incremental: on a committed change, only changed symbols get re-compiled/re-embedded/re-indexed.

YOUR MISSION — exhaustively audit workspace/compiler (the live tree; root-level /Users/philocalyst/Projects/Backend/compiler is LEGACY — consult only to clarify intent). Read every module: lib.rs, error.rs, protocol.rs, bin/, daemon/, sandbox/ (Cage trait, backends, WorkerPool), compile/producer/ (the Producer trait + plan→cage/worker→decode lifecycle), each language under compile/ (rust, typescript, python, go, java, csharp, nix), generate/ (parse cache, producer dispatch, linked_data, tar), graph/ (IR→TerminusDB lowering: model.rs, from_ir.rs, link.rs), render/, treesitter. Also read workspace/heart (identity types, Connect typestates, cache) as it's the shared vocabulary.

Answer these questions with file:line evidence:
1. Full pipeline map: from "here is a package source archive/path" to "IR + blobs emitted" — every stage, every type crossing stage boundaries, every serialization format (serde? facet? which blobs exist, what are their schemas/names?).
2. State inventory: every piece of ambient state the compiler touches — filesystem paths, env vars, process globals, network access, toolchain discovery, temp dirs, caches, database/object-store handles. Which of these block (a) embedding as a pure library, (b) running as a stateless sealed server binary?
3. The daemon/ and protocol.rs: what exists of the daemon/server form today (a DAEMON-PLAN existed: SealedInput/Cage/Cas/Producer/ForgeRuntime concepts)? What's implemented vs stubbed?
4. Producer-by-producer: which run in-process (library-form) vs subprocess/toolchain-required? What toolchains/binaries does each need at runtime? Which could run embedded in a desktop app on a developer machine, and what's the cost (binary size, build deps)?
5. Sandbox: how Cage/Policy/WorkerPool work; Linux-only? What happens on macOS (the GUI targets developer machines)? What would "trusted mode = no sandbox" look like?
6. The graph/ lowering to TerminusDB documents: what the document model is, whether it could be evaluated directly over IR blobs WITHOUT TerminusDB (for the cold-package path).
7. treesitter integration: how CSTs are produced/stored today; is there any tree serialization, or are trees reparsed on demand?
8. Propose the concrete crate split for dual-form: what goes in a `compiler-core` library (pure, no ambient state), what in the binary shell, what the library-facing API should look like (sketch the top-level types/functions/traits with attention to existing names), and what refactors are required to get there. Be specific: list the actual modules/functions that need to move or change signature.
9. Incrementality readiness: where content hashes already exist in the pipeline, where determinism holds/breaks (timestamps, ordering, absolute paths), what would be needed for symbol-level change detection between two generations of the same package.

DELIVERABLE:
- Write your FULL report as dense markdown to /Users/philocalyst/Projects/Backend/.research/librarification/01-compiler-audit.md (Write tool; parent dirs are created automatically). Be exhaustive — the report is the artifact; typically 600-1500 lines. Cite file:line for every load-bearing claim.
- Do NOT attempt to build anything; pre-existing build failures are known and out of scope.
- Your final reply must contain ONLY: (a) a 300-600 word executive summary, (b) top concrete recommendations as bullets (exact modules to move, API sketch names), (c) open questions/risks. Do not repeat the full report in the reply.


## EXTRACTED TOOL RESULTS (stale — re-verify key facts)

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/BUCK
```
1	load("//build:rust.bzl", "crate", "deps", "member", "nudox_toolchain_test_env", "rust_bin", "rust_crate", "rust_tests", "target")
2	
3	rust_crate(
4	    name = "compiler",
5	    srcs = native.glob(
6	        ["**/*.rs"],
7	        exclude = ["tests/**", "bin/**"],
8	    ),
9	    resources = {
10	        "go-oracle": "//workspace/compiler/compile/go/oracle:oracle",
11	        "java-oracle.jar": "//workspace/compiler/compile/java/oracle:extractor",
12	        # Pulls in the dotnet-published oracle.dll directory (CSHARP-PLAN §1.3).
13	        # `buck_resource("csharp-oracle")` in producer.rs resolves this at runtime.
14	        "csharp-oracle": "//workspace/compiler/compile/csharp/oracle:oracle",
15	    },
16	    deps = deps([
17	        member("heart"),
18	        member("ir"),
19	        member("sandbox"),
20	        crate("serde"),
21	        crate("serde_json"),
22	        crate("anyhow"),
23	        crate("thiserror"),
24	        crate("tracing"),
25	        crate("bytes"),
26	        crate("flate2"),
27	        crate("sha2"),
28	        crate("tar"),
29	        crate("reqwest"),
30	        crate("rustc_hash"),
31	        crate("semver"),
32	        crate("smol_str"),
33	        # C# XML-doc parsing (replaces hand-rolled element scanner).
34	        crate("quick-xml"),
35	        # rust-analyzer producer (ra_ap_* 0.0.341; see RUST-ANALYZER-PLAN.md)
36	        crate("ra_ap_load_cargo"),
37	        crate("ra_ap_project_model"),
38	        crate("ra_ap_hir"),
39	        crate("ra_ap_ide_db"),
40	        crate("ra_ap_base_db"),
41	        crate("ra_ap_vfs"),
42	        crate("ra_ap_syntax"),
43	        crate("ra_ap_paths"),
44	        crate("ra_ap_cfg"),
45	        crate("ra_ap_proc_macro_api"),
46	
47	        crate("arborium"),
48	        crate("arborium_tree_sitter"),
49	        crate("uv_pep440"),
50	        crate("uv_pep508"),
51	        crate("terminusdb_schema"),
52	        crate("terminusdb_schema_derive"),
53	        crate("pyrefly"),
54	        crate("pyref
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/README.md
```
1	# Compiler
2	
3	*Handles the compilation into:*
4	- **Treesitter CST**: For resolution of implementation
5	- **Surface IR**: For resolution of function and structure contracts
6	- **Tarred Source**: For source referencing
7	
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/lib.rs
```
1	//! Compiler — handles the compilation of a package into the three resolutions
2	//! we care about:
3	//!
4	//! - **Treesitter CST** (`treesitter`): for resolution of implementation.
5	//! - **Surface IR** (the `ir` crate + `parse`): for resolution of function and
6	//!   structure contracts.
7	//! - **Tarred Source** (`tar`): for source referencing.
8	//!
9	//! The processed output is handed to `linked_data` for graph-store emission.
10	
11	pub mod error;
12	pub mod generate;
13	/// Wire contract with the Cargo-side `server` (formerly the standalone
14	/// `protocol` crate; its Cargo-side twin lives at `registry::protocol`).
15	pub mod protocol;
16	pub mod graph;
17	pub mod compile;
18	pub use crate::compile as languages;
19	pub mod render;
20	pub mod treesitter;
21	pub mod daemon;
22	
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/error.rs
```
1	//! The compiler's error taxonomy for the generation pipeline.
2	
3	use thiserror::Error;
4	
5	/// Concrete failures from the linked-data emit phase (no more dyn boxes).
6	#[derive(Debug, Error)]
7	pub enum EmitLinkedDataError {
8		#[error("conflicting document bodies for @id {id}")]
9		ConflictingDocumentBody { id: String },
10	}
11	
12	/// A failure while generating the resolutions (surface IR, CST, source archive,
13	/// blob info, linked data) for a package.
14	///
15	/// Lower/Emit no longer use `Box<dyn Error>`; every language backend and
16	/// emit failure is carried by a named concrete variant so sources are
17	/// explicitly chained and downcasting / matching is possible without
18	/// type erasure.
19	#[derive(Debug, Error)]
20	pub enum GenerateError {
21		/// Rust source lowering to IR failed (cargo/rustdoc + rustdoc JSON parse).
22		#[error("failed to lower Rust source to IR")]
23		LowerRust(#[from] crate::languages::rust::Package),
24	
25		/// TypeScript source lowering to IR failed (deno-graph + deno-doc).
26		#[error("failed to lower TypeScript source to IR")]
27		LowerTypescript(#[from] crate::languages::typescript::Package),
28	
29		/// Go module lowering to IR failed.
30		#[error("failed to lower Go source to IR")]
31		LowerGo(#[from] crate::languages::go::GoError),
32	
33		/// Java project lowering to IR failed.
34		#[error("failed to lower Java source to IR")]
35		LowerJava(#[from] crate::languages::java::JavaError),
36	
37		/// Nix flake lowering to IR failed (FlakeHub acquisition, static analysis,
38		/// or hermetic evaluation).
39		#[error("failed to lower Nix source to IR")]
40		LowerNix(#[from] crate::languages::nix::NixError),
41	
42		/// Extracting the CST for a file failed.
43		#[error("failed to extract CST for {path}")]
44		Cst {
45			/// The offending source file.
46			path: String,
47			/// The underlying parse error (now may carry its own source file +
48			/// the original LanguageError via source chain).
49			#[source]
50			sou
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/protocol.rs
```
1	//! Wire contract between the Cargo-side `server` and the Buck2 compiler daemon.
2	//!
3	//! The Buck2-side copy of the wire types, owned by the `compiler` crate and used
4	//! by its daemon binary (`bin/compiler_daemon.rs`). The Cargo consumer (`server`)
5	//! reaches the identical source through `registry::protocol`; the two copies are
6	//! kept byte-identical on purpose so both ends agree on the postcard layout.
7	//!
8	//! ## HTTP transport
9	//!
10	//! `POST /compile` is **postcard** (`Content-Type: application/x-postcard`), not
11	//! JSON. The daemon returns IR surface bytes; the *server* emits those into the
12	//! content-addressed blob store (`registry::blob::emit`) — the daemon does not
13	//! write blobs itself.
14	//!
15	//! ## Wire layout stability
16	//!
17	//! [`WireReference`] and [`WireFile`] are the postcard-serializable mirrors of
18	//! `ir::syntax::ResolvedReference`.  Their field shapes and the
19	//! `ReferenceKind` discriminant table are intentionally copied (not moved) from
20	//! `registry/blob/mod.rs` so both the server and the compiler daemon agree on
21	//! the byte layout.  The duplication will be reconciled in Wave 5 when
22	//! `registry` is merged into `server`.
23	
24	use serde::{Deserialize, Serialize};
25	
26	// ─────────────────────────────────────────────────────────────────────────────
27	// Error type
28	// ─────────────────────────────────────────────────────────────────────────────
29	
30	/// Errors that can arise while encoding or decoding protocol wire types.
31	#[derive(Debug, thiserror::Error)]
32	pub enum ProtocolError {
33	    /// A `WireReference::span_start > span_end`, which is physically impossible.
34	    #[error("reference span is inverted (start > end)")]
35	    InvertedReferenceSpan,
36	
37	    /// A `WireReference::kind` byte did not map to any known `ReferenceKind`.
38	    #[error("unknown reference-kind discriminant on wire: {wire}")]
39	    UnknownReferenceKindDiscriminant { wire: u8 },
40	
41	    /// A
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/compile/mod.rs
```
1	//! The per-language compiler backends. Each lowers its toolchain's
2	//! documentation form into the shared `ir::Index`, and resolves versions over
3	//! the language's VCS/manifest conventions (`traversal`).
4	//!
5	//! [`producer`] is the shared shape (DAEMON-PLAN §2.3): plan → cage/worker → decode.
6	
7	/// C# (.NET / NuGet) producer: Roslyn oracle → IR.
8	pub mod csharp;
9	pub mod go;
10	/// Sandbox seam for external toolchains and worker-isolated interpreters.
11	pub mod isolate;
12	pub mod java;
13	pub mod nix;
14	/// Producer trait + shared substrate + one dispatch path.
15	pub mod producer;
16	pub mod python;
17	pub mod rust;
18	pub mod typescript;
19	/// Shared `gix` plumbing (clone/fetch/walk/materialize) behind the
20	/// per-language `traversal` modules.
21	pub mod vcs;
22	
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/compile/producer/mod.rs
```
1	//! Producers as pure(ish) functions of [`SealedInput`] (DAEMON-PLAN §2.3).
2	//!
3	//! One trait, six thin impls, one dispatch path:
4	//!
5	//! ```text
6	//! plan(SealedInput) → ExecPlan
7	//!   Commands  → cage/shim run each SealedCommand
8	//!   Library   → WorkerPool (prod) / in-process (dev fallback)
9	//! decode(Captured) → ProducerOutput { index, aux }
10	//! ```
11	//!
12	//! Adaptive multi-step toolchains (Rust multi-crate, Java javac+javadoc) override
13	//! [`Producer::produce`] and return [`ProducerError::Plan`] / [`ProducerError::Decode`]
14	//! from `plan`/`decode` so introspection never sees a hollow empty command list.
15	
16	mod resource;
17	pub mod runtime;
18	mod scratch;
19	mod wire;
20	
21	pub use resource::buck_resource;
22	pub use runtime::{
23		ForgeContext, LocalForgeContext, cache_get_or_build, execute_plan, run_producer,
24	};
25	pub use sandbox::SandboxKey;
26	pub use scratch::Scratch;
27	pub use wire::{
28		FsPathParent, PathParent, apply_members, apply_members_to_index, members_by_parent,
29		wire_index_members, wire_members,
30	};
31	
32	use std::collections::HashMap;
33	use std::path::{Path, PathBuf};
34	use std::process::ExitStatus;
35	use std::time::Duration;
36	
37	use heart::{JobKey, Language};
38	use ir::entry::Index;
39	use sandbox::{
40		CancelToken, Captured, Env, Mounts, ProcessEnd, ProducerProfile, SealedCommand,
41		SealedInput, ToolchainSet, WorkerLang,
42	};
43	use serde::{Deserialize, Serialize};
44	use thiserror::Error;
45	
46	use crate::compile::isolate::{
47		IsolatedFailure, IsolatedFailureKind, package_tree_binds,
48	};
49	
50	// ─── Identity & policy ───────────────────────────────────────────────────────
51	
52	/// Versioned producer identity — part of `JobKey` once CAS wiring lands.
53	#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
54	pub struct ProducerId(pub &'static str);
55	
56	impl std::fmt::Display for ProducerId {
57		fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
58			f.write_
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/compile/producer/runtime.rs
```
1	//! The injected compile-plane context (DAEMON-PLAN §2.5).
2	//!
3	//! [`ForgeContext`] replaces every compile-plane process global: the cage, the
4	//! CAS, the toolchain set, the override table, the observer, and the node
5	//! identity are all borrowed from an owned runtime instead of read from
6	//! `OnceLock`s or the environment.
7	//!
8	//! [`run_producer`] is the sole cache client: `cas.get` → hit (decode) or miss
9	//! (run under the cage / in-process, then `cas.put`), with observer events on
10	//! the boundary.
11	
12	use bytes::Bytes;
13	use heart::cache::{Cas, EvictableCas, Tiered};
14	use heart::ContentHash;
15	use sandbox::{
16		Cage, ForgeObserver, NodeId, OverrideTable, SealedInput, ToolchainSet, WorkerLang,
17		WorkerPool,
18	};
19	use serde::{Deserialize, Serialize};
20	
21	use super::{Producer, ProducerError, ProducerOutput};
22	
23	/// Borrowed compile-plane capabilities, injected by the server's `ForgeRuntime`.
24	///
25	/// The `compiler` crate reads nothing from process globals or the environment;
26	/// everything policy-relevant arrives through this trait.
27	pub trait ForgeContext: Send + Sync {
28		/// The concrete content-addressed store this context owns.
29		///
30		/// A fully-generic associated type (not a boxed `dyn`): the CAS is the sole
31		/// erasure point in the design, and it is erased *statically*. The
32		/// [`EvictableCas`] bound lets the poison-repair path invalidate a wrong
33		/// entry without any object-safety gymnastics.
34		type Cas: Cas + EvictableCas;
35	
36		/// Stable node identity (scratch / cgroup naming).
37		fn node(&self) -> &NodeId;
38	
39		/// The isolation cage sealed commands run under.
40		fn cage(&self) -> &dyn Cage;
41	
42		/// The content-addressed store (sole cache).
43		///
44		/// Returns the concrete `&Self::Cas`. Because this names an associated type,
45		/// `ForgeContext` is consumed *generically* (`fn f<C: ForgeContext>(ctx: &C)`)
46		/// rather than through a `&dyn ForgeContext` trait obje
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/compile/isolate.rs
```
1	//! Seam F: build isolation inputs for external producer toolchains.
2	//!
3	//! Producers keep their language-specific logic; this module projects an
4	//! [`IsolatedCommand`] builder into a [`sandbox::SealedCommand`] under the
5	//! injected [`ForgeContext`]'s toolchains, and runs it through `ctx.cage()`.
6	//!
7	//! There are no process globals here anymore: toolchains, the cage, and the
8	//! override table all arrive through the context.
9	
10	use std::ffi::OsString;
11	use std::path::{Path, PathBuf};
12	use std::process::Output as StdOutput;
13	
14	use sandbox::{
15		CancelToken, Env, KillReason, LimitOverride, Mounts, Network, ProducerProfile, SealedCommand,
16		Sealer,
17	};
18	
19	use crate::compile::producer::ForgeContext;
20	
21	/// Map a sandbox kill / denial into a displayable process-style failure.
22	#[derive(Debug)]
23	pub struct IsolatedFailure {
24		/// Command label for error messages.
25		pub command: String,
26		/// Structured reason.
27		pub kind: IsolatedFailureKind,
28		/// Captured stdout (capped).
29		pub stdout: Option<String>,
30		/// Captured stderr (capped).
31		pub stderr: Option<String>,
32	}
33	
34	/// Why isolation failed or the guest was killed.
35	#[derive(Debug)]
36	pub enum IsolatedFailureKind {
37		/// Wall / CPU / OOM / output / pids ceiling.
38		Resource(KillReason),
39		/// Non-zero exit.
40		NonZero {
41			/// Exit status display.
42			status: String,
43		},
44		/// Sandbox refused or could not spawn.
45		Sandbox(String),
46		/// Toolchain binary missing.
47		ToolchainMissing(String),
48	}
49	
50	impl std::fmt::Display for IsolatedFailure {
51		fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
52			write!(f, "isolated `{}` failed: {}", self.command, self.kind)
53		}
54	}
55	
56	impl std::error::Error for IsolatedFailure {}
57	
58	impl std::fmt::Display for IsolatedFailureKind {
59		fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
60			match self {
61				Self::Resource(r) => write!(f,
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/generate
```
EISDIR: illegal operation on a directory, read '/Users/philocalyst/Projects/Backend/workspace/compiler/generate'
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/generate/mod.rs
```
1	//! Generate — the "runs computer" half of the indexing flow: take a materialized
2	//! package and produce the resolutions that become a blob and the graph
3	//! documents.
4	//!
5	//! - [`surface`]: the API surface (`ir::Index`), lowered by `crate::languages`;
6	//! - [`cst`]: the concrete-syntax-tree resolution (tree-sitter) as *serializable*
7	//!   resolved-reference spans (the live tree is transient — see [`cst`]);
8	//! - [`source_archive`] + [`tar`]: the condensed, content-addressed source;
9	//! - [`blob_info`]: the canonical [`heart::Generation`] + per-file digests the
10	//!   registry blob layer consumes;
11	//! - [`linked_data`]: streamed JSON-LD graph documents for the graph store.
12	//!
13	//! The output is aligned with the registry's content-addressed blob model: the
14	//! same per-file [`heart::ContentHash`]es and the canonical [`heart::Generation`]
15	//! the registry keys storage and freshness on.
16	
17	pub mod blob_info;
18	pub mod cst;
19	pub mod linked_data;
20	pub mod occurrences;
21	pub mod parse_cache;
22	pub mod resolve;
23	pub mod source_archive;
24	pub mod surface;
25	pub mod tar;
26	
27	use std::path::PathBuf;
28	
29	use heart::{ContentHash, JobKey, Toolchain};
30	use heart::package::Coordinates as PackageCoordinates;
31	use ir::entry::Index;
32	use ir::syntax::OccurrenceSet;
33	
34	pub use blob_info::BlobInfo;
35	pub use cst::CstSet;
36	pub use source_archive::{FileDigest, SourceArchive};
37	
38	use crate::generate::resolve::RESOLVER_VERSION;
39	
40	use crate::compile::producer::{self, ForgeContext, LocalForgeContext};
41	use crate::error::GenerateError;
42	
43	/// A materialized package ready to generate from: its verified identity, the
44	/// toolchain it was resolved against, and the root of its extracted source.
45	pub struct PackageInput {
46		/// The package's canonical coordinates (origin × name × version).
47		pub coordinates: PackageCoordinates,
48		/// The toolchain the source was resolved/analyzed against.
49		pub toolch
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/generate/surface.rs
```
1	//! Generating the API surface resolution (the `ir::Index`) for a package.
2	//!
3	//! One dispatch path for all six languages (DAEMON-PLAN §2.3):
4	//!
5	//! ```text
6	//! PackageInput → seal → run_producer (cas.get → cage/worker → cas.put)
7	//! ```
8	
9	use heart::{Language, PackageVersion, RegistryOrigin};
10	use ir::{
11		entry::Index,
12		pipeline::{Collected, Ir},
13	};
14	
15	use crate::compile::producer::{self, ForgeContext, Producer, SandboxKey};
16	use crate::{
17		error::GenerateError,
18		generate::{PackageInput, parse_cache},
19		languages::{
20			csharp::CSharpProducer, go::GoProducer, java::JavaProducer, nix::NixProducer,
21			python::PythonProducer, rust::RustProducer, typescript::TypescriptProducer,
22		},
23	};
24	
25	/// Lower a package's source into the collected IR, dispatching on ecosystem.
26	pub fn collect<C: ForgeContext>(
27		ctx: &C,
28		input: &PackageInput,
29	) -> Result<Ir<Collected>, GenerateError> {
30		let out = run_producer(ctx, input)?;
31		Ok(Ir::from_entries(
32			out.index.entries_by_path.into_values().collect(),
33		))
34	}
35	
36	/// Lower a package's source into the indexed API surface (the `ir::Index`).
37	pub fn build<C: ForgeContext>(ctx: &C, input: &PackageInput) -> Result<Index, GenerateError> {
38		Ok(collect(ctx, input)?.index().into_index())
39	}
40	
41	/// The per-package [`SandboxKey`] for override resolution.
42	fn package_key(input: &PackageInput) -> SandboxKey {
43		SandboxKey::package(
44			input.coordinates.origin.token().into_owned(),
45			input.coordinates.name.original().to_string(),
46			input.coordinates.version.canonical(),
47		)
48	}
49	
50	/// Seal + run the producer for the package's language — the only surface dispatch.
51	///
52	/// [`producer::run_producer`] is the sole cache client: the sealed input's job
53	/// key drives `ctx.cas().get` (hit → decoded [`ProducerOutput`]) / miss (run,
54	/// then `put`). Scratch lives in [`producer::SealedPackage`] (RAII).
55	fn run_producer<C: ForgeContext>(

```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/generate/blob_info.rs
```
1	//! Assembling the sink-ready blob information from the generated resolutions.
2	//!
3	//! This is the bridge to the registry's content-addressed blob model: it folds
4	//! the per-file digests into the canonical snapshot [`heart::ContentHash`] — the
5	//! sorted fold of per-file hashes — that postgres records as a package's identity
6	//! and freshness key, and carries the leaf references (surface IR, CST spans) the
7	//! blob manifest points at.
8	
9	use heart::content::ContentHash;
10	use ir::entry::Index;
11	
12	use crate::generate::{cst::CstSet, source_archive::SourceArchive};
13	
14	/// The assembled blob information the registry blob layer consumes: the
15	/// content-addressed source archive plus the canonical package snapshot hash.
16	pub struct BlobInfo {
17		/// The source archive (per-file content hashes, sorted by path).
18		pub archive: SourceArchive,
19		/// The canonical content hash of the whole package snapshot.
20		pub snapshot: ContentHash,
21	}
22	
23	impl BlobInfo {
24		/// Fold the generated resolutions into the canonical snapshot [`ContentHash`]
25		/// and assemble the blob info.
26		///
27		/// The hash is a deterministic fold: per-file `(path, hash)` pairs in sorted
28		/// order streamed through a [`ContentHasher`], so the same package snapshot
29		/// always yields the same hash on any machine.
30		pub fn assemble(surface: &Index, cst: &CstSet, archive: SourceArchive) -> Self {
31			// The surface and CST are derived deterministically from the same
32			// source bytes the archive digests, so they carry no extra identity;
33			// they ride along only for the manifest's leaf references.
34			let _ = (surface, cst);
35	
36			let mut hasher = ContentHash::builder();
37			for file in &archive.files {
38				// Length-prefix each path so the (path, hash) stream is injective —
39				// no split point ambiguity between neighbouring entries.
40				let path = file.path.to_string_lossy();
41				hasher.update(&(path.len() as u64).to_le_bytes());
4
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/generate/cst.rs
```
1	//! Generating the concrete-syntax-tree resolution (tree-sitter) for a package.
2	//!
3	//! The live tree-sitter `Tree` is C-allocated and non-serializable, and one tree
4	//! per package is the wrong granularity. So the CST resolution carried forward is
5	//! *per-file* and reduced to the serializable data the rest of the system needs:
6	//! the [`ResolvedReference`] spans that link identifiers to IR entries. The tree
7	//! itself is transient — parsed, walked for references, and dropped.
8	
9	use std::{fs, path::PathBuf};
10	
11	use arborium_tree_sitter as tree_sitter;
12	use heart::Language;
13	use ir::syntax::{walk_references, ParseError, ResolvedReference};
14	
15	use crate::{error::GenerateError, generate::PackageInput, treesitter::classify_for};
16	
17	/// One source file's CST resolution: the resolved reference spans extracted from
18	/// it. Serializable and self-contained — no live tree.
19	#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
20	pub struct Cst {
21		/// The file, relative to the package root.
22		pub path: PathBuf,
23		/// The references resolved within it, in source order.
24		pub references: Vec<ResolvedReference>,
25	}
26	
27	/// The per-file CST resolution for a whole package.
28	#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
29	pub struct CstSet {
30		/// One entry per parsed source file, sorted by path for reproducibility.
31		pub files: Vec<Cst>,
32	}
33	
34	/// Map a language + file extension to an arborium grammar name.
35	///
36	/// Returns `None` when the extension is not a source file for that language.
37	fn grammar_for_extension(lang: Language, ext: &str) -> Option<&'static str> {
38		match lang {
39			Language::Rust if ext == "rs" => Some("rust"),
40			Language::Python if ext == "py" => Some("python"),
41			Language::Typescript => match ext {
42				"ts" | "mts" | "cts" => Some("typescript"),
43				"tsx" => Some("tsx"),
44				// Optional JS companions of a TypeS
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/generate/source_archive.rs
```
1	//! The condensed source resolution: a content-addressed set of the package's
2	//! source files.
3	//!
4	//! Rather than one opaque tar blob, each file is hashed individually (BLAKE3 via
5	//! [`heart::ContentHash`]) so the registry can content-address, dedupe across
6	//! versions, and serve ranged reads. A tar view is still producible on demand
7	//! (see [`super::tar`]); it is not the stored representation.
8	
9	use std::{fs, io::BufRead, path::{Path, PathBuf}};
10	
11	use heart::content::ContentHash;
12	
13	use crate::{error::GenerateError, generate::PackageInput};
14	
15	/// One source file, content-addressed. Mirrors the registry's `FileEntry`, so
16	/// the generated archive maps directly onto the stored blob manifest.
17	#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
18	pub struct FileDigest {
19	    /// The file path, relative to the package root.
20	    pub path: PathBuf,
21	
22	    /// The BLAKE3 hash of the file's exact bytes.
23	    pub hash: ContentHash,
24	
25	    /// The file size in bytes.
26	    pub size: u64,
27	}
28	
29	/// The content-addressed source archive: every source file's digest, sorted by
30	/// path for a reproducible manifest fingerprint.
31	#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
32	pub struct SourceArchive {
33	    /// The per-file digests, sorted by path.
34	    pub files: Vec<FileDigest>,
35	}
36	
37	/// Walk the package source, hashing each file as it is read (bounded memory —
38	/// files stream through the hasher, never all held at once) into a sorted set of
39	/// [`FileDigest`]s.
40	///
41	/// Runs on a blocking pool (filesystem + hashing are sync CPU/IO work).
42	pub fn build(input: &PackageInput) -> Result<SourceArchive, GenerateError> {
43	    let mut files = Vec::new();
44	    let mut pending = vec![input.root.clone()];
45	
46	    while let Some(dir) = pending.pop() {
47	        for entry in fs::read_dir(&dir)? {
48	            let entry = entr
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/generate/linked_data/mod.rs
```
1	//! Processing the IR into linked data (JSON-LD) for the graph store.
2	//!
3	//! The document *schema* is derived from the graph model (`crate::graph`) via
4	//! `terminusdb-schema-derive`; this module owns the [`schema`] context and the
5	//! streaming [`emit`] pipeline that projects an `ir::Index` into graph documents
6	//! and ships them to a sink in bounded batches (never a whole-corpus `Vec`).
7	
8	pub mod emit;
9	pub mod schema;
10	
11	pub use emit::{DocumentSink, emit};
12	
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/generate/linked_data/emit.rs
```
1	//! The emission pipeline: project an `ir::Index` into graph documents and stream
2	//! them to a sink.
3	//!
4	//! The index is projected to a [`crate::graph::from_ir::GraphCorpus`] via
5	//! [`crate::graph::from_ir::project`], then serialized to JSON-LD documents and
6	//! handed to the [`DocumentSink`] in the wave order the graph store's
7	//! referential integrity expects (GRAPH-ARCHITECTURE.md §5): packages → bare
8	//! symbols → symbols with links → reified relations → version membership.
9	
10	use std::collections::HashMap;
11	use std::hash::{Hash, Hasher};
12	
13	use ir::entry::Index;
14	use ir::syntax::OccurrenceSet;
15	use serde_json::Value;
16	use terminusdb_schema::{ToJson, ToTDBInstance};
17	
18	use crate::error::{EmitLinkedDataError, GenerateError};
19	use crate::graph::from_ir::project;
20	use crate::graph::link::PackageCtx;
21	
22	/// The `Symbol` fields that are links to other symbols. Wave 2 emits symbols
23	/// with these stripped (targets may not exist yet); wave 3 re-emits the full
24	/// documents once every target does. `resolves_to` lives on the shape's name
25	/// leaves, so the strip is recursive.
26	const EDGE_FIELDS: [&str; 7] =
27		["member_of", "implements", "extends", "mentions", "takes", "returns", "resolves_to"];
28	
29	/// A destination for emitted graph documents — e.g. an NDJSON writer, a batching
30	/// uploader to the graph store, or a test collector. Implementors decide how (and
31	/// whether) to batch; the emitter just streams.
32	pub trait DocumentSink {
33		/// Accept one serialized graph document.
34		fn write(&mut self, document: Value) -> Result<(), GenerateError>;
35	
36		/// Flush any buffered batch. Called once at the end of [`emit`].
37		fn flush(&mut self) -> Result<(), GenerateError> {
38			Ok(())
39		}
40	}
41	
42	/// Per-wave first-write-wins bookkeeping: re-emitting an identical body for an
43	/// `@id` is a no-op, a *different* body for the same `@id` within a wave is a
44	/// conflict. (Across waves, re-emis
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/graph
```
EISDIR: illegal operation on a directory, read '/Users/philocalyst/Projects/Backend/workspace/compiler/graph'
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/graph/mod.rs
```
1	//! Graph-store model: the graph-native TerminusDB projection of the compiler IR.
2	//!
3	//! The IR (`ir` crate) is the working representation; these types are its
4	//! graph form for TerminusDB. `#[derive(TerminusDBModel)]` generates both the
5	//! schema (uploaded once) and the instance JSON-LD (per document), so there is
6	//! no hand-written schema emitter or serde glue — Rust is the source of truth.
7	//! Design rationale and the query cookbook live in GRAPH-ARCHITECTURE.md at
8	//! the repo root.
9	//!
10	//! - [`model`] holds the derived document types: [`model::Symbol`] nodes with
11	//!   real link edges (`member_of`/`implements`/`extends`/`mentions`/…), the
12	//!   inline [`model::Shape`] payload, and the reified
13	//!   [`model::Implementation`] / [`model::Reference`] relation nodes.
14	//! - [`link`] is the resolution pass: name-strings → symbol IRIs, total via
15	//!   stub minting.
16	//! - [`from_ir`] projects an indexed package into a [`from_ir::GraphCorpus`]
17	//!   (`from_ir::project`).
18	
19	pub mod from_ir;
20	pub mod link;
21	pub mod model;
22	pub mod symtab;
23	
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/graph/model.rs
```
1	//! The TerminusDB document model for the nudox IR — the graph-native projection
2	//! of the `ir` crate. `#[derive(TerminusDBModel)]` generates both the schema
3	//! (class/enum/tagged-union definitions) and the instance JSON-LD, so there is
4	//! no hand-written emitter or serde glue.
5	//!
6	//! Design (see GRAPH-ARCHITECTURE.md at the repo root):
7	//! - [`Symbol`] is the node class, client-minted id `Symbol/{lang}/{pkg}/{fq}`.
8	//!   Symbol→symbol relations (`member_of`, `implements`, `extends`, `mentions`,
9	//!   `takes`, `returns`) are `TdbLazy<Symbol>` fields — real class-typed links
10	//!   in the schema, so WOQL/GraphQL traverse them server-side. `EntityIDFor` is
11	//!   never used for edges: it renders as `xsd:string`, not a link.
12	//! - The structural payload stays an inline subdocument tree ([`Shape`]), the
13	//!   document of record for rendering. Its name leaves ([`TyReference`],
14	//!   [`TraitRef`]) carry `resolves_to` links so path queries can walk *through*
15	//!   a signature to named nodes.
16	//! - Attributed relations are reified, `value_hash`-keyed (content-addressed):
17	//!   [`Implementation`] and [`Reference`].
18	//!
19	//! Shape conventions (unchanged):
20	//! - Recursive data enums (e.g. [`Type`], [`ConstExpr`]) use *newtype* variants
21	//!   wrapping named structs/enums — never inline `Variant { … }` struct
22	//!   variants. The derive's schema-tree walker shares its dedup set across
23	//!   newtype payloads but not across inline-struct variants, so only the
24	//!   newtype form terminates on self-reference.
25	//! - IR `Option<Vec<T>>` collapses to `Vec<T>` (TDB List) or `BTreeSet<T>` (TDB
26	//!   Set); genuinely optional singles stay `Option<T>`.
27	//! - Link-bearing types derive only `Debug + Clone` ([`TdbLazy`] is not
28	//!   `PartialEq`), and that leaks transitively through the [`Type`] tree.
29	
30	use std::collections::BTreeSet;
31	
32	// The derive emits Serialize/Deserialize and calls that need these traits 
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/graph/link.rs
```
1	//! The linker: the resolution pass that turns the IR's name-strings into
2	//! symbol IRIs, so every relation in the graph is a real link.
3	//!
4	//! Resolution is **total**: a name that matches no index entry still yields an
5	//! IRI — a stub [`model::Symbol`] (`kind: Unresolved`, `resolved: false`) is
6	//! minted under a deterministic address, so edges never dangle and cross
7	//! package boundaries. When the defining package is ingested later under the
8	//! same identity scheme, its real node lands at a knowable address.
9	//!
10	//! Lookup order: exact fq path → alias → unique last-segment suffix → stub.
11	
12	use std::cell::RefCell;
13	use std::collections::{BTreeMap, BTreeSet, HashMap};
14	
15	use ir::entry::{Index, NudoxPath};
16	use terminusdb_schema::{EntityIDFor, TdbLazy};
17	
18	use super::model as m;
19	use super::symtab::SymbolTable;
20	
21	/// The pseudo-package for names whose owning package is unknown (bare
22	/// identifiers in signatures that resolve nowhere). References with a known
23	/// dependency (`NudoxPath::External`) go under the real package name instead.
24	pub const EXTERN_PACKAGE: &str = "~extern";
25	
26	/// Coordinates of the package being projected.
27	#[derive(Debug, Clone)]
28	pub struct PackageCtx {
29	    pub language: String,
30	    pub package: String,
31	    pub version: Option<String>,
32	}
33	
34	/// Keep IRIs to a conservative charset so client-minted ids are always valid
35	/// TerminusDB document ids. `::` survives (the legacy `Entry/...` URIs proved
36	/// it); everything exotic collapses to `_`.
37	fn sanitize(segment: &str) -> String {
38	    segment
39	        .chars()
40	        .map(|c| {
41	            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':' | '~') {
42	                c
43	            } else {
44	                '_'
45	            }
46	        })
47	        .collect()
48	}
49	
50	/// Hierarchical coordinate → single id segment.
51	///
52	/// [`EntityIDFor`] only accepts `TypeName/
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/graph/from_ir.rs
```
1	//! Projection from the compiler IR (`ir` crate) into the graph [`model`] —
2	//! the linking pass that makes the store graph-native.
3	//!
4	//! Unlike the old 1:1 mirror, this projection resolves every name-string it
5	//! meets (via [`Linker`]) and emits edges alongside the structural payload:
6	//!
7	//! - `TypeReference`/`TraitRef` leaves get `resolves_to` links;
8	//! - each symbol accumulates derived adjacency (`mentions`, and for functions
9	//!   `takes`/`returns`) as it is projected;
10	//! - `implemented_protocols` → `implements` links, supertypes/supertraits →
11	//!   `extends` links, inverted `members` → `member_of`;
12	//! - `TraitImpl` entries additionally reify an [`model::Implementation`];
13	//! - tree-sitter-resolved function-body references — dropped entirely by the
14	//!   old projection — become [`model::Reference`] nodes (the call graph).
15	//!
16	//! Deliberate losses: `type_links` (superseded by the linker) and the
17	//! structured `LogicalPredicate` tree, flattened to a string as before.
18	//! Multivalued `Option<Vec<_>>` IR fields collapse to `Vec`/`BTreeSet`
19	//! (absent ⇒ empty).
20	
21	use std::collections::BTreeSet;
22	
23	use ir::entry::{Index, NudoxPath};
24	use ir::kind::Visibility as IrVis;
25	use ir::syntax::OccurrenceSet;
26	use terminusdb_schema::{EntityIDFor, TdbLazy};
27	
28	use super::link::{Linker, PackageCtx, package_iri, package_version_iri};
29	use super::model as m;
30	
31	// ───────────────────────── the emit context ─────────────────────────
32	
33	/// Which signature slot is being projected; decides which derived-adjacency
34	/// buckets a resolved name lands in.
35	#[derive(Clone, Copy, PartialEq)]
36	enum Role {
37	    Neutral,
38	    Input,
39	    Output,
40	}
41	
42	/// Per-symbol projection state: the linker plus the adjacency accumulated
43	/// while walking this symbol's shape.
44	struct EmitCx<'l> {
45	    linker: &'l Linker,
46	    role: Role,
47	    mentions: BTreeSet<String>,
48	    takes: BTreeSet
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/daemon
```
EISDIR: illegal operation on a directory, read '/Users/philocalyst/Projects/Backend/workspace/compiler/daemon'
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/daemon/mod.rs
```
1	//! The compiler daemon's runtime modules: forge context and HTTP server logic.
2	//!
3	//! This module is compiled into the `compiler` library so the daemon binary
4	//! (`compiler-daemon`) can reach the `ForgeRuntime` without creating a
5	//! circular dependency.
6	
7	pub mod forge;
8	
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/bin/compiler_daemon.rs
```
1	//! Compiler daemon — an HTTP service that accepts [`compiler::protocol::CompileRequest`]
2	//! and returns [`compiler::protocol::CompileResponse`].
3	//!
4	//! Bind address: `NUDOX_COMPILER_ADDR` env var, defaulting to `0.0.0.0:8080`.
5	//!
6	//! Routes:
7	//!   GET  /health  → 200 OK
8	//!   POST /compile → **postcard** body: CompileRequest; postcard response:
9	//!                   CompileResponse (`Content-Type: application/x-postcard`)
10	//!
11	//! The protocol types are postcard-serializable by design (see
12	//! `compiler::protocol` / `registry::protocol`). JSON was a temporary mistake —
13	//! shipping `Vec<u8>` source files as JSON number arrays ballooned the body past
14	//! any reasonable limit and disagreed with the wire contract.
15	
16	use std::sync::Arc;
17	
18	use anyhow::Context as _;
19	use axum::{
20	    body::Bytes,
21	    extract::State,
22	    http::{HeaderValue, StatusCode, header},
23	    response::IntoResponse,
24	    routing::{get, post},
25	};
26	use compiler::daemon::forge::{ForgeConfig, ForgeRuntime};
27	use compiler::protocol::{CompileRequest, CompileResponse, WireFile, WireReference};
28	use sandbox::Policy;
29	use tracing::info;
30	
31	/// Content-Type for the postcard compile envelope (must match the server client).
32	const POSTCARD_CONTENT_TYPE: &str = "application/x-postcard";
33	
34	// ─────────────────────────────────────────────────────────────────────────────
35	// Application state
36	// ─────────────────────────────────────────────────────────────────────────────
37	
38	/// Shared, cloneable application state.
39	#[derive(Clone)]
40	struct AppState {
41	    /// The owned forge runtime.  Wrapped in `Arc` so it is cheap to clone into
42	    /// every request handler.
43	    forge: Arc<ForgeRuntime>,
44	}
45	
46	// ─────────────────────────────────────────────────────────────────────────────
47	// Entry point
48	// ─────────────────────────────────────────────────────────────────────────────
49	
50	#[tokio::main]
51	async f
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/daemon/forge.rs
```
1	//! The `ForgeRuntime` for the compiler daemon — owned compile-plane capabilities.
2	//!
3	//! Ported from `server/forge.rs`; stripped of the server-only `registry::StoreCas`
4	//! L3 tier.  The daemon runs with a local L1+L2 tiered CAS (no distributed object
5	//! store at the compiler side of the wire boundary).
6	//!
7	//! Assembly follows the same Cold→Ready typestate pattern:
8	//! `ForgeRuntime<Cold>::assemble(...)` → `ForgeRuntime<Ready>`, which implements
9	//! `ForgeContext` and can be passed directly into `compiler::generate::generate_with`.
10	
11	use std::collections::HashMap;
12	use std::marker::PhantomData;
13	use std::path::{Path, PathBuf};
14	use std::sync::Arc;
15	
16	use bytes::Bytes;
17	use heart::cache::{Cas, CasError, ContentHash, DiskCas, Tiered};
18	use sandbox::{
19	    Cage, CageError, DevPassthrough, ForgeObserver, LinuxNamespaces, NodeId, NullObserver,
20	    OverrideTable, Policy, ToolchainSet, WorkerLang, WorkerPool, WorkerPoolConfig,
21	};
22	
23	use crate::compile::producer::ForgeContext;
24	
25	// ─────────────────────────────────────────────────────────────────────────────
26	// Daemon L3: always "none" — the daemon has no distributed object store.
27	// We mirror the NoL3 semantics inline so the CAS type is fully concrete.
28	// ─────────────────────────────────────────────────────────────────────────────
29	
30	/// A placeholder L3 type that always returns `Unsupported`.
31	///
32	/// The daemon runs with L1 (memory) + optional L2 (disk) only; there is no
33	/// distributed object store at this side of the wire boundary.
34	pub enum DaemonL3 {
35	    /// No L3 configured (the only variant for the daemon).
36	    None,
37	}
38	
39	impl Cas for DaemonL3 {
40	    async fn get(&self, _key: ContentHash) -> Result<Option<Bytes>, CasError> {
41	        Err(CasError::Unsupported("L3 not configured in compiler daemon"))
42	    }
43	    async fn put(&self, _bytes: Bytes) -> Result<ContentHash, CasError> {
44	        Err(CasError::Unsupport
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/compile/rust/producer.rs
```
1	//! Rust [`Producer`](crate::compile::producer::Producer) — in-process
2	//! rust-analyzer HIR walk → IR.
3	
4	use std::path::Path;
5	
6	use heart::Language;
7	use sandbox::{Captured, SealedInput};
8	use semver::Version;
9	
10	use crate::compile::producer::{
11		AuxOutputs, ExecPlan, ForgeContext, Producer, ProducerError, ProducerId, ProducerOutput,
12		ThreatTier,
13	};
14	
15	/// Rust crate/workspace producer (in-process rust-analyzer HIR walk → IR).
16	///
17	/// Planning is adaptive: the workspace is loaded once and lowered in-process,
18	/// so [`produce`](Producer::produce) runs the in-process path directly.
19	/// `plan`/`decode` return explicit errors (not hollow empty commands).
20	#[derive(Debug, Clone)]
21	pub struct RustProducer {
22		/// Root package name as cargo metadata reports it.
23		pub name: String,
24		/// Cargo version from package coordinates.
25		pub version: Version,
26		/// Direct-repo mode (document private items + workspace members).
27		pub direct_repo: bool,
28	}
29	
30	impl Producer for RustProducer {
31		const ID: ProducerId = ProducerId("rustdoc/3");
32	
33		fn language(&self) -> Language {
34			Language::Rust
35		}
36	
37		fn tier(&self) -> ThreatTier {
38			ThreatTier::Untrusted
39		}
40	
41		fn plan<C: ForgeContext>(
42			&self,
43			_ctx: &C,
44			_input: &SealedInput,
45		) -> Result<ExecPlan, ProducerError> {
46			Err(ProducerError::adaptive("rustdoc multi-crate"))
47		}
48	
49		fn decode(
50			&self,
51			_input: &SealedInput,
52			_captured: Captured,
53		) -> Result<ProducerOutput, ProducerError> {
54			Err(ProducerError::decode(
55				"rustdoc multi-crate: adaptive — call produce() (Phase 4 stages sealed commands)",
56			))
57		}
58	
59		fn produce<C: ForgeContext>(
60			&self,
61			ctx: &C,
62			input: &SealedInput,
63		) -> Result<ProducerOutput, ProducerError> {
64			self.lower_in_process(ctx, &input.root)
65		}
66	
67		fn lower_in_process<C: ForgeContext>(
68			&self,
69			_ctx: &C,
70			root: &Path,
71		) -> Result<P
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/compile/rust/mod.rs
```
1	//! Lowering Rust into the surface IR.
2	//!
3	//! The Rust producer is an in-process rust-analyzer (`ra_ap_*`) HIR walk that
4	//! lowers directly to [`Index`]. The legacy `cargo rustdoc --output-format
5	//! json` path was removed in RUST-ANALYZER-PLAN P3.
6	
7	pub mod error;
8	pub mod producer;
9	pub mod ra;
10	pub mod traversal;
11	
12	use std::path::Path;
13	
14	use ir::entry::Index;
15	use rustc_hash::FxHashMap as HashMap;
16	use semver::Version;
17	
18	pub use self::{
19		error::{
20			GenericError, ImplError, ItemError, MetadataError, Package, Parse, ProcessFailure,
21			ProcessFailureKind, SignatureError, TypeResolutionError,
22		},
23		producer::RustProducer,
24	};
25	
26	pub type Result<T> = std::result::Result<T, Parse>;
27	
28	/// Convert an empty vec to `None`, wrapping a non-empty vec in `Some`.
29	pub(crate) fn empty_to_none<T>(v: Vec<T>) -> Option<Vec<T>> {
30		if v.is_empty() { None } else { Some(v) }
31	}
32	
33	/// Lower a materialized Rust package/workspace at `root` into the indexed API
34	/// surface plus the fq-name → source-text map for downstream tree-sitter
35	/// extraction.
36	///
37	/// `name` is the root package to document; `document_private` includes private
38	/// items and pulls the workspace's local library dependencies into the surface
39	/// (the direct-repo behaviour).
40	pub fn generate_ir(
41		root: &Path,
42		name: &str,
43		version: &Version,
44		document_private: bool,
45	) -> std::result::Result<(Index, HashMap<String, String>), Package> {
46		ra::generate_ir(root, name, version, document_private)
47	}
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/compile/go/producer.rs
```
1	//! Go [`Producer`](crate::compile::producer::Producer) — oracle form.
2	
3	use std::path::Path;
4	
5	use heart::Language;
6	use sandbox::{Captured, ProducerProfile, SealedInput};
7	
8	use crate::compile::isolate::{self, IsolatedCommand};
9	use crate::compile::producer::{
10		AuxOutputs, ExecPlan, ForgeContext, Producer, ProducerError, ProducerId, ProducerOutput,
11		ThreatTier,
12	};
13	
14	use super::context::GoContext;
15	use super::oracle;
16	use super::package;
17	
18	/// Go module producer (`go run` oracle → IR).
19	#[derive(Debug, Default, Clone, Copy)]
20	pub struct GoProducer;
21	
22	impl Producer for GoProducer {
23		const ID: ProducerId = ProducerId("go-oracle/1");
24	
25		fn language(&self) -> Language {
26			Language::Go
27		}
28	
29		fn tier(&self) -> ThreatTier {
30			ThreatTier::Untrusted
31		}
32	
33		fn plan<C: ForgeContext>(
34			&self,
35			ctx: &C,
36			input: &SealedInput,
37		) -> Result<ExecPlan, ProducerError> {
38			let module = package::discover_module(&input.root).map_err(ProducerError::plan)?;
39			let bin = package::oracle_binary().map_err(ProducerError::plan)?;
40			let target = module.root.canonicalize().map_err(ProducerError::plan)?;
41	
42			let cmd = IsolatedCommand::new(&bin, ProducerProfile::Go)
43				.arg(&target)
44				.env("GOWORK", "off")
45				.env("GOFLAGS", "-mod=mod")
46				.env("GOPROXY", "off")
47				.ro(&bin)
48				.ro(&target)
49				.rw(input.budget.fs.scratch_path())
50				.rw(std::env::temp_dir());
51	
52			Ok(ExecPlan::Commands(vec![isolate::seal(ctx, cmd)]))
53		}
54	
55		fn decode(
56			&self,
57			input: &SealedInput,
58			captured: Captured,
59		) -> Result<ProducerOutput, ProducerError> {
60			let output: oracle::Output =
61				serde_json::from_slice(&captured.stdout).map_err(ProducerError::decode)?;
62			for error in &output.errors {
63				tracing::warn!("go oracle diagnostic: {error}");
64			}
65			let module = package::discover_module(&input.root).map_err(ProducerError::lower)?;
66			let ctx = GoContext {
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/compile/typescript/oxc/mod.rs
```
1	//! Full-OXC TypeScript producer pipeline (OXC-PLAN.md).
2	//!
3	//! Replaces the deno_doc/deno_graph/swc stack with a hand-written extractor on
4	//! the `oxc` crates. Three stages, facts-first two-pass (OXC-PLAN §3):
5	//!   1. [`entry`] — resolver-first entry / declaration-root discovery.
6	//!   2. [`graph`] — module-graph worklist + pass-1 per-module [`extract`]ion to
7	//!      owned [`extract::ModuleFacts`] (arena dropped per module).
8	//!   3. [`link`]  — pass-2 cross-module resolution → `ir::entry::Index`.
9	//!
10	//! Coexists with the legacy deno files during migration; cutover (OXC-PLAN
11	//! §Phase 4) deletes those and promotes this module up.
12	
13	use std::path::Path;
14	
15	use ir::entry::Index;
16	
17	pub mod entry;
18	pub mod error;
19	pub mod extract;
20	pub mod graph;
21	pub mod link;
22	
23	pub use error::{Package, Parse, TsDeclarationError, TsInterfaceError, TsTypeError};
24	
25	/// Lower a materialized TypeScript package at `root` into the indexed API
26	/// surface, using the OXC pipeline.
27	///
28	/// `name` is the package name as its manifest reports it. Synchronous end to
29	/// end (no tokio / deno_graph futures).
30	pub fn generate_ir(root: &Path, name: &str) -> Result<Index, Package> {
31		let roots = entry::discover_entry_points(root)?;
32		let modules = graph::build_and_extract(roots)?;
33		link::link(modules, name)
34	}
35	
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/compile/python/producer.rs
```
1	//! Python [`Producer`](crate::compile::producer::Producer) — library form (pyrefly).
2	
3	use std::path::Path;
4	
5	use heart::Language;
6	use sandbox::{Captured, SealedInput, WorkerLang};
7	
8	use crate::compile::producer::{
9		AuxOutputs, ExecPlan, ForgeContext, Producer, ProducerError, ProducerId, ProducerOutput,
10		ThreatTier, decode_index_json,
11	};
12	
13	/// Python package producer (pyrefly via worker pool / in-process).
14	#[derive(Debug, Default, Clone, Copy)]
15	pub struct PythonProducer;
16	
17	impl Producer for PythonProducer {
18		const ID: ProducerId = ProducerId("pyrefly/1");
19	
20		fn language(&self) -> Language {
21			Language::Python
22		}
23	
24		fn tier(&self) -> ThreatTier {
25			ThreatTier::Hostile
26		}
27	
28		fn plan<C: ForgeContext>(
29			&self,
30			_ctx: &C,
31			_input: &SealedInput,
32		) -> Result<ExecPlan, ProducerError> {
33			Ok(ExecPlan::Library(WorkerLang::Python))
34		}
35	
36		fn decode(
37			&self,
38			_input: &SealedInput,
39			captured: Captured,
40		) -> Result<ProducerOutput, ProducerError> {
41			let index = decode_index_json(&captured.stdout)?;
42			Ok(ProducerOutput {
43				index,
44				aux: AuxOutputs::default(),
45			})
46		}
47	
48		fn lower_in_process<C: ForgeContext>(
49			&self,
50			_ctx: &C,
51			root: &Path,
52		) -> Result<ProducerOutput, ProducerError> {
53			let ctx = super::context::PythonContext::new();
54			Ok(ProducerOutput::from_index(ctx.lower_package(root)))
55		}
56	}
57	
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/compile/nix/producer.rs
```
1	//! Nix [`Producer`](crate::compile::producer::Producer) — library form (snix).
2	
3	use std::path::Path;
4	
5	use heart::Language;
6	use sandbox::{Captured, SealedInput, WorkerLang};
7	
8	use crate::compile::producer::{
9		AuxOutputs, ExecPlan, ForgeContext, Producer, ProducerError, ProducerId, ProducerOutput,
10		ThreatTier, decode_index_json,
11	};
12	
13	/// Nix flake producer (snix hermetic eval via worker pool / in-process).
14	///
15	/// Hermeticity is part of the budget: pure builtins + sealed [`DocsIO`](super::eval::DocsIO).
16	/// Worker children start with `env_clear` (seal-time projection); in-process
17	/// eval never reads host secrets through EvalIO.
18	#[derive(Debug, Default, Clone, Copy)]
19	pub struct NixProducer;
20	
21	impl Producer for NixProducer {
22		const ID: ProducerId = ProducerId("snix/1");
23	
24		fn language(&self) -> Language {
25			Language::Nix
26		}
27	
28		fn tier(&self) -> ThreatTier {
29			ThreatTier::Hostile
30		}
31	
32		fn plan<C: ForgeContext>(
33			&self,
34			_ctx: &C,
35			_input: &SealedInput,
36		) -> Result<ExecPlan, ProducerError> {
37			Ok(ExecPlan::Library(WorkerLang::Nix))
38		}
39	
40		fn decode(
41			&self,
42			_input: &SealedInput,
43			captured: Captured,
44		) -> Result<ProducerOutput, ProducerError> {
45			let index = decode_index_json(&captured.stdout)?;
46			Ok(ProducerOutput {
47				index,
48				aux: AuxOutputs::default(),
49			})
50		}
51	
52		fn lower_in_process<C: ForgeContext>(
53			&self,
54			_ctx: &C,
55			root: &Path,
56		) -> Result<ProducerOutput, ProducerError> {
57			let index = super::lower_package(root).map_err(ProducerError::lower)?;
58			Ok(ProducerOutput::from_index(index))
59		}
60	}
61	
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/compile/csharp/producer.rs
```
1	//! C# [`Producer`](crate::compile::producer::Producer) — Roslyn oracle
2	//! (`dotnet oracle.dll`) → IR.
3	//!
4	//! The oracle publish dir is a Buck2-built resource; source collection +
5	//! `dotnet oracle.dll` invocation happen inside the oracle path via
6	//! [`produce`](Producer::produce). `plan`/`decode` return explicit errors
7	//! (adaptive multi-step, like Java) until ForgeRuntime stages sealed commands.
8	
9	use std::path::Path;
10	
11	use heart::Language;
12	use sandbox::{Captured, SealedInput};
13	
14	use crate::compile::producer::{
15		AuxOutputs, ExecPlan, ForgeContext, Producer, ProducerError, ProducerId, ProducerOutput,
16		ThreatTier,
17	};
18	
19	/// C# project producer (Roslyn oracle → IR).
20	#[derive(Debug, Default, Clone, Copy)]
21	pub struct CSharpProducer;
22	
23	impl Producer for CSharpProducer {
24		const ID: ProducerId = ProducerId("roslyn-oracle/1");
25	
26		fn language(&self) -> Language {
27			Language::CSharp
28		}
29	
30		fn tier(&self) -> ThreatTier {
31			// Source mode runs no user code (Roslyn only parses), but package
32			// analyzers/source-generators are a code-exec vector — treat like Java.
33			ThreatTier::Untrusted
34		}
35	
36		fn plan<C: ForgeContext>(
37			&self,
38			_ctx: &C,
39			_input: &SealedInput,
40		) -> Result<ExecPlan, ProducerError> {
41			Err(ProducerError::adaptive("roslyn oracle multi-step"))
42		}
43	
44		fn decode(
45			&self,
46			_input: &SealedInput,
47			_captured: Captured,
48		) -> Result<ProducerOutput, ProducerError> {
49			Err(ProducerError::decode(
50				"roslyn oracle multi-step: adaptive — call produce()",
51			))
52		}
53	
54		fn produce<C: ForgeContext>(
55			&self,
56			ctx: &C,
57			input: &SealedInput,
58		) -> Result<ProducerOutput, ProducerError> {
59			self.lower_in_process(ctx, &input.root)
60		}
61	
62		fn lower_in_process<C: ForgeContext>(
63			&self,
64			ctx: &C,
65			root: &Path,
66		) -> Result<ProducerOutput, ProducerError> {
67			let index = super::package::lower_package(c
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/compile/java/producer.rs
```
1	//! Java [`Producer`](crate::compile::producer::Producer) — javac + javadoc doclet.
2	
3	use std::path::Path;
4	
5	use heart::Language;
6	use sandbox::{Captured, SealedInput};
7	
8	use crate::compile::producer::{
9		AuxOutputs, ExecPlan, ForgeContext, Producer, ProducerError, ProducerId, ProducerOutput,
10		ThreatTier,
11	};
12	
13	/// Java project producer (vendored javadoc doclet → IR).
14	///
15	/// The doclet jar is a Buck2-built resource (no runtime javac step); source
16	/// collection + `javadoc -doclet` invocation is orchestrated inside the
17	/// existing oracle path via [`produce`](Producer::produce). `plan`/`decode`
18	/// return explicit errors (not hollow empty commands); ForgeRuntime (Phase 4)
19	/// will stage sequential sealed commands under one scratch.
20	#[derive(Debug, Default, Clone, Copy)]
21	pub struct JavaProducer;
22	
23	impl Producer for JavaProducer {
24		const ID: ProducerId = ProducerId("javadoc/1");
25	
26		fn language(&self) -> Language {
27			Language::Java
28		}
29	
30		fn tier(&self) -> ThreatTier {
31			ThreatTier::Untrusted
32		}
33	
34		fn plan<C: ForgeContext>(
35			&self,
36			_ctx: &C,
37			_input: &SealedInput,
38		) -> Result<ExecPlan, ProducerError> {
39			Err(ProducerError::adaptive("javadoc multi-step"))
40		}
41	
42		fn decode(
43			&self,
44			_input: &SealedInput,
45			_captured: Captured,
46		) -> Result<ProducerOutput, ProducerError> {
47			Err(ProducerError::decode(
48				"javadoc multi-step: adaptive — call produce() (Phase 4 stages sealed commands)",
49			))
50		}
51	
52		fn produce<C: ForgeContext>(
53			&self,
54			ctx: &C,
55			input: &SealedInput,
56		) -> Result<ProducerOutput, ProducerError> {
57			self.lower_in_process(ctx, &input.root)
58		}
59	
60		fn lower_in_process<C: ForgeContext>(
61			&self,
62			ctx: &C,
63			root: &Path,
64		) -> Result<ProducerOutput, ProducerError> {
65			let index = super::package::lower_package(ctx, root).map_err(ProducerError::lower)?;
66			Ok(ProducerOutput {
67				index,

```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/compile/typescript/mod.rs
```
1	//! Lowering TypeScript into the surface IR via the OXC pipeline.
2	//!
3	//! Entry discovery, module-graph construction, per-module extraction, and
4	//! cross-module linking all live in [`oxc`].
5	
6	use std::path::Path;
7	
8	use ir::entry::Index;
9	
10	pub mod oracle;
11	pub mod oxc;
12	pub mod producer;
13	pub mod traversal;
14	
15	pub use self::{
16		oxc::error::{Package, Parse, TsDeclarationError, TsInterfaceError, TsTypeError},
17		producer::TypescriptProducer,
18	};
19	
20	/// Lower a materialized TypeScript package at `root` into the indexed API
21	/// surface, using the OXC pipeline. `name` is the package name from its manifest.
22	pub fn generate_ir(root: &Path, name: &str) -> Result<Index, Package> {
23		oxc::generate_ir(root, name)
24	}
25	
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/compile/typescript/producer.rs
```
1	//! TypeScript [`Producer`](crate::compile::producer::Producer) — library form (OXC).
2	
3	use std::path::Path;
4	
5	use heart::Language;
6	use sandbox::{Captured, SealedInput, WorkerLang};
7	
8	use crate::compile::producer::{
9		AuxOutputs, ExecPlan, ForgeContext, Producer, ProducerError, ProducerId, ProducerOutput,
10		ThreatTier, decode_index_json,
11	};
12	
13	/// TypeScript package producer (OXC via worker pool / in-process).
14	#[derive(Debug, Clone)]
15	pub struct TypescriptProducer {
16		/// Package name (from package.json / coordinates).
17		pub name: String,
18	}
19	
20	impl Producer for TypescriptProducer {
21		const ID: ProducerId = ProducerId("oxc/1");
22	
23		fn language(&self) -> Language {
24			Language::Typescript
25		}
26	
27		fn tier(&self) -> ThreatTier {
28			ThreatTier::Hostile
29		}
30	
31		fn plan<C: ForgeContext>(
32			&self,
33			_ctx: &C,
34			_input: &SealedInput,
35		) -> Result<ExecPlan, ProducerError> {
36			Ok(ExecPlan::Library(WorkerLang::Typescript))
37		}
38	
39		fn decode(
40			&self,
41			_input: &SealedInput,
42			captured: Captured,
43		) -> Result<ProducerOutput, ProducerError> {
44			let index = decode_index_json(&captured.stdout)?;
45			Ok(ProducerOutput {
46				index,
47				aux: AuxOutputs::default(),
48			})
49		}
50	
51		fn lower_in_process<C: ForgeContext>(
52			&self,
53			_ctx: &C,
54			root: &Path,
55		) -> Result<ProducerOutput, ProducerError> {
56			// Tier C (opt-in via NUDOX_TYPESCRIPT_ORACLE): try the in-process tsz
57			// checker oracle first; on ANY failure fall back to the syntactic OXC
58			// pass. Unset env → the default path below, byte-identical to Tier A/B.
59			if super::oracle::tsz::enabled() {
60				match super::oracle::tsz::normalize(root, &self.name) {
61					Ok(index) => {
62						let mut out = ProducerOutput::from_index(index);
63						out.aux.extraction_tier = "tsz-emit".to_string();
64						return Ok(out);
65					}
66					Err(reason) => {
67						tracing::warn!("tsz oracle fell back to syntac
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/treesitter/mod.rs
```
1	//! Production tree-sitter CST extraction.
2	//!
3	//! Given a symbol's source and span, finds the enclosing function, extracts a
4	//! self-contained snippet, re-parses it to an s-expression, and resolves the
5	//! references within it. Falls back gracefully (full text, no CST) on an
6	//! unsupported language or a parse failure.
7	//!
8	//! The [`spec`] submodule and per-language extractors ([`rust`], [`go`], …)
9	//! implement the richer [`spec::LanguageSpec`] contract (definitions, imports,
10	//! qualified references) that the occurrence resolver consumes; the classifiers
11	//! below remain for the legacy snippet path.
12	
13	pub mod spec;
14	
15	pub mod csharp;
16	pub mod go;
17	pub mod java;
18	pub mod nix;
19	pub mod python;
20	pub mod rust;
21	pub mod typescript;
22	
23	use std::{ops::Range, path::PathBuf};
24	
25	use arborium_tree_sitter as tree_sitter;
26	use heart::Language;
27	use ir::{
28		entry::NudoxPath,
29		syntax::{ReferenceKind, ResolvedReference, walk_references},
30	};
31	use serde::{Deserialize, Deserializer, Serialize};
32	
33	/// The structural [`spec::LanguageSpec`] extractor for `lang`.
34	pub fn spec_for(lang: Language) -> &'static dyn spec::LanguageSpec {
35		match lang {
36			Language::Rust => &rust::RustSpec,
37			Language::Python => &python::PythonSpec,
38			Language::Typescript => &typescript::TypescriptSpec,
39			Language::Go => &go::GoSpec,
40			Language::Java => &java::JavaSpec,
41			Language::Nix => &nix::NixSpec,
42			Language::CSharp => &csharp::CSharpSpec,
43		}
44	}
45	
46	// ─── Serializable primitives ─────────────────────────────────────────────────
47	
48	/// Opaque tree-sitter representation of a source chunk as raw bytes.
49	///
50	/// The concrete format is the JSON-serialized [`TreesitterPayload`].
51	#[derive(Debug, Clone, Serialize, Deserialize)]
52	pub struct TreesitterRepr(pub Vec<u8>);
53	
54	/// A byte range within a source file or buffer.
55	///
56	/// Invariant: `start <= end`. Enforced on construction and 
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/sandbox/lib.rs
```
1	//! Process isolation for untrusted producer execution.
2	//!
3	//! # One cage (DAEMON-PLAN §2.2)
4	//!
5	//! Isolation is the [`Cage`] trait: run a [`SealedCommand`] under a
6	//! [`CapabilityBudget`], with cooperative [`CancelToken`]. Production:
7	//! [`LinuxNamespaces`] (bwrap + cgroup + seccomp). Dev: [`DevPassthrough`]
8	//! (constructible only under [`Policy::Development`]).
9	//!
10	//! [`WorkerPool`] is **cage-internal** for library-form producers (nix/ts/python),
11	//! not a peer of the Backend enum. Real parallelism equals pool size.
12	//!
13	//! # Layering (Linux production path)
14	//!
15	//! ```text
16	//! cgroup (memory.max + swap.max=0 + pids)  — real RAM / fork bomb
17	//!   └─ bwrap namespaces (net/pid/mount/…)  — cage
18	//!        ├─ rlimits (AS=4×mem VA, CPU, FSIZE, NOFILE)
19	//!        └─ bwrap --seccomp FD            — LPE denylist AFTER setup
20	//! ```
21	//!
22	//! **Never** install seccomp/Landlock on the bwrap process itself — that
23	//! blocks `unshare`/`mount` and breaks the sandbox. Direct spawn (no bwrap)
24	//! uses `pre_exec`: rlimit → Landlock → seccomp.
25	//!
26	//! # Invariants encoded in the type system
27	//!
28	//! - [`Env`] is *only* an allowlist — ambient host environment is never inherited.
29	//! - [`Network`] / [`NetGrant`] is explicit; the default is off.
30	//! - [`Limits`] requires every ceiling (no silent "unlimited" field).
31	//! - [`LimitOverride`] is sparse — zeros are unrepresentable via `NonZero*`.
32	//! - [`DevPassthrough`] cannot be constructed under [`Policy::Production`].
33	
34	#![deny(missing_docs)]
35	
36	pub mod backend;
37	pub mod budget;
38	pub mod cage;
39	pub mod cancel;
40	pub mod cgroup;
41	pub mod error;
42	pub mod job;
43	pub mod limits;
44	pub mod node;
45	pub mod observer;
46	pub mod overrides;
47	pub mod probe;
48	pub mod profiles;
49	pub mod seal;
50	pub mod spec;
51	pub mod toolchains;
52	pub mod worker;
53	
54	/// Landlock LSM helpers (Linux only).
55	#[cfg(target_os = "linux")]
56	
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/sandbox/cage.rs
```
1	//! The one cage abstraction (DAEMON-PLAN §2.2).
2	//!
3	//! Isolation is a trait, not a peer-of-backends enum. Production work runs under
4	//! [`LinuxNamespaces`]; dev work under [`DevPassthrough`] (constructible only
5	//! with [`Policy::Development`]). [`WorkerPool`](crate::WorkerPool) is
6	//! cage-internal for library-form producers, not a top-level backend peer.
7	
8	use std::path::PathBuf;
9	
10	use crate::backend::supervisor::{self, apply_rlimits};
11	use crate::budget::NetGrant;
12	use crate::cancel::CancelToken;
13	use crate::cgroup::Cgroup;
14	use crate::error::CageError;
15	#[cfg(target_os = "linux")]
16	use crate::error::SandboxError;
17	use crate::seal::SealedCommand;
18	use crate::spec::Output;
19	
20	/// Stable identity of a cage instance (logs / metrics).
21	#[derive(Debug, Clone, PartialEq, Eq, Hash)]
22	pub struct CageId(pub &'static str);
23	
24	impl CageId {
25		/// Borrow the id string.
26		pub fn as_str(&self) -> &'static str {
27			self.0
28		}
29	}
30	
31	impl std::fmt::Display for CageId {
32		fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
33			f.write_str(self.0)
34		}
35	}
36	
37	/// What a cage can enforce.
38	#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
39	pub struct CageCaps {
40		/// Namespaces / seatbelt profile available.
41		pub isolation: bool,
42		/// Network can be forced off.
43		pub network_off: bool,
44		/// Landlock or equivalent FS scoping.
45		pub fs_scope: bool,
46		/// seccomp denylist.
47		pub seccomp: bool,
48		/// cgroup resource control.
49		pub cgroups: bool,
50		/// Suitable as production security boundary of record.
51		pub production_grade: bool,
52	}
53	
54	
55	/// Runtime policy resolved once at assemble (Phase 4 owns full wiring).
56	#[derive(Debug, Clone, Copy, PartialEq, Eq)]
57	pub enum Policy {
58		/// Production: only production-grade cages.
59		Production,
60		/// Development: passthrough / seatbelt allowed.
61		Development,
62	}
63	
64	impl Policy {
65		/// From the exi
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/sandbox/worker.rs
```
1	//! Pooled sandboxed workers for library-form producer isolation.
2	//!
3	//! Cage-internal (DAEMON-PLAN §2.2): long-lived children under rlimits + cgroup
4	//! speak a line-oriented JSON protocol. Not a peer of the Backend enum —
5	//! [`LinuxNamespaces`](crate::LinuxNamespaces) remains the production OS cage;
6	//! this pool is how library producers (nix/ts/python) run inside that model.
7	//!
8	//! Concurrency: free-list of slots (`Mutex<Vec<WorkerSlot>>` + condvar) so the
9	//! free-list lock is never held across worker I/O. Real parallelism equals pool
10	//! size. A hung worker costs one slot for one wall budget, not the whole pool
11	//! forever (deadline-safe non-blocking pipe I/O).
12	
13	use std::io::{self, Read, Write};
14	use std::path::{Path, PathBuf};
15	use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
16	use std::sync::{Condvar, Mutex};
17	use std::time::{Duration, Instant};
18	
19	use serde::{Deserialize, Serialize};
20	
21	use crate::cage::Cage;
22	use crate::cancel::CancelToken;
23	use crate::error::{CageError, KillReason, SandboxError};
24	use crate::limits::Limits;
25	use crate::profiles::ProducerProfile;
26	use crate::spec::Env;
27	
28	/// Languages the worker binary can lower (closed set — no free strings at the
29	/// isolate boundary).
30	#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
31	#[serde(rename_all = "snake_case")]
32	pub enum WorkerLang {
33		/// snix hermetic eval + static fusion.
34		Nix,
35		/// deno_doc.
36		Typescript,
37		/// pyrefly.
38		Python,
39	}
40	
41	impl WorkerLang {
42		/// Wire token for the worker CLI / protocol.
43		pub const fn as_str(self) -> &'static str {
44			match self {
45				Self::Nix => "nix",
46				Self::Typescript => "typescript",
47				Self::Python => "python",
48			}
49		}
50	
51		/// Parse a wire token.
52		pub fn parse(s: &str) -> Option<Self> {
53			match s {
54				"nix" => Some(Self::Nix),
55				"typescript" | "ts" => Some(Self::Typescript),
56				"python"
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/sandbox/probe.rs
```
1	//! Host isolation capability probe and production policy (design §5, §16.3–4).
2	//!
3	//! Call once at process start. [`IsolationPolicy::RequireProduction`] fails
4	//! closed when the host cannot provide a production-grade cage.
5	
6	use crate::cage::{CageCaps, DevPassthrough, Policy, Cage};
7	#[cfg(target_os = "linux")]
8	use crate::cage::LinuxNamespaces;
9	use crate::error::SandboxError;
10	
11	/// Snapshot of what this host can enforce.
12	#[derive(Debug, Clone, Copy, PartialEq, Eq)]
13	pub struct HostIsolation {
14		/// Selected cage capabilities.
15		pub capabilities: CageCaps,
16		/// Selected cage name.
17		pub backend: &'static str,
18		/// bubblewrap present on PATH (Linux).
19		pub bwrap: bool,
20		/// cgroup v2 writable parent discovered.
21		pub cgroup: bool,
22		/// Landlock ABI available (Linux; best-effort probe).
23		pub landlock: LandlockAbi,
24		/// seccomp denylist compiles for this arch.
25		pub seccomp: bool,
26		/// Kernel major.minor when readable from `/proc/version` / `uname`.
27		pub kernel: Option<(u32, u32)>,
28	}
29	
30	/// Landlock support tier.
31	#[derive(Debug, Clone, Copy, PartialEq, Eq)]
32	pub enum LandlockAbi {
33		/// Not Linux or probe failed.
34		Unavailable,
35		/// Kernel has Landlock but below our preferred ABI.
36		Partial,
37		/// Full FS isolation at V3+ (Truncate).
38		Full,
39	}
40	
41	/// How strictly isolation is required.
42	#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
43	pub enum IsolationPolicy {
44		/// Degrade gracefully (dev default).
45		#[default]
46		BestEffort,
47		/// Refuse to run without a production-grade backend.
48		RequireProduction,
49	}
50	
51	impl IsolationPolicy {
52		/// Single resolution of the production isolation gate from env.
53		///
54		/// True when `NUDOX_SANDBOX_REQUIRE=1|true` or `NUDOX_ENV=prod|production`.
55		/// Every call site that needs the prod gate must use this (or [`Self::from_env`])
56		/// rather than re-reading the environment.
57		pub fn env_requires_product
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/generate/parse_cache.rs
```
1	//! Content-addressed source / lock hashing for the producer job key.
2	//!
3	//! The CAS itself is owned by the injected [`ForgeContext`](crate::compile::producer::ForgeContext);
4	//! this module only computes the hashes that feed `JobKey` derivation (in
5	//! [`seal_package`](crate::compile::producer::seal_package)). No process-global
6	//! cache lives here anymore.
7	
8	use std::path::Path;
9	
10	use heart::ContentHash;
11	
12	/// Hash a source tree (sorted path ‖ file blake3).
13	pub fn hash_source_tree(root: &Path) -> std::io::Result<ContentHash> {
14		let mut files: Vec<(String, ContentHash)> = Vec::new();
15		walk(root, root, &mut files)?;
16		files.sort_by(|a, b| a.0.cmp(&b.0));
17		let mut h = ContentHash::builder();
18		for (path, digest) in files {
19			h.update(path.as_bytes());
20			h.update(&[0]);
21			h.update(digest.as_bytes());
22		}
23		Ok(h.finalize())
24	}
25	
26	fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, ContentHash)>) -> std::io::Result<()> {
27		for entry in std::fs::read_dir(dir)? {
28			let entry = entry?;
29			let ft = entry.file_type()?;
30			let path = entry.path();
31			if ft.is_dir() {
32				if entry.file_name() == ".git" || entry.file_name() == "target" {
33					continue;
34				}
35				walk(root, &path, out)?;
36			} else if ft.is_file() {
37				let rel = path
38					.strip_prefix(root)
39					.unwrap_or(&path)
40					.to_string_lossy()
41					.into_owned();
42				let bytes = std::fs::read(&path)?;
43				out.push((rel, ContentHash::of_bytes(&bytes)));
44			}
45		}
46		Ok(())
47	}
48	
49	/// Hash common lockfiles if present; empty digest when none.
50	pub fn hash_dep_lock(root: &Path) -> ContentHash {
51		const LOCKS: &[&str] = &[
52			"Cargo.lock",
53			"package-lock.json",
54			"yarn.lock",
55			"pnpm-lock.yaml",
56			"go.sum",
57			"poetry.lock",
58			"Pipfile.lock",
59			"flake.lock",
60		];
61		let mut h = ContentHash::builder();
62		let mut any = false;
63		for name in LOCKS {
64			let p = root.join(name);
65			if 
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/generate/occurrences.rs
```
1	//! The occurrences pipeline stage: parse every source file, run its
2	//! [`LanguageSpec`](crate::treesitter::spec::LanguageSpec) extractor, and
3	//! resolve + attribute the results against the surface index into an
4	//! [`OccurrenceSet`] (REFERENCES-PLAN §4.3).
5	//!
6	//! This is the surface-downstream successor to [`super::cst`]: where `cst`
7	//! produced unqualified [`ResolvedReference`](ir::syntax::ResolvedReference)
8	//! spans, this produces fully-qualified [`Occurrence`](ir::syntax::Occurrence)s
9	//! keyed by `NudoxPath` + span + confidence. The live trees are transient —
10	//! parsed, walked, resolved, and dropped.
11	
12	use std::{fs, path::PathBuf};
13	
14	use arborium_tree_sitter as tree_sitter;
15	use heart::Language;
16	use ir::{entry::Index, syntax::OccurrenceSet};
17	
18	use crate::{
19		error::GenerateError,
20		generate::{resolve::resolve, PackageInput},
21		treesitter::{
22			spec::{Extraction, PackageLayout},
23			spec_for,
24		},
25	};
26	
27	/// Map a language + file extension to an arborium grammar name (identical to
28	/// the [`super::cst`] dispatch; kept here so the two stages can diverge).
29	fn grammar_for_extension(lang: Language, ext: &str) -> Option<&'static str> {
30		match lang {
31			Language::Rust if ext == "rs" => Some("rust"),
32			Language::Python if ext == "py" => Some("python"),
33			Language::Typescript => match ext {
34				"ts" | "mts" | "cts" => Some("typescript"),
35				"tsx" => Some("tsx"),
36				"js" => Some("javascript"),
37				"jsx" => Some("tsx"),
38				_ => None,
39			},
40			Language::Go if ext == "go" => Some("go"),
41			Language::Java if ext == "java" => Some("java"),
42			Language::Nix if ext == "nix" => Some("nix"),
43			Language::CSharp if ext == "cs" => Some("c-sharp"),
44			_ => None,
45		}
46	}
47	
48	/// Directory names that never hold first-party source (vendored deps, build
49	/// output, tool caches). Extends the `cst` skip list per REFERENCES-PLAN §5.
50	fn is_skipped_dir(name: &std::ffi::OsStr)
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/compile/producer/scratch.rs
```
1	//! Writable scratch carved from an [`FsGrant`] (or a fresh temp dir).
2	
3	use std::fs;
4	use std::path::{Path, PathBuf};
5	
6	use sandbox::FsGrant;
7	
8	use super::ProducerError;
9	
10	/// RAII scratch directory — the only preferred way to get a producer-private RW tree.
11	#[derive(Debug)]
12	pub struct Scratch {
13		dir: PathBuf,
14		/// When we created a private subdir, remove it on drop.
15		owned: bool,
16	}
17	
18	impl Scratch {
19		/// Create a unique child under the grant's primary scratch.
20		pub fn from_grant(fs: &FsGrant) -> Result<Self, ProducerError> {
21			let dir = fs.scratch.join(format!(
22				"run-{}-{:x}",
23				std::process::id(),
24				std::time::SystemTime::now()
25					.duration_since(std::time::UNIX_EPOCH)
26					.map(|d| d.as_nanos())
27					.unwrap_or(0)
28			));
29			fs::create_dir_all(&dir)?;
30			Ok(Self { dir, owned: true })
31		}
32	
33		/// Fresh system-temp scratch (when no grant is available yet).
34		pub fn temp(label: &str) -> Result<Self, ProducerError> {
35			let dir = tempfile::Builder::new()
36				.prefix(&format!("nudox-{label}-"))
37				.tempdir()
38				.map_err(|e| ProducerError::Io(e.into()))?
39				.keep();
40			Ok(Self { dir, owned: true })
41		}
42	
43		/// Borrow the directory path.
44		pub fn path(&self) -> &Path {
45			&self.dir
46		}
47	
48		/// Join a child path under this scratch.
49		pub fn child(&self, name: impl AsRef<Path>) -> PathBuf {
50			self.dir.join(name)
51		}
52	}
53	
54	impl Drop for Scratch {
55		fn drop(&mut self) {
56			if self.owned {
57				let _ = fs::remove_dir_all(&self.dir);
58			}
59		}
60	}
61	
62	impl AsRef<Path> for Scratch {
63		fn as_ref(&self) -> &Path {
64			&self.dir
65		}
66	}
67	
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/sandbox/toolchains.rs
```
1	//! Resolved toolchain store paths (DAEMON-PLAN §2.5).
2	//!
3	//! Absorbs the compiler's former `ToolchainPaths::from_env`. This is the one
4	//! sanctioned place that may read `NUDOX_TOOLCHAIN_*` / `RUSTUP_HOME` etc. — the
5	//! seal boundary. The result is hashed into every `JobKey` via [`Self::digest`]
6	//! so a toolchain change invalidates the content-addressed cache.
7	//!
8	//! Owned by the `ForgeRuntime` and injected downstream; the `compiler` crate
9	//! never reads toolchain env itself.
10	
11	use std::path::PathBuf;
12	
13	use heart::ContentHash;
14	
15	use crate::spec::Env;
16	
17	/// Pinned toolchain paths for hermetic producer runs.
18	///
19	/// Loaded once at assemble from env; ambient `HOME` is never a writable mount.
20	#[derive(Debug, Clone, Default, PartialEq, Eq)]
21	pub struct ToolchainSet {
22		/// fenix/rustup root (RO).
23		pub rustup_home: Option<PathBuf>,
24		/// cargo home (RO preferred; RW only for explicit registries).
25		pub cargo_home: Option<PathBuf>,
26		/// JAVA_HOME.
27		pub java_home: Option<PathBuf>,
28		/// GOROOT.
29		pub go_root: Option<PathBuf>,
30		/// GOPATH (scratch-like; optional).
31		pub go_path: Option<PathBuf>,
32		/// Extra directories to prepend to the sealed `PATH`, from
33		/// `NUDOX_TOOLCHAIN_PATH` (colon-separated). Empty by default, so the
34		/// hermetic PATH is unchanged unless explicitly opted in. Intended for dev
35		/// hosts where the toolchain binaries (`go`, `javadoc`) live outside the
36		/// fixed hermetic PATH (e.g. a Nix devshell store path).
37		pub path_dirs: Vec<PathBuf>,
38	}
39	
40	impl ToolchainSet {
41		/// Empty set (no toolchains resolved).
42		pub fn empty() -> Self {
43			Self::default()
44		}
45	
46		/// From `NUDOX_TOOLCHAIN_*` then standard env vars.
47		///
48		/// The single sanctioned toolchain-env read (the sealer boundary). Called
49		/// once at `ForgeRuntime::assemble`, never from the compiler.
50		///
51		/// Language oracles (Go/Java/C#) still shell out to host SDKs aft
```

### Read: /Users/philocalyst/Projects/Backend/workspace/heart/connection.rs
```
1	//! Our typestate markers. We of course want to avoid connections or attempted
2	//! sends to a database that isn't actually alive.
3	//!
4	//! This is our attempt to mark this.
5	
6	use crate::error::ConnectError;
7	
8	/// A configured-but-unverified store handle.
9	pub struct Cold;
10	
11	/// A connected, ready store handle.
12	pub struct Live;
13	
14	/// A `Cold` store handle that can verify itself and transition to a `Live`
15	/// handle of the associated type.
16	#[diagnostic::on_unimplemented(
17		message = "`{Self}` cannot be connected",
18		note = "implement `Connect` so the server can bring this store up uniformly"
19	)]
20	pub trait Connect: Sized {
21		/// The `Live` handle produced on success (query methods live there).
22		type Live;
23	
24		/// Verify reachability/credentials/schema, then promote to `Live`.
25		async fn connect(self) -> Result<Self::Live, ConnectError>;
26	}
27	
```

### Read: /Users/philocalyst/Projects/Backend/workspace/heart/lib.rs
```
1	//! Our shared vocabulary.
2	#![feature(adt_const_params)]
3	#![feature(return_type_notation)]
4	
5	pub mod access;
6	pub mod cache;
7	pub mod connection;
8	pub mod content;
9	pub mod cursor;
10	pub mod ecosystem;
11	pub mod error;
12	pub mod health;
13	pub mod identity;
14	pub mod package;
15	pub mod progress;
16	pub mod score;
17	pub mod search;
18	pub mod sink;
19	pub mod symbol;
20	pub mod tenant;
21	pub mod version;
22	
23	/// Unified observability (OTLP traces/logs/metrics + Pyroscope profiling).
24	///
25	/// Gated behind the optional `telemetry` feature so the heavy OpenTelemetry
26	/// stack is pulled in only by the crate that actually installs it (`server`);
27	/// every other `heart` consumer — and the Buck build — stays lean.
28	#[cfg(feature = "telemetry")]
29	pub mod telemetry;
30	
31	pub use access::{Federation, Source, SourceId, SourceRole, Sourced};
32	pub use health::{assert_probe_future_send, timed as timed_probe, Probe, Probeable};
33	pub use connection::{Cold, Connect, Live};
34	pub use content::{ContentHash, ContentHasher, Freshness, JobKey};
35	pub use cursor::{Advisory, Cursor, CursorError, Enforced, PolicyTag, SnapshotPolicy};
36	pub use ecosystem::{Edition, Language, Toolchain};
37	pub use error::{
38	    BackendKind, ConnectError, ConnectFailure, ErrorDetails, Failure, FailureKind, Phase, ResolutionState,
39	    Retryable, StoreError,
40	};
41	pub use identity::{
42	    CargoVersionError, EntryUri, Id, NameError, NpmVersionError, Package, PackageId,
43	    PackageCoordinates, PackageVersion, PythonVersionError, RegistryOrigin, SymbolId, VersionError,
44	};
45	pub use progress::{JobProgress, Percent, Progressive};
46	pub use score::{Score, Scored};
47	pub use search::Page;
48	pub use sink::DerivedStore;
49	pub use symbol::{Name, Symbol, SymbolKind};
50	pub use tenant::{OwnerKind, Visibility};
51	pub use version::Versioned;
52	
53	/// A globally-unique identifier.
54	pub type Guid = uuid::Uuid;
55	
```

### Read: /Users/philocalyst/Projects/Backend/workspace/heart/content.rs
```
1	//! Content addressing, job keys, and freshness.
2	
3	use serde::{Deserialize, Serialize};
4	
5	/// The content hash which serves three roles:
6	/// 1. Ensuring that package freshness hasn't changed.
7	/// 2. Dedupe on the content
8	/// 3. type marking anything that depends on it
9	#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
10	pub struct ContentHash([u8; 32]);
11	
12	impl ContentHash {
13		/// Wrap a raw 32-byte digest (e.g. read back from postgres).
14		pub const fn from_bytes(bytes: [u8; 32]) -> Self { Self(bytes) }
15	
16		/// The raw digest bytes.
17		pub const fn as_bytes(&self) -> &[u8; 32] { &self.0 }
18	
19		/// Hash a contiguous byte buffer.
20		pub fn of_bytes(bytes: &[u8]) -> Self { Self(*blake3::hash(bytes).as_bytes()) }
21	
22		/// Lower-hex encoding for filesystem names and log lines.
23		pub fn hex(&self) -> String { data_encoding::HEXLOWER.encode(&self.0) }
24	
25		pub fn builder() -> ContentHasher { ContentHasher(blake3::Hasher::new()) }
26	}
27	
28	impl std::fmt::Display for ContentHash {
29		fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
30			f.write_str(&self.hex())
31		}
32	}
33	
34	/// An incremental hasher for building a [`ContentHash`] from a stream of parts
35	/// without holding the whole package in memory.
36	pub struct ContentHasher(blake3::Hasher);
37	
38	impl ContentHasher {
39		/// Fold another chunk into the digest.
40		pub fn update(&mut self, bytes: &[u8]) -> &mut Self {
41			self.0.update(bytes);
42			self
43		}
44	
45		/// Finalize into a [`ContentHash`].
46		pub fn finalize(&self) -> ContentHash { ContentHash(*self.0.finalize().as_bytes()) }
47	}
48	
49	/// Domain-separated cache / producer job identity.
50	///
51	/// `JobKey = H(producer_version ‖ toolchain ‖ source ‖ dep_lock)` with each
52	/// component length-prefixed (little-endian `u64`), matching the historical
53	/// `CacheKey::derive` layout so keys stay stable across the CAS migration.
54	///
55	//
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/compile/producer/wire.rs
```
1	//! Shared module-membership wiring over language path conventions.
2	
3	use ir::entry::{Index, NudoxPath};
4	use ir::kind::Entry;
5	use ir::module::Module;
6	use rustc_hash::FxHashMap as HashMap;
7	
8	/// How a language spells parent/child relationships on [`NudoxPath`].
9	pub trait PathParent {
10		/// Parent path for membership, if any (skip roots / externals).
11		fn parent_of(&self, path: &NudoxPath) -> Option<NudoxPath>;
12	}
13	
14	/// Filesystem-style parents (`lib/attrsets/mapAttrs` → `lib/attrsets`).
15	///
16	/// Used by Nix attrpaths and any Local path that nests with `/`.
17	#[derive(Debug, Default, Clone, Copy)]
18	pub struct FsPathParent;
19	
20	impl PathParent for FsPathParent {
21		fn parent_of(&self, path: &NudoxPath) -> Option<NudoxPath> {
22			match path {
23				NudoxPath::Local(p) => {
24					// `Path::parent` of a single-segment path is `Some("")` — the
25					// flake/package root module. Do not treat empty as "no parent".
26					let parent = p.parent()?;
27					Some(NudoxPath::Local(parent.to_path_buf()))
28				}
29				NudoxPath::External { .. } => None,
30			}
31		}
32	}
33	
34	/// Build parent → children from a path-parent convention.
35	pub fn members_by_parent<P: PathParent>(
36		paths: impl Iterator<Item = NudoxPath>,
37		split: &P,
38		known: &HashMap<NudoxPath, Entry>,
39	) -> HashMap<NudoxPath, Vec<NudoxPath>> {
40		let mut members: HashMap<NudoxPath, Vec<NudoxPath>> = HashMap::default();
41		for child in paths {
42			if let Some(parent) = split.parent_of(&child) {
43				if known.contains_key(&parent) {
44					members.entry(parent).or_default().push(child);
45				}
46			}
47		}
48		members
49	}
50	
51	/// Assign `members` onto each `Entry::Module` in `by_path`.
52	pub fn apply_members(
53		by_path: &mut HashMap<NudoxPath, Entry>,
54		members: HashMap<NudoxPath, Vec<NudoxPath>>,
55	) {
56		for (parent, mut children) in members {
57			children.sort_by(|a, b| path_str(a).cmp(&path_str(b)));
58			if let Some(Entry::Module(sym)) = by_pat
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/sandbox/budget.rs
```
1	//! Capability budgets for sealed compute.
2	
3	use std::num::{NonZeroU32, NonZeroU64};
4	use std::path::{Path, PathBuf};
5	use std::time::Duration;
6	
7	use crate::limits::{Limits, Network};
8	use crate::spec::{Env, Mounts};
9	
10	/// Threat posture of the code a producer runs, driving *default* capability
11	/// policy (DAEMON-PLAN §2.3 / §5 Phase 6).
12	///
13	/// This is a policy dial, not a resource profile: [`ProducerProfile`] still
14	/// supplies the language-appropriate ceilings. The tier *clamps* those ceilings
15	/// (and picks the default network posture) so that regardless of how loose a
16	/// profile or per-package override is, hostile interpreter code can never be
17	/// granted more than the tier permits.
18	///
19	/// [`crate::ProducerProfile`]
20	#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
21	pub enum ThreatTier {
22		/// Interpreters that execute package code (nix / typescript / python).
23		/// Tightest ceilings; network defaults off.
24		Hostile,
25		/// Compilers / oracles over untrusted source (rust / go / java).
26		/// The default posture.
27		#[default]
28		Untrusted,
29		/// Fully trusted host tooling (none today). Loosest; the profile ceilings
30		/// pass through unclamped.
31		Trusted,
32	}
33	
34	impl ThreatTier {
35		/// Default network posture for freshly sealed work at this tier.
36		///
37		/// Every tier seals with the network *off* — the acquire phase is the only
38		/// place network is granted (see [`crate::seal::Job`]). This exists so the
39		/// default is expressed by policy, not by an implicit call-site constant.
40		pub const fn net_default(self) -> NetGrant {
41			NetGrant::Off
42		}
43	
44		/// Per-tier hard ceiling: the loosest limits this tier may ever be granted.
45		///
46		/// `None` (Trusted) means "no tier ceiling — the profile stands".
47		const fn ceiling(self) -> Option<Limits> {
48			match self {
49				// Hostile: 2 GiB / 5 min wall / 300 cpu-s / 128 pids. An interpreter
50				// running package c
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/compile/rust/ra/load.rs
```
1	//! Load a Cargo workspace once via `ra_ap_load_cargo` into a
2	//! [`LoadedWorkspace`] held for the duration of lowering.
3	
4	use std::{
5		path::{Path, PathBuf},
6		thread,
7	};
8	
9	use ra_ap_ide_db::{RootDatabase, prime_caches};
10	use ra_ap_load_cargo::{LoadCargoConfig, ProcMacroServerChoice, load_workspace};
11	use ra_ap_paths::AbsPathBuf;
12	use ra_ap_proc_macro_api::ProcMacroClient;
13	use ra_ap_project_model::{CargoConfig, ProjectManifest, ProjectWorkspace, RustLibSource};
14	use ra_ap_vfs::Vfs;
15	use tracing::{debug, warn};
16	
17	use super::super::error::Package;
18	
19	/// Producer knobs for a single workspace load.
20	#[derive(Debug, Clone)]
21	pub(crate) struct ExtractConfig {
22		pub document_private: bool,
23		/// → `CARGO_NET_OFFLINE` + cargo `--offline`.
24		pub offline: bool,
25		/// Run build scripts (`load_out_dirs_from_check`); default true.
26		pub run_build_scripts: bool,
27		pub proc_macros: ProcMacroPolicy,
28		/// Blanket / auto-trait probe depth (§6.2).
29		pub probe: ProbeTier,
30		/// `parallel_prime_caches` worker count; default `min(8, cores)`.
31		pub num_threads: usize,
32	}
33	
34	impl ExtractConfig {
35		/// Defaults matched to the rustdoc path (offline, build scripts on, std probes).
36		pub(crate) fn for_extract(document_private: bool) -> Self {
37			Self {
38				document_private,
39				offline: true,
40				run_build_scripts: true,
41				proc_macros: ProcMacroPolicy::Sysroot,
42				probe: ProbeTier::Std,
43				num_threads: thread::available_parallelism()
44					.map(|n| n.get().min(8))
45					.unwrap_or(1),
46			}
47		}
48	}
49	
50	/// How the proc-macro server is located.
51	#[derive(Debug, Clone)]
52	pub(crate) enum ProcMacroPolicy {
53		/// Use the sysroot's `rust-analyzer-proc-macro-srv`.
54		Sysroot,
55		/// Explicit path to a server binary built with the same toolchain.
56		Explicit(PathBuf),
57		/// Skip expansion entirely.
58		Disabled,
59	}
60	
61	/// How deep auto/blanket-impl synthesis goes (§6.2).
62	#[derive(Deb
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/compile/go/oracle.rs
```
1	//! Serde mirror of the Go oracle's JSON schema.
2	//!
3	//! These structs deserialize — field for field — the document emitted by
4	//! the vendored extractor at `oracle/` (see `oracle/serialize.go` for the
5	//! authoritative Go-side definitions). The oracle uses `omitempty`
6	//! aggressively, so every optional field here carries `#[serde(default)]`.
7	//!
8	//! The one structural invariant inherited from the oracle: named types
9	//! and aliases appear as *references* (`kind: "named"/"alias"` with
10	//! `pkg` + `name`), never expanded inline. Their definitions live in the
11	//! owning package's `decls` list. Anonymous composites (structs,
12	//! interfaces, funcs, maps, …) are expanded structurally. Go cannot form
13	//! a cyclic type without a named intermediary, so the [`Type`] tree is
14	//! always finite.
15	
16	use std::collections::HashMap;
17	
18	use serde::Deserialize;
19	
20	/// The root of the oracle's JSON document.
21	#[derive(Debug, Clone, Deserialize)]
22	#[serde(rename_all = "camelCase")]
23	pub struct Output {
24		/// go.mod-derived metadata for the loaded module.
25		#[serde(default)]
26		pub module: Option<Module>,
27	
28		/// One entry per package, sorted by import path.
29		#[serde(default)]
30		pub packages: Vec<Package>,
31	
32		/// Package-load diagnostics. The oracle proceeds best-effort, so a
33		/// non-empty list does not invalidate `packages`.
34		#[serde(default)]
35		pub errors: Vec<String>,
36	}
37	
38	/// go.mod-derived module metadata.
39	#[derive(Debug, Clone, Deserialize)]
40	#[serde(rename_all = "camelCase")]
41	pub struct Module {
42		/// The module path (the `module` directive).
43		#[serde(default)]
44		pub path: String,
45	
46		/// The on-disk module root.
47		#[serde(default)]
48		pub dir: String,
49	
50		/// The `go` directive (e.g. `"1.23"`).
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/compile/java/oracle.rs
```
1	//! Running the vendored javadoc doclet.
2	//!
3	//! The doclet (`//workspace/compiler/compile/java/oracle:extractor`) is
4	//! built ahead of time by Buck2 as a plain `java_library` (it has no
5	//! dependencies beyond the JDK itself — JDK 17+; the doclet uses
6	//! `getPermittedSubclasses`, records, and pattern matching) and shipped as a
7	//! `resources` artifact jar alongside this binary — see
8	//! `producer::buck_resource`. There is no runtime `javac` step.
9	//!
10	//! Invocation: `javadoc -doclet nudox.oracle.Extractor -docletpath <jar>
11	//! -private -quiet -encoding UTF-8 -outfile <json> @<argfile>` over every
12	//! `.java` file found under the requested source roots.
13	//!
14	//! `-private` includes every declaration regardless of access — lowering
15	//! (not extraction) is where policy lives. The file list rides an @argfile
16	//! to dodge OS argv limits; entries are quoted per javadoc's argfile rules.
17	//!
18	//! `javadoc` comes from `PATH` (via the sandboxed toolchain). JEP 467
19	//! Markdown doc comments (`///`) are only *parsed as documentation* by
20	//! JDK ≥ 23 toolchains — an older JDK silently reports `doc: null` for
21	//! them, so prefer a current JDK.
22	
23	use std::fs;
24	use std::path::{Path, PathBuf};
25	
26	use crate::compile::isolate::{self, IsolatedCommand, IsolatedFailure, IsolatedFailureKind};
27	use crate::compile::producer;
28	use sandbox::ProducerProfile;
29	
30	use super::schema;
31	use super::error::{DocletError, ExtractionError, JavadocError, OracleError};
32	
33	/// Resolve the Buck2-built doclet jar shipped alongside this executable.
34	fn doclet_jar() -> Result<PathBuf, DocletError> {
35		producer::buck_resource("java-oracle.jar").map_err(|source| DocletError::ResourceNotFound { source })
36	}
37	
38	/// Run the oracle over `source_roots` and deserialize its JSON document.
39	///
40	/// This is the one-call entry point: it collects the source set, runs
41	/// `javadoc` against the prebuilt doclet jar, and parses the
```

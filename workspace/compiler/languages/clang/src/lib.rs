//! C/C++ producer for the nudox-ir pipeline.
//!
//! # Architecture
//!
//! The producer is split into two phases, mirroring the [`Producer`] contract:
//!
//! **`invoke`** — drives libclang over the package's sources, extracts
//! everything into fully-owned plain-Rust data ([`ClangOracle`]), then drops
//! all libclang objects.  No arena references escape this phase.
//!
//! **`lower`** — walks the owned [`ClangOracle`] in a single pass and emits
//! declarations into [`Lowering`] using USRs as the stable [`Producer::Id`].
//!
//! # Header-filtering
//!
//! A libclang TU for a `.c`/`.cpp` file transitively includes all headers it
//! pulls in — including system headers, libc, STL, etc.  The producer filters
//! all cursors down to the file it was asked to parse by comparing each
//! entity's *resolved* file (`clang_getFileLocation`, which follows macro
//! expansion to a concrete file — see `extract::is_in_main_file`'s doc
//! comment) against that path directly, rather than trusting
//! [`Entity::is_in_main_file`]/`clang_Location_isFromMainFile`: that call
//! answers `false` for any declaration whose opening token comes from a
//! macro expansion — e.g. `namespace nlohmann` opened via
//! `NLOHMANN_JSON_NAMESPACE_BEGIN` — even when every byte of it is in the
//! file being parsed, which silently discarded such a declaration's entire
//! subtree.
//!
//! # System-header discovery
//!
//! `clang_parseTranslationUnit` calls straight into libclang's C API — no
//! shell, no wrapper script. On a system where the "real" compiler on `PATH`
//! is a wrapper that injects its toolchain's `-isystem`/`-isysroot` flags
//! itself (nix's `cc-wrapper` is exactly this), that injection never
//! happens, so even `#include <algorithm>` fails to resolve and every real
//! C++ file becomes a near-empty translation unit. [`producer::invoke`]
//! asks a real compiler driver once, up front, what its default search path
//! is (`system_includes::discover`), and threads that into every file's
//! argument list. See that module's docs for how and why.
//!
//! # Id choice: USR
//!
//! The producer uses Clang's **Unified Symbol Resolution** (USR) string as
//! `Self::Id`.  The USR is a stable, cross-TU, language-agnostic unique key
//! for every declaration.  It is exactly the right choice — analogous to how
//! the C# producer uses Roslyn DocIds — and is what the old compiler/clang
//! producer should have used.
//!
//! # IR gaps
//!
//! * **Macro constants**: `#define FOO 42`-style macros that are purely
//!   numeric constants are not emitted; libclang does not expose their values
//!   in the AST in a typed way without extra token work.
//!
//! * **Operator overloads**: emitted as regular functions with their mangled
//!   name (e.g. `"operator+"`) — the IR has no `FnModifier::Operator`.
//!
//! * **Anonymous structs/unions**: entities with no name are skipped (USR is
//!   the empty string in that case).  The old compiler emitted them; the new IR
//!   requires a non-empty name.
//!
//! * **`constexpr`**: mapped to `FnModifier::Const`; there is no separate IR
//!   concept for `constexpr` vs `const`.
//!
//! * **Multiple overloads as separate declarations**: the spec requires this;
//!   each overload gets its own distinct USR from libclang, so the Lowering
//!   sink naturally produces one entry per overload.

pub(crate) mod compile_commands;
pub(crate) mod extract;
pub mod lower;
pub mod oracle;
pub mod producer;
pub(crate) mod system_includes;

#[cfg(test)]
mod tests;

pub use oracle::ClangOracle;
pub use producer::ClangProducer;

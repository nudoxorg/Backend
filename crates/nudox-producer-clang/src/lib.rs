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
//! all cursors through [`Entity::is_in_main_file`] in the visitor, which
//! libclang defines as "the entity was lexically defined in the compilation
//! unit's main source file, not in a header or included file".  This is the
//! only cheap, correct filter that avoids emitting all of libc.
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
//! * **Union**: [`RecordForm`] has no `Union` variant.  C unions are emitted
//!   as `RecordForm::Struct` with a field attribute comment.  This is a gap in
//!   the IR that needs a new `RecordForm::Union` variant.
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

pub mod oracle;
pub mod lower;
pub mod producer;
pub(crate) mod extract;

#[cfg(test)]
mod tests;

pub use producer::ClangProducer;
pub use oracle::ClangOracle;

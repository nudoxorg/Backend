//! The per-declaration identifier used as `Producer::Id`.
//!
//! # Why this shape
//!
//! OXC's `SymbolId` is per-file and only valid within one parse+semantic session;
//! it cannot survive across `invoke()` because we drop the arena after extraction.
//! The old pipeline used path segments as the stable key; we follow that here.
//!
//! `TsId` is a `(module: PathBuf, name: String, discriminant: u32)` triple:
//!   - `module` — absolute canonical path of the source file.
//!   - `name`   — the declaration name (the exported/declared identifier).
//!   - `discriminant` — disambiguates same-named declarations within one module
//!     (overload signatures get distinct discriminants; merged declarations like
//!     `interface Foo + namespace Foo` each get their own TsId).
//!
//! # Declaration merging
//!
//! TypeScript allows an interface and a namespace, or a function and a namespace,
//! to share a name. Each individual declaration gets its own `TsId` (with a
//! different `discriminant`). The `Lowering` sink assigns each a separate IR
//! entry; they remain distinct nodes. This is the correct representation: the IR
//! has no concept of merged declarations and the consumer must reconcile them via
//! the name field if it cares.
//!
//! # Overload signatures
//!
//! Each overload signature gets its own `TsId` (discriminant = declaration index
//! within the group). The spec requires two distinct IR declarations for an
//! overload pair — this naturally satisfies that requirement.

use std::path::PathBuf;

/// Stable identifier for one TypeScript declaration.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TsId {
    /// Absolute canonical path of the module that contains this declaration.
    pub module: PathBuf,
    /// The declaration's exported/local name.
    pub name: String,
    /// Disambiguates multiple same-named declarations within one module
    /// (overloads, merged declarations).
    pub discriminant: u32,
}

impl TsId {
    pub fn new(module: PathBuf, name: impl Into<String>, discriminant: u32) -> Self {
        TsId {
            module,
            name: name.into(),
            discriminant,
        }
    }
}

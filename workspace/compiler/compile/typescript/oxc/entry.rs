//! Entry-point + declaration-root discovery (OXC-PLAN §Phase 1), resolver-first.
//!
//! Replaces the hand-rolled `.d.ts` discovery: a package root resolves through
//! `oxc_resolver` (`ts.resolveModuleName` semantics), with the existing
//! conventional fallbacks (`mod.ts`, `index.ts`, ...) and the `repo:` hint kept
//! for manifest-less trees.
//!
//! LEAF FILE — fill the `todo!()` bodies.

use std::path::{Path, PathBuf};

use oxc_resolver::{ResolveOptions, Resolver};

use super::error::Package;

/// Build the resolver used for both entry discovery and graph edges, configured
/// for TypeScript `.d.ts`-first resolution (OXC-PLAN §Phase 1.1 ResolveOptions).
pub(crate) fn make_resolver() -> Resolver {
	// condition_names ["types","import","node"], main_fields
	// ["types","typings","module","main"], extensions [".d.ts",".ts",".tsx",
	// ".js",".json"], extension_alias for .js/.mjs/.cjs, builtin_modules true,
	// node_path false.
	let _ = ResolveOptions::default();
	todo!("entry.rs: construct the .d.ts-first Resolver")
}

/// Discover the declaration roots for the package at `root`.
///
/// Resolves the package entry (`resolver.resolve(root, ".")`), expands the
/// `exports` fan-out, and falls back to conventional entry points and the
/// `repo:` hint. Returns absolute module file paths.
pub(crate) fn discover_entry_points(root: &Path) -> Result<Vec<PathBuf>, Package> {
	let _ = root;
	todo!("entry.rs: resolver-first entry discovery + fallbacks")
}

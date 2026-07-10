//! Documentation, deprecation, intra-doc links, and cfg gating (§3.8).

use ir::{entry::NudoxPath, kind::Deprecation};
use ra_ap_hir::{DocLinkDef, HasAttrs, IsInnerDoc, resolve_doc_path_on};
use rustc_hash::FxHashMap as HashMap;

use super::ctx::LowerCtx;

/// Docs string for any `HasAttrs` def — parity with rustdoc's `Item.docs`.
///
/// Uses `hir_docs` (structured docs owner) rather than raw attributes so
/// module-level and item-level docs both resolve consistently.
pub(crate) fn documentation<D: HasAttrs + Copy>(
	ctx: &LowerCtx<'_>,
	def: D,
) -> Option<String> {
	def.hir_docs(ctx.db).map(|d| d.docs().to_owned()).filter(|s| !s.is_empty())
}

/// Deprecation marker for any `HasAttrs` def (`#[deprecated]`).
///
/// `AttrsWithOwner::is_deprecated` on ra_ap 0.0.341 is a bitflag — it reports
/// *that* an item is deprecated but not the `since`/`note` payload (the string
/// arguments are discarded when the flag is built). We therefore emit a marker
/// with empty `since`/`note`.
///
/// TODO(P4): recover `since`/`note`. Not available on `ra_ap_hir::AttrsWithOwner`
/// (0.0.341); would need the AST `#[deprecated(...)]` attribute (via `HasSource`)
/// or dropping to `hir_def` raw attrs (`by_key`), neither reachable uniformly
/// from `HasAttrs` here.
pub(crate) fn deprecation<D: HasAttrs + Copy>(
	ctx: &LowerCtx<'_>,
	def: D,
) -> Option<Deprecation> {
	if def.attrs(ctx.db).is_deprecated() {
		Some(Deprecation { since: None, note: None })
	} else {
		None
	}
}

/// cfg-gating predicate string for any `HasAttrs` def (`#[cfg(...)]`).
///
/// `AttrsWithOwner::cfgs` returns a typed `&CfgExpr`; rendered via its `Display`
/// impl (e.g. `feature = "x"`). `None` when the item is unconditional.
pub(crate) fn cfg_string<D: HasAttrs + Copy>(
	ctx: &LowerCtx<'_>,
	def: D,
) -> Option<String> {
	def.attrs(ctx.db).cfgs(ctx.db).map(|c| c.to_string())
}

/// Resolved intra-doc links for a def: `link text → target NudoxPath`.
///
/// Extracts `[link]` / `[link](target)` markdown targets from `docs`, resolves
/// each with `resolve_doc_path_on`, and keeps the ones that resolve to a
/// `ModuleDef` (mapped to its canonical [`NudoxPath`]). `Field` / `SelfType`
/// doc-link targets are skipped — they have no standalone module path. This is
/// the input to the linked-data `mentions` edge.
pub(crate) fn doc_links<D: HasAttrs + Copy>(
	ctx: &mut LowerCtx<'_>,
	def: D,
	docs: Option<&str>,
) -> Option<HashMap<String, NudoxPath>> {
	let docs = docs?;
	let mut out: HashMap<String, NudoxPath> = HashMap::default();
	for link in extract_doc_link_targets(docs) {
		let resolved = resolve_doc_path_on(ctx.db, def, &link, None, IsInnerDoc::No);
		if let Some(DocLinkDef::ModuleDef(def)) = resolved
			&& let Some(path) = ctx.nudox_path(def)
		{
			out.insert(link, path);
		}
	}
	if out.is_empty() { None } else { Some(out) }
}

/// Candidate intra-doc link targets from a markdown doc string.
///
/// Handles the common inline forms: `[Type]`, `[`Type`]`, `[text](Type)` and
/// reference definitions `[text]: Type`. The resolver rejects non-item targets
/// (URLs, prose), so a permissive extractor is fine — false candidates simply
/// fail to resolve. Intentionally lightweight (no full markdown parse).
fn extract_doc_link_targets(docs: &str) -> Vec<String> {
	let mut out = Vec::new();
	let mut seen = std::collections::HashSet::new();
	let bytes = docs.as_bytes();
	let mut i = 0;
	while i < bytes.len() {
		if bytes[i] != b'[' {
			i += 1;
			continue;
		}
		// Find the matching `]`.
		let Some(close_rel) = docs[i + 1..].find(']') else {
			break;
		};
		let close = i + 1 + close_rel;
		let inner = docs[i + 1..close].trim().trim_matches('`').trim();

		// Explicit target: `](target)` or `]: target`.
		let after = &docs[close + 1..];
		let target = if let Some(rest) = after.strip_prefix('(') {
			rest.split(')').next().map(|s| s.trim())
		} else if let Some(rest) = after.strip_prefix(':') {
			rest.trim_start()
				.split_whitespace()
				.next()
		} else {
			None
		};

		let candidate = target.unwrap_or(inner);
		let candidate = candidate.trim().trim_matches('`').trim();
		if is_pathish(candidate) && seen.insert(candidate.to_owned()) {
			out.push(candidate.to_owned());
		}
		i = close + 1;
	}
	out
}

/// A cheap filter: a doc-link target worth trying to resolve looks like a Rust
/// path (identifiers + `::`, optional leading method `Self::`), not a URL or
/// prose. Skips anything with a scheme (`http:`) or whitespace.
fn is_pathish(s: &str) -> bool {
	if s.is_empty() || s.contains("://") || s.contains(char::is_whitespace) {
		return false;
	}
	// Reject if it is not made of identifier chars / `:` / `.` (method links).
	s.chars()
		.all(|c| c.is_alphanumeric() || matches!(c, '_' | ':' | '.'))
		&& s.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_')
}

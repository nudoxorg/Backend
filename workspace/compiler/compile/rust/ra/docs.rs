//! Documentation, deprecation, and (later) intra-doc links (§3.8).

use ra_ap_hir::HasAttrs;

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

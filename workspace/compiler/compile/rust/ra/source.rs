//! `HasSource` → file range / source text for the function body map (§3.9).

use ra_ap_hir::{Function, HasSource, ModuleDef};
use ra_ap_base_db::SourceDatabase;
use ra_ap_syntax::AstNode;

use super::ctx::{LowerCtx, PathKey};

/// Byte-precise source slice for a function (macro-aware via `original_range_opt`).
///
/// Key is the canonical path string (`calculator::add`). Text is the full `fn`
/// item span — same shape as the rustdoc line-range source map.
pub(crate) fn fn_source(
	ctx: &mut LowerCtx<'_>,
	f: Function,
) -> Option<(PathKey, String)> {
	let key = ctx.canonical(ModuleDef::Function(f))?;

	// Prefer Semantics so the parse tree is cached for original_range_opt.
	let src = ctx.sema.source(f).or_else(|| f.source(ctx.db))?;
	let syntax = src.value.syntax();

	let text = match ctx.sema.original_range_opt(syntax) {
		Some(range) => slice_file_range(ctx, range).unwrap_or_else(|| syntax.text().to_string()),
		None => syntax.text().to_string(),
	};

	if text.is_empty() {
		return None;
	}
	Some((key, text))
}

/// Slice vfs file text for a macro-upmapped `FileRange` (EditionedFileId).
fn slice_file_range(
	ctx: &LowerCtx<'_>,
	range: ra_ap_hir::FileRange,
) -> Option<String> {
	// EditionedFileId::file_id takes any salsa Database.
	let file_id = range.file_id.file_id(ctx.db);
	let full = ctx.db.file_text(file_id).text(ctx.db);
	let start: usize = range.range.start().into();
	let end: usize = range.range.end().into();
	full.get(start..end).map(str::to_owned)
}

//! Documentation, deprecation, intra-doc links, and cfg gating (§3.8).

use ir::{entry::NudoxPath, kind::Deprecation};
use ra_ap_hir::{
	Adt, DocLinkDef, HasAttrs, HasSource, IsInnerDoc, ModuleDef, resolve_doc_path_on,
};
use ra_ap_syntax::{
	AstNode, AstToken,
	ast::{self, HasAttrs as AstHasAttrs},
};
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

/// Deprecation marker for a `ModuleDef` (`#[deprecated]`).
///
/// The gate ("is it deprecated at all") stays HIR-driven:
/// `AttrsWithOwner::is_deprecated` on ra_ap 0.0.341 is a bitflag that discards
/// the `since`/`note` string arguments. To recover them we drop to the AST: find
/// the def's `#[deprecated]` `ast::Attr` (via `HasSource`) and parse its meta.
///
/// Payload shapes handled:
/// - `#[deprecated]`                       → `since=None, note=None`
/// - `#[deprecated = "note"]`              → `note` from `Meta::expr()`
/// - `#[deprecated(since="x", note="y")]`  → both, from `Meta::token_tree()`
///
/// A bare `#[deprecated]` still yields `Some(Deprecation { .. })` off the HIR
/// gate even when the AST is unavailable (macro-generated items).
pub(crate) fn deprecation(ctx: &LowerCtx<'_>, def: ModuleDef) -> Option<Deprecation> {
	if !def.attrs(ctx.db).is_deprecated() {
		return None;
	}
	// HIR says deprecated; enrich with AST-parsed `since`/`note` when reachable.
	let (since, note) = module_def_deprecated_attr(ctx, def)
		.map(|attr| parse_deprecated_meta(&attr))
		.unwrap_or((None, None));
	Some(Deprecation { since, note })
}

/// The `#[deprecated]` `ast::Attr` on a `ModuleDef`, if its AST is reachable.
///
/// Each variant carries a distinct `HasSource::Ast` node; all item nodes impl
/// `ast::HasAttrs`, so we scan `attrs()` for the one whose path is `deprecated`.
fn module_def_deprecated_attr(ctx: &LowerCtx<'_>, def: ModuleDef) -> Option<ast::Attr> {
	fn find<N: AstHasAttrs>(node: N) -> Option<ast::Attr> {
		node.attrs().find(|a| {
			a.path()
				.and_then(|p| p.as_single_name_ref())
				.is_some_and(|n| n.text() == "deprecated")
		})
	}
	match def {
		ModuleDef::Function(f) => find(ctx.sema.source(f)?.value),
		ModuleDef::Adt(Adt::Struct(s)) => find(ctx.sema.source(s)?.value),
		ModuleDef::Adt(Adt::Enum(e)) => find(ctx.sema.source(e)?.value),
		ModuleDef::Adt(Adt::Union(u)) => find(ctx.sema.source(u)?.value),
		ModuleDef::Trait(t) => find(ctx.sema.source(t)?.value),
		ModuleDef::TypeAlias(ta) => find(ctx.sema.source(ta)?.value),
		ModuleDef::Const(c) => find(ctx.sema.source(c)?.value),
		ModuleDef::Static(s) => find(ctx.sema.source(s)?.value),
		// `Module` source is a `ModuleSource` enum, not a single `HasAttrs` item
		// node; module-level `#[deprecated]` is vanishingly rare and still yields
		// a bare marker off the HIR gate. `EnumVariant`s aren't emitted as symbols.
		ModuleDef::Module(_)
		| ModuleDef::Macro(_)
		| ModuleDef::BuiltinType(_)
		| ModuleDef::EnumVariant(_) => None,
	}
}

/// Parse `(since, note)` from a `#[deprecated ...]` `ast::Attr`.
///
/// `Meta::expr()` covers `#[deprecated = "note"]` (name-value form); its token
/// tree covers `#[deprecated(since = "x", note = "y")]`. String literals are
/// unquoted/unescaped via `ast::String::value`. Order-independent, tolerant of
/// missing keys.
fn parse_deprecated_meta(attr: &ast::Attr) -> (Option<String>, Option<String>) {
	match attr.meta() {
		// `#[deprecated = "note"]`
		Some(ast::Meta::KeyValueMeta(kv)) => {
			if let Some(ast::Expr::Literal(lit)) = kv.expr()
				&& let ast::LiteralKind::String(s) = lit.kind()
			{
				(None, string_value(&s))
			} else {
				(None, None)
			}
		}
		// `#[deprecated(since = "x", note = "y")]`
		Some(ast::Meta::TokenTreeMeta(ttm)) => {
			let Some(tt) = ttm.token_tree() else {
				return (None, None);
			};
			let mut since = None;
			let mut note = None;
			extract_kv_strings(&tt, &mut since, &mut note);
			(since, note)
		}
		// Bare `#[deprecated]` (`PathMeta`) or unknown shape.
		_ => (None, None),
	}
}

/// Scan a `deprecated(...)` token tree for `since = "…"` / `note = "…"`.
///
/// Walks the flat token stream: on an ident `since`/`note` followed by `=` then
/// a string literal, records the literal's value. String tokens are unquoted via
/// `ast::String`. Deliberately simple — the attr grammar here is flat key = lit.
fn extract_kv_strings(tt: &ast::TokenTree, since: &mut Option<String>, note: &mut Option<String>) {
	use ra_ap_syntax::{SyntaxKind, T};
	let mut key: Option<String> = None;
	let mut expect_value = false;
	for tok in tt.syntax().children_with_tokens() {
		let Some(tok) = tok.into_token() else { continue };
		match tok.kind() {
			SyntaxKind::IDENT => {
				key = Some(tok.text().to_owned());
				expect_value = false;
			}
			T![=] => {
				expect_value = key.is_some();
			}
			SyntaxKind::STRING if expect_value => {
				let value = ast::String::cast(tok)
					.and_then(|s| s.value().ok().map(|v| v.into_owned()));
				match key.take().as_deref() {
					Some("since") => *since = value,
					Some("note") => *note = value,
					_ => {}
				}
				expect_value = false;
			}
			SyntaxKind::WHITESPACE | SyntaxKind::COMMENT => {}
			T![,] => {
				key = None;
				expect_value = false;
			}
			_ => {}
		}
	}
}

/// Unquote/unescape a `#[deprecated = "…"]` string-literal AST node.
fn string_value(s: &ast::String) -> Option<String> {
	s.value().ok().map(|v| v.into_owned())
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

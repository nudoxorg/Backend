//! Documentation, deprecation, cfg gating, and intra-doc links.
//!
//! Ported from `workspace/compiler/compile/rust/ra/docs.rs`.
//!
//! # What changed
//!
//! - `doc_links` now returns `Option<Vec<nudox_ir::entry::DocLink>>` instead
//!   of `Option<HashMap<String, NudoxPath>>`.  The new IR `DocLink` carries
//!   `target: String` (the canonical path string) and `label: Option<String>`.
//! - `deprecation` returns `nudox_ir::entry::Deprecation` directly (the field
//!   names are the same: `note`, `since`).
//! - `cfg_string` is renamed `cfg_expr` and now returns
//!   `Option<nudox_ir::entry::CfgExpr>` (a structured enum).  Structural
//!   parsing is best-effort; unknown forms fall through to `CfgExpr::Other`.

use ra_ap_hir::{
    Adt, DocLinkDef, HasAttrs, HasSource, IsInnerDoc, ModuleDef, resolve_doc_path_on,
};
use ra_ap_syntax::AstToken;
use ra_ap_syntax::{
    AstNode,
    ast::{self, HasAttrs as AstHasAttrs},
};

use nudox_ir::entry::{CfgExpr, Deprecation, DocLink};

use super::ctx::LowerCtx;

// ── Documentation ─────────────────────────────────────────────────────────────

/// Doc string for any `HasAttrs` def.
///
/// Uses `hir_docs` (structured docs owner) so module-level and item-level docs
/// both resolve consistently.
pub(crate) fn documentation<D: HasAttrs + Copy>(
    ctx: &LowerCtx<'_>,
    def: D,
) -> Option<String> {
    def.hir_docs(ctx.db)
        .map(|d| d.docs().to_owned())
        .filter(|s| !s.is_empty())
}

// ── Deprecation ───────────────────────────────────────────────────────────────

/// `#[deprecated]` marker for a `ModuleDef`, parsing `since`/`note` from AST.
///
/// Ported verbatim from old `docs.rs`.
pub(crate) fn deprecation(ctx: &LowerCtx<'_>, def: ModuleDef) -> Option<Deprecation> {
    if !def.attrs(ctx.db).is_deprecated() {
        return None;
    }
    let (since, note) = module_def_deprecated_attr(ctx, def)
        .map(|attr| parse_deprecated_meta(&attr))
        .unwrap_or((None, None));
    Some(Deprecation { since, note })
}

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
        ModuleDef::Module(_)
        | ModuleDef::Macro(_)
        | ModuleDef::BuiltinType(_)
        | ModuleDef::EnumVariant(_) => None,
    }
}

fn parse_deprecated_meta(attr: &ast::Attr) -> (Option<String>, Option<String>) {
    match attr.meta() {
        Some(ast::Meta::KeyValueMeta(kv)) => {
            if let Some(ast::Expr::Literal(lit)) = kv.expr()
                && let ast::LiteralKind::String(s) = lit.kind()
            {
                (None, string_value(&s))
            } else {
                (None, None)
            }
        }
        Some(ast::Meta::TokenTreeMeta(ttm)) => {
            let Some(tt) = ttm.token_tree() else {
                return (None, None);
            };
            let mut since = None;
            let mut note = None;
            extract_kv_strings(&tt, &mut since, &mut note);
            (since, note)
        }
        _ => (None, None),
    }
}

fn extract_kv_strings(
    tt: &ast::TokenTree,
    since: &mut Option<String>,
    note: &mut Option<String>,
) {
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

fn string_value(s: &ast::String) -> Option<String> {
    s.value().ok().map(|v| v.into_owned())
}

// ── Cfg ───────────────────────────────────────────────────────────────────────

/// Structured cfg predicate for a `HasAttrs` def.
///
/// The `ra_ap_cfg::CfgExpr` from `AttrsWithOwner::cfgs` carries a recursive
/// boolean structure.  We map it to the IR's `CfgExpr`, which mirrors that
/// structure with named variants for common predicates and an `Other` fallback.
///
/// `None` when the item is unconditional.
pub(crate) fn cfg_expr<D: HasAttrs + Copy>(
    ctx: &LowerCtx<'_>,
    def: D,
) -> Option<CfgExpr> {
    let ra_cfg = def.attrs(ctx.db).cfgs(ctx.db)?;
    Some(lower_cfg(&ra_cfg))
}

fn lower_cfg(cfg: &ra_ap_cfg::CfgExpr) -> CfgExpr {
    use ra_ap_cfg::CfgExpr as Ra;
    match cfg {
        Ra::Invalid => CfgExpr::Other("invalid".into()),
        Ra::Atom(atom) => lower_cfg_atom(atom),
        Ra::All(inner) => {
            let exprs: Box<[CfgExpr]> = inner.iter().map(lower_cfg).collect();
            CfgExpr::All(exprs)
        }
        Ra::Any(inner) => {
            let exprs: Box<[CfgExpr]> = inner.iter().map(lower_cfg).collect();
            CfgExpr::Any(exprs)
        }
        Ra::Not(inner) => CfgExpr::Not(Box::new(lower_cfg(inner))),
    }
}

fn lower_cfg_atom(atom: &ra_ap_cfg::CfgAtom) -> CfgExpr {
    use ra_ap_cfg::CfgAtom;
    match atom {
        CfgAtom::Flag(flag) => {
            // e.g. `cfg(feature = "foo")` becomes Flag("feature") in some RA
            // versions when no value is present; treat as Other.
            CfgExpr::Other(flag.as_str().into())
        }
        CfgAtom::KeyValue { key, value } => {
            let k = key.as_str();
            let v = value.as_str();
            match k {
                "feature" => CfgExpr::Feature(v.into()),
                "target_os" => CfgExpr::TargetOs(v.into()),
                "target_arch" => CfgExpr::TargetArch(v.into()),
                _ => CfgExpr::Other(format!("{k} = \"{v}\"")),
            }
        }
    }
}

// ── Intra-doc links ───────────────────────────────────────────────────────────

/// Resolved intra-doc links: `Vec<DocLink>` (target = canonical path string,
/// label = the link text as written).
///
/// Empty when no links resolve to known `ModuleDef`s.
pub(crate) fn doc_links<D: HasAttrs + Copy>(
    ctx: &mut LowerCtx<'_>,
    def: D,
    docs: Option<&str>,
) -> Option<Vec<DocLink>> {
    let docs = docs?;
    let mut out: Vec<DocLink> = Vec::new();

    for link in extract_doc_link_targets(docs) {
        let resolved = resolve_doc_path_on(ctx.db, def, &link, None, IsInnerDoc::No);
        if let Some(DocLinkDef::ModuleDef(resolved_def)) = resolved {
            if let Some(target_key) = ctx.canonical(resolved_def) {
                out.push(DocLink {
                    target: target_key.to_string(),
                    label: Some(link),
                });
            }
        }
    }

    if out.is_empty() { None } else { Some(out) }
}

/// Candidate intra-doc link targets from a markdown doc string.
///
/// Ported verbatim from old `docs.rs`.
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
        let Some(close_rel) = docs[i + 1..].find(']') else {
            break;
        };
        let close = i + 1 + close_rel;
        let inner = docs[i + 1..close].trim().trim_matches('`').trim();

        let after = &docs[close + 1..];
        let target = if let Some(rest) = after.strip_prefix('(') {
            rest.split(')').next().map(|s| s.trim())
        } else if let Some(rest) = after.strip_prefix(':') {
            rest.trim_start().split_whitespace().next()
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

fn is_pathish(s: &str) -> bool {
    if s.is_empty() || s.contains("://") || s.contains(char::is_whitespace) {
        return false;
    }
    s.chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '_' | ':' | '.'))
        && s.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_')
}

// ── Attribute tokens ──────────────────────────────────────────────────────────

/// Extract well-known attribute tokens from a `ModuleDef` for `Symbol.attrs`.
///
/// Only a representative subset is produced: `must_use`, `repr(...)`,
/// `doc(hidden)`.  The full attribute list is intentionally not scraped — that
/// would require walking raw token trees for every item and is deferred.
///
/// UNCERTAINTY: `ra_ap_hir::HasAttrs::attrs(db)` returns an `AttrsWithOwner`
/// but the public API for iterating individual `#[...]` attributes on arbitrary
/// `ModuleDef` items is limited in ra_ap 0.0.341.  Confirmed fields:
/// `is_deprecated()`, `cfgs()`, `hir_docs()`.  Iterating arbitrary attrs
/// requires either a downcast to a specific HIR item type or use of
/// `hir_def::attr::Attrs`, which is not fully pub in ra_ap.
/// The `attrs` field on `Symbol` is therefore emitted empty here; this is
/// documented in the producer report as a known gap.
pub(crate) fn symbol_attrs(
    _ctx: &LowerCtx<'_>,
    _def: ModuleDef,
) -> Box<[nudox_ir::entry::AttrTok]> {
    Box::new([])
}

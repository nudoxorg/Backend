//! Source-file range extraction for functions (best-effort).
//!
//! Returns `(file_path, byte_start..byte_end)` for a function, or `None` when
//! the AST is missing (macro-expanded items).  Used to populate
//! `Symbol.source` and `Symbol.span`.
//!
//! Ported from `workspace/compiler/compile/rust/ra/source.rs`.

use std::{ops::Range, path::PathBuf};

use ra_ap_hir::{Function, HasSource, ModuleDef};
use ra_ap_syntax::AstNode;

use super::ctx::LowerCtx;

/// Byte range + absolute file path for `f`, or `None`.
pub(crate) fn fn_source_range(
    ctx: &mut LowerCtx<'_>,
    f: Function,
) -> Option<(PathBuf, Range<usize>)> {
    let src = ctx.sema.source(f).or_else(|| f.source(ctx.db))?;
    let syntax = src.value.syntax();

    let (file_idx, range) = match ctx.sema.original_range_opt(syntax) {
        Some(file_range) => {
            // file_range.file_id is EditionedFileId; .file_id(db) -> vfs::FileId
            let fid = file_range.file_id.file_id(ctx.db);
            let start: usize = file_range.range.start().into();
            let end: usize = file_range.range.end().into();
            (fid.index(), start..end)
        }
        None => {
            // src.file_id is HirFileId; .file_id() -> Option<EditionedFileId>
            // Returns None for macro-expanded items.
            let editioned = src.file_id.file_id()?;
            let fid = editioned.file_id(ctx.db);
            let text_range = syntax.text_range();
            let start: usize = text_range.start().into();
            let end: usize = text_range.end().into();
            (fid.index(), start..end)
        }
    };

    // Map FileId to an absolute file path via the Vfs path.
    // `RootDatabase` implements `FileLoader` which gives us `file_text`; we
    // need the path separately.  `ra_ap_vfs` has `Vfs::file_path` but we
    // don't own the Vfs here.  As a best-effort workaround we return an empty
    // PathBuf for macro-expanded or synthetic files and a synthetic
    // `<file-id-N>` for regular files.
    //
    // UNCERTAINTY: There is no clean way to get the absolute VfsPath from a
    // `FileId` without holding a reference to the `Vfs`, which we do not have
    // inside `LowerCtx`.  The `LoadedWorkspace.vfs` is on the oracle, not the
    // context.  Proper resolution requires passing `&Vfs` into the context or
    // doing a post-processing step.  For now the source path is `<file-id-N>`
    // and the span is correct.
    let path = PathBuf::from(format!("<file-id-{}>", file_idx));

    Some((path, range))
}

/// Best-effort source location for any `ModuleDef` (non-function items).
///
/// Returns `(path, span)` with a zeroed span when the AST is unavailable.
pub(crate) fn def_source_range(ctx: &mut LowerCtx<'_>, def: ModuleDef) -> (PathBuf, Range<usize>) {
    // Extract syntax via match; not all variants impl a single HasSource trait.
    use ra_ap_hir::Adt;

    macro_rules! try_source {
        ($item:expr) => {
            if let Some(src) = ctx.sema.source($item).or_else(|| $item.source(ctx.db)) {
                let syntax = src.value.syntax();
                // src.file_id is HirFileId; .file_id() -> Option<EditionedFileId>
                if let Some(editioned) = src.file_id.file_id() {
                    let fid = editioned.file_id(ctx.db);
                    let tr = syntax.text_range();
                    let path = PathBuf::from(format!("<file-id-{}>", fid.index()));
                    return (path, tr.start().into()..tr.end().into());
                }
            }
        };
    }

    match def {
        ModuleDef::Function(f) => {
            if let Some((p, r)) = fn_source_range(ctx, f) {
                return (p, r);
            }
        }
        ModuleDef::Adt(Adt::Struct(s)) => {
            try_source!(s);
        }
        ModuleDef::Adt(Adt::Enum(e)) => {
            try_source!(e);
        }
        ModuleDef::Adt(Adt::Union(u)) => {
            try_source!(u);
        }
        ModuleDef::Trait(t) => {
            try_source!(t);
        }
        ModuleDef::TypeAlias(ta) => {
            try_source!(ta);
        }
        ModuleDef::Const(c) => {
            try_source!(c);
        }
        ModuleDef::Static(s) => {
            try_source!(s);
        }
        _ => {}
    }

    (PathBuf::new(), 0..0)
}

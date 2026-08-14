//! Source-file location extraction: package-relative path, byte range, and
//! 1-based line/column range for every item rust-analyzer can point at.
//!
//! # What was here before, and why it was wrong
//!
//! This module used to return `(PathBuf, Range<usize>)` where the path was the
//! literal string `<file-id-806>` — a `vfs::FileId` index formatted into a
//! filename-shaped placeholder. It reached the GUI verbatim (`docs/LIMITATIONS.md`
//! L42.2 records the screenshot) and named nothing on any filesystem. The
//! comment left behind said "there is no clean way to get the absolute VfsPath
//! from a `FileId` without holding a reference to the `Vfs`, which we do not
//! have inside `LowerCtx`". That was a true statement about the *context*, not
//! about the API: [`Vfs::file_path`] is public and infallible, and
//! `LoadedWorkspace` owns the `Vfs`. The fix is to give the context what it
//! needs — doctrine §2 — rather than to format a diagnostic into a path.
//!
//! # What it returns now
//!
//! A [`SourceLocation`], which is a closed enum rather than a pair of
//! optimistically-shaped values. Every path out of this module is one of:
//!
//! - `Declared` — a file under the documented package's root, with both a byte
//!   range and a line/column range computed from that file's text.
//! - `Unlocated(MacroExpanded)` — the item exists only post-expansion.
//! - `Unlocated(OutsideDocumentedPackage)` — a real file, but not under the
//!   package root, so no package-relative path can name it.
//! - `Unlocated(Synthesized)` — for callers that never had an AST at all.
//!
//! There is no `(PathBuf::new(), 0..0)` return any more, which is the point.

use ra_ap_base_db::SourceDatabase;
use ra_ap_hir::{
    EnumVariant, Field, FieldSource, Function, HasSource, HirFileId, Impl, Module, ModuleDef,
};
use ra_ap_ide_db::RootDatabase;
use ra_ap_paths::AbsPath;
use ra_ap_syntax::{AstNode, SyntaxNode, TextRange};
use ra_ap_vfs::{FileId, Vfs};
use rustc_hash::FxHashMap;

use nudox_ir::entry::{ByteSpan, LineCol, SourceFile, SourceLocation, Unlocated};

use super::ctx::LowerCtx;

// ── FileMap ───────────────────────────────────────────────────────────────────

/// Resolves a rust-analyzer `FileId` to a package-relative path and a line
/// index, caching both per file.
///
/// # Why the cache is not optional
///
/// A line index is `O(file length)` to build and every declaration in a file
/// needs one. `memchr` lowers 11,329 entries out of a few dozen files; building
/// the index per declaration would be quadratic in file size for no reason.
/// The cache is keyed by `FileId` and is therefore exact — two declarations in
/// one file share one index by construction, not by luck.
pub(crate) struct FileMap<'db> {
    vfs: &'db Vfs,
    /// Root of the package being documented. Paths are reported relative to
    /// this, so the IR does not embed the build machine's directory layout
    /// (which would make content hashes machine-specific).
    root: &'db AbsPath,
    resolved: FxHashMap<FileId, Resolved>,
}

/// What a `FileId` resolved to, computed once.
enum Resolved {
    /// A file under the package root, with the offset of the first byte of
    /// each line (`line_starts[0]` is always 0).
    Inside {
        file: SourceFile,
        line_starts: Box<[u32]>,
    },
    /// A real file that is not under the package root — a dependency, the
    /// sysroot, or a generated `OUT_DIR` source.
    Outside,
}

impl<'db> FileMap<'db> {
    /// Build a resolver over `vfs`, reporting paths relative to `root`.
    pub(crate) fn new(vfs: &'db Vfs, root: &'db AbsPath) -> Self {
        FileMap {
            vfs,
            root,
            resolved: FxHashMap::default(),
        }
    }

    /// Full location of `range` within `file_id`.
    ///
    /// The `db` is threaded in rather than held, because the file text is a
    /// salsa query: holding the database here would make this struct's
    /// lifetime the analysis lifetime for no gain.
    pub(crate) fn locate(
        &mut self,
        db: &RootDatabase,
        file_id: FileId,
        range: TextRange,
    ) -> SourceLocation {
        let start_offset: u32 = range.start().into();
        let end_offset: u32 = range.end().into();

        let Some(bytes) = ByteSpan::new(start_offset, end_offset) else {
            // An empty range is not a location. rust-analyzer produces one for
            // items whose syntax node has been collapsed; saying "byte 0" here
            // is the exact defect this module was rewritten to remove.
            return SourceLocation::Unlocated(Unlocated::Synthesized);
        };

        match self.resolve(db, file_id) {
            Resolved::Outside => SourceLocation::Unlocated(Unlocated::OutsideDocumentedPackage),
            Resolved::Inside { file, line_starts } => SourceLocation::Declared {
                file: file.clone(),
                bytes,
                start: line_col(line_starts, start_offset),
                end: line_col(line_starts, end_offset),
            },
        }
    }

    /// Resolve and memoise one `FileId`.
    fn resolve(&mut self, db: &RootDatabase, file_id: FileId) -> &Resolved {
        self.resolved.entry(file_id).or_insert_with(|| {
            // `Vfs::file_path` is infallible for any `FileId` the database
            // handed us; `as_path` is `None` only for virtual (non-disk)
            // paths, which cannot be under a real package root.
            let Some(abs) = self.vfs.file_path(file_id).as_path() else {
                return Resolved::Outside;
            };
            let Some(relative) = abs.strip_prefix(self.root) else {
                return Resolved::Outside;
            };
            let Some(file) = SourceFile::new(relative.as_str()) else {
                // `strip_prefix` produced an empty path: `abs == root`, i.e. the
                // root itself is being reported as a file. Not a declaration
                // site.
                return Resolved::Outside;
            };
            let text = db.file_text(file_id).text(db).clone();
            Resolved::Inside {
                file,
                line_starts: line_starts(&text),
            }
        })
    }
}

/// Byte offsets of the first character of every line.
///
/// `\n` only: a `\r\n` file yields the same line *numbers*, with the `\r`
/// counted as a column of the preceding line, which is what every editor does.
fn line_starts(text: &str) -> Box<[u32]> {
    let mut starts = vec![0u32];
    starts.extend(
        text.bytes()
            .enumerate()
            .filter(|(_, b)| *b == b'\n')
            .map(|(i, _)| (i as u32).saturating_add(1)),
    );
    starts.into_boxed_slice()
}

/// 1-based line/column of `offset`, given a file's line-start table.
fn line_col(line_starts: &[u32], offset: u32) -> LineCol {
    // `partition_point` gives the count of line starts at or before `offset`,
    // which is the 1-based line number; subtract one for the 0-based index the
    // conversion helper expects.
    let line_ix = line_starts.partition_point(|&s| s <= offset).saturating_sub(1);
    let col = offset - line_starts[line_ix];
    LineCol::from_zero_based(line_ix as u32, col)
}

// ── Item lookup ───────────────────────────────────────────────────────────────

/// Location of a syntax node that came out of `Semantics::source`.
///
/// The single funnel every non-function item kind goes through, so "what does a
/// macro-expanded item report?" has one answer instead of one per caller —
/// which is how the old module ended up with two different fallbacks for the
/// same situation.
pub(crate) fn node_location(
    ctx: &mut LowerCtx<'_>,
    file_id: HirFileId,
    syntax: &SyntaxNode,
) -> SourceLocation {
    let db = ctx.db;
    let Some(editioned) = file_id.file_id() else {
        return SourceLocation::Unlocated(Unlocated::MacroExpanded);
    };
    let fid = editioned.file_id(db);
    let range = syntax.text_range();
    ctx.files.locate(db, fid, range)
}

/// Location of an `impl` block's header.
///
/// `wire::ImplRow` had no source field at all (`docs/LIMITATIONS.md` L42.3); this is
/// what fills it. `Impl` is not a `ModuleDef`, so it needs its own entry point
/// rather than a variant of [`def_location`].
pub(crate) fn impl_location(ctx: &mut LowerCtx<'_>, imp: Impl) -> SourceLocation {
    let Some(src) = ctx.sema.source(imp).or_else(|| imp.source(ctx.db)) else {
        return SourceLocation::Unlocated(Unlocated::MacroExpanded);
    };
    let file_id = src.file_id;
    let syntax = src.value.syntax().clone();
    node_location(ctx, file_id, &syntax)
}

/// Location of an enum variant's declaration.
pub(crate) fn variant_location(ctx: &mut LowerCtx<'_>, v: EnumVariant) -> SourceLocation {
    let Some(src) = ctx.sema.source(v).or_else(|| v.source(ctx.db)) else {
        return SourceLocation::Unlocated(Unlocated::MacroExpanded);
    };
    let file_id = src.file_id;
    let syntax = src.value.syntax().clone();
    node_location(ctx, file_id, &syntax)
}

/// Location of a module's declaration.
///
/// `Module` has no `HasSource` impl: a module is either a `mod foo;` item in a
/// parent file or an entire file with no declaration of its own. Prefer the
/// `mod foo;` item, because that is the line a reader wants to be taken to;
/// fall back to the file's own root node for a directory-layout module, whose
/// "declaration" is the file itself.
pub(crate) fn module_location(ctx: &mut LowerCtx<'_>, m: Module) -> SourceLocation {
    if let Some(decl) = m.declaration_source(ctx.db) {
        let file_id = decl.file_id;
        let syntax = decl.value.syntax().clone();
        return node_location(ctx, file_id, &syntax);
    }
    let def = m.definition_source(ctx.db);
    let file_id = def.file_id;
    let syntax = match &def.value {
        ra_ap_hir::ModuleSource::SourceFile(f) => f.syntax().clone(),
        ra_ap_hir::ModuleSource::Module(md) => md.syntax().clone(),
        ra_ap_hir::ModuleSource::BlockExpr(b) => b.syntax().clone(),
    };
    node_location(ctx, file_id, &syntax)
}

/// Location of a struct/union field's declaration.
pub(crate) fn field_location(ctx: &mut LowerCtx<'_>, f: Field) -> SourceLocation {
    let Some(src) = ctx.sema.source(f).or_else(|| f.source(ctx.db)) else {
        return SourceLocation::Unlocated(Unlocated::MacroExpanded);
    };
    let file_id = src.file_id;
    let syntax = match &src.value {
        FieldSource::Named(rf) => rf.syntax().clone(),
        FieldSource::Pos(tf) => tf.syntax().clone(),
    };
    node_location(ctx, file_id, &syntax)
}

/// Location of a function's declaration.
///
/// Prefers `Semantics::original_range_opt`, which maps a macro-expanded node
/// back to the call site, and falls back to the node's own file range.
pub(crate) fn fn_location(ctx: &mut LowerCtx<'_>, f: Function) -> SourceLocation {
    let db = ctx.db;
    let Some(src) = ctx.sema.source(f).or_else(|| f.source(ctx.db)) else {
        return SourceLocation::Unlocated(Unlocated::MacroExpanded);
    };
    let syntax = src.value.syntax();

    if let Some(file_range) = ctx.sema.original_range_opt(syntax) {
        let fid = file_range.file_id.file_id(db);
        return ctx.files.locate(db, fid, file_range.range);
    }

    // `src.file_id.file_id()` is `None` exactly for macro-expanded items.
    let Some(editioned) = src.file_id.file_id() else {
        return SourceLocation::Unlocated(Unlocated::MacroExpanded);
    };
    let fid = editioned.file_id(db);
    let range = syntax.text_range();
    ctx.files.locate(db, fid, range)
}

/// Location of any `ModuleDef`'s declaration.
///
/// Returns `Unlocated(Synthesized)` for the `ModuleDef` variants that have no
/// single declaration site (modules declared by directory layout, builtin
/// types), rather than a zeroed span that reads as byte 0 of an unnamed file.
pub(crate) fn def_location(ctx: &mut LowerCtx<'_>, def: ModuleDef) -> SourceLocation {
    use ra_ap_hir::Adt;

    macro_rules! locate {
        ($item:expr) => {{
            let db = ctx.db;
            if let Some(src) = ctx.sema.source($item).or_else(|| $item.source(db)) {
                let file_id = src.file_id;
                let syntax = src.value.syntax().clone();
                return node_location(ctx, file_id, &syntax);
            }
        }};
    }

    match def {
        ModuleDef::Function(f) => return fn_location(ctx, f),
        ModuleDef::Adt(Adt::Struct(s)) => locate!(s),
        ModuleDef::Adt(Adt::Enum(e)) => locate!(e),
        ModuleDef::Adt(Adt::Union(u)) => locate!(u),
        ModuleDef::Trait(t) => locate!(t),
        ModuleDef::TypeAlias(ta) => locate!(ta),
        ModuleDef::Const(c) => locate!(c),
        ModuleDef::Static(s) => locate!(s),
        ModuleDef::Macro(m) => locate!(m),
        ModuleDef::EnumVariant(v) => return variant_location(ctx, v),
        ModuleDef::Module(m) => return module_location(ctx, m),
        // `u32` and friends are declared by the language, not by a file.
        ModuleDef::BuiltinType(_) => {}
    }

    SourceLocation::Unlocated(Unlocated::Synthesized)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The line table must agree with how a person counts: the first byte of
    /// the file is line 1 column 1, and the byte just after a newline starts
    /// the next line at column 1.
    #[test]
    fn first_byte_of_a_file_is_line_one_column_one() {
        let starts = line_starts("abc\ndef\n");
        assert_eq!(line_col(&starts, 0), LineCol::from_zero_based(0, 0));
        assert_eq!(line_col(&starts, 4), LineCol::from_zero_based(1, 0));
    }

    /// An offset in the middle of a line must report the byte column within
    /// that line, not the offset within the file — the mistake that would make
    /// every column past line 1 wrong and still look plausible.
    #[test]
    fn column_is_measured_from_the_line_start_not_the_file_start() {
        let starts = line_starts("abc\ndefgh\n");
        // Offset 6 is 'f': line 2 (index 1), column index 2 → 1-based (2, 3).
        let lc = line_col(&starts, 6);
        assert_eq!((lc.line(), lc.column()), (2, 3));
    }

    /// A file with no trailing newline must not lose its last line, and an
    /// offset at end-of-file must still resolve rather than panic on an
    /// out-of-range index.
    #[test]
    fn offset_at_end_of_a_file_without_trailing_newline_resolves() {
        let text = "one\ntwo";
        let starts = line_starts(text);
        let lc = line_col(&starts, text.len() as u32);
        assert_eq!((lc.line(), lc.column()), (2, 4));
    }
}

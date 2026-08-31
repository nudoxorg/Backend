#![feature(rustc_private)]
#![warn(unused_extern_crates)]

dylint_linting::dylint_library!();

extern crate rustc_ast;
extern crate rustc_hir;
extern crate rustc_lint;
extern crate rustc_middle;
extern crate rustc_session;
extern crate rustc_span;

mod dynamic_dispatch;
mod dynamic_json_construction;
mod enum_static_str_projection;
mod erased_map_err;
mod poison_sync_primitive;
mod redundant_public_accessor;
mod stringly_state_field;

use rustc_hir::{AmbigArg, Expr, FieldDef, ImplItem, Item, Ty};
use rustc_lint::{LateContext, LateLintPass, LintStore};
use rustc_session::{Session, impl_lint_pass};
use rustc_span::Span;
use std::collections::HashSet;

impl_lint_pass!(NudoxSemanticLints => [
    erased_map_err::NUDOX_ERASED_MAP_ERR,
    poison_sync_primitive::NUDOX_POISON_SYNC_PRIMITIVE,
    stringly_state_field::NUDOX_STRINGLY_STATE_FIELD,
    redundant_public_accessor::NUDOX_REDUNDANT_PUBLIC_ACCESSOR,
    dynamic_dispatch::NUDOX_DYNAMIC_DISPATCH,
    dynamic_json_construction::NUDOX_DYNAMIC_JSON_CONSTRUCTION,
    enum_static_str_projection::NUDOX_ENUM_STATIC_STR_PROJECTION,
]);

struct NudoxSemanticLints {
    reported_dynamic_json_calls: HashSet<Span>,
}

#[unsafe(no_mangle)]
pub fn register_lints(_session: &Session, lint_store: &mut LintStore) {
    lint_store.register_lints(&[
        erased_map_err::NUDOX_ERASED_MAP_ERR,
        poison_sync_primitive::NUDOX_POISON_SYNC_PRIMITIVE,
        stringly_state_field::NUDOX_STRINGLY_STATE_FIELD,
        redundant_public_accessor::NUDOX_REDUNDANT_PUBLIC_ACCESSOR,
        dynamic_dispatch::NUDOX_DYNAMIC_DISPATCH,
        dynamic_json_construction::NUDOX_DYNAMIC_JSON_CONSTRUCTION,
        enum_static_str_projection::NUDOX_ENUM_STATIC_STR_PROJECTION,
    ]);
    lint_store.register_late_pass(|_| {
        Box::new(NudoxSemanticLints {
            reported_dynamic_json_calls: HashSet::new(),
        })
    });
}

impl<'tcx> LateLintPass<'tcx> for NudoxSemanticLints {
    fn check_expr(&mut self, context: &LateContext<'tcx>, expression: &'tcx Expr<'_>) {
        erased_map_err::check(context, expression);
        poison_sync_primitive::check_expr(context, expression);
        dynamic_json_construction::check(
            context,
            expression,
            &mut self.reported_dynamic_json_calls,
        );
    }

    fn check_field_def(&mut self, context: &LateContext<'tcx>, field: &'tcx FieldDef<'_>) {
        stringly_state_field::check(context, field);
    }

    fn check_impl_item(&mut self, context: &LateContext<'tcx>, item: &'tcx ImplItem<'_>) {
        redundant_public_accessor::check(context, item);
    }

    fn check_item(&mut self, context: &LateContext<'tcx>, item: &'tcx Item<'tcx>) {
        enum_static_str_projection::check(context, item);
    }

    fn check_ty(&mut self, context: &LateContext<'tcx>, ty: &'tcx Ty<'tcx, AmbigArg>) {
        dynamic_dispatch::check(context, ty);
        poison_sync_primitive::check_ty(context, ty);
    }
}

#[test]
fn ui() -> std::io::Result<()> {
    use std::{ffi::OsStr, io::ErrorKind, path::Path};

    let fixture_root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/ui"));
    let mut source_count = 0;
    for entry in fixture_root.read_dir()? {
        let path = entry?.path();
        match path.extension() {
            Some(extension) if extension == OsStr::new("rs") => {
                source_count += 1;
                if !path.with_extension("stderr").is_file() {
                    return Err(ErrorKind::InvalidData.into());
                }
            }
            Some(extension) if extension == OsStr::new("stderr") => {
                if !path.with_extension("rs").is_file() {
                    return Err(ErrorKind::InvalidData.into());
                }
            }
            _ => {}
        }
    }
    if source_count == 0 {
        return Err(ErrorKind::NotFound.into());
    }

    dylint_testing::ui::Test::src_base(env!("CARGO_PKG_NAME"), fixture_root)
        .rustc_flags([
            "-Fnudox_dynamic_dispatch",
            "-Fnudox_dynamic_json_construction",
            "-Fnudox_erased_map_err",
            "-Fnudox_poison_sync_primitive",
            "-Fnudox_redundant_public_accessor",
            "-Fnudox_stringly_state_field",
            "-Fnudox_enum_static_str_projection",
        ])
        .run();
    Ok(())
}

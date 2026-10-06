//! A typed project semantic option selects stable home-declaration DefId
//! election alongside declaration-scoped type-parameter identity.

use tsz_solver::construction::TypeInterner;
use tsz_solver::def::{DefId, DefinitionStore};
use tsz_common::ProjectSemanticOptions;

fn entry(name: &str, file_id: u32, span_start: u32) -> tsz_binder::SemanticDefEntry {
    tsz_binder::SemanticDefEntry {
        kind: tsz_binder::SemanticDefKind::Interface,
        name: name.to_string(),
        file_id,
        span_start,
        type_param_count: 0,
        type_param_names: Vec::new(),
        is_exported: false,
        enum_member_names: Vec::new(),
        is_const: false,
        is_abstract: false,
        extends_names: Vec::new(),
        implements_names: Vec::new(),
        parent_namespace: None,
        is_global_augmentation: false,
        is_declare: false,
    }
}

#[test]
fn declaration_scoped_project_options_select_deterministic_election() {
    let interner = TypeInterner::new();
    let mut defs = rustc_hash::FxHashMap::default();
    defs.insert(tsz_binder::SymbolId(300), entry("Late", 7, 40));
    defs.insert(tsz_binder::SymbolId(100), entry("Early", 3, 10));
    defs.insert(tsz_binder::SymbolId(200), entry("Middle", 3, 20));

    let store = DefinitionStore::from_semantic_defs_with_project_semantic_options(
        &defs,
        |name| interner.intern_string(name),
        ProjectSemanticOptions::declaration_scoped(),
    );

    assert_eq!(
        [
            store.find_def_by_symbol(100),
            store.find_def_by_symbol(200),
            store.find_def_by_symbol(300),
        ],
        [Some(DefId(1)), Some(DefId(2)), Some(DefId(3))],
        "DeclScoped programs must elect DefIds by stable (file, span, symbol) provenance"
    );
}

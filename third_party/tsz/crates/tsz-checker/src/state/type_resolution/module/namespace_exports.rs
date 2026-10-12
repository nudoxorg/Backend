use crate::module_resolution::module_specifier_candidates;
use crate::state::CheckerState;
use crate::symbols_domain::alias_cycle::AliasCycleTracker;
use tsz_parser::parser::{NodeIndex, syntax_kind_ext};
use tsz_scanner::SyntaxKind;

#[derive(Clone, Copy, PartialEq, Eq)]
enum OwnerReexportPhase {
    Named,
    Wildcard,
    Both,
}

// The key is used only during one admitted External lookup. Borrowed symbol
// storage and the retained table/row addresses remain live for that invocation.
type SelectedAmbientExportTableKey<'a> = (
    *const tsz_binder::SymbolTable,
    tsz_binder::symbols::SymbolArenaStorageKey<'a>,
    Option<*const tsz_binder::FileReexports>,
);

impl<'a> CheckerState<'a> {
    /// Resolve a namespace import (import * as ns) from another file using cross-file resolution.
    ///
    /// Returns a `SymbolTable` containing all exports from the target module.
    pub(crate) fn resolve_cross_file_namespace_exports(
        &self,
        module_specifier: &str,
    ) -> Option<tsz_binder::SymbolTable> {
        let cache_key = (self.ctx.current_file_idx, module_specifier.to_string());
        if let Some(cached) = self
            .ctx
            .namespace_exports_cache
            .borrow()
            .get(&cache_key)
            .cloned()
        {
            return cached;
        }

        if let Some(exports) = self.resolve_ambient_module_namespace_exports(module_specifier) {
            self.ctx
                .namespace_exports_cache
                .borrow_mut()
                .insert(cache_key, Some(exports.clone()));
            return Some(exports);
        }

        let Some(target_file_idx) = self.ctx.resolve_import_target(module_specifier) else {
            self.ctx
                .namespace_exports_cache
                .borrow_mut()
                .insert(cache_key, None);
            return None;
        };
        let Some(target_binder) = self.ctx.get_binder_for_file(target_file_idx) else {
            self.ctx
                .namespace_exports_cache
                .borrow_mut()
                .insert(cache_key, None);
            return None;
        };
        let target_arena = self.ctx.get_arena_for_file(target_file_idx as u32);
        let Some(target_file_name) = target_arena
            .source_files
            .first()
            .map(|source_file| source_file.file_name.clone())
        else {
            self.ctx
                .namespace_exports_cache
                .borrow_mut()
                .insert(cache_key, None);
            return None;
        };

        // Helper: record cross-file origin for all symbols in a table.
        let record_symbols = |table: &tsz_binder::SymbolTable| {
            for (_, &sym_id) in table.iter() {
                self.ctx
                    .register_symbol_file_target(sym_id, target_file_idx);
            }
        };

        // Try to find exports in the target binder's module_exports.
        // Prefer canonical file key first, then module specifier fallback.
        let direct_exports = self
            .ctx
            .module_exports_for_module(target_binder, &target_file_name)
            .or_else(|| {
                self.ctx
                    .module_exports_for_module(target_binder, module_specifier)
            });

        if let Some(exports) = direct_exports {
            let mut combined = exports.clone();
            self.merge_export_equals_members(target_binder, exports, &mut combined);
            if let Some(export_equals_sym_id) = exports.get("export=")
                && let Some(export_equals_symbol) = target_binder.get_symbol(export_equals_sym_id)
            {
                let _ = self.merge_export_equals_import_type_members(
                    export_equals_symbol,
                    Some(target_file_idx),
                    &mut combined,
                );
            }
            let mut visited = rustc_hash::FxHashSet::default();
            self.collect_reexported_symbols(
                target_file_idx,
                Some(module_specifier),
                &mut combined,
                &mut visited,
            );
            self.merge_module_augmentation_namespace_exports(
                &mut combined,
                target_file_idx,
                Some(module_specifier),
            );
            record_symbols(&combined);
            self.ctx
                .namespace_exports_cache
                .borrow_mut()
                .insert(cache_key, Some(combined.clone()));
            return Some(combined);
        }

        // No direct exports found, but the module may still re-export symbols
        // via `export * from './other'` or `export { X } from './other'`.
        // Collect re-exported symbols even when there are no direct exports.
        let has_reexports = self
            .ctx
            .wildcard_reexports_for_file(target_binder, &target_file_name)
            .is_some()
            || self
                .ctx
                .reexports_for_file(target_binder, &target_file_name)
                .is_some();
        if has_reexports {
            let mut combined = tsz_binder::SymbolTable::new();
            let mut visited = rustc_hash::FxHashSet::default();
            self.collect_reexported_symbols(
                target_file_idx,
                Some(module_specifier),
                &mut combined,
                &mut visited,
            );
            self.merge_module_augmentation_namespace_exports(
                &mut combined,
                target_file_idx,
                Some(module_specifier),
            );
            if !combined.is_empty() {
                record_symbols(&combined);
            }
            // Return the table even if empty — the module exists but may have only
            // type-only exports (e.g., `export type * from '...'`). An empty namespace
            // object type is correct and will produce TS2339 for value access, instead
            // of falling through to "module not found" → TypeId::ANY.
            self.ctx
                .namespace_exports_cache
                .borrow_mut()
                .insert(cache_key, Some(combined.clone()));
            return Some(combined);
        }

        self.ctx
            .namespace_exports_cache
            .borrow_mut()
            .insert(cache_key, None);
        None
    }

    /// Like `resolve_cross_file_namespace_exports` but with a pre-resolved target file index.
    /// Used when the module specifier was already resolved from a different source file.
    fn resolve_cross_file_namespace_exports_for_file(
        &self,
        target_file_idx: usize,
        module_specifier: Option<&str>,
    ) -> Option<tsz_binder::SymbolTable> {
        let target_binder = self.ctx.get_binder_for_file(target_file_idx)?;
        let target_arena = self.ctx.get_arena_for_file(target_file_idx as u32);
        let target_file_name = target_arena.source_files.first()?.file_name.clone();

        let record_symbols = |table: &tsz_binder::SymbolTable| {
            for (_, &sym_id) in table.iter() {
                self.ctx
                    .register_symbol_file_target(sym_id, target_file_idx);
            }
        };

        let direct_exports = self
            .ctx
            .module_exports_for_module(target_binder, &target_file_name)
            .or_else(|| {
                module_specifier.and_then(|specifier| {
                    self.ctx.module_exports_for_module(target_binder, specifier)
                })
            });

        if let Some(exports) = direct_exports {
            let mut combined = exports.clone();
            self.merge_export_equals_members(target_binder, exports, &mut combined);
            let mut visited = rustc_hash::FxHashSet::default();
            self.collect_reexported_symbols(
                target_file_idx,
                module_specifier,
                &mut combined,
                &mut visited,
            );
            self.merge_module_augmentation_namespace_exports(
                &mut combined,
                target_file_idx,
                module_specifier,
            );
            record_symbols(&combined);
            return Some(combined);
        }

        let has_reexports = self
            .ctx
            .wildcard_reexports_for_file(target_binder, &target_file_name)
            .is_some()
            || self
                .ctx
                .reexports_for_file(target_binder, &target_file_name)
                .is_some();
        if has_reexports {
            let mut combined = tsz_binder::SymbolTable::new();
            let mut visited = rustc_hash::FxHashSet::default();
            self.collect_reexported_symbols(
                target_file_idx,
                module_specifier,
                &mut combined,
                &mut visited,
            );
            self.merge_module_augmentation_namespace_exports(
                &mut combined,
                target_file_idx,
                module_specifier,
            );
            if !combined.is_empty() {
                record_symbols(&combined);
            }
            return Some(combined);
        }

        // The target file is a real ES module (has top-level `import`/`export`
        // statements or a module file extension) but its public surface is
        // empty — e.g. `main.mts` only declares `import` aliases, no exports.
        // tsc still types `import * as ns from "./main.mjs"` as the empty
        // module namespace `{}`, so `ns.default` / `ns.imported` correctly
        // report TS2339 instead of leaking the local imports as members.
        // Returning an empty table here matches that behavior; falling
        // through to `None` would let the caller widen the namespace to
        // `any`, which silently accepts any property access.
        if target_binder.is_external_module {
            return Some(tsz_binder::SymbolTable::new());
        }

        None
    }

    pub(crate) fn merge_module_augmentation_namespace_exports(
        &self,
        exports: &mut tsz_binder::SymbolTable,
        target_file_idx: usize,
        module_specifier: Option<&str>,
    ) {
        // Skip the wildcard-chain helper cost when no augmentations exist.
        if !self.ctx.program_has_module_augmentations() {
            return;
        }
        let mut names: Vec<String> = Vec::new();

        if let Some(module_specifier) = module_specifier {
            for name in self.collect_module_augmentation_names(module_specifier) {
                if !names.iter().any(|existing| existing == &name) {
                    names.push(name);
                }
            }
        }

        let target_arena = self.ctx.get_arena_for_file(target_file_idx as u32);
        if let Some(target_file_name) = target_arena
            .source_files
            .first()
            .map(|sf| sf.file_name.as_str())
        {
            for name in self.collect_module_augmentation_names(target_file_name) {
                if !names.iter().any(|existing| existing == &name) {
                    names.push(name);
                }
            }
        }

        for name in names {
            if exports.get(name.as_str()).is_some() {
                continue;
            }
            if let Some((sym_id, owner_file_idx)) =
                self.resolve_module_augmentation_export_for_file(target_file_idx, &name)
            {
                exports.set(name, sym_id);
                self.ctx.register_symbol_file_target(sym_id, owner_file_idx);
            }
        }
    }

    /// Resolve a module's effective export surface.
    ///
    /// This canonicalizes module-specifier variants and ensures `export =` target
    /// members are merged into the result. Prefer this over ad-hoc lookups against
    /// `binder.module_exports`.
    pub(crate) fn resolve_effective_module_exports(
        &self,
        module_specifier: &str,
    ) -> Option<tsz_binder::SymbolTable> {
        self.resolve_effective_module_exports_from_file(module_specifier, None)
    }

    /// Resolve a syntax-owned request without borrowing another kind's target.
    pub(crate) fn resolve_effective_module_exports_for_request(
        &self,
        module_specifier: &str,
        resolution_mode: Option<crate::context::ResolutionModeOverride>,
        request_kind: crate::context::ResolutionRequestKind,
    ) -> Option<tsz_binder::SymbolTable> {
        if self.ctx.project_module_resolution_outcomes.is_some() {
            let request = crate::context::BoundModuleRequest {
                importer: self.ctx.current_file_idx,
                specifier: module_specifier,
                mode: resolution_mode,
                kind: request_kind,
            };
            return self
                .resolve_exports_for_selected_request(self.ctx.select_module_request(request)?);
        }
        self.resolve_effective_module_exports_with_mode(module_specifier, resolution_mode)
    }

    fn first_selected_ambient_export_table<'b>(
        &self,
        binder: &'b tsz_binder::BinderState,
        ambient_key: &str,
        exports: &'b tsz_binder::SymbolTable,
        seen: &mut rustc_hash::FxHashSet<SelectedAmbientExportTableKey<'b>>,
    ) -> bool {
        // Legacy binders can resolve the same numeric ID through different lib
        // fallbacks. Retain their original per-binder processing.
        if !binder.lib_symbols_are_merged() {
            return true;
        }
        seen.insert((
            exports as *const _,
            binder.get_symbols().storage_key(),
            self.ctx.reexports_for_file(binder, ambient_key)
                .map(|rows| rows as *const _),
        ))
    }

    /// Type and value member lookup preserves the selected request and owner.
    /// Runtime namespace collection separately filters type-only wildcard edges.
    pub(crate) fn resolve_member_for_selected_request(
        &self,
        selected: crate::context::SelectedModuleRequest<'_>,
        name: &str,
    ) -> Option<tsz_binder::SymbolId> {
        if let Some(target) = selected.file_target() {
            // Use the same syntax-owned public member walker as an ambient
            // owner's selected File edge, including export= member/default
            // semantics. A raw export-table alias is not a terminal declaration.
            return self.resolve_owner_edge_file_member(
                target,
                name,
                &mut rustc_hash::FxHashSet::default(),
                &mut rustc_hash::FxHashSet::default(),
            );
        }
        selected.external_identity()?;
        let mut visited = rustc_hash::FxHashSet::default();
        let mut admitted_key = None;
        let mut owners = rustc_hash::FxHashSet::default();
        let mut ambient_guard = rustc_hash::FxHashSet::default();
        let mut merged_direct = None;
        let mut tables = Vec::new();
        let mut seen_tables = rustc_hash::FxHashSet::default();
        for (file_idx, binder) in self.ctx.all_binders.as_ref()?.iter().enumerate() {
            let Some((ambient_key, exports)) = self
                .ctx
                .ambient_exports_for_selected_request(binder, selected)
            else {
                continue;
            };
            if admitted_key.is_some_and(|key| key != ambient_key) {
                continue;
            }
            admitted_key = Some(ambient_key);
            // The existing export= target remains authoritative for default,
            // including when a direct declaration has a member named default.
            if name == "default" && exports.get("export=").is_some()
                && let Some(symbol) = self.resolve_export_from_table(binder, exports, name)
            {
                return Some(symbol);
            }
            if let Some(local_exports) = self.ambient_declaration_scope_exports(file_idx, ambient_key) {
                owners.insert(file_idx);
                if let Some(symbol) = self.resolve_export_from_table(binder, &local_exports, name) {
                    return Some(symbol);
                }
            }
            if self.first_selected_ambient_export_table(
                binder, ambient_key, exports, &mut seen_tables,
            ) {
                tables.push((binder.as_ref(), exports));
            }
        }
        let ambient_key = admitted_key?;
        // Every admitted declaration scope precedes merged/export= work. A
        // shared merged table must not hide a later owner's direct member.
        for (binder, exports) in tables {
            if merged_direct.is_none()
                && let Some(symbol) = self.resolve_export_from_table(binder, exports, name)
                && !self.is_owner_named_reexport_alias(binder, ambient_key, name, symbol)
            {
                merged_direct = Some(symbol);
            }
            if let Some(symbol) = exports.get("export=").and_then(|id| binder.get_symbol(id)) {
                let mut members = tsz_binder::SymbolTable::new();
                let _ = self.merge_export_equals_import_type_members_with_ambient_guard(
                    symbol,
                    exports
                        .get("export=")
                        .and_then(|id| self.ctx.resolve_symbol_declaring_file_index(id)),
                    &mut members,
                    &mut ambient_guard,
                );
                if merged_direct.is_none() {
                    merged_direct = members.get(name);
                }
            }
            for (_, &symbol) in exports.iter() {
                if let Some(owner) = self.ctx.resolve_symbol_declaring_file_index(symbol) {
                    owners.insert(owner);
                }
            }
        }
        if let Some(symbol) = merged_direct {
            return Some(symbol);
        }
        if let Some(files) = self.ctx.files_for_module_specifier(ambient_key) {
            owners.extend(files.iter().copied());
        }
        // Finish all direct declarations before following the same owner's
        // edges; an empty first declaration does not terminate the lookup.
        let mut owners: Vec<_> = owners.into_iter().collect();
        owners.sort_unstable();
        for phase in [OwnerReexportPhase::Named, OwnerReexportPhase::Wildcard] {
            for &owner in &owners {
                visited.clear();
                if let Some(symbol) = self.resolve_member_from_owner_reexports(
                    owner,
                    ambient_key,
                    name,
                    &mut visited,
                    &mut ambient_guard,
                    phase,
                ) {
                    return Some(symbol);
                }
            }
        }
        None
    }

    /// Recover public direct rows from each literal declaration's own bound
    /// scope. Reconstructed binders share a merged module_exports map, whose
    /// first ALIAS row can hide a later declaration's direct member.
    fn ambient_declaration_scope_exports(
        &self,
        owner: usize,
        ambient_key: &str,
    ) -> Option<tsz_binder::SymbolTable> {
        let binder = self.ctx.get_binder_for_file(owner)?;
        let arena = self.ctx.all_arenas.as_ref()?.get(owner)?.as_ref();
        let mut exports = None;
        for (scope_idx, scope) in binder.scopes.iter().enumerate() {
            if scope.kind != tsz_binder::ContainerKind::Module {
                continue;
            }
            let Some(module) = arena.get(scope.container_node)
                .and_then(|node| arena.get_module(node)) else {
                continue;
            };
            let Some(name_node) = arena.get(module.name) else {
                continue;
            };
            if module.body.is_none()
                || !matches!(name_node.kind, k if k == SyntaxKind::StringLiteral as u16
                    || k == SyntaxKind::NoSubstitutionTemplateLiteral as u16)
                || arena.get_literal(name_node).is_none_or(|literal| literal.text != ambient_key)
                || binder.node_scope_ids.get(&module.body.0)
                    != Some(&tsz_binder::ScopeId(scope_idx as u32))
                || !binder.node_symbols.get(&scope.container_node.0)
                    .and_then(|symbol| binder.get_symbol(*symbol))
                    .is_some_and(|symbol| symbol.has_any_flags(tsz_binder::symbol_flags::MODULE))
            {
                continue;
            }
            // Augmentation bodies bind in the boundary scope, so they cannot
            // satisfy this Module-scope/container/body identity. Identifier
            // namespaces and other literal keys are rejected above.
            let exports = exports.get_or_insert_with(tsz_binder::SymbolTable::new);
            for (name, &symbol) in binder.module_scope_exports(arena, scope) {
                if !exports.has(name)
                    && !self.is_owner_named_reexport_alias(binder, ambient_key, name, symbol)
                {
                    exports.set(name.clone(), symbol);
                    let declaring_owner = self.ctx.resolve_symbol_declaring_file_index(symbol)
                        .unwrap_or(owner);
                    self.ctx.register_symbol_file_target(symbol, declaring_owner);
                }
            }
        }
        exports
    }

    /// A re-export target is a selected File, so only its canonical public
    /// table and syntax-owned edges are eligible; file locals never fill holes.
    fn resolve_owner_edge_file_member(
        &self,
        target: usize,
        name: &str,
        visited: &mut rustc_hash::FxHashSet<(usize, String)>,
        ambient_guard: &mut rustc_hash::FxHashSet<(Option<usize>, String)>,
    ) -> Option<tsz_binder::SymbolId> {
        if !visited.insert((target, name.to_owned())) {
            return None;
        }
        let binder = self.ctx.get_binder_for_file(target)?;
        let arena = self.ctx.get_arena_for_file(target as u32);
        let file_name = &arena.source_files.first()?.file_name;
        let default_skips_export_equals = name == "default"
            && (file_name.ends_with(".mjs")
                || file_name.ends_with(".mts")
                || self.source_file_idx_is_js_with_esm_syntax(target))
            && self.ctx.compiler_options.module != tsz_common::common::ModuleKind::Preserve;
        if let Some(exports) = self.ctx.module_exports_for_module(binder, file_name) {
            // Preserve export= default precedence and its named members.
            let symbol = if default_skips_export_equals {
                exports.get("default").filter(|id| binder.get_symbol(*id).is_some())
            } else {
                self.resolve_export_from_table(binder, exports, name)
            };
            if let Some(symbol) = symbol
                && !self.is_owner_named_reexport_alias(binder, file_name, name, symbol)
            {
                let owner = self
                    .ctx
                    .resolve_symbol_declaring_file_index(symbol)
                    .unwrap_or(target);
                self.ctx.register_symbol_file_target(symbol, owner);
                return Some(symbol);
            }
            if let Some(symbol) = symbol
                && self.ctx.project_module_resolution_outcomes.is_some()
                && binder
                    .get_symbol(symbol)
                    .is_some_and(|alias| alias.escaped_name == name)
                && self.ctx.source_module_alias_kind(symbol)
                    == Some(crate::context::SourceModuleAliasKind::FileReexport)
                && self.ctx.bound_module_request_for_alias(symbol).is_none()
            {
                // The actual file declaration still publishes this explicit
                // name. Its refused request is not a target proof: retain the
                // source alias, whose selected type walk remains unresolved.
                return Some(symbol);
            }
            if !default_skips_export_equals
                && let Some(symbol) = exports.get("export=").and_then(|id| binder.get_symbol(id))
            {
                let mut members = tsz_binder::SymbolTable::new();
                let _ = self.merge_export_equals_import_type_members_with_ambient_guard(
                    symbol,
                    Some(target),
                    &mut members,
                    ambient_guard,
                );
                if let Some(symbol) = members.get(name) {
                    return Some(symbol);
                }
            }
        }
        self.resolve_member_from_owner_reexports(
            target, file_name, name, visited, ambient_guard, OwnerReexportPhase::Both,
        )
    }

    /// Retained source reexports can also appear as binder ALIAS rows. They
    /// belong to the named-edge phase, rather than to direct declarations.
    fn is_owner_named_reexport_alias(
        &self,
        binder: &tsz_binder::BinderState,
        owner_key: &str,
        name: &str,
        symbol: tsz_binder::SymbolId,
    ) -> bool {
        let Some(alias) = binder.get_symbol(symbol) else {
            return false;
        };
        if self.ctx.project_module_resolution_outcomes.is_some()
            && alias.has_any_flags(tsz_binder::symbol_flags::ALIAS)
            && alias.import_module().is_some()
        {
            // Classification is source-owned, independent of successful
            // request admission. Missing proof cannot promote an alias into
            // the direct declaration tier; the named phase still admits it.
            return self.ctx.source_module_alias_kind(symbol)
                != Some(crate::context::SourceModuleAliasKind::Import);
        }
        alias.has_any_flags(tsz_binder::symbol_flags::ALIAS)
            && self.ctx.bound_module_request_for_alias(symbol).is_some_and(|request| {
                request.kind == crate::context::ResolutionRequestKind::EsmReExport
            })
            && self.ctx.reexports_for_file(binder, owner_key).and_then(|rows| rows.get(name))
                .is_some_and(|(source, original)| {
                    alias.import_module() == Some(source.as_str())
                        && alias.import_name() == Some(original.as_deref().unwrap_or(name))
                })
    }

    /// Follow only edges declared by this proven ambient key or canonical File.
    fn resolve_member_from_owner_reexports(
        &self,
        owner: usize,
        owner_key: &str,
        name: &str,
        visited: &mut rustc_hash::FxHashSet<(usize, String)>,
        ambient_guard: &mut rustc_hash::FxHashSet<(Option<usize>, String)>,
        phase: OwnerReexportPhase,
    ) -> Option<tsz_binder::SymbolId> {
        let binder = self.ctx.get_binder_for_file(owner)?;
        if phase != OwnerReexportPhase::Wildcard
            && let Some((source, original)) = self
            .ctx
            .reexports_for_file(binder, owner_key)
            .and_then(|rows| rows.get(name))
        {
            let original = original.as_deref().unwrap_or(name);
            if self.ctx.project_module_resolution_outcomes.is_some() {
                if let Some((alias, request)) = self.ctx.bound_named_reexport(
                    owner, owner_key, source, name, original,
                ) {
                    match self.ctx.project_module_outcome_for_request(
                        request.importer, request.specifier, request.mode, request.kind,
                    ) {
                        Some(crate::context::ResolvedModuleRequestOutcome::File(_)
                            | crate::context::ResolvedModuleRequestOutcome::External { .. }
                            | crate::context::ResolvedModuleRequestOutcome::Unresolved) => {
                            // The exact source declaration publishes this name.
                            // Retain its real ALIAS instead of stripping the wrapper:
                            // the shared alias walker follows this same bound request
                            // with its cycle guard, and sees every type-only edge.
                            // An unresolved named alias remains present; a wildcard
                            // or absent/malformed owner cannot invent the name.
                            return Some(alias);
                        }
                        _ => {}
                    }
                }
            } else if let Some(target) = self.ctx.resolve_named_reexport_target_from_file(
                owner, owner_key, source, name, original,
            ) && let Some(symbol) =
                self.resolve_owner_edge_file_member(target, original, visited, ambient_guard)
            {
                return Some(symbol);
            }
        }
        if phase != OwnerReexportPhase::Named
            && name != "default"
            && let Some(rows) = self.ctx.wildcard_reexports_for_file(binder, owner_key)
        {
            for edge in rows.shared().edges() {
                let mut edge_visited = visited.clone();
                if let Some(target) = self
                    .ctx
                    .resolve_wildcard_reexport_target_from_file(owner, owner_key, edge)
                    && let Some(symbol) = self.resolve_owner_edge_file_member(
                        target, name, &mut edge_visited, ambient_guard,
                    )
                {
                    return Some(symbol);
                }
            }
        }
        None
    }

    fn resolve_owner_edge_file_exports(
        &self,
        target: usize,
        visited: &mut rustc_hash::FxHashSet<usize>,
        ambient_guard: &mut rustc_hash::FxHashSet<(Option<usize>, String)>,
    ) -> Option<tsz_binder::SymbolTable> {
        if !visited.insert(target) {
            return None;
        }
        let binder = self.ctx.get_binder_for_file(target)?;
        let arena = self.ctx.get_arena_for_file(target as u32);
        let file_name = &arena.source_files.first()?.file_name;
        let identity = (Some(target), file_name.clone());
        if !ambient_guard.insert(identity.clone()) {
            return None;
        }
        let mut combined = tsz_binder::SymbolTable::new();
        if let Some(exports) = self.ctx.module_exports_for_module(binder, file_name) {
            for (name, &symbol) in exports.iter() {
                if !self.is_owner_named_reexport_alias(binder, file_name, name, symbol) {
                    combined.set(name.clone(), symbol);
                }
            }
            self.merge_export_equals_members(binder, exports, &mut combined);
            if let Some(symbol) = exports.get("export=").and_then(|id| binder.get_symbol(id)) {
                let _ = self.merge_export_equals_import_type_members_with_ambient_guard(
                    symbol, Some(target), &mut combined, ambient_guard,
                );
            }
            for (_, &symbol) in exports.iter() {
                let owner = self.ctx.resolve_symbol_declaring_file_index(symbol).unwrap_or(target);
                self.ctx.register_symbol_file_target(symbol, owner);
            }
        }
        self.collect_owner_reexported_symbols(
            target,
            file_name,
            &mut combined,
            visited,
            ambient_guard,
            OwnerReexportPhase::Both,
        );
        self.merge_module_augmentation_namespace_exports(&mut combined, target, None);
        for (_, &symbol) in combined.iter() {
            let owner = self
                .ctx
                .resolve_symbol_declaring_file_index(symbol)
                .or_else(|| self.ctx.resolve_dynamic_symbol_file_index(symbol))
                .unwrap_or(target);
            self.ctx.register_symbol_file_target(symbol, owner);
        }
        ambient_guard.remove(&identity);
        Some(combined)
    }

    /// Collect one declared owner's value namespace. File cycles share visited
    /// state; nested ambient import-type hydration shares the active owner guard.
    fn collect_owner_reexported_symbols(
        &self,
        owner: usize,
        owner_key: &str,
        result: &mut tsz_binder::SymbolTable,
        visited: &mut rustc_hash::FxHashSet<usize>,
        ambient_guard: &mut rustc_hash::FxHashSet<(Option<usize>, String)>,
        phase: OwnerReexportPhase,
    ) {
        let Some(binder) = self.ctx.get_binder_for_file(owner) else {
            return;
        };
        if phase != OwnerReexportPhase::Wildcard
            && let Some(rows) = self.ctx.reexports_for_file(binder, owner_key) {
            for (name, (source, original)) in rows {
                if result.has(name) {
                    continue;
                }
                let original = original.as_deref().unwrap_or(name);
                let mut edge_visited = rustc_hash::FxHashSet::default();
                let target = if self.ctx.project_module_resolution_outcomes.is_some() {
                    let Some((alias, request)) = self.ctx.bound_named_reexport(
                        owner, owner_key, source, name, original,
                    ) else {
                        continue;
                    };
                    // Prove the exact admitted chain through the shared runtime
                    // validator, retaining every source alias and its type-only
                    // flag. Its selected resolver supports both File and External
                    // targets; no specifier-only or file-local fallback is used.
                    if self.ctx.select_module_request(request).is_some()
                        && self.runtime_namespace_alias_target(alias).is_some()
                    {
                        result.set(name.clone(), alias);
                    }
                    continue;
                } else {
                    self.ctx.resolve_named_reexport_target_from_file(
                        owner, owner_key, source, name, original,
                    )
                };
                if let Some(target) = target
                    && let Some(symbol) = self.resolve_owner_edge_file_member(
                    target,
                    original,
                    &mut edge_visited,
                    ambient_guard,
                ) {
                    result.set(name.clone(), symbol);
                }
            }
        }
        if phase != OwnerReexportPhase::Named
            && let Some(rows) = self.ctx.wildcard_reexports_for_file(binder, owner_key) {
            for edge in rows.shared().edges() {
                if edge.target.1 {
                    continue;
                }
                if let Some(target) = self
                    .ctx
                    .resolve_wildcard_reexport_target_from_file(owner, owner_key, edge)
                    && let Some(exports) =
                        self.resolve_owner_edge_file_exports(target, visited, ambient_guard)
                {
                    for (name, symbol) in exports.iter() {
                        if name != "default" && !result.has(name) {
                            result.set(name.clone(), *symbol);
                        }
                    }
                }
            }
        }
    }

    pub(crate) fn resolve_exports_for_selected_request(
        &self,
        selected: crate::context::SelectedModuleRequest<'_>,
    ) -> Option<tsz_binder::SymbolTable> {
        self.resolve_exports_for_selected_request_with_ambient_guard(
            selected,
            &mut rustc_hash::FxHashSet::default(),
        )
    }

    fn resolve_exports_for_selected_request_with_ambient_guard(
        &self,
        selected: crate::context::SelectedModuleRequest<'_>,
        ambient_guard: &mut rustc_hash::FxHashSet<(Option<usize>, String)>,
    ) -> Option<tsz_binder::SymbolTable> {
        if let Some(target) = selected.file_target() {
            return self.resolve_owner_edge_file_exports(
                target,
                &mut rustc_hash::FxHashSet::default(),
                ambient_guard,
            );
        }
        selected.external_identity()?;
        let mut admitted_key = None;
        let mut combined = tsz_binder::SymbolTable::new();
        let mut merged_direct = tsz_binder::SymbolTable::new();
        let mut owners = rustc_hash::FxHashSet::default();
        let mut tables = Vec::new();
        let mut seen_tables = rustc_hash::FxHashSet::default();
        for (file_idx, binder) in self.ctx.all_binders.as_ref()?.iter().enumerate() {
            let Some((ambient_key, exports)) = self
                .ctx
                .ambient_exports_for_selected_request(binder, selected)
            else {
                continue;
            };
            if admitted_key.is_some_and(|key| key != ambient_key) {
                continue;
            }
            if admitted_key.is_none() && !ambient_guard.insert((None, ambient_key.to_owned())) {
                return None;
            }
            admitted_key = Some(ambient_key);
            if let Some(local_exports) = self.ambient_declaration_scope_exports(file_idx, ambient_key) {
                owners.insert(file_idx);
                for (name, &symbol) in local_exports.iter() {
                    if !combined.has(name) {
                        combined.set(name.clone(), symbol);
                    }
                }
            }
            if self.first_selected_ambient_export_table(
                binder, ambient_key, exports, &mut seen_tables,
            ) {
                tables.push((binder.as_ref(), exports));
            }
        }
        let ambient_key = admitted_key?;
        // Keep all per-file direct rows, then hydrate each borrowed immutable
        // merged export context once before the existing named/wildcard phases.
        for (binder, exports) in tables {
            let mut declaration = exports.clone();
            self.merge_export_equals_members(binder, exports, &mut declaration);
            if let Some(symbol) = exports.get("export=").and_then(|id| binder.get_symbol(id)) {
                let _ = self.merge_export_equals_import_type_members_with_ambient_guard(
                    symbol,
                    exports
                        .get("export=")
                        .and_then(|id| self.ctx.resolve_symbol_declaring_file_index(id)),
                    &mut declaration,
                    ambient_guard,
                );
            }
            for (name, &symbol) in declaration.iter() {
                if !merged_direct.has(name)
                    && !self.is_owner_named_reexport_alias(binder, ambient_key, name, symbol)
                {
                    merged_direct.set(name.clone(), symbol);
                }
            }
            for (_, &symbol) in exports.iter() {
                if let Some(owner) = self.ctx.resolve_symbol_declaring_file_index(symbol) {
                    owners.insert(owner);
                }
            }
        }
        for (name, &symbol) in merged_direct.iter() {
            if !combined.has(name) {
                combined.set(name.clone(), symbol);
            }
        }
        if let Some(files) = self.ctx.files_for_module_specifier(ambient_key) {
            owners.extend(files.iter().copied());
        }
        let mut owners: Vec<_> = owners.into_iter().collect();
        owners.sort_unstable();
        for phase in [OwnerReexportPhase::Named, OwnerReexportPhase::Wildcard] {
            for &owner in &owners {
                let mut visited = rustc_hash::FxHashSet::default();
                self.collect_owner_reexported_symbols(
                    owner,
                    ambient_key,
                    &mut combined,
                    &mut visited,
                    ambient_guard,
                    phase,
                );
            }
        }
        ambient_guard.remove(&(None, ambient_key.to_owned()));
        Some(combined)
    }

    /// Like `resolve_effective_module_exports` but uses an explicit `resolution-mode`
    /// override from import attributes (e.g., `with { "resolution-mode": "require" }`).
    /// Falls back to the non-mode-aware path when no override is provided.
    pub(crate) fn resolve_effective_module_exports_with_mode(
        &self,
        module_specifier: &str,
        resolution_mode: Option<crate::context::ResolutionModeOverride>,
    ) -> Option<tsz_binder::SymbolTable> {
        if let Some(mode) = resolution_mode
            && let Some(target_idx) = self.ctx.resolve_import_target_from_file_with_mode(
                self.ctx.current_file_idx,
                module_specifier,
                Some(mode),
            )
        {
            if let Some(exports) = self
                .resolve_cross_file_namespace_exports_for_file(target_idx, Some(module_specifier))
            {
                return Some(exports);
            }
            return Some(tsz_binder::SymbolTable::new());
        }
        self.resolve_effective_module_exports_from_file(
            module_specifier,
            Some(self.ctx.current_file_idx),
        )
        .or_else(|| self.resolve_effective_module_exports(module_specifier))
    }

    /// Like `resolve_effective_module_exports` but optionally resolves relative paths
    /// from a specific source file. This is needed for cross-file namespace re-exports
    /// where the module specifier (e.g., `"./b"`) is relative to the declaring file,
    /// not the current file being checked.
    pub(crate) fn resolve_effective_module_exports_from_file(
        &self,
        module_specifier: &str,
        source_file_idx: Option<usize>,
    ) -> Option<tsz_binder::SymbolTable> {
        if let Some(source_idx) = source_file_idx
            && let Some(target_idx) = self
                .ctx
                .resolve_import_target_from_file(source_idx, module_specifier)
            && let Some(exports) = self
                .resolve_cross_file_namespace_exports_for_file(target_idx, Some(module_specifier))
        {
            return Some(exports);
        }

        if let Some(target_idx) = self.ctx.resolve_import_target(module_specifier)
            && let Some(exports) = self
                .resolve_cross_file_namespace_exports_for_file(target_idx, Some(module_specifier))
        {
            return Some(exports);
        }

        for candidate in module_specifier_candidates(module_specifier) {
            // When resolving from a specific source file (cross-file symbol),
            // also try resolving the module specifier from that file's perspective
            if let Some(source_idx) = source_file_idx
                && let Some(target_idx) = self
                    .ctx
                    .resolve_import_target_from_file(source_idx, &candidate)
                && let Some(exports) =
                    self.resolve_cross_file_namespace_exports_for_file(target_idx, Some(&candidate))
            {
                return Some(exports);
            }

            if let Some(exports) = self.resolve_cross_file_namespace_exports(&candidate) {
                return Some(exports);
            }

            if let Some(exports) = self
                .ctx
                .module_exports_for_module(self.ctx.binder, &candidate)
            {
                let mut combined = exports.clone();
                self.merge_export_equals_members(self.ctx.binder, exports, &mut combined);
                if let Some(export_equals_sym_id) = exports.get("export=")
                    && let Some(export_equals_symbol) =
                        self.ctx.binder.get_symbol(export_equals_sym_id)
                {
                    let _ = self.merge_export_equals_import_type_members(
                        export_equals_symbol,
                        source_file_idx.or_else(|| self.ctx.resolve_import_target(&candidate)),
                        &mut combined,
                    );
                }
                return Some(combined);
            }
        }

        None
    }

    fn resolve_ambient_module_namespace_exports(
        &self,
        module_specifier: &str,
    ) -> Option<tsz_binder::SymbolTable> {
        let binders = self.ctx.all_binders.as_ref()?;
        // Use O(1) module binder index when available.
        if let Some(file_indices) = self.ctx.files_for_module_specifier(module_specifier) {
            for &file_idx in file_indices {
                if let Some(binder) = binders.get(file_idx)
                    && let Some(exports) =
                        self.ctx.module_exports_for_module(binder, module_specifier)
                {
                    let mut combined = exports.clone();
                    self.merge_export_equals_members(binder, exports, &mut combined);
                    if let Some(export_equals_sym_id) = exports.get("export=")
                        && let Some(export_equals_symbol) = binder.get_symbol(export_equals_sym_id)
                    {
                        let _ = self.merge_export_equals_import_type_members(
                            export_equals_symbol,
                            Some(file_idx),
                            &mut combined,
                        );
                    }
                    return Some(combined);
                }
            }
        } else {
            for (file_idx, binder) in binders.iter().enumerate() {
                if let Some(exports) = self.ctx.module_exports_for_module(binder, module_specifier)
                {
                    let mut combined = exports.clone();
                    self.merge_export_equals_members(binder, exports, &mut combined);
                    if let Some(export_equals_sym_id) = exports.get("export=")
                        && let Some(export_equals_symbol) = binder.get_symbol(export_equals_sym_id)
                    {
                        let _ = self.merge_export_equals_import_type_members(
                            export_equals_symbol,
                            Some(file_idx),
                            &mut combined,
                        );
                    }
                    return Some(combined);
                }
            }
        }
        None
    }

    fn merge_export_equals_members(
        &self,
        binder: &tsz_binder::BinderState,
        exports: &tsz_binder::SymbolTable,
        combined: &mut tsz_binder::SymbolTable,
    ) {
        let Some(export_equals_sym_id) = exports.get("export=") else {
            return;
        };
        let Some(export_equals_symbol) = binder.get_symbol(export_equals_sym_id) else {
            return;
        };

        if let Some(symbol_exports) = export_equals_symbol.exports.as_ref() {
            for (name, sym_id) in symbol_exports.iter() {
                if name != "default" && !combined.has(name) {
                    combined.set(name.to_string(), *sym_id);
                }
            }
        }

        // The `.members` table on a class symbol holds INSTANCE members (e.g. `bar`
        // from `class D { bar: string; }`). Those live on D's prototype and on
        // instances of D — they are never accessible at the module-namespace level.
        // Merging them here would synthesize a phantom `{ bar }` namespace surface
        // and force the import type to be `typeof D & { bar }` instead of `typeof D`.
        // tsc treats `import x = require()` of an `export = D` module as `typeof D`
        // directly. Static members and namespace augmentations live in `.exports`,
        // which we already merged above.
        let is_class = export_equals_symbol.has_any_flags(tsz_binder::symbol_flags::CLASS);
        if !is_class && let Some(symbol_members) = export_equals_symbol.members.as_ref() {
            for (name, sym_id) in symbol_members.iter() {
                if name != "default" && !combined.has(name) {
                    combined.set(name.to_string(), *sym_id);
                }
            }
        }
    }

    /// When `export =` targets a `typeof import("./...")` declaration, the binder symbol
    /// itself has no exports table. Re-hydrate the referenced module's named exports so
    /// namespace imports see the same surface as the imported module.
    pub(crate) fn merge_export_equals_import_type_members(
        &self,
        export_equals_symbol: &tsz_binder::Symbol,
        fallback_decl_file_idx: Option<usize>,
        combined: &mut tsz_binder::SymbolTable,
    ) -> Option<String> {
        self.merge_export_equals_import_type_members_with_ambient_guard(
            export_equals_symbol,
            fallback_decl_file_idx,
            combined,
            &mut rustc_hash::FxHashSet::default(),
        )
    }

    fn merge_export_equals_import_type_members_with_ambient_guard(
        &self,
        export_equals_symbol: &tsz_binder::Symbol,
        fallback_decl_file_idx: Option<usize>,
        combined: &mut tsz_binder::SymbolTable,
        ambient_guard: &mut rustc_hash::FxHashSet<(Option<usize>, String)>,
    ) -> Option<String> {
        let decl_file_idx = if export_equals_symbol.decl_file_idx == u32::MAX {
            fallback_decl_file_idx?
        } else {
            export_equals_symbol.decl_file_idx as usize
        };
        let binder = self.ctx.get_binder_for_file(decl_file_idx)?;
        let arena = self.ctx.get_arena_for_file(decl_file_idx as u32);

        let module_specifier_from_decl = |decl_idx: NodeIndex| -> Option<(String, NodeIndex)> {
            let node = arena.get(decl_idx)?;
            if node.kind != syntax_kind_ext::VARIABLE_DECLARATION {
                return None;
            }
            let var_decl = arena.get_variable_declaration(node)?;
            if !var_decl.type_annotation.is_some() {
                return None;
            }
            self.import_type_module_specifier_from_type_node(arena, var_decl.type_annotation)
                .map(|specifier| (specifier, var_decl.type_annotation))
        };

        let mut module_specifier = export_equals_symbol
            .value_declaration
            .into_option()
            .and_then(module_specifier_from_decl)
            .or_else(|| {
                export_equals_symbol
                    .declarations
                    .iter()
                    .find_map(|&decl_idx| module_specifier_from_decl(decl_idx))
            });

        // Handle `export = x` where `x` carries the import-type annotation.
        if module_specifier.is_none() {
            let export_assign_decl = export_equals_symbol
                .value_declaration
                .into_option()
                .and_then(|decl_idx| {
                    arena.get(decl_idx).and_then(|node| {
                        (node.kind == syntax_kind_ext::EXPORT_ASSIGNMENT).then_some(decl_idx)
                    })
                })
                .or_else(|| {
                    export_equals_symbol
                        .declarations
                        .iter()
                        .find_map(|&decl_idx| {
                            arena.get(decl_idx).and_then(|node| {
                                (node.kind == syntax_kind_ext::EXPORT_ASSIGNMENT)
                                    .then_some(decl_idx)
                            })
                        })
                });

            if let Some(export_assign_idx) = export_assign_decl
                && let Some(assign) = arena
                    .get(export_assign_idx)
                    .and_then(|node| arena.get_export_assignment(node))
                && let Some(target_sym_id) = binder
                    .get_node_symbol(assign.expression)
                    .or_else(|| binder.resolve_identifier(arena, assign.expression))
            {
                let resolved_target = {
                    let mut visited = AliasCycleTracker::new();
                    self.resolve_alias_symbol(target_sym_id, &mut visited)
                        .unwrap_or(target_sym_id)
                };
                let target_symbol = binder
                    .get_symbol(resolved_target)
                    .or_else(|| self.get_symbol_globally(resolved_target))
                    .or_else(|| self.get_cross_file_symbol(resolved_target));
                if let Some(target_symbol) = target_symbol {
                    module_specifier = target_symbol
                        .value_declaration
                        .into_option()
                        .and_then(module_specifier_from_decl)
                        .or_else(|| {
                            target_symbol
                                .declarations
                                .iter()
                                .find_map(|&decl_idx| module_specifier_from_decl(decl_idx))
                        });
                }
            }
        }

        let (module_specifier, annotation) = module_specifier?;
        let (nested_exports, exact_target) =
            if self.ctx.project_module_resolution_outcomes.is_some() {
                let request = self
                    .ctx
                    .bound_import_type_request_at(decl_file_idx, annotation)?;
                let selected = self.ctx.select_module_request(request)?;
                (
                    self.resolve_exports_for_selected_request_with_ambient_guard(selected, ambient_guard)?,
                    selected.file_target(),
                )
            } else {
                (
                    self.resolve_effective_module_exports_from_file(
                        &module_specifier,
                        Some(decl_file_idx),
                    )?,
                    None,
                )
            };
        let nested_target_idx = if self.ctx.project_module_resolution_outcomes.is_some() {
            exact_target
        } else {
            nested_exports
                .iter()
                .find_map(|(_, &symbol)| self.ctx.resolve_symbol_file_index_stable(symbol))
                .or_else(|| {
                    self.ctx
                        .resolve_import_target_from_file(decl_file_idx, &module_specifier)
                })
                .or_else(|| self.ctx.resolve_import_target(&module_specifier))
        };

        for (name, sym_id) in nested_exports.iter() {
            let owner_file_idx = if self.ctx.project_module_resolution_outcomes.is_some() {
                // A selected namespace can retain exports from several files.
                // Preserve each declaration's owner instead of retargeting the
                // complete table to its first symbol or selected file.
                self.ctx
                    .resolve_symbol_declaring_file_index(*sym_id)
                    .or_else(|| self.ctx.resolve_dynamic_symbol_file_index(*sym_id))
                    .or(exact_target)
            } else {
                nested_target_idx
            };
            if let Some(target_idx) = owner_file_idx {
                self.ctx.register_symbol_file_target(*sym_id, target_idx);
            }
            if name != "export=" && !combined.has(name) {
                combined.set(name.to_string(), *sym_id);
            }
        }
        Some(module_specifier)
    }

    fn import_type_module_specifier_from_type_node(
        &self,
        arena: &tsz_parser::parser::node::NodeArena,
        type_idx: NodeIndex,
    ) -> Option<String> {
        let node = arena.get(type_idx)?;
        if node.kind != syntax_kind_ext::TYPE_QUERY {
            return None;
        }
        let type_query = arena.get_type_query(node)?;
        let call_idx = self.leftmost_import_call_in_entity_name(arena, type_query.expr_name)?;
        let call = arena.get_call_expr(arena.get(call_idx)?)?;
        let args = call.arguments.as_ref()?;
        let &first_arg = args.nodes.first()?;
        let arg_node = arena.get(first_arg)?;
        let literal = arena.get_literal(arg_node)?;
        Some(literal.text.clone())
    }

    fn leftmost_import_call_in_entity_name(
        &self,
        arena: &tsz_parser::parser::node::NodeArena,
        mut idx: NodeIndex,
    ) -> Option<NodeIndex> {
        const MAX_DEPTH: usize = 64;
        for _ in 0..MAX_DEPTH {
            let node = arena.get(idx)?;
            if node.kind == syntax_kind_ext::QUALIFIED_NAME {
                let qn = arena.get_qualified_name(node)?;
                idx = qn.left;
                continue;
            }
            if node.kind != syntax_kind_ext::CALL_EXPRESSION {
                return None;
            }
            let call = arena.get_call_expr(node)?;
            let expr_node = arena.get(call.expression)?;
            return (expr_node.kind == SyntaxKind::ImportKeyword as u16).then_some(idx);
        }
        None
    }
}

#[cfg(test)]
mod selected_file_default_tests {
    use super::*;
    use crate::context::CheckerOptions;
    use crate::query_boundaries::common::TypeInterner;
    use std::sync::Arc;
    use tsz_common::common::ModuleKind;
    use tsz_parser::parser::ParserState;

    fn exact_default(file_name: &str, module: ModuleKind, real_default: bool) -> (
        Option<tsz_binder::SymbolId>,
        tsz_binder::SymbolId,
        Option<tsz_binder::SymbolId>,
    ) {
        let mut source = if file_name.ends_with(".js") {
            "class api { static value = 42; static default = 'shadow'; } export = api;".to_owned()
        } else {
            "declare class api { static value: number; static default: string; } export = api;".to_owned()
        };
        if real_default {
            source.push_str(" declare const actual: string; export default actual;");
        }
        if file_name.ends_with(".js") {
            source.push_str(" export const marker = 1;");
        }
        let mut parser = ParserState::new(file_name.to_owned(), source);
        let root = parser.parse_source_file();
        assert!(parser.get_diagnostics().is_empty());
        let mut binder = tsz_binder::BinderState::new();
        binder.bind_source_file(parser.get_arena(), root);
        let exports = binder.module_exports.get(file_name).unwrap();
        let export_equals = exports.get("export=").unwrap();
        let declared_default = exports.get("default");
        let arena = Arc::new(parser.into_arena());
        let binder = Arc::new(binder);
        let types = TypeInterner::new();
        let mut checker = CheckerState::new(
            &arena,
            &binder,
            &types,
            file_name.to_owned(),
            CheckerOptions { module, ..CheckerOptions::default() },
        );
        checker.ctx.set_all_arenas(Arc::new(vec![Arc::clone(&arena)]));
        checker.ctx.set_all_binders(Arc::new(vec![Arc::clone(&binder)]));
        checker.ctx.set_current_file_idx(0);
        // Exact project mode forbids the binder's legacy import fallback.
        checker.ctx.set_project_module_resolution_outcomes(Arc::new(Default::default()));
        let actual = checker.resolve_owner_edge_file_member(
            0,
            "default",
            &mut rustc_hash::FxHashSet::default(),
            &mut rustc_hash::FxHashSet::default(),
        );
        if let Some(symbol) = actual {
            assert_eq!(checker.ctx.resolve_symbol_file_index(symbol), Some(0));
        }
        (actual, export_equals, declared_default)
    }

    #[test]
    fn exact_edge_default_preserves_export_equals_precedence_and_esm_refusal() {
        for file_name in ["/cjs.ts", "/module.mts", "/module.mjs", "/module.js"] {
            for module in [ModuleKind::ESNext, ModuleKind::Preserve] {
                let (actual, export_equals, _) = exact_default(file_name, module, false);
                if file_name == "/cjs.ts" || module == ModuleKind::Preserve {
                    assert_eq!(actual, Some(export_equals), "{file_name} {module:?}");
                } else {
                    assert_eq!(actual, None, "{file_name} {module:?}");
                }
            }
        }
    }

    #[test]
    fn exact_edge_esm_default_keeps_a_real_default_without_export_equals_borrowing() {
        let (actual, export_equals, declared_default) = exact_default("/module.mts", ModuleKind::ESNext, true);
        assert!(declared_default.is_some());
        assert_eq!(actual, declared_default);
        assert_ne!(actual, Some(export_equals));
    }
}

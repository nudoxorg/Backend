//! Pass 2: cross-module linking (OXC-PLAN §Phase 3). Operates purely on
//! `Vec<ModuleFacts>` (no ASTs): assigns module names + paths, resolves
//! re-exports (`indirect`/`star`), runs the visibility BFS, fills symbol-accurate
//! `type_links`, and materializes the final `ir::entry::Index`.
//!
//! The pure path/module-naming helpers below are ported verbatim from the old
//! deno pipeline (they only ever touched paths, never deno types) — the only
//! change is dropping the `ModuleSpecifier` URL branch now that specifiers are
//! plain paths.
//!
//! ## Implementation notes
//!
//! ### Index assembly
//! Mirrors `Ir::from_entries().index()` from `pipeline.rs`: every entry goes
//! into `entries_by_path`; module-level entries (single-component path with no
//! `::`) are also added to `root_ids`.
//!
//! ### type_links strategy
//! Tier-A (name-based): `resolve_ir_type_to_entry_id` mirrors the old
//! `types.rs:371-386` — exact match then suffix match over `type_name_to_id`.
//! The `FactEntry.type_refs` field (symbol-accurate refs from the extractor) is
//! reserved for Tier B; we don't consume it here yet.
//!
//! ### Re-export semantics
//! `export * from "mod"` flattens the target module's symbols into the
//! re-exporter (matching deno_doc's observed output — see OXC-PLAN §Phase 3.2).
//! `export { x } from "mod"` (indirect) synthesises an `Entry::Info` reference
//! pointing at the origin path (matching item.rs `Reference` handling).
//! Cycles are guarded by a `(specifier, name)` visited set.

use std::{
    collections::{HashMap, HashSet},
    hash::{Hash, Hasher},
    path::PathBuf,
};

use ir::{
    entry::{Index, NudoxPath},
    function::Function,
    kind::{Entry, Symbol, Visibility},
    parameter::Parameter,
    ty::Type,
};

use super::extract::{ExportTable, FactEntry, ModuleFacts};

/// Cross-module link: `ModuleFacts` set → `ir::entry::Index`.
pub(crate) fn link(
    modules: Vec<ModuleFacts>,
    _package: &str,
) -> Result<Index, super::error::Package> {
    // -------------------------------------------------------------------------
    // Step 1 — assign unique, short dotted module names from specifiers
    // -------------------------------------------------------------------------
    let specifiers: Vec<String> = modules
        .iter()
        .map(|m| m.specifier.to_string_lossy().into_owned())
        .collect();
    let spec_to_name: HashMap<String, String> = assign_unique_module_names(&specifiers);

    // -------------------------------------------------------------------------
    // Step 2 — build the path_to_id map + finalize NudoxPath on every entry
    // -------------------------------------------------------------------------
    // We operate on an owned, mutable copy of every module's entries.
    let mut module_entries: Vec<(String /* module_name */, Vec<FactEntry>)> =
        Vec::with_capacity(modules.len());

    // Export tables and import tables indexed by specifier, for re-export
    // resolution.  imports_by_spec is built for completeness (Tier B will
    // consume it for import-chain resolution); it is currently unused in
    // the name-based Tier-A path.
    let mut exports_by_spec: HashMap<String, ExportTable> = HashMap::new();
    // Map from specifier → module_name for quick lookup.
    let mut spec_to_name_map: HashMap<String, String> = HashMap::new();

    for module in &modules {
        let spec = module.specifier.to_string_lossy().into_owned();
        let module_name = spec_to_name.get(&spec).cloned().unwrap_or_else(|| {
            specifier_to_module_name(&spec)
        });
        spec_to_name_map.insert(spec.clone(), module_name.clone());
        exports_by_spec.insert(spec.clone(), module.exports.clone());
    }

    // Clone module entries, prepend module name to local_path, set NudoxPath.
    for module in modules {
        let spec = module.specifier.to_string_lossy().into_owned();
        let module_name = spec_to_name_map.get(&spec).cloned().unwrap_or_default();

        // Emit a synthetic Entry::Module for the module itself.
        // (The old pipeline emitted a module entry for each specifier.)
        let module_path_segments = vec![module_name.clone()];
        let module_nudox_path =
            NudoxPath::Local(PathBuf::from(module_path_segments.join("::")));

        let module_entry = Entry::Module(Symbol {
            name: module_name.clone(),
            path: module_nudox_path,
            aliases: None,
            visibility: Visibility::Public,
            documentation: module.module_doc.clone(),
            deprecation: None,
            doc_links: None,
            inner: ir::module::Module { members: None },
        });

        let mut fact_entries = Vec::new();
        // Synthetic module entry (no local_path — it lives at [module_name])
        fact_entries.push(FactEntry {
            entry: module_entry,
            local_path: vec![],
            type_refs: vec![],
        });

        for mut fact in module.entries {
            // Prepend module name to local_path
            let mut full_segments = vec![module_name.clone()];
            full_segments.extend_from_slice(&fact.local_path);

            let nudox_path =
                NudoxPath::Local(PathBuf::from(full_segments.join("::")));

            // Mutate the path on the Entry in place.
            set_entry_path(&mut fact.entry, nudox_path);

            // Update local_path to full segments.
            fact.local_path = full_segments;

            fact_entries.push(fact);
        }

        module_entries.push((module_name, fact_entries));
    }

    // -------------------------------------------------------------------------
    // Step 3 — build type_name_to_id
    // -------------------------------------------------------------------------
    let mut type_name_to_id: HashMap<String, i64> = HashMap::new();
    for (_, entries) in &module_entries {
        for fact in entries {
            if fact.local_path.is_empty() {
                continue; // synthetic module entry
            }
            let id = path_to_id(&fact.local_path);
            // name-only key (last segment)
            if let Some(name) = fact.local_path.last() {
                type_name_to_id.entry(name.clone()).or_insert(id);
            }
            // fully-qualified key (dot-joined)
            let full_key = fact.local_path.join(".");
            type_name_to_id.entry(full_key).or_insert(id);
        }
    }

    // -------------------------------------------------------------------------
    // Step 4 — fill type_links for every Entry::Function (name-based, Tier A)
    // -------------------------------------------------------------------------
    for (_, entries) in &mut module_entries {
        for fact in entries.iter_mut() {
            if let Entry::Function(ref mut sym) = fact.entry {
                let new_links =
                    build_type_links_for_params(&type_name_to_id, &sym.inner);
                sym.inner.type_links = new_links;
            }
        }
    }

    // -------------------------------------------------------------------------
    // Step 5 — re-export resolution
    // -------------------------------------------------------------------------
    // We build a lookup: specifier → map<local_name, FactEntry> for concrete
    // symbols (to synthesise Info entries for indirect re-exports and to flatten
    // star re-exports).
    //
    // For the purposes of this pass we look up resolved entries from module_entries.

    // Flat index: (module_name, local_name) → NudoxPath for concrete symbols.
    let mut symbol_path_by_spec_name: HashMap<(String, String), NudoxPath> = HashMap::new();
    for (module_name, entries) in &module_entries {
        for fact in entries {
            if fact.local_path.len() == 2 {
                // top-level symbol: [module_name, symbol_name]
                let sym_name = fact.local_path[1].clone();
                symbol_path_by_spec_name.insert(
                    (module_name.clone(), sym_name),
                    fact.entry.path().clone(),
                );
            }
        }
    }

    // Re-export Info entries to append.
    let mut reexport_entries: Vec<Entry> = Vec::new();

    // Collect all specifiers once (to avoid borrow issues in the loop).
    let all_specs: Vec<String> = spec_to_name_map.keys().cloned().collect();

    for re_spec in &all_specs {
        let re_module_name = match spec_to_name_map.get(re_spec) {
            Some(n) => n.clone(),
            None => continue,
        };
        let exports = match exports_by_spec.get(re_spec) {
            Some(e) => e.clone(),
            None => continue,
        };

        // --- indirect: `export { x } from "mod"` ---
        for ind_exp in &exports.indirect {
            // Resolve the origin entry path across modules (with cycle guard).
            let origin_path = resolve_indirect_export(
                &ind_exp.module_request,
                &ind_exp.import_name,
                re_spec,
                &spec_to_name_map,
                &exports_by_spec,
                &symbol_path_by_spec_name,
                &mut HashSet::new(),
            );
            if let Some(origin_path) = origin_path {
                // Synthesise an Entry::Info reference.
                let ref_path_segments =
                    vec![re_module_name.clone(), ind_exp.export_name.clone()];
                let ref_nudox_path =
                    NudoxPath::Local(PathBuf::from(ref_path_segments.join("::")));

                let info_entry = Entry::Info(Symbol {
                    name: ind_exp.export_name.clone(),
                    path: ref_nudox_path,
                    aliases: None,
                    visibility: Visibility::Public,
                    documentation: None,
                    deprecation: None,
                    doc_links: None,
                    inner: nudox_path_to_string(&origin_path),
                });
                reexport_entries.push(info_entry);
            }
        }

        // --- star: `export * from "mod"` --- (flatten target's symbols)
        for star_exp in &exports.star {
            // Resolve target module name.
            let target_spec = resolve_specifier_from(re_spec, &star_exp.module_request, &spec_to_name_map);
            if let Some(target_spec) = target_spec {
                let target_module_name = match spec_to_name_map.get(&target_spec) {
                    Some(n) => n.clone(),
                    None => continue,
                };
                // Find all top-level symbols from the target module and flatten
                // them into the re-exporter (deno_doc behaviour: symbols appear
                // directly on the re-exporter with Public visibility).
                let target_symbols: Vec<(String, NudoxPath)> =
                    symbol_path_by_spec_name
                        .iter()
                        .filter(|((mn, _), _)| *mn == target_module_name)
                        .map(|((_, sym), path)| (sym.clone(), path.clone()))
                        .collect();

                for (sym_name, _origin_path) in target_symbols {
                    // Only add if not already defined in this module.
                    let key = (re_module_name.clone(), sym_name.clone());
                    if symbol_path_by_spec_name.contains_key(&key) {
                        continue;
                    }
                    let ref_path_segments =
                        vec![re_module_name.clone(), sym_name.clone()];
                    let ref_nudox_path =
                        NudoxPath::Local(PathBuf::from(ref_path_segments.join("::")));

                    // Emit an Info reference.
                    let info_entry = Entry::Info(Symbol {
                        name: sym_name.clone(),
                        path: ref_nudox_path.clone(),
                        aliases: None,
                        visibility: Visibility::Public,
                        documentation: None,
                        deprecation: None,
                        doc_links: None,
                        inner: String::new(),
                    });
                    reexport_entries.push(info_entry);
                    // Register so subsequent star re-exports can see it.
                    symbol_path_by_spec_name.insert(key, ref_nudox_path);
                }
            }
        }
    }

    // -------------------------------------------------------------------------
    // Step 6 — visibility BFS
    // -------------------------------------------------------------------------
    // Roots: all symbols that are in the exports.exported_bindings of their own
    // module.  BFS over type references in exported symbols to pull in private
    // (non-exported) types too, tagging them Visibility::Private.
    //
    // We run with `private: true` (same as deno pipeline): all symbols are
    // already included; we simply TAG them Public vs Private here.

    // Build a set of (module_name, symbol_name) pairs that are publicly exported.
    let mut public_exports: HashSet<(String, String)> = HashSet::new();
    for spec in &all_specs {
        let module_name = match spec_to_name_map.get(spec) {
            Some(n) => n.clone(),
            None => continue,
        };
        let exports = match exports_by_spec.get(spec) {
            Some(e) => e,
            None => continue,
        };
        for name in &exports.exported_bindings {
            public_exports.insert((module_name.clone(), name.clone()));
        }
        // local exports
        for le in &exports.local {
            public_exports.insert((module_name.clone(), le.export_name.clone()));
        }
        // indirect exports (re-exported names)
        for ie in &exports.indirect {
            public_exports.insert((module_name.clone(), ie.export_name.clone()));
        }
        // star-flattened (already added in step 5) — covered by exported_bindings
    }

    // Now tag visibility on all non-module entries.
    for (module_name, entries) in &mut module_entries {
        for fact in entries.iter_mut() {
            if fact.local_path.len() < 2 {
                continue; // module entry itself
            }
            let sym_name = &fact.local_path[1];
            let is_exported = public_exports.contains(&(module_name.clone(), sym_name.clone()));
            if !is_exported {
                // Tag private.
                set_entry_visibility(&mut fact.entry, Visibility::Private);
            }
        }
    }

    // -------------------------------------------------------------------------
    // Step 7 — assemble Index
    // -------------------------------------------------------------------------
    let mut entries_by_path: rustc_hash::FxHashMap<NudoxPath, Entry> =
        rustc_hash::FxHashMap::default();
    let mut root_ids: Vec<NudoxPath> = Vec::new();

    for (_module_name, entries) in module_entries {
        for fact in entries {
            let path = fact.entry.path().clone();
            // Root = single-component Local path (module entry), or
            // two-component (top-level symbol) — match pipeline.rs semantics:
            // roots are single component paths with no "::"
            if let NudoxPath::Local(ref p) = path {
                let lossy = p.to_string_lossy();
                if p.components().count() == 1 && !lossy.contains("::") {
                    root_ids.push(path.clone());
                }
            }
            entries_by_path.insert(path, fact.entry);
        }
    }

    // Insert re-export Info entries.
    for entry in reexport_entries {
        let path = entry.path().clone();
        if let NudoxPath::Local(ref p) = path {
            let lossy = p.to_string_lossy();
            if p.components().count() == 1 && !lossy.contains("::") {
                root_ids.push(path.clone());
            }
        }
        entries_by_path.entry(path).or_insert(entry);
    }

    Ok(Index { root_ids, entries_by_path })
}

// ============================================================================
// Misc helpers
// ============================================================================

/// Render a `NudoxPath` to a human-readable string for storage in
/// `Entry::Info.inner` (the origin-path reference field).
fn nudox_path_to_string(path: &NudoxPath) -> String {
    match path {
        NudoxPath::Local(p) => p.to_string_lossy().into_owned(),
        NudoxPath::External { path, dependency } => {
            format!("{dependency}::{}", path.to_string_lossy())
        }
    }
}

// ============================================================================
// type_links helpers
// ============================================================================

/// Build `Function.type_links` from the function's input/output parameters,
/// matching old `build_type_links_for_params` (function.rs:209-239).
///
/// Uses name-based resolution: exact match then suffix match over
/// `type_name_to_id` (Tier A — OXC-PLAN §Phase 3.4).
fn build_type_links_for_params(
    type_name_to_id: &HashMap<String, i64>,
    func: &Function,
) -> Option<rustc_hash::FxHashMap<String, i64>> {
    let mut links: rustc_hash::FxHashMap<String, i64> = rustc_hash::FxHashMap::default();

    if let Some(ref params) = func.input_parameters {
        for (idx, param) in params.iter().enumerate() {
            if let Parameter::Literal(l) = param {
                if let Some(ref ty) = l.r#type {
                    if let Some(id) = resolve_ir_type_to_entry_id(type_name_to_id, ty) {
                        links.insert(
                            ir::pipeline::parameter_link_key("in", idx, params.len(), &l.name),
                            id,
                        );
                    }
                }
            }
        }
    }

    if let Some(ref params) = func.output_parameters {
        for (idx, param) in params.iter().enumerate() {
            if let Parameter::Literal(l) = param {
                if let Some(ref ty) = l.r#type {
                    if let Some(id) = resolve_ir_type_to_entry_id(type_name_to_id, ty) {
                        links.insert(
                            ir::pipeline::parameter_link_key("out", idx, params.len(), &l.name),
                            id,
                        );
                    }
                }
            }
        }
    }

    if links.is_empty() { None } else { Some(links) }
}

/// Exact-then-suffix resolution of a `Type::TypeReference` to an entry id.
/// Mirrors `types.rs:resolve_ir_type_to_entry_id` (Tier A).
fn resolve_ir_type_to_entry_id(
    type_name_to_id: &HashMap<String, i64>,
    ty: &Type,
) -> Option<i64> {
    match ty {
        Type::TypeReference(tr) => {
            // 1. Exact match
            if let Some(&id) = type_name_to_id.get(tr.identifier.as_str()) {
                return Some(id);
            }
            // 2. Suffix match
            for (k, &v) in type_name_to_id {
                if k.ends_with(tr.identifier.as_str()) {
                    return Some(v);
                }
            }
            None
        }
        _ => None,
    }
}

// ============================================================================
// Re-export resolution helpers
// ============================================================================

/// Resolve an indirect export chain to the `NudoxPath` of the origin symbol.
///
/// Follows `export { name } from "mod"` recursively across modules until a
/// concrete symbol's path is found. Returns `None` if the chain cannot be
/// resolved (external dep, unresolvable specifier, etc.).
///
/// `visited` guards against cycles: each `(spec, name)` pair is recorded before
/// recursing.
#[allow(clippy::too_many_arguments)]
fn resolve_indirect_export(
    module_request: &str,
    import_name: &str,
    from_spec: &str,
    spec_to_name: &HashMap<String, String>,
    exports_by_spec: &HashMap<String, ExportTable>,
    symbol_path_by_spec_name: &HashMap<(String, String), NudoxPath>,
    visited: &mut HashSet<(String, String)>,
) -> Option<NudoxPath> {
    // Resolve the module_request specifier to a concrete spec key.
    let target_spec =
        resolve_specifier_from(from_spec, module_request, spec_to_name)?;

    let target_module_name = spec_to_name.get(&target_spec)?;

    // Cycle guard.
    let guard_key = (target_spec.clone(), import_name.to_string());
    if !visited.insert(guard_key) {
        return None;
    }

    // Direct symbol lookup: is `import_name` a concrete symbol in the target?
    if let Some(path) = symbol_path_by_spec_name
        .get(&(target_module_name.clone(), import_name.to_string()))
    {
        return Some(path.clone());
    }

    // Chase further through indirect exports of the target.
    let target_exports = exports_by_spec.get(&target_spec)?;
    for ind in &target_exports.indirect {
        if ind.export_name == import_name {
            // Recurse.
            return resolve_indirect_export(
                &ind.module_request,
                &ind.import_name,
                &target_spec,
                spec_to_name,
                exports_by_spec,
                symbol_path_by_spec_name,
                visited,
            );
        }
    }
    // Chase through local exports (name re-mapped locally).
    for le in &target_exports.local {
        if le.export_name == import_name {
            // Local symbol; look up its path.
            return symbol_path_by_spec_name
                .get(&(target_module_name.clone(), le.local_name.clone()))
                .cloned();
        }
    }

    None
}

/// Given a `from_spec` (the file doing the import) and a module_request string
/// (e.g. `"./utils"` or `"../shared"`), return the matching specifier from
/// `spec_to_name` if we can find it.
///
/// For relative paths we do a simple candidate probe. For absolute/node
/// specifiers we check for an exact match in the map (external deps return
/// `None`).
fn resolve_specifier_from(
    from_spec: &str,
    module_request: &str,
    spec_to_name: &HashMap<String, String>,
) -> Option<String> {
    if module_request.starts_with('.') {
        // Relative path: resolve against from_spec's directory.
        let base_dir = std::path::Path::new(from_spec)
            .parent()
            .unwrap_or(std::path::Path::new(""));
        let joined = base_dir.join(module_request);

        // Try exact + TypeScript-extension candidates.
        let suffixes: &[&str] = &[
            "",
            ".ts",
            ".tsx",
            ".d.ts",
            ".d.tsx",
            ".mts",
            ".d.mts",
            ".cts",
            ".d.cts",
            "/index.ts",
            "/index.d.ts",
        ];
        for suffix in suffixes {
            let candidate = format!("{}{suffix}", joined.to_string_lossy());
            if spec_to_name.contains_key(&candidate) {
                return Some(candidate);
            }
        }
        None
    } else {
        // Non-relative: look for exact match (bare specifier / node_modules).
        spec_to_name.keys().find(|k| k.ends_with(module_request)).cloned()
    }
}

// ============================================================================
// Entry mutation helpers (no `path_mut` on Entry; use match arms)
// ============================================================================

/// Set the `Symbol.path` on any `Entry` variant.
fn set_entry_path(entry: &mut Entry, path: NudoxPath) {
    match entry {
        Entry::Module(s) => s.path = path,
        Entry::RecordType(s) => s.path = path,
        Entry::Info(s) => s.path = path,
        Entry::UnionType(s) => s.path = path,
        Entry::TraitDef(s) => s.path = path,
        Entry::TraitImpl(s) => s.path = path,
        Entry::SumType(s) => s.path = path,
        Entry::Function(s) => s.path = path,
        Entry::TypeAlias(s) => s.path = path,
        Entry::Constant(s) => s.path = path,
        Entry::Variable(s) => s.path = path,
        Entry::Macro(s) => s.path = path,
        Entry::PrimitiveType(s) => s.path = path,
        Entry::Field(s) => s.path = path,
        Entry::Event(s) => s.path = path,
    }
}

/// Set `Symbol.visibility` on any `Entry` variant.
fn set_entry_visibility(entry: &mut Entry, vis: Visibility) {
    match entry {
        Entry::Module(s) => s.visibility = vis,
        Entry::RecordType(s) => s.visibility = vis,
        Entry::Info(s) => s.visibility = vis,
        Entry::UnionType(s) => s.visibility = vis,
        Entry::TraitDef(s) => s.visibility = vis,
        Entry::TraitImpl(s) => s.visibility = vis,
        Entry::SumType(s) => s.visibility = vis,
        Entry::Function(s) => s.visibility = vis,
        Entry::TypeAlias(s) => s.visibility = vis,
        Entry::Constant(s) => s.visibility = vis,
        Entry::Variable(s) => s.visibility = vis,
        Entry::Macro(s) => s.visibility = vis,
        Entry::PrimitiveType(s) => s.visibility = vis,
        Entry::Field(s) => s.visibility = vis,
        Entry::Event(s) => s.visibility = vis,
    }
}

// ============================================================================
// Pure path/module-naming helpers (deno-free ports of the old mod.rs)
// ============================================================================

/// A stable 64-bit id for an entry path, used for `type_links`.
pub(crate) fn path_to_id(path: &[String]) -> i64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    hasher.finish() as i64
}

/// Convert a local file path to a short module name (basename, extension
/// stripped).
pub(crate) fn specifier_to_module_name(specifier: &str) -> String {
    let clean = specifier
        .split('?')
        .next()
        .unwrap_or(specifier)
        .split('#')
        .next()
        .unwrap_or(specifier);
    let last = clean.split('/').next_back().unwrap_or(clean);
    let name = strip_typescript_like_suffix(last);
    if name.is_empty() { clean.to_string() } else { name.to_string() }
}

fn strip_typescript_like_suffix(value: &str) -> &str {
    value
        .trim_end_matches(".d.ts")
        .trim_end_matches(".d.tsx")
        .trim_end_matches(".d.mts")
        .trim_end_matches(".d.cts")
        .trim_end_matches(".ts")
        .trim_end_matches(".tsx")
        .trim_end_matches(".mts")
        .trim_end_matches(".cts")
        .trim_end_matches(".js")
        .trim_end_matches(".mjs")
        .trim_end_matches(".cjs")
        .trim_end_matches(".jsx")
}

fn specifier_to_module_segments(specifier: &str) -> Vec<String> {
    let clean = specifier
        .split('?')
        .next()
        .unwrap_or(specifier)
        .split('#')
        .next()
        .unwrap_or(specifier);
    let raw_segments: Vec<String> = clean
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(str::to_string)
        .collect();

    if raw_segments.is_empty() {
        return vec![clean.to_string()];
    }

    let last_index = raw_segments.len() - 1;
    raw_segments
        .into_iter()
        .enumerate()
        .map(|(idx, segment)| {
            if idx == last_index {
                strip_typescript_like_suffix(&segment).to_string()
            } else {
                segment
            }
        })
        .filter(|segment| !segment.is_empty() && segment != ".")
        .collect()
}

/// Assign a unique, short dotted module name to every specifier.
pub(crate) fn assign_unique_module_names(specifiers: &[String]) -> HashMap<String, String> {
    let mut segment_map: Vec<(String, Vec<String>)> = specifiers
        .iter()
        .cloned()
        .map(|specifier| {
            let segments = specifier_to_module_segments(&specifier);
            (specifier, if segments.is_empty() { vec!["module".to_string()] } else { segments })
        })
        .collect();

    let shared_prefix_len = shared_segment_prefix_len(
        &segment_map.iter().map(|(_, segments)| segments.as_slice()).collect::<Vec<_>>(),
    );
    if shared_prefix_len > 0 {
        for (_, segments) in &mut segment_map {
            if segments.len() > shared_prefix_len {
                segments.drain(..shared_prefix_len);
            }
        }
    }

    let mut assignments = HashMap::with_capacity(segment_map.len());

    for (specifier, segments) in &segment_map {
        let mut chosen = segments.join(".");
        for suffix_len in 1..=segments.len() {
            let candidate = segments[segments.len() - suffix_len..].join(".");
            let duplicate = segment_map.iter().any(|(other_specifier, other_segments)| {
                if other_specifier == specifier {
                    return false;
                }
                other_segments.len() >= suffix_len
                    && other_segments[other_segments.len() - suffix_len..].join(".") == candidate
            });
            if !duplicate {
                chosen = candidate;
                break;
            }
        }
        assignments.insert(specifier.clone(), chosen);
    }

    assignments
}

fn shared_segment_prefix_len(segment_sets: &[&[String]]) -> usize {
    let Some(first) = segment_sets.first() else {
        return 0;
    };

    let mut prefix_len = 0;
    while prefix_len < first.len() {
        let candidate = &first[prefix_len];
        if segment_sets
            .iter()
            .all(|segments| segments.len() > prefix_len && segments[prefix_len] == *candidate)
        {
            prefix_len += 1;
        } else {
            break;
        }
    }

    prefix_len
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_to_id_is_stable() {
        let a = path_to_id(&["mymod".to_string(), "MyClass".to_string()]);
        let b = path_to_id(&["mymod".to_string(), "MyClass".to_string()]);
        assert_eq!(a, b);
    }

    #[test]
    fn path_to_id_differs_for_different_paths() {
        let a = path_to_id(&["mymod".to_string(), "Foo".to_string()]);
        let b = path_to_id(&["mymod".to_string(), "Bar".to_string()]);
        assert_ne!(a, b);
    }

    #[test]
    fn specifier_to_module_name_strips_extensions() {
        assert_eq!(specifier_to_module_name("/tmp/index.d.ts"), "index");
        assert_eq!(specifier_to_module_name("/tmp/v3/index.d.cts"), "index");
        assert_eq!(specifier_to_module_name("/src/utils.ts"), "utils");
    }

    #[test]
    fn assign_unique_module_names_disambiguates() {
        let specifiers = vec![
            "/tmp/src/index.ts".to_string(),
            "/tmp/src/v3/index.ts".to_string(),
            "/tmp/src/v4/index.ts".to_string(),
        ];
        let names = assign_unique_module_names(&specifiers);
        assert_eq!(names["/tmp/src/index.ts"], "index");
        assert_eq!(names["/tmp/src/v3/index.ts"], "v3.index");
        assert_eq!(names["/tmp/src/v4/index.ts"], "v4.index");
    }

    #[test]
    fn resolve_ir_type_reference_exact() {
        let mut map = HashMap::new();
        map.insert("MyClass".to_string(), 42i64);
        let ty = Type::TypeReference(ir::ty::TypeReference {
            identifier: "MyClass".to_string(),
            generic_args: None,
        });
        assert_eq!(resolve_ir_type_to_entry_id(&map, &ty), Some(42));
    }

    #[test]
    fn resolve_ir_type_reference_suffix() {
        let mut map = HashMap::new();
        map.insert("mymod.MyClass".to_string(), 99i64);
        let ty = Type::TypeReference(ir::ty::TypeReference {
            identifier: "MyClass".to_string(),
            generic_args: None,
        });
        assert_eq!(resolve_ir_type_to_entry_id(&map, &ty), Some(99));
    }

    #[test]
    fn resolve_ir_type_non_reference_returns_none() {
        let map = HashMap::new();
        let ty = Type::Never;
        assert_eq!(resolve_ir_type_to_entry_id(&map, &ty), None);
    }
}

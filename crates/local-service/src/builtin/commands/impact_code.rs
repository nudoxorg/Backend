//! Code adapter for the shared impact framework: how TypeScript, TSX and JSX
//! use stylesheets, and how components use each other.
//!
//! Sources come from the selected index (native TypeScript evidence, or the
//! structural file records), are re-read through the confined project reader,
//! and are checked against the recorded identity before they are parsed. The
//! parse is the lexical authority already used by the TypeScript lane (OXC);
//! no CSS or SQL grammar is involved. Every link states its proof:
//!
//! * `styles.card` where `styles` is an import binding of a `*.module.css`
//!   file is an **exact** source-level link to that module's class;
//! * a literal class token in `className`/`class`, including the static
//!   arguments of `clsx`-style helpers, is a **candidate**: nothing here
//!   proves the element renders or which build transform applies;
//! * a JSX tag bound by import to a component in an indexed file is an
//!   **exact** render link; aliases, packages and unresolved barrels are gaps.
use super::impact_css::{global_class_key, module_class_key, stylesheet_key, custom_property_key};
use super::impact_imports::{CompilerImports, ImportLookup, Lookup, RelativeImports, is_code_specifier};
use super::impact_framework::{
    DomainLoad, Edge, FileWitness, ImpactAdapter, ImpactContext, Key, Node, node_key,
};
use super::impact_scan::{FileScan, ImportBinding, Imported, is_component_name, profile_for, scan_module};
use backend_frontend_typescript::legacy::with_analysis;
use backend_library::impact::{
    ImpactDomain, ImpactDomainBasis, ImpactDomainState, ImpactGap, ImpactSeedSpec, ImpactStrength,
};
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct CodeAdapter;

const MAX_FILES: usize = 4_000;
const MAX_FILE_BYTES: usize = 2 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 48 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Cross-file graph
// ---------------------------------------------------------------------------

/// Join `spec` to the directory of `file`, collapsing `.` and `..`.
/// Returns `None` for non-relative specifiers or escapes above the root.
pub(super) fn resolve_relative(file: &str, spec: &str) -> Option<String> {
    if !(spec.starts_with("./") || spec.starts_with("../")) {
        return None;
    }
    let mut parts: Vec<&str> = file.split('/').collect();
    parts.pop();
    for part in spec.split('/') {
        match part {
            "." | "" => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other),
        }
    }
    Some(parts.join("/"))
}

fn component_key(file: &str, name: &str) -> Key {
    node_key(ImpactDomain::Code, "component", file, name)
}
fn module_key(file: &str) -> Key {
    node_key(ImpactDomain::Code, "module", file, "")
}

struct Resolver<'a> {
    scans: &'a BTreeMap<String, FileScan>,
    lookup: &'a dyn ImportLookup,
}
impl Resolver<'_> {
    fn module(&self, from: &str, spec: &str) -> Option<String> {
        match self.lookup.lookup(from, spec) {
            Lookup::File(path) => Some(path),
            Lookup::Package | Lookup::Unresolved(_) => None,
        }
    }
    /// Component key behind `exported` of `file`, following barrels a few hops.
    fn export_target(&self, file: &str, exported: &str, hops: u8) -> Option<Key> {
        let scan = self.scans.get(file)?;
        if let Some(local) = scan.exports.get(exported) {
            if scan.components.contains_key(local) {
                return Some(component_key(file, local));
            }
            // `export default Foo` where Foo is imported or wrapped: treat the
            // local name as the component identity in this file.
            return is_component_name(local).then(|| component_key(file, local));
        }
        if hops == 0 {
            return None;
        }
        let (spec, imported) = scan.reexports.get(exported)?;
        let target = self.module(file, spec)?;
        self.export_target(&target, if imported == "*" { exported } else { imported }, hops - 1)
    }
    fn import_target(&self, file: &str, binding: &ImportBinding) -> Option<Key> {
        let target = self.module(file, &binding.spec)?;
        let exported = match &binding.imported {
            Imported::Default => "default".to_owned(),
            Imported::Named(name) => name.clone(),
            Imported::Namespace => return None,
        };
        self.export_target(&target, &exported, 4)
    }
}

/// Build the code graph from scans. Pure over `scans`, unit-testable.
pub(super) fn build_graph(scans: &BTreeMap<String, FileScan>) -> (BTreeMap<Key, Node>, Vec<Edge>, Vec<ImpactGap>) {
    let files: BTreeSet<String> = scans.keys().cloned().collect();
    build_graph_with(scans, &RelativeImports(&files))
}

/// Build the code graph with import specifiers resolved by `lookup`.
pub(super) fn build_graph_with(
    scans: &BTreeMap<String, FileScan>,
    lookup: &dyn ImportLookup,
) -> (BTreeMap<Key, Node>, Vec<Edge>, Vec<ImpactGap>) {
    let resolver = Resolver { scans, lookup };
    let mut unresolved_imports: Vec<(String, String, String)> = Vec::new();
    let mut nodes: BTreeMap<Key, Node> = BTreeMap::new();
    let mut edges: Vec<Edge> = Vec::new();
    let mut alias_imports = 0u32;
    let mut dynamic = 0u32;
    let mut dynamic_files: Vec<&str> = Vec::new();
    let mut unresolved_renders = 0u32;
    let mut owner_node = |nodes: &mut BTreeMap<Key, Node>, file: &str, owner: &Option<String>, line: u32| -> Key {
        match owner {
            Some(name) => {
                let key = component_key(file, name);
                nodes.entry(key).or_insert_with(|| {
                    let mut node = Node::new(ImpactDomain::Code, "component", file, name);
                    node.path = Some(file.to_owned());
                    node.line = scans.get(file).and_then(|scan| scan.components.get(name).copied()).or(Some(line));
                    node
                });
                key
            }
            None => {
                let key = module_key(file);
                nodes.entry(key).or_insert_with(|| {
                    let mut node = Node::new(ImpactDomain::Code, "module", file, file);
                    node.path = Some(file.to_owned());
                    node.line = Some(1);
                    node
                });
                key
            }
        }
    };
    for (file, scan) in scans {
        dynamic += scan.dynamic_class_expressions;
        if scan.dynamic_class_expressions > 0 {
            dynamic_files.push(file.as_str());
        }
        // Declared components exist even when nothing links to them yet.
        for (name, line) in &scan.components {
            nodes.entry(component_key(file, name)).or_insert_with(|| {
                let mut node = Node::new(ImpactDomain::Code, "component", file, name);
                node.path = Some(file.clone());
                node.line = Some(*line);
                node
            });
        }
        // Code imports which name no indexed file are reported, never dropped.
        // Stylesheet and asset specifiers have their own links and gaps.
        let reexported = scan.reexports.values().map(|(spec, _)| spec.as_str());
        let imported = scan.imports.iter().map(|binding| binding.spec.as_str());
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for spec in imported.chain(reexported).filter(|spec| is_code_specifier(spec)) {
            if !seen.insert(spec) {
                continue;
            }
            if let Lookup::Unresolved(reason) = lookup.lookup(file, spec) {
                unresolved_imports.push((file.clone(), spec.to_owned(), reason));
            }
        }
        // Class tokens.
        for usage in &scan.class_uses {
            let to = owner_node(&mut nodes, file, &usage.owner, usage.line);
            if file.ends_with(".vue") {
                // A scoped or module `<style>` block of the same component owns
                // its class names; the token may also name a global class.
                edges.push(Edge {
                    from: module_class_key(file, &usage.token),
                    to,
                    strength: ImpactStrength::Candidate,
                    relation: "code.uses-scoped-class",
                    why: format!(
                        "{}class \"{}\" in the template at line {}; applies if a scoped or module <style> of this component defines it",
                        if usage.conditional { "conditional " } else { "" },
                        usage.token,
                        usage.line
                    ),
                });
            }
            edges.push(Edge {
                from: global_class_key(&usage.token),
                to,
                strength: ImpactStrength::Candidate,
                relation: "code.uses-class",
                why: format!(
                    "{}class \"{}\" in className at line {}",
                    if usage.conditional { "conditional " } else { "" },
                    usage.token,
                    usage.line
                ),
            });
        }
        // CSS-module members.
        for usage in &scan.module_class_uses {
            let binding = &scan.imports[usage.binding];
            let Some(target) = resolve_relative(file, &binding.spec) else {
                alias_imports += 1;
                continue;
            };
            let to = owner_node(&mut nodes, file, &usage.owner, usage.line);
            edges.push(Edge {
                from: module_class_key(&target, &usage.class),
                to,
                strength: ImpactStrength::Exact,
                relation: "code.uses-module-class",
                why: format!("{}.{} from {} at line {}", binding.local, usage.class, binding.spec, usage.line),
            });
        }
        // Stylesheet imports reach the importing module and its components.
        let sheet_imports = scan
            .imports
            .iter()
            .map(|binding| (binding.spec.as_str(), binding.line))
            .chain(scan.side_effect_imports.iter().map(|(spec, line)| (spec.as_str(), *line)))
            .filter(|(spec, _)| spec.ends_with(".css"));
        let mut importer = None;
        for (spec, line) in sheet_imports {
            let Some(target) = resolve_relative(file, spec) else {
                alias_imports += 1;
                continue;
            };
            let module = *importer.get_or_insert_with(|| owner_node(&mut nodes, file, &None, line));
            edges.push(Edge {
                from: stylesheet_key(&target),
                to: module,
                strength: ImpactStrength::Exact,
                relation: "code.imports-stylesheet",
                why: format!("imported by {file} at line {line}"),
            });
            for name in scan.components.keys() {
                let ckey = owner_node(&mut nodes, file, &Some(name.clone()), line);
                if scan.exports.values().any(|local| local == name) {
                    edges.push(Edge {
                        from: module,
                        to: ckey,
                        strength: ImpactStrength::Candidate,
                        relation: "code.module-component",
                        why: format!("{name} lives in a module which loads {spec}"),
                    });
                }
            }
        }
        // Component renders.
        for render in &scan.renders {
            let Some(parent) = Some(owner_node(&mut nodes, file, &render.owner, render.line)) else { continue };
            let child = if let Some(binding) = scan.imports.iter().find(|binding| binding.local == render.local) {
                resolver.import_target(file, binding)
            } else if scan.components.contains_key(&render.local) {
                Some(component_key(file, &render.local))
            } else {
                None
            };
            match child {
                Some(child) if nodes.contains_key(&child) || child != parent => {
                    // The child node may live in a file not yet visited.
                    edges.push(Edge {
                        from: child,
                        to: parent,
                        strength: ImpactStrength::Exact,
                        relation: "code.renders",
                        why: format!("<{}> rendered at line {}", render.local, render.line),
                    });
                }
                _ => unresolved_renders += 1,
            }
        }
        // Custom properties used from code.
        for usage in &scan.property_uses {
            let node = owner_node(&mut nodes, file, &usage.owner, usage.line);
            if usage.writes {
                edges.push(Edge {
                    from: node,
                    to: custom_property_key(&usage.name),
                    strength: ImpactStrength::Candidate,
                    relation: "code.sets-property",
                    why: format!("sets {} at line {}", usage.name, usage.line),
                });
            } else {
                edges.push(Edge {
                    from: custom_property_key(&usage.name),
                    to: node,
                    strength: ImpactStrength::Candidate,
                    relation: "code.reads-property",
                    why: format!("reads {} at line {}", usage.name, usage.line),
                });
            }
        }
    }
    // Make sure every component endpoint has a node (children in unvisited files).
    for (file, scan) in scans {
        for (name, line) in &scan.components {
            nodes.entry(component_key(file, name)).or_insert_with(|| {
                let mut node = Node::new(ImpactDomain::Code, "component", file, name);
                node.path = Some(file.clone());
                node.line = Some(*line);
                node
            });
        }
    }
    let mut gaps = vec![
        ImpactGap {
            facet: "code.render-model".to_owned(),
            note: "class and render links are lexical: conditional rendering, portals, slots, runtime-built class names and spread props are not followed".to_owned(),
            count: None,
        },
        ImpactGap {
            facet: "code.build-transform".to_owned(),
            note: "bundler class-name transforms (CSS-module hashing, utility generators, CSS-in-JS) are not read; a literal class link is a candidate".to_owned(),
            count: None,
        },
    ];
    if dynamic > 0 {
        gaps.push(ImpactGap {
            facet: "code.dynamic-class".to_owned(),
            note: format!(
                "className expressions with runtime parts in {}{}; only their static tokens are linked, and every link from them is a candidate",
                dynamic_files.iter().take(4).copied().collect::<Vec<_>>().join(", "),
                if dynamic_files.len() > 4 { format!(" and {} more", dynamic_files.len() - 4) } else { String::new() },
            ),
            count: Some(dynamic),
        });
    }
    if alias_imports > 0 {
        gaps.push(ImpactGap {
            facet: "code.alias-imports".to_owned(),
            note: "stylesheet imports through path aliases or packages are not resolved, so style links through them are missing".to_owned(),
            count: Some(alias_imports),
        });
    }
    if !unresolved_imports.is_empty() {
        let shown = unresolved_imports
            .iter()
            .take(3)
            .map(|(file, spec, reason)| format!("{file} imports \"{spec}\" ({reason})"))
            .collect::<Vec<_>>()
            .join("; ");
        let more = unresolved_imports.len().saturating_sub(3);
        // First, so a truncated gap list still shows that consumers may be missing.
        gaps.insert(0, ImpactGap {
            facet: "code.unresolved-imports".to_owned(),
            note: format!(
                "import specifiers which name no indexed file, so components and styles reached through them are not followed and consumers may be missing: {shown}{}",
                if more > 0 { format!("; and {more} more") } else { String::new() }
            ),
            count: Some(unresolved_imports.len() as u32),
        });
    }
    if unresolved_renders > 0 {
        gaps.push(ImpactGap {
            facet: "code.unresolved-renders".to_owned(),
            note: "JSX tags whose component could not be bound to an indexed file".to_owned(),
            count: Some(unresolved_renders),
        });
    }
    let vue_files = scans.keys().filter(|file| file.ends_with(".vue")).count();
    gaps.push(if vue_files > 0 {
        ImpactGap {
            facet: "code.vue".to_owned(),
            note: "Vue templates are read for static class tokens, :class string literals and $style members; the <script>, component renders, slots and v-bind() in <style> are not".to_owned(),
            count: Some(vue_files as u32),
        }
    } else {
        ImpactGap {
            facet: "code.vue".to_owned(),
            note: "no Vue single-file components were indexed with style links, so their <style> blocks and template classes are not seen".to_owned(),
            count: None,
        }
    });
    (nodes, edges, gaps)
}

// ---------------------------------------------------------------------------
// Reading the selected index
// ---------------------------------------------------------------------------

struct IndexedFile {
    path: String,
    length: Option<u64>,
    source: Key,
}


fn indexed_files(cx: &ImpactContext<'_>) -> Result<Vec<IndexedFile>, String> {
    use backend_engine::builtin::{native_source_evidence_relation_budgeted, native_source_evidence_page_budgeted};
    let mut budget = backend_engine::builtin::MAX_NATIVE_EVIDENCE_BYTES;
    let mut out: Vec<IndexedFile> = Vec::new();
    // Native TypeScript evidence.
    if let Some(relation) = native_source_evidence_relation_budgeted(cx.snapshot, &mut budget).map_err(|e| format!("native evidence: {e}"))? {
        let mut after = None;
        loop {
            let page = native_source_evidence_page_budgeted(&relation, after.as_ref(), &mut budget)
                .map_err(|e| format!("native evidence page: {e}"))?;
            let entries = page.entries();
            if entries.is_empty() {
                break;
            }
            for (key, row) in entries {
                after = Some(*key);
                if row.project == cx.project && row.admits_image() && profile_for(&row.path).is_some() {
                    out.push(IndexedFile { path: row.path.clone(), length: Some(u64::from(row.byte_length)), source: row.source_digest });
                }
            }
            if out.len() > MAX_FILES * 4 {
                break;
            }
        }
    }
    // Structural lane records.
    let package = backend_engine::PackageKey::from_value(cx.package.as_str());
    if let Ok(sources) = super::super::read_package_sources(cx.snapshot, package) {
        for (_, record) in &sources.files {
            let Some(fields) = record.file_fields() else { continue };
            if fields.project != cx.project || profile_for(fields.path).is_none() {
                continue;
            }
            if let Some(identity) = fields.source_identity {
                out.push(IndexedFile { path: fields.path.to_owned(), length: None, source: *identity.as_ref() });
            }
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out.dedup_by(|a, b| a.path == b.path);
    Ok(out)
}

impl ImpactAdapter for CodeAdapter {
    fn domain(&self) -> ImpactDomain {
        ImpactDomain::Code
    }

    fn load(&self, cx: &ImpactContext<'_>) -> DomainLoad {
        use backend_version::{ContentId, SourceFactDomain};
        let empty = |state: ImpactDomainState| DomainLoad::empty(ImpactDomain::Code, state);
        let Some(root) = cx.project_root.clone() else {
            return empty(ImpactDomainState::Unavailable { reason: "the project directory is not known to the owner".to_owned() });
        };
        let listed = match indexed_files(cx) {
            Ok(files) => files,
            Err(error) => return empty(ImpactDomainState::Unavailable { reason: format!("the indexed sources could not be listed: {error}") }),
        };
        let mut scans: BTreeMap<String, FileScan> = BTreeMap::new();
        let mut witnesses = Vec::new();
        let mut gaps = Vec::new();
        let mut reasons = Vec::new();
        let (mut stale, mut unparsed, mut total) = (Vec::new(), Vec::new(), 0usize);
        // Links the index published are the graph; a file with no published
        // facts (older index, budget, no CSS worker) is scanned here instead.
        let persisted = match super::code_links::read_persisted(cx.snapshot, cx.project, cx.cancelled) {
            Ok(persisted) => persisted,
            Err(error) => {
                if (cx.cancelled)() {
                    return empty(ImpactDomainState::Unavailable { reason: "cancelled".to_owned() });
                }
                reasons.push(format!("the published code links could not be read ({error}); sources were scanned at query time"));
                None
            }
        };
        let mut from_index = 0u32;
        let mut at_query = 0u32;
        for (index, file) in listed.iter().enumerate() {
            if (cx.cancelled)() {
                return empty(ImpactDomainState::Unavailable { reason: "cancelled".to_owned() });
            }
            if let Some(host) = persisted.as_ref().and_then(|code| code.hosts.get(&file.path)) {
                let same_bytes = host.source == file.source && file.length.is_none_or(|length| length == host.byte_length);
                if same_bytes {
                    witnesses.push(FileWitness {
                        root: root.clone(),
                        path: file.path.clone(),
                        length: host.byte_length,
                        source: file.source,
                    });
                    match host.state {
                        backend_engine::builtin::CodeHostState::Scanned { dynamic_class_expressions } => {
                            scans.insert(
                                file.path.clone(),
                                super::code_links::scan_from_facts(&file.path, dynamic_class_expressions, &host.facts),
                            );
                            from_index += 1;
                            continue;
                        }
                        backend_engine::builtin::CodeHostState::Unparsed => {
                            unparsed.push(file.path.clone());
                            continue;
                        }
                        // The index could not scan it; try here.
                        backend_engine::builtin::CodeHostState::Unavailable(_) => {}
                    }
                }
            }
            at_query += 1;
            if index >= MAX_FILES || total >= MAX_TOTAL_BYTES {
                reasons.push(format!("only the first {index} of {} sources were scanned", listed.len()));
                gaps.push(ImpactGap {
                    facet: "code.scan-limit".to_owned(),
                    note: "the source scan budget was reached; later files were not read".to_owned(),
                    count: Some((listed.len() - index) as u32),
                });
                break;
            }
            let bytes = match super::super::ingest::read_stylo_selected_source(&root, &file.path, MAX_FILE_BYTES) {
                Ok(bytes) => bytes,
                Err(_) => {
                    stale.push(file.path.clone());
                    continue;
                }
            };
            total += bytes.len();
            let current = *ContentId::<SourceFactDomain>::from_canonical_bytes(&bytes).as_ref();
            witnesses.push(FileWitness {
                root: root.clone(),
                path: file.path.clone(),
                length: file.length.unwrap_or(bytes.len() as u64),
                source: file.source,
            });
            if current != file.source || file.length.is_some_and(|length| length != bytes.len() as u64) {
                stale.push(file.path.clone());
                continue;
            }
            let (Ok(text), Some(profile)) = (std::str::from_utf8(&bytes), profile_for(&file.path)) else {
                unparsed.push(file.path.clone());
                continue;
            };
            match with_analysis(profile, text, |module| scan_module(&file.path, text, &module)) {
                Ok(scan) => {
                    scans.insert(file.path.clone(), scan);
                }
                Err(_) => unparsed.push(file.path.clone()),
            }
        }
        // Hosts which only the index can list: single-file components.
        let mut extra_known: BTreeSet<String> = BTreeSet::new();
        if let Some(code) = &persisted {
            for (path, host) in &code.hosts {
                if !path.ends_with(".vue") || listed.iter().any(|file| &file.path == path) {
                    continue;
                }
                extra_known.insert(path.clone());
                witnesses.push(FileWitness {
                    root: root.clone(),
                    path: path.clone(),
                    length: host.byte_length,
                    source: host.source,
                });
                match host.state {
                    backend_engine::builtin::CodeHostState::Scanned { dynamic_class_expressions } => {
                        scans.insert(
                            path.clone(),
                            super::code_links::scan_from_facts(path, dynamic_class_expressions, &host.facts),
                        );
                        from_index += 1;
                    }
                    backend_engine::builtin::CodeHostState::Unparsed => unparsed.push(path.clone()),
                    backend_engine::builtin::CodeHostState::Unavailable(_) => {}
                }
            }
        }
        if !stale.is_empty() {
            reasons.push(format!("{} source files changed since indexing and were not scanned", stale.len()));
            gaps.push(ImpactGap {
                facet: "code.stale-files".to_owned(),
                note: format!("changed since indexing, not scanned: {}", stale.iter().take(4).cloned().collect::<Vec<_>>().join(", ")),
                count: Some(stale.len() as u32),
            });
        }
        if !unparsed.is_empty() {
            reasons.push(format!("{} source files do not parse", unparsed.len()));
            gaps.push(ImpactGap {
                facet: "code.unparsed-files".to_owned(),
                note: format!("syntax errors, not scanned: {}", unparsed.iter().take(4).cloned().collect::<Vec<_>>().join(", ")),
                count: Some(unparsed.len() as u32),
            });
        }
        if at_query > 0 {
            gaps.push(ImpactGap {
                facet: "code.links-unpublished".to_owned(),
                note: format!(
                    "the index published style links for {from_index} sources; {at_query} were scanned at query time because no published facts matched their indexed bytes"
                ),
                count: Some(at_query),
            });
        }
        if listed.is_empty() {
            gaps.push(ImpactGap {
                facet: "code.no-sources".to_owned(),
                note: "no TypeScript or JavaScript sources are indexed for this project, so code that uses these styles was not searched".to_owned(),
                count: None,
            });
        }
        // Imports are placed by the compiler's resolver over the indexed files
        // and the project's own tsconfig, not by path arithmetic.
        let indexed: BTreeSet<String> = listed.iter().map(|file| file.path.clone()).collect();
        let read_project = |relative: &str| match super::super::ingest::read_stylo_selected_source(&root, relative, MAX_FILE_BYTES) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(super::super::ingest::SelectedSourceReadError::Vanished) => Ok(None),
            Err(error) => Err(std::io::Error::other(format!("{error:?}"))),
        };
        let imports = CompilerImports::new(&indexed, &read_project);
        let (nodes, edges, mut graph_gaps) = build_graph_with(&scans, &imports);
        for (config, error) in imports.configuration_errors() {
            reasons.push(format!("{config} could not be used to resolve imports: {error}"));
            graph_gaps.insert(0, ImpactGap {
                facet: "code.tsconfig".to_owned(),
                note: format!("{config} could not be loaded ({error}); imports which need it are reported as unresolved"),
                count: None,
            });
        }
        if let Some(unresolved) = graph_gaps.iter().find(|gap| gap.facet == "code.unresolved-imports") {
            reasons.push(format!(
                "{} import specifiers name no indexed file, so some consumers may be missing",
                unresolved.count.unwrap_or(0)
            ));
        }
        gaps.extend(graph_gaps);
        let state = if reasons.is_empty() { ImpactDomainState::Analyzed } else { ImpactDomainState::Partial { reasons } };
        DomainLoad {
            nodes,
            edges,
            gaps,
            basis: ImpactDomainBasis { domain: ImpactDomain::Code, state, files: scans.len() as u32 },
            files: witnesses,
            known_paths: listed.iter().map(|file| file.path.clone()).chain(extra_known).collect(),
        }
    }

    fn seed_matches(&self, spec: &ImpactSeedSpec, node: &Node) -> bool {
        match spec.kind.as_str() {
            "component" => {
                node.kind == "component"
                    && node.name == spec.name
                    && spec.path.as_ref().is_none_or(|path| node.path.as_ref() == Some(path))
            }
            "file" => node.kind == "module" && node.path.as_deref() == Some(spec.name.trim_start_matches("./")),
            _ => false,
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    fn scan(path: &str, source: &str) -> FileScan {
        let profile = profile_for(path).expect("profile");
        with_analysis(profile, source, |module| scan_module(path, source, &module)).expect("parse")
    }

    #[test]
    fn relative_resolution_collapses_and_refuses_escapes() {
        assert_eq!(resolve_relative("src/ui/Card.tsx", "./Card.module.css").as_deref(), Some("src/ui/Card.module.css"));
        assert_eq!(resolve_relative("src/ui/Card.tsx", "../theme.css").as_deref(), Some("src/theme.css"));
        assert_eq!(resolve_relative("a.tsx", "../x.css"), None);
        assert_eq!(resolve_relative("src/a.tsx", "react"), None);
        assert_eq!(resolve_relative("src/a.tsx", "@/x.css"), None);
    }

    #[test]
    fn class_names_are_read_from_literals_templates_and_helpers() {
        let source = r#"
import clsx from "clsx";
export function Button({ big, kind }) {
  return <button className={clsx("btn", big && "btn-big", { active: big, "is-on": kind }, `x ${kind} y`)}>go</button>;
}
export const Plain = () => <div className="a b" class="c" />;
"#;
        let scan = scan("src/Button.tsx", source);
        let tokens: BTreeSet<&str> = scan.class_uses.iter().map(|usage| usage.token.as_str()).collect();
        for expected in ["btn", "btn-big", "active", "is-on", "x", "y", "a", "b", "c"] {
            assert!(tokens.contains(expected), "{expected} missing from {tokens:?}");
        }
        assert!(scan.class_uses.iter().any(|usage| usage.token == "btn-big" && usage.conditional));
        assert!(!scan.class_uses.iter().find(|usage| usage.token == "btn").expect("btn").conditional);
        assert_eq!(scan.dynamic_class_expressions, 1);
        assert_eq!(scan.class_uses.iter().find(|usage| usage.token == "a").expect("a").owner.as_deref(), Some("Plain"));
        assert_eq!(scan.class_uses.iter().find(|usage| usage.token == "btn").expect("btn").owner.as_deref(), Some("Button"));
    }

    #[test]
    fn partial_words_at_a_template_boundary_are_not_class_names() {
        let scan = scan("a.tsx", "export const A = ({s}) => <i className={`btn-${s} x`} />;");
    use crate::builtin::commands::impact_scan::ModuleClassUse;
        let tokens: BTreeSet<&str> = scan.class_uses.iter().map(|usage| usage.token.as_str()).collect();
        assert!(tokens.contains("x") && !tokens.contains("btn-"), "{tokens:?}");
    }

    #[test]
    fn module_members_need_a_stylesheet_import_binding() {
        let source = r#"
import styles from "./Card.module.css";
import other from "./other";
export function Card() { return <div className={styles.card + " " + styles["title"]}>{other.card}</div>; }
"#;
        let scan = scan("src/Card.tsx", source);
        let classes: BTreeSet<&str> = scan.module_class_uses.iter().map(|usage| usage.class.as_str()).collect();
        assert_eq!(classes, BTreeSet::from(["card", "title"]));
    }

    #[test]
    fn custom_properties_used_from_code_are_found() {
        let source = r#"
export function Theme({ el }) {
  el.style.setProperty("--brand", "red");
  const v = getComputedStyle(el).getPropertyValue("--accent");
  return <div style={{ "--gap": 4 }} />;
}
"#;
        let scan = scan("src/Theme.tsx", source);
        let uses: BTreeSet<(&str, bool)> = scan.property_uses.iter().map(|usage| (usage.name.as_str(), usage.writes)).collect();
        assert!(uses.contains(&("--brand", true)));
        assert!(uses.contains(&("--accent", false)));
        assert!(uses.contains(&("--gap", true)));
    }

    #[test]
    fn graph_links_css_to_components_and_components_to_their_renderers() {
        let mut scans = BTreeMap::new();
        scans.insert(
            "src/Button.tsx".to_owned(),
            scan(
                "src/Button.tsx",
                r#"import styles from "./Button.module.css";
import "./theme.css";
export function Button() { return <button className={styles.btn + " shared"} />; }"#,
            ),
        );
        scans.insert(
            "src/Page.tsx".to_owned(),
            scan(
                "src/Page.tsx",
                r#"import { Button } from "./Button";
export default function Page() { return <main><Button /></main>; }"#,
            ),
        );
        let (nodes, edges, _) = build_graph(&scans);
        let exact_module = edges
            .iter()
            .find(|edge| edge.relation == "code.uses-module-class")
            .expect("module class edge");
        assert_eq!(exact_module.strength, ImpactStrength::Exact);
        assert_eq!(exact_module.from, module_class_key("src/Button.module.css", "btn"));
        let shared = edges.iter().find(|edge| edge.relation == "code.uses-class").expect("class edge");
        assert_eq!(shared.strength, ImpactStrength::Candidate);
        assert_eq!(shared.from, global_class_key("shared"));
        let render = edges.iter().find(|edge| edge.relation == "code.renders").expect("render edge");
        assert_eq!(render.strength, ImpactStrength::Exact);
        assert_eq!(nodes[&render.from].name, "Button");
        assert_eq!(nodes[&render.to].name, "Page");
        assert!(edges.iter().any(|edge| edge.relation == "code.imports-stylesheet" && edge.from == stylesheet_key("src/theme.css")));
    }

    #[test]
    fn css_changes_reach_components_and_their_renderers_across_both_adapters() {
        use super::super::impact_css::{CssAdapter, CssModel, HostModel};
        use backend_library::impact::{
            ImpactRequest, ImpactSeed, ImpactSeedSpec, ImpactSeedState, ImpactStrength,
        };
        use backend_semantic::stylo_analysis::{
            StyloDependencyCoverage, StyloLocation, StyloRule, StyloRuleKind, StyloSelector,
        };
        let style_rule = |path: &[u32], line: u32| StyloRule {
            path: path.to_vec(),
            kind: StyloRuleKind::Style,
            native_kind: "Style".into(),
            condition: None,
            location: Some(StyloLocation { line, column: 1, approximate: false }),
        };
        let selector = |path: &[u32], canonical: &str| StyloSelector {
            rule_path: path.to_vec(),
            ordinal: 0,
            canonical: canonical.into(),
            specificity: 0,
            components: Vec::new(),
            component_coverage: StyloDependencyCoverage::Unknown,
        };
        let module = HostModel {
            key: [1; 32],
            path: "src/Button.module.css".into(),
            parsed: true,
            rules: vec![style_rule(&[0], 0)],
            selectors: vec![selector(&[0], ".btn")],
            ..HostModel::default()
        };
        let global = HostModel {
            key: [2; 32],
            path: "src/theme.css".into(),
            parsed: true,
            rules: vec![style_rule(&[0], 0)],
            selectors: vec![selector(&[0], ".shared")],
            ..HostModel::default()
        };
        let css = CssModel {
            state: ImpactDomainState::Analyzed,
            source_root: None,
            hosts: vec![module, global],
            inventory_incomplete: false,
        };
        let (css_load, _) = super::super::impact_css::build_graph(&css, &|_, _| None);
        let mut scans = BTreeMap::new();
        scans.insert(
            "src/Button.tsx".to_owned(),
            scan(
                "src/Button.tsx",
                r#"import styles from "./Button.module.css";
export function Button() { return <button className={styles.btn + " shared"} />; }"#,
            ),
        );
        scans.insert(
            "src/Page.tsx".to_owned(),
            scan(
                "src/Page.tsx",
                r#"import { Button } from "./Button";
export default function Page() { return <Button />; }"#,
            ),
        );
        let (nodes, edges, gaps) = build_graph(&scans);
        let code_load = DomainLoad {
            nodes,
            edges,
            gaps,
            basis: ImpactDomainBasis { domain: ImpactDomain::Code, state: ImpactDomainState::Analyzed, files: 2 },
            files: Vec::new(),
            known_paths: BTreeSet::new(),
        };
        let seed = |kind: &str, name: &str| {
            ImpactRequest::new(
                backend_library::PackageReference::parse("app".to_owned()).expect("package"),
                ImpactSeed::Named(ImpactSeedSpec { domain: ImpactDomain::Css, kind: kind.into(), name: name.into(), path: None }),
            )
        };
        let run = |request: &ImpactRequest| {
            let (css_load, _) = super::super::impact_css::build_graph(&css, &|_, _| None);
            let _ = &css_load;
            super::super::impact_framework::answer(
                [9; 32],
                [2; 32],
                &[7; 32],
                vec![
                    (ImpactDomain::Css, super::super::impact_css::build_graph(&css, &|_, _| None).0),
                    (ImpactDomain::Code, DomainLoad {
                        nodes: code_load.nodes.clone(),
                        edges: code_load.edges.clone(),
                        gaps: code_load.gaps.clone(),
                        basis: ImpactDomainBasis { domain: ImpactDomain::Code, state: ImpactDomainState::Analyzed, files: 2 },
                        files: Vec::new(),
                        known_paths: BTreeSet::new(),
                    }),
                ],
                &|spec, node| match node.domain {
                    ImpactDomain::Css => CssAdapter.seed_matches(spec, node),
                    _ => CodeAdapter.seed_matches(spec, node),
                },
                request,
                &|| false,
            )
            .expect("answer")
        };
        let _ = css_load;
        // The module class is an exact link to Button; Page renders Button.
        let report = run(&seed("selector", ".btn"));
        assert!(matches!(report.seed_state, ImpactSeedState::Resolved));
        let button = report.rows.iter().find(|row| row.node.name == "Button").expect("Button affected");
        assert_eq!(button.strength, ImpactStrength::Exact);
        assert_eq!(button.relation, "code.uses-module-class");
        let page = report.rows.iter().find(|row| row.node.name == "Page").expect("Page affected");
        assert_eq!(page.strength, ImpactStrength::Exact);
        assert_eq!(page.depth, 3);
        assert_eq!(page.via, report.rows.iter().find(|r| r.node.name == "Button").map(|r| r.n).expect("via"));
        // A global class used by literal is only a candidate, and so is everything after it.
        let shared = run(&seed("selector", ".shared"));
        let button = shared.rows.iter().find(|row| row.node.name == "Button").expect("Button affected");
        assert_eq!(button.strength, ImpactStrength::Candidate);
        assert_eq!(shared.rows.iter().find(|row| row.node.name == "Page").expect("Page").strength, ImpactStrength::Candidate);
        // Dependencies walk the same graph backwards: Page relies on Button which relies on the module rule.
        let mut back = seed("component", "Page");
        back.seed = ImpactSeed::Named(ImpactSeedSpec { domain: ImpactDomain::Code, kind: "component".into(), name: "Page".into(), path: None });
        back.direction = backend_library::impact::ImpactDirection::Dependencies;
        back.depth = 4;
        let deps = run(&back);
        assert!(deps.rows.iter().any(|row| row.node.name == "Button"), "{deps:?}");
        assert!(deps.rows.iter().any(|row| row.node.kind == "rule" && row.node.name == ".btn"), "{deps:?}");
    }
}

#[cfg(test)]
#[path = "impact_imports_tests.rs"]
mod imports_tests;

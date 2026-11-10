//! CSS adapter for the shared impact framework.
//!
//! Reads the selected native Stylo analysis, never a tree-sitter or regex
//! parse of the stylesheets, and turns it into the "affects" graph:
//!
//! * `custom-property --reads/derives--> rule | custom-property` (candidate:
//!   the cascade, not this adapter, decides which definition a use sees)
//! * `rule --defines--> custom-property` (candidate, same reason)
//! * `rule --styles--> class | id` (exact for a rule whose whole selector is one
//!   compound, candidate behind combinators or functional pseudo-classes)
//! * `at-rule | rule --conditions/nests--> rule` (exact structure)
//! * `rule --in-sheet--> file` for element-level rules which no class reaches
//!
//! Nodes which a seed also changes (the rules mentioning a class, the rules of
//! a stylesheet) are carried as companions of the seed node.
use super::impact_framework::{
    DomainLoad, Edge, FileWitness, ImpactAdapter, ImpactContext, Key, Node, node_key,
};
use super::stylo_query as native;
use backend_engine::builtin::{
    StyloAnalysisRecord, StyloInventory, StyloProjectedRow, StyloTerminal,
    stylo_attempt_key, stylo_selection_key,
};
use backend_library::impact::{
    ImpactDomain, ImpactDomainBasis, ImpactDomainState, ImpactGap, ImpactSeedSpec, ImpactStrength,
};
use backend_library::stylo_query::{StyloAttemptState, StyloSourceClaims, StyloSourceReference};
use backend_semantic::stylo_analysis::{
    StyloDeclaration, StyloDeclarationKind, StyloRule, StyloRuleKind, StyloSelector,
    StyloSourceGap, StyloSourceOccurrence, StyloSourceRole, StyloVariableDependency,
};
use std::collections::{BTreeMap, BTreeSet};

/// Largest relation scan one impact question may spend on CSS.
const MAX_SCAN_ROWS: usize = 400_000;
const SCAN_PAGE: usize = 64;

pub(super) struct CssAdapter;

/// Facts of one parsed stylesheet, as retained in the selected relation.
#[derive(Default)]
pub(super) struct HostModel {
    pub key: Key,
    pub path: String,
    pub source: Option<Key>,
    pub byte_length: Option<u64>,
    pub parsed: bool,
    pub unavailable: Option<String>,
    pub diagnostics: bool,
    pub source_gaps: Vec<StyloSourceGap>,
    pub rules: Vec<StyloRule>,
    pub selectors: Vec<StyloSelector>,
    pub declarations: Vec<StyloDeclaration>,
    pub dependencies: Vec<StyloVariableDependency>,
    /// Occurrence and its relation row key (the authored-site identity).
    pub occurrences: Vec<(StyloSourceOccurrence, Key)>,
}

/// The selected analysis of one project.
pub(super) struct CssModel {
    pub state: ImpactDomainState,
    pub source_root: Option<std::path::PathBuf>,
    pub hosts: Vec<HostModel>,
    pub inventory_incomplete: bool,
}

// ---------------------------------------------------------------------------
// Selector reading
// ---------------------------------------------------------------------------

/// What a canonical selector says about the elements it styles.
#[derive(Debug, Default, Eq, PartialEq)]
pub(super) struct SelectorShape {
    /// Class names anywhere in the selector, decoded.
    pub mentions_classes: BTreeSet<String>,
    pub mentions_ids: BTreeSet<String>,
    /// Classes and ids of the last compound: the styled element itself.
    pub subject_classes: BTreeSet<String>,
    pub subject_ids: BTreeSet<String>,
    /// One compound, no combinators and no functional pseudo-class: the rule
    /// applies to every element bearing the subject's classes.
    pub simple: bool,
}

fn unescape(raw: &str) -> String {
    let mut out = String::new();
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let mut hex = String::new();
        while hex.len() < 6 {
            match chars.peek() {
                Some(d) if d.is_ascii_hexdigit() => {
                    hex.push(*d);
                    chars.next();
                }
                _ => break,
            }
        }
        if hex.is_empty() {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            if matches!(chars.peek(), Some(' ' | '\t' | '\n')) {
                chars.next();
            }
            out.push(u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32).unwrap_or('\u{fffd}'));
        }
    }
    out
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_' || !c.is_ascii() || c == '\\'
}

/// Reads one canonical (Stylo-serialized) selector. Strings and attribute
/// selectors are skipped; parenthesized content counts as a mention but never
/// as a subject. Escapes are decoded so names compare with the native components.
pub(super) fn read_selector(canonical: &str) -> SelectorShape {
    let chars: Vec<char> = canonical.chars().collect();
    let mut shape = SelectorShape::default();
    let mut depth = 0usize;
    let mut compound_classes: Vec<String> = Vec::new();
    let mut compound_ids: Vec<String> = Vec::new();
    let mut compounds = 1usize;
    let mut functional = false;
    let mut pending_space = false;
    let mut i = 0usize;
    let mut quote: Option<char> = None;
    let mut bracket = 0usize;
    let mut fresh = true;
    while i < chars.len() {
        let c = chars[i];
        if let Some(q) = quote {
            if c == '\\' {
                i += 2;
                continue;
            }
            if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '[' => bracket += 1,
            ']' => bracket = bracket.saturating_sub(1),
            _ if bracket > 0 => {}
            '(' => {
                depth += 1;
                functional = true;
            }
            ')' => depth = depth.saturating_sub(1),
            '.' | '#' => {
                let start = i + 1;
                let mut end = start;
                while end < chars.len() && is_ident_char(chars[end]) {
                    if chars[end] == '\\' {
                        end += 1;
                    }
                    end += 1;
                }
                let end = end.min(chars.len());
                let name = unescape(&chars[start.min(end)..end].iter().collect::<String>());
                if !name.is_empty() {
                    if c == '.' {
                        shape.mentions_classes.insert(name.clone());
                        if depth == 0 {
                            if pending_space && !fresh {
                                compounds += 1;
                                compound_classes.clear();
                                compound_ids.clear();
                            }
                            pending_space = false;
                            compound_classes.push(name);
                        }
                    } else {
                        shape.mentions_ids.insert(name.clone());
                        if depth == 0 {
                            if pending_space && !fresh {
                                compounds += 1;
                                compound_classes.clear();
                                compound_ids.clear();
                            }
                            pending_space = false;
                            compound_ids.push(name);
                        }
                    }
                    fresh = false;
                }
                i = end;
                continue;
            }
            ' ' | '>' | '+' | '~' if depth == 0 => {
                if !fresh {
                    pending_space = true;
                }
            }
            ',' if depth == 0 => {
                // Selector lists arrive one selector at a time; a top-level
                // comma means the caller passed a list. Treat the last as subject.
                compounds += 1;
                compound_classes.clear();
                compound_ids.clear();
                pending_space = false;
                fresh = true;
            }
            _ if depth == 0 => {
                if pending_space && !fresh {
                    compounds += 1;
                    compound_classes.clear();
                    compound_ids.clear();
                }
                pending_space = false;
                fresh = false;
            }
            _ => {}
        }
        i += 1;
    }
    shape.subject_classes = compound_classes.into_iter().collect();
    shape.subject_ids = compound_ids.into_iter().collect();
    shape.simple = compounds == 1 && !functional;
    shape
}

/// Names written as `var(--x` in a serialized declaration value. Quoted text
/// and `url(...)` contents are skipped; fallbacks are scanned like any text.
pub(super) fn var_references(value: &str) -> Vec<String> {
    let chars: Vec<char> = value.chars().collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if c == '"' || c == '\'' {
            let q = c;
            i += 1;
            while i < chars.len() && chars[i] != q {
                if chars[i] == '\\' {
                    i += 1;
                }
                i += 1;
            }
            i += 1;
            continue;
        }
        let word_start = i == 0 || !is_ident_char(chars[i - 1]);
        let lowered: String = chars[i..chars.len().min(i + 4)].iter().collect::<String>().to_ascii_lowercase();
        if word_start && lowered == "url(" {
            i += 4;
            while i < chars.len() && chars[i] != ')' {
                if chars[i] == '\\' {
                    i += 1;
                }
                i += 1;
            }
            i += 1;
            continue;
        }
        if word_start && lowered == "var(" {
            i += 4;
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
            if chars.get(i) == Some(&'-') && chars.get(i + 1) == Some(&'-') {
                let start = i;
                i += 2;
                while i < chars.len() && is_ident_char(chars[i]) {
                    if chars[i] == '\\' {
                        i += 1;
                    }
                    i += 1;
                }
                let end = i.min(chars.len());
                out.push(unescape(&chars[start..end].iter().collect::<String>()));
            }
            continue;
        }
        i += 1;
    }
    out
}

// ---------------------------------------------------------------------------
// Graph
// ---------------------------------------------------------------------------

fn is_module(path: &str) -> bool {
    path.ends_with(".module.css")
}
fn class_key(host_path: &str, name: &str) -> Key {
    node_key(ImpactDomain::Css, "class", if is_module(host_path) { host_path } else { "" }, name)
}
fn id_key(name: &str) -> Key {
    node_key(ImpactDomain::Css, "id", "", name)
}
fn property_key(name: &str) -> Key {
    node_key(ImpactDomain::Css, "custom-property", "", name)
}
fn rule_key(path: &str, rule_path: &[u32]) -> Key {
    node_key(ImpactDomain::Css, "rule", path, &format!("{rule_path:?}"))
}
fn file_key(path: &str) -> Key {
    node_key(ImpactDomain::Css, "file", "", path)
}

/// Public so the code adapter links to exactly these keys.
pub(super) fn module_class_key(module_path: &str, name: &str) -> Key {
    class_key(module_path, name)
}
/// Key of a global (non-module) class.
pub(super) fn global_class_key(name: &str) -> Key {
    class_key("", name)
}
/// Key of an id selector name.
pub(super) fn id_node_key(name: &str) -> Key {
    id_key(name)
}
/// Key of a stylesheet file node.
pub(super) fn stylesheet_key(path: &str) -> Key {
    file_key(path)
}
/// Key of a custom property name including the dashes.
pub(super) fn custom_property_key(name: &str) -> Key {
    property_key(name)
}

fn normalize_kind(native_kind: &str) -> String {
    native_kind.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase()
}

fn kebab(native_kind: &str) -> String {
    let mut out = String::new();
    for (i, c) in native_kind.chars().enumerate() {
        if c.is_ascii_uppercase() && i > 0 {
            out.push('-');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

struct Builder<'a> {
    nodes: BTreeMap<Key, Node>,
    edges: Vec<Edge>,
    class_mentions: BTreeMap<Key, BTreeSet<Key>>,
    issue: &'a dyn Fn(&Key, &Key) -> Option<String>,
}
impl Builder<'_> {
    fn ensure(&mut self, key: Key, make: impl FnOnce() -> Node) -> &mut Node {
        self.nodes.entry(key).or_insert_with(make)
    }
    fn edge(&mut self, from: Key, to: Key, strength: ImpactStrength, relation: &'static str, why: String) {
        if from != to {
            self.edges.push(Edge { from, to, strength, relation, why });
        }
    }
}

fn line_of(rule: &StyloRule) -> Option<u32> {
    rule.location.map(|location| location.line + 1)
}

fn rule_label(host: &HostModel, rule: &StyloRule) -> String {
    match rule.kind {
        StyloRuleKind::Style => {
            let names: Vec<&str> = host
                .selectors
                .iter()
                .filter(|selector| selector.rule_path == rule.path)
                .map(|selector| selector.canonical.as_str())
                .collect();
            if names.is_empty() { "(rule)".to_owned() } else { names.join(", ") }
        }
        StyloRuleKind::Media => format!("@media {}", rule.condition.as_deref().unwrap_or("")).trim().to_owned(),
        StyloRuleKind::Other => match rule.condition.as_deref() {
            Some(prelude) if !prelude.is_empty() => format!("@{} {prelude}", kebab(&rule.native_kind)),
            _ => format!("@{}", kebab(&rule.native_kind)),
        },
    }
}

/// Build the graph for the parsed hosts of one project. `issue` turns a
/// (host key, site key) pair into an executable `stylo-read-site` argument.
pub(super) fn build_graph(model: &CssModel, issue: &dyn Fn(&Key, &Key) -> Option<String>) -> (DomainLoad, usize) {
    let mut b = Builder { nodes: BTreeMap::new(), edges: Vec::new(), class_mentions: BTreeMap::new(), issue };
    let issue = b.issue;
    let mut files = Vec::new();
    let mut unparsed: Vec<String> = Vec::new();
    let mut diagnostics = 0u32;
    let mut observer_skipped = 0u32;
    let mut unprojected: BTreeMap<String, u32> = BTreeMap::new();
    let mut unknown_values = 0u32;
    let mut known_paths: BTreeSet<String> = BTreeSet::new();
    let mut parsed_files = 0u32;
    let mut var_uses = 0u32;
    for host in &model.hosts {
        known_paths.insert(host.path.clone());
        if !host.parsed {
            unparsed.push(match &host.unavailable {
                Some(why) => format!("{} ({why})", host.path),
                None => host.path.clone(),
            });
            continue;
        }
        parsed_files += 1;
        if host.diagnostics {
            diagnostics += 1;
        }
        if host.source_gaps.iter().any(|gap| matches!(gap, StyloSourceGap::ObserverSkipped)) {
            observer_skipped += 1;
        }
        if let (Some(source), Some(length), Some(root)) = (host.source, host.byte_length, model.source_root.as_ref()) {
            files.push(FileWitness { root: root.clone(), path: host.path.clone(), length, source });
        }
        let fkey = file_key(&host.path);
        b.ensure(fkey, || {
            let mut node = Node::new(ImpactDomain::Css, "file", "", &host.path);
            node.path = Some(host.path.clone());
            node
        });
        // Nodes for rules.
        let by_path: BTreeMap<&[u32], &StyloRule> = host.rules.iter().map(|rule| (rule.path.as_slice(), rule)).collect();
        let first_site = |rule_path: &[u32], role: StyloSourceRole, name: Option<&str>| -> Option<String> {
            host.occurrences
                .iter()
                .find(|(site, _)| site.rule_path == rule_path && site.role == role && name.is_none_or(|n| site.decoded == n))
                .and_then(|(_, site_key)| issue(&host.key, site_key))
                .map(|reference| format!("stylo-read-site {reference}"))
        };
        for rule in &host.rules {
            let rkey = rule_key(&host.path, &rule.path);
            let label = rule_label(host, rule);
            let (kind, scope) = match rule.kind {
                StyloRuleKind::Style => ("rule", String::new()),
                StyloRuleKind::Media => ("at-rule", "media".to_owned()),
                StyloRuleKind::Other => {
                    if rule.condition.is_none() {
                        *unprojected.entry(kebab(&rule.native_kind)).or_default() += 1;
                    }
                    ("at-rule", normalize_kind(&rule.native_kind))
                }
            };
            let read = first_site(&rule.path, StyloSourceRole::ClassName, None)
                .or_else(|| first_site(&rule.path, StyloSourceRole::CustomPropertyDeclarationName, None));
            let mut node = Node::new(ImpactDomain::Css, kind, &scope, &label);
            node.path = Some(host.path.clone());
            node.line = line_of(rule);
            node.read = read;
            b.nodes.insert(rkey, node);
            // Structure: a parent at-rule or style rule governs its children.
            if rule.path.len() > 1 {
                let parent_path = &rule.path[..rule.path.len() - 1];
                if let Some(parent) = by_path.get(parent_path) {
                    let relation = if parent.kind == StyloRuleKind::Style { "css.nests-rule" } else { "css.conditions-rule" };
                    b.edge(
                        rule_key(&host.path, parent_path),
                        rkey,
                        ImpactStrength::Exact,
                        relation,
                        format!("nested inside {}", rule_label(host, parent)),
                    );
                }
            }
        }
        // Selectors: classes, ids, element-level rules.
        let mut class_subject: BTreeMap<(Vec<u32>, String), bool> = BTreeMap::new();
        let mut id_subject: BTreeMap<(Vec<u32>, String), bool> = BTreeMap::new();
        let mut element_level: BTreeSet<Vec<u32>> = BTreeSet::new();
        for selector in &host.selectors {
            let shape = read_selector(&selector.canonical);
            let rkey = rule_key(&host.path, &selector.rule_path);
            for name in &shape.mentions_classes {
                let ckey = class_key(&host.path, name);
                b.class_mentions.entry(ckey).or_default().insert(rkey);
                let line = by_path.get(selector.rule_path.as_slice()).and_then(|rule| line_of(rule));
                let read = host
                    .occurrences
                    .iter()
                    .find(|(site, _)| site.role == StyloSourceRole::ClassName && site.decoded == *name)
                    .and_then(|(_, site_key)| issue(&host.key, site_key))
                    .map(|reference| format!("stylo-read-site {reference}"));
                let host_path = host.path.clone();
                let module = is_module(&host_path);
                let class_node = b.ensure(ckey, || {
                    let mut node = Node::new(ImpactDomain::Css, "class", if module { &host_path } else { "" }, name);
                    node.path = Some(host_path.clone());
                    node.line = line;
                    node.read = read.clone();
                    if module {
                        node.within = Some(format!("CSS module {host_path}"));
                    }
                    node
                });
                if class_node.read.is_none() {
                    class_node.read = read;
                }
            }
            for name in &shape.mentions_ids {
                let ikey = id_key(name);
                b.class_mentions.entry(ikey).or_default().insert(rkey);
                let host_path = host.path.clone();
                let line = by_path.get(selector.rule_path.as_slice()).and_then(|rule| line_of(rule));
                b.ensure(ikey, || {
                    let mut node = Node::new(ImpactDomain::Css, "id", "", name);
                    node.path = Some(host_path);
                    node.line = line;
                    node
                });
            }
            for name in &shape.subject_classes {
                let entry = class_subject.entry((selector.rule_path.clone(), name.clone())).or_insert(false);
                *entry |= shape.simple;
            }
            for name in &shape.subject_ids {
                let entry = id_subject.entry((selector.rule_path.clone(), name.clone())).or_insert(false);
                *entry |= shape.simple;
            }
            if shape.subject_classes.is_empty() && shape.subject_ids.is_empty() {
                element_level.insert(selector.rule_path.clone());
            }
        }
        for ((rule_path, name), simple) in class_subject {
            let rkey = rule_key(&host.path, &rule_path);
            let strength = if simple { ImpactStrength::Exact } else { ImpactStrength::Candidate };
            let why = if simple {
                format!("selector styles every element with .{name}")
            } else {
                format!(".{name} is the styled element only where the rest of the selector also matches")
            };
            b.edge(rkey, class_key(&host.path, &name), strength, "css.styles-class", why);
        }
        for ((rule_path, name), simple) in id_subject {
            let rkey = rule_key(&host.path, &rule_path);
            let strength = if simple { ImpactStrength::Exact } else { ImpactStrength::Candidate };
            b.edge(rkey, id_key(&name), strength, "css.styles-id", format!("selector styles #{name}"));
        }
        for rule_path in element_level {
            // Only rules which actually declare something reach elements.
            b.edge(
                rule_key(&host.path, &rule_path),
                fkey,
                ImpactStrength::Candidate,
                "css.in-sheet",
                "element-level rule applies wherever this stylesheet is loaded".to_owned(),
            );
        }
        // Declarations: custom properties and var() uses.
        let mut reads: BTreeMap<(Key, Key, String), BTreeSet<String>> = BTreeMap::new();
        let structured: BTreeSet<(Vec<u32>, u32)> = host
            .dependencies
            .iter()
            .filter(|dependency| dependency.kind == "var")
            .map(|dependency| (dependency.rule_path.clone(), dependency.declaration_ordinal))
            .collect();
        for declaration in &host.declarations {
            let rkey = rule_key(&host.path, &declaration.rule_path);
            let line = by_path.get(declaration.rule_path.as_slice()).and_then(|rule| line_of(rule));
            let mut used: BTreeSet<String> = host
                .dependencies
                .iter()
                .filter(|dependency| {
                    dependency.kind == "var"
                        && dependency.rule_path == declaration.rule_path
                        && dependency.declaration_ordinal == declaration.ordinal
                })
                .map(|dependency| format!("--{}", dependency.name))
                .collect();
            if declaration.value_available
                && !structured.contains(&(declaration.rule_path.clone(), declaration.ordinal))
            {
                used.extend(var_references(&declaration.value));
            } else if !declaration.value_available && declaration.kind == StyloDeclarationKind::WithVariables {
                unknown_values += 1;
            }
            var_uses += used.len() as u32;
            if declaration.kind == StyloDeclarationKind::Custom {
                let pkey = property_key(&declaration.property);
                let host_path = host.path.clone();
                let read = first_site(&declaration.rule_path, StyloSourceRole::CustomPropertyDeclarationName, Some(&declaration.property));
                let node = b.ensure(pkey, || {
                    let mut node = Node::new(ImpactDomain::Css, "custom-property", "", &declaration.property);
                    node.path = Some(host_path);
                    node.line = line;
                    node
                });
                if node.path.is_none() {
                    node.path = Some(host.path.clone());
                    node.line = line;
                }
                if node.read.is_none() {
                    node.read = read.clone();
                }
                b.edge(
                    rkey,
                    pkey,
                    ImpactStrength::Candidate,
                    "css.defines-property",
                    format!("{} defined here; which definition applies depends on the cascade", declaration.property),
                );
                for name in &used {
                    let source = property_key(name);
                    let host_path = host.path.clone();
                    b.ensure(source, || {
                        let mut node = Node::new(ImpactDomain::Css, "custom-property", "", name);
                        node.path = None;
                        node
                    });
                    let _ = host_path;
                    b.edge(
                        source,
                        pkey,
                        ImpactStrength::Candidate,
                        "css.derives-property",
                        format!("{}: var({name})", declaration.property),
                    );
                }
            } else {
                for name in &used {
                    let source = property_key(name);
                    b.ensure(source, || {
                        let mut node = Node::new(ImpactDomain::Css, "custom-property", "", name);
                        node.path = None;
                        node
                    });
                    reads.entry((source, rkey, name.clone())).or_default().insert(declaration.property.clone());
                }
            }
        }
        // One edge per (property, rule), naming the declarations which read it.
        for ((source, rkey, name), properties) in std::mem::take(&mut reads) {
            let shown: Vec<&str> = properties.iter().map(String::as_str).take(3).collect();
            let more = if properties.len() > 3 { ", ..." } else { "" };
            b.edge(
                source,
                rkey,
                ImpactStrength::Candidate,
                "css.reads-property",
                format!("{}{more}: var({name})", shown.join(", ")),
            );
        }
    }
    // Companions: a seed on a class also changes every rule mentioning it; a
    // seed on a stylesheet changes every rule in it.
    let mut nodes = b.nodes;
    for (key, rules) in b.class_mentions {
        if let Some(node) = nodes.get_mut(&key) {
            node.companions = rules.into_iter().collect();
        }
    }
    let mut by_file: BTreeMap<String, Vec<Key>> = BTreeMap::new();
    for (key, node) in &nodes {
        if matches!(node.kind, "rule" | "at-rule") {
            if let Some(path) = &node.path {
                by_file.entry(path.clone()).or_default().push(*key);
            }
        }
    }
    for (path, rules) in by_file {
        if let Some(node) = nodes.get_mut(&file_key(&path)) {
            node.companions = rules;
        }
    }
    // Definition sites for properties which are only used.
    let mut gaps = vec![
        ImpactGap {
            facet: "css.cascade".to_owned(),
            note: "cascade order, specificity, layers, inheritance and media state are not evaluated; a property or rule edge says a name is used, not that this declaration wins".to_owned(),
            count: None,
        },
        ImpactGap {
            facet: "css.dom".to_owned(),
            note: "selectors are not matched against elements; links through combinators, :is/:not/:has or nesting are possible, not proven".to_owned(),
            count: None,
        },
        ImpactGap {
            facet: "css.var-sites".to_owned(),
            note: "var() uses are read from Stylo's serialized declaration values, so they carry the rule's line, not a byte site; shorthands expanded by variables are not visible".to_owned(),
            count: (unknown_values > 0).then_some(unknown_values),
        },
        ImpactGap {
            facet: "css.site-coverage".to_owned(),
            note: "exact byte sites exist for class names and custom-property declaration names of top-level rules; at-rules, ids, nested rules and var() uses carry the rule's native line only".to_owned(),
            count: None,
        },
        ImpactGap {
            facet: "css.imports".to_owned(),
            note: "@import is not followed, so stylesheet-to-stylesheet impact is not traversed".to_owned(),
            count: None,
        },
    ];
    if !unparsed.is_empty() {
        unparsed.sort();
        let shown: Vec<String> = unparsed.iter().take(4).cloned().collect();
        gaps.push(ImpactGap {
            facet: "css.unparsed-files".to_owned(),
            note: format!("stylesheets without analysis: {}", shown.join(", ")),
            count: Some(unparsed.len() as u32),
        });
    }
    if diagnostics > 0 {
        gaps.push(ImpactGap {
            facet: "css.parse-diagnostics".to_owned(),
            note: "the parser rejected some syntax in these stylesheets; rejected rules are absent".to_owned(),
            count: Some(diagnostics),
        });
    }
    if observer_skipped > 0 {
        gaps.push(ImpactGap {
            facet: "css.authored-sites".to_owned(),
            note: "some authored source sites were skipped by the observer".to_owned(),
            count: Some(observer_skipped),
        });
    }
    if !unprojected.is_empty() {
        let kinds: Vec<String> = unprojected.iter().map(|(kind, count)| format!("@{kind} x{count}")).collect();
        gaps.push(ImpactGap {
            facet: "css.at-rule-conditions".to_owned(),
            note: format!("these at-rules are present but their preludes are not projected: {}", kinds.join(", ")),
            count: Some(unprojected.values().sum()),
        });
    }
    if model.inventory_incomplete {
        gaps.push(ImpactGap {
            facet: "css.inventory".to_owned(),
            note: "the stylesheet inventory is incomplete (a limit or policy gap stopped discovery)".to_owned(),
            count: None,
        });
    }
    let _ = var_uses;
    let state = model.state.clone();
    (
        DomainLoad {
            nodes,
            edges: b.edges,
            gaps,
            basis: ImpactDomainBasis { domain: ImpactDomain::Css, state, files: parsed_files },
            files,
            known_paths,
        },
        parsed_files as usize,
    )
}

// ---------------------------------------------------------------------------
// Reading the selected relation
// ---------------------------------------------------------------------------

fn failure_text(reason: backend_library::stylo_query::StyloFailure) -> &'static str {
    use backend_library::stylo_query::StyloFailure as F;
    match reason {
        F::ReadUnavailable => "stylesheet bytes could not be read",
        F::SelectionUnavailable => "project selection evidence was unavailable",
        F::InvalidUtf8 => "a stylesheet is not valid UTF-8",
        F::Limit => "a work limit was reached",
        F::ParserUnavailable => "no admitted CSS worker is available on this build or installation",
        F::UnsupportedHardLimit => "the platform cannot enforce the worker's hard limits",
        F::Deadline => "the CSS worker ran out of time",
        F::WorkerFailure => "the CSS worker failed",
        F::PreferencesChanged => "parser preferences changed during analysis",
    }
}

fn unavailable_text(reason: backend_engine::builtin::StyloUnavailable) -> String {
    use backend_engine::builtin::StyloUnavailable as U;
    match reason {
        U::ReadUnavailable => "bytes unreadable",
        U::SelectionUnavailable => "selection unavailable",
        U::InvalidUtf8 => "not valid UTF-8",
        U::Limit => "work limit",
        U::ParserUnavailable => "no CSS worker",
        U::UnsupportedHardLimit => "hard limit unsupported",
        U::Deadline => "worker deadline",
        U::WorkerFailure => "worker failed",
        U::PreferencesChanged => "preferences changed",
    }
    .to_owned()
}

/// Read the whole selected CSS relation of the project, bounded. A missing or
/// unusable analysis comes back as a state with no hosts.
pub(super) fn read_model(cx: &ImpactContext<'_>) -> Result<(CssModel, [u8; 32]), String> {
    let snapshot = cx.snapshot;
    let project = cx.project;
    let empty = |state: ImpactDomainState| CssModel { state, source_root: None, hosts: Vec::new(), inventory_incomplete: false };
    let relation = match native::selected_native_relation(snapshot) {
        Ok(Some(relation)) => relation,
        Ok(None) => {
            return Ok((
                empty(ImpactDomainState::Unavailable {
                    reason: "no CSS analysis is selected for this project: either it has no stylesheets, none were indexed, or the CSS worker was not available when it was indexed".to_owned(),
                }),
                [0; 32],
            ));
        }
        Err(error) => return Err(format!("CSS relation: {}", error.0)),
    };
    let native_root = *relation.root().as_bytes();
    let selection = relation.lookup(&stylo_selection_key(project)).map_err(|e| format!("{e:?}"))?;
    let attempt = relation.lookup(&stylo_attempt_key(project)).map_err(|e| format!("{e:?}"))?;
    let (attempt_state, _) = native::classify_attempt(project, selection.as_ref(), attempt.clone())
        .map_err(|e| e.0)?;
    let Some(StyloAnalysisRecord::Selection { source_root, inventory, policy_complete, .. }) = selection else {
        let reason = match attempt_state {
            StyloAttemptState::Unavailable { reason } => format!("the CSS analysis attempt failed: {}", failure_text(reason)),
            StyloAttemptState::ProjectNotSelected => "the project is not part of the selected workspace".to_owned(),
            _ => "no CSS analysis is selected for this project (no stylesheet was analyzed)".to_owned(),
        };
        return Ok((empty(ImpactDomainState::Unavailable { reason }), native_root));
    };
    let mut model = CssModel {
        state: ImpactDomainState::Analyzed,
        source_root: Some(std::path::PathBuf::from(&source_root)),
        hosts: Vec::new(),
        inventory_incomplete: !policy_complete || !matches!(inventory, StyloInventory::Complete),
    };
    let mut reasons = Vec::new();
    match attempt_state {
        StyloAttemptState::LatestObserved => {}
        StyloAttemptState::RetainedAfterUnavailable { reason } => reasons.push(format!(
            "the latest CSS analysis attempt failed ({}); these are facts from an earlier index",
            failure_text(reason)
        )),
        StyloAttemptState::UnknownLegacy => reasons.push("the CSS analysis is from an older index with no recorded attempt".to_owned()),
        StyloAttemptState::Unavailable { reason } => reasons.push(failure_text(reason).to_owned()),
        _ => {}
    }
    if model.inventory_incomplete {
        reasons.push("the stylesheet inventory is incomplete".to_owned());
    }
    // One pass over the relation in key order. Hosts, then their facts.
    let mut hosts: BTreeMap<Key, HostModel> = BTreeMap::new();
    let mut after: Option<[u8; 32]> = None;
    let mut scanned = 0usize;
    loop {
        if (cx.cancelled)() {
            return Err("impact cancelled".to_owned());
        }
        let page = relation.page(after.as_ref(), SCAN_PAGE).map_err(|e| format!("{e:?}"))?;
        let entries = page.entries();
        if entries.is_empty() {
            break;
        }
        for (key, row) in entries {
            scanned += 1;
            if scanned > MAX_SCAN_ROWS {
                reasons.push(format!("the CSS relation exceeds the {MAX_SCAN_ROWS}-row impact scan budget; later rows were not read"));
                after = None;
                break;
            }
            match row {
                StyloAnalysisRecord::Host { project: owner, path, source, byte_length, terminal, source_gaps, .. } if *owner == project => {
                    let host = hosts.entry(*key).or_default();
                    host.key = *key;
                    host.path = path.clone();
                    host.source = *source;
                    host.byte_length = *byte_length;
                    host.source_gaps = source_gaps.clone();
                    match terminal {
                        StyloTerminal::Parsed(_) => host.parsed = true,
                        StyloTerminal::Unavailable(reason) => host.unavailable = Some(unavailable_text(*reason)),
                    }
                }
                StyloAnalysisRecord::Fact { host, fact, .. } => {
                    let entry = hosts.entry(*host).or_default();
                    entry.key = *host;
                    match fact {
                        StyloProjectedRow::Rule(rule) => entry.rules.push(rule.clone()),
                        StyloProjectedRow::Selector(selector) => entry.selectors.push(selector.clone()),
                        StyloProjectedRow::RetainedDeclaration(declaration) => entry.declarations.push(declaration.clone()),
                        StyloProjectedRow::Dependency(dependency) => entry.dependencies.push(dependency.clone()),
                        StyloProjectedRow::SourceOccurrence(site) => entry.occurrences.push((site.clone(), *key)),
                        StyloProjectedRow::Diagnostic(_) => entry.diagnostics = true,
                    }
                }
                _ => {}
            }
            after = Some(*key);
        }
        if after.is_none() {
            break;
        }
    }
    // Facts whose Host row never appeared belong to another project or a torn read.
    model.hosts = hosts
        .into_values()
        .filter(|host| !host.path.is_empty())
        .collect();
    model.hosts.sort_by(|a, b| a.path.cmp(&b.path));
    if !reasons.is_empty() {
        model.state = ImpactDomainState::Partial { reasons };
    }
    Ok((model, native_root))
}

fn normalize_selector(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_lowercase()
}

impl ImpactAdapter for CssAdapter {
    fn domain(&self) -> ImpactDomain {
        ImpactDomain::Css
    }

    fn load(&self, cx: &ImpactContext<'_>) -> DomainLoad {
        let (model, native_root) = match read_model(cx) {
            Ok(read) => read,
            Err(error) => {
                return DomainLoad::empty(
                    ImpactDomain::Css,
                    ImpactDomainState::Unavailable { reason: format!("the CSS analysis could not be read: {error}") },
                );
            }
        };
        let manifest_root = *cx.snapshot.root().as_bytes();
        let issue = |host: &Key, site: &Key| -> Option<String> {
            StyloSourceReference::issue(
                StyloSourceClaims { manifest_root, native_root, project: cx.project, host: *host, site: *site },
                cx.secret,
            )
            .ok()
            .map(|reference| reference.as_str().to_owned())
        };
        build_graph(&model, &issue).0
    }

    fn seed_matches(&self, spec: &ImpactSeedSpec, node: &Node) -> bool {
        let path_ok = |node: &Node| match (&spec.path, node.kind) {
            (None, _) => true,
            (Some(path), "class" | "id") => node.scope.is_empty() || &node.scope == path,
            (Some(path), _) => node.path.as_deref() == Some(path.as_str()),
        };
        let name = spec.name.trim();
        match spec.kind.as_str() {
            "class" => node.kind == "class" && node.name == name.trim_start_matches('.') && path_ok(node),
            "id" => node.kind == "id" && node.name == name.trim_start_matches('#'),
            "custom-property" => {
                let wanted = if name.starts_with("--") { name.to_owned() } else { format!("--{name}") };
                node.kind == "custom-property" && node.name == wanted
            }
            "selector" => {
                node.kind == "rule"
                    && path_ok(node)
                    && {
                        let wanted = normalize_selector(name);
                        normalize_selector(&node.name) == wanted
                            || node.name.split(", ").any(|part| normalize_selector(part) == wanted)
                    }
            }
            "at-rule" => {
                let wanted = normalize_kind(name.trim_start_matches('@'));
                node.kind == "at-rule" && path_ok(node) && !wanted.is_empty() && node.scope.starts_with(&wanted)
            }
            "file" => node.kind == "file" && node.path.as_deref() == Some(name.trim_start_matches("./")),
            _ => false,
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use backend_library::impact::{
        ImpactDirection, ImpactRequest, ImpactSeed, ImpactSeedState, ImpactStrengthFilter, ImpactTerminal,
    };
    use backend_semantic::stylo_analysis::StyloLocation;

    fn rule(path: &[u32], kind: StyloRuleKind, native: &str, condition: Option<&str>, line: u32) -> StyloRule {
        StyloRule {
            path: path.to_vec(),
            kind,
            native_kind: native.to_owned(),
            condition: condition.map(str::to_owned),
            location: Some(StyloLocation { line, column: 1, approximate: false }),
        }
    }
    fn selector(path: &[u32], ordinal: u32, canonical: &str) -> StyloSelector {
        StyloSelector {
            rule_path: path.to_vec(),
            ordinal,
            canonical: canonical.to_owned(),
            specificity: 0,
            components: Vec::new(),
            component_coverage: backend_semantic::stylo_analysis::StyloDependencyCoverage::Unknown,
        }
    }
    fn decl(path: &[u32], ordinal: u32, kind: StyloDeclarationKind, property: &str, value: &str) -> StyloDeclaration {
        StyloDeclaration {
            rule_path: path.to_vec(),
            ordinal,
            kind,
            property: property.to_owned(),
            value: value.to_owned(),
            value_available: true,
            important: false,
            dependency_coverage: backend_semantic::stylo_analysis::StyloDependencyCoverage::Unknown,
        }
    }

    fn model() -> CssModel {
        // :root { --brand: #36f; --accent: var(--brand) }
        // .btn { color: var(--accent) }
        // .card .title { color: var(--brand) }
        // @media (min-width: 600px) { .btn { padding: 4px } }
        // h1 { margin: 0 }
        let mut host = HostModel { key: [1; 32], path: "src/app.css".into(), parsed: true, ..HostModel::default() };
        host.rules = vec![
            rule(&[0], StyloRuleKind::Style, "Style", None, 0),
            rule(&[1], StyloRuleKind::Style, "Style", None, 3),
            rule(&[2], StyloRuleKind::Style, "Style", None, 6),
            rule(&[3], StyloRuleKind::Media, "Media", Some("(min-width: 600px)"), 9),
            rule(&[3, 0], StyloRuleKind::Style, "Style", None, 10),
            rule(&[4], StyloRuleKind::Style, "Style", None, 13),
        ];
        host.selectors = vec![
            selector(&[0], 0, ":root"),
            selector(&[1], 0, ".btn"),
            selector(&[2], 0, ".card .title"),
            selector(&[3, 0], 0, ".btn"),
            selector(&[4], 0, "h1"),
        ];
        host.declarations = vec![
            decl(&[0], 0, StyloDeclarationKind::Custom, "--brand", "#36f"),
            decl(&[0], 1, StyloDeclarationKind::Custom, "--accent", "var(--brand)"),
            decl(&[1], 0, StyloDeclarationKind::WithVariables, "color", "var(--accent)"),
            decl(&[2], 0, StyloDeclarationKind::WithVariables, "color", "var( --brand , red)"),
            decl(&[3, 0], 0, StyloDeclarationKind::TypedLonghand, "padding-top", "4px"),
            decl(&[4], 0, StyloDeclarationKind::TypedLonghand, "margin-top", "0px"),
        ];
        CssModel {
            state: ImpactDomainState::Analyzed,
            source_root: None,
            hosts: vec![host],
            inventory_incomplete: false,
        }
    }

    fn ask(model: &CssModel, kind: &str, name: &str, direction: ImpactDirection, depth: u8) -> backend_library::impact::ImpactReport {
        let (load, _) = build_graph(model, &|_, _| None);
        let mut request = ImpactRequest::new(
            backend_library::PackageReference::parse("app".to_owned()).expect("package"),
            ImpactSeed::Named(ImpactSeedSpec {
                domain: ImpactDomain::Css,
                kind: kind.to_owned(),
                name: name.to_owned(),
                path: None,
            }),
        );
        request.direction = direction;
        request.depth = depth;
        super::super::impact_framework::answer(
            [9; 32],
            [2; 32],
            &[7; 32],
            vec![(ImpactDomain::Css, load)],
            &|spec, node| CssAdapter.seed_matches(spec, node),
            &request,
            &|| false,
        )
        .expect("answer")
    }

    #[test]
    fn selector_reading_separates_subject_from_context() {
        let shape = read_selector(".card .title:hover");
        assert_eq!(shape.mentions_classes, std::collections::BTreeSet::from(["card", "title"].map(String::from)));
        assert_eq!(shape.subject_classes, std::collections::BTreeSet::from(["title"].map(String::from)));
        assert!(!shape.simple);
        let simple = read_selector(".btn.primary");
        assert_eq!(simple.subject_classes, std::collections::BTreeSet::from(["btn", "primary"].map(String::from)));
        assert!(simple.simple);
        let functional = read_selector(".a:not(.b)");
        assert!(!functional.simple);
        assert_eq!(functional.subject_classes, std::collections::BTreeSet::from(["a"].map(String::from)));
        assert!(functional.mentions_classes.contains("b"));
        let attribute = read_selector("a[href=\".fake\"].link");
        assert_eq!(attribute.mentions_classes, std::collections::BTreeSet::from(["link"].map(String::from)));
        assert_eq!(read_selector(r".a\:b").subject_classes, std::collections::BTreeSet::from(["a:b"].map(String::from)));
        assert!(read_selector("h1").subject_classes.is_empty());
    }

    #[test]
    fn var_scanner_ignores_strings_urls_and_finds_fallback_uses() {
        assert_eq!(var_references("var(--a, var(--b))"), ["--a", "--b"]);
        assert_eq!(var_references("calc(var( --x ) * 2)"), ["--x"]);
        assert!(var_references("url(\"var(--no)\") 'var(--no)'").is_empty());
        assert!(var_references("revar(--no)").is_empty());
    }

    #[test]
    fn a_custom_property_reaches_readers_and_derived_properties_with_candidate_strength() {
        let report = ask(&model(), "custom-property", "--brand", ImpactDirection::Dependents, 3);
        assert!(matches!(report.seed_state, ImpactSeedState::Resolved));
        let names: Vec<(&str, &str)> = report.rows.iter().map(|row| (row.node.kind.as_str(), row.node.name.as_str())).collect();
        assert!(names.contains(&("custom-property", "--accent")), "{names:?}");
        assert!(names.contains(&("rule", ".card .title")), "{names:?}");
        assert!(names.contains(&("rule", ".btn")), "{names:?}");
        // The chain brand -> accent -> .btn is candidate end to end.
        assert!(report.rows.iter().all(|row| row.strength == ImpactStrength::Candidate || row.node.kind == "class"));
        assert_eq!(report.totals.exact, 0);
        assert!(report.gaps.iter().any(|gap| gap.facet == "css.cascade"));
        assert!(!report.proves_no_impact());
    }

    #[test]
    fn a_class_seed_follows_the_rules_which_mention_it_to_what_they_style() {
        let report = ask(&model(), "class", "card", ImpactDirection::Dependents, 2);
        let names: Vec<(&str, &str, ImpactStrength)> =
            report.rows.iter().map(|row| (row.node.kind.as_str(), row.node.name.as_str(), row.strength)).collect();
        // `.card .title` mentions card and styles `title` only through context.
        assert!(names.contains(&("class", "title", ImpactStrength::Candidate)), "{names:?}");
        let simple = ask(&model(), "class", "btn", ImpactDirection::Dependents, 2);
        // `.btn` rules style every .btn element exactly... but the seed is the class itself.
        assert!(simple.rows.iter().all(|row| row.node.name != "btn" || row.depth > 0));
    }

    #[test]
    fn a_rule_seed_reaches_its_class_exactly_when_the_selector_is_one_compound() {
        let report = ask(&model(), "selector", ".btn", ImpactDirection::Dependents, 1);
        let class = report.rows.iter().find(|row| row.node.kind == "class" && row.node.name == "btn").expect("class row");
        assert_eq!(class.strength, ImpactStrength::Exact);
        assert_eq!(class.relation, "css.styles-class");
        let context = ask(&model(), "selector", ".card .title", ImpactDirection::Dependents, 1);
        let title = context.rows.iter().find(|row| row.node.name == "title").expect("title row");
        assert_eq!(title.strength, ImpactStrength::Candidate);
    }

    #[test]
    fn exact_only_filter_hides_candidates_but_counts_and_reports_them() {
        let mut request = ImpactRequest::new(
            backend_library::PackageReference::parse("app".to_owned()).expect("package"),
            ImpactSeed::Named(ImpactSeedSpec {
                domain: ImpactDomain::Css,
                kind: "custom-property".into(),
                name: "--brand".into(),
                path: None,
            }),
        );
        request.strength = ImpactStrengthFilter::Exact;
        let (load, _) = build_graph(&model(), &|_, _| None);
        let report = super::super::impact_framework::answer(
            [9; 32], [2; 32], &[7; 32], vec![(ImpactDomain::Css, load)],
            &|spec, node| CssAdapter.seed_matches(spec, node), &request, &|| false,
        )
        .expect("answer");
        assert!(report.rows.is_empty());
        assert!(report.totals.candidate > 0);
        assert!(report.gaps.iter().any(|gap| gap.facet == "impact.hidden-candidates"));
        assert!(!report.proves_no_impact());
    }

    #[test]
    fn at_rules_condition_the_rules_nested_in_them() {
        let report = ask(&model(), "at-rule", "media", ImpactDirection::Dependents, 1);
        assert!(report.rows.iter().any(|row| row.relation == "css.conditions-rule" && row.strength == ImpactStrength::Exact));
        assert_eq!(report.seeds.len(), 1);
    }

    #[test]
    fn element_level_rules_reach_the_stylesheet_not_a_class() {
        let report = ask(&model(), "selector", "h1", ImpactDirection::Dependents, 1);
        assert!(report.rows.iter().any(|row| row.relation == "css.in-sheet" && row.strength == ImpactStrength::Candidate));
        assert!(report.rows.iter().all(|row| row.node.kind != "class"));
    }

    #[test]
    fn a_missing_name_is_not_found_and_cannot_prove_absence() {
        let report = ask(&model(), "class", "nope", ImpactDirection::Dependents, 3);
        assert!(matches!(report.seed_state, ImpactSeedState::NotFound { .. }));
        assert!(report.rows.is_empty());
        assert!(!report.proves_no_impact());
    }

    #[test]
    fn unavailable_css_is_reported_as_unavailable_not_as_empty() {
        let model = CssModel {
            state: ImpactDomainState::Unavailable { reason: "no admitted CSS worker is available".into() },
            source_root: None,
            hosts: Vec::new(),
            inventory_incomplete: false,
        };
        let report = ask(&model, "class", "btn", ImpactDirection::Dependents, 3);
        assert!(matches!(report.seed_state, ImpactSeedState::Unavailable { .. }));
        assert!(report.rows.is_empty());
        assert!(!report.proves_no_impact());
    }

    #[test]
    fn dependencies_walk_the_graph_backwards() {
        let report = ask(&model(), "selector", ".btn", ImpactDirection::Dependencies, 3);
        // `.btn` reads --accent which derives from --brand.
        let names: Vec<&str> = report.rows.iter().map(|row| row.node.name.as_str()).collect();
        assert!(names.contains(&"--accent") && names.contains(&"--brand"), "{names:?}");
        assert!(matches!(report.terminal, ImpactTerminal::Complete | ImpactTerminal::More { .. }));
    }

    #[test]
    fn paging_numbers_rows_stably_and_cursors_resume() {
        let mut request = ImpactRequest::new(
            backend_library::PackageReference::parse("app".to_owned()).expect("package"),
            ImpactSeed::Named(ImpactSeedSpec {
                domain: ImpactDomain::Css,
                kind: "custom-property".into(),
                name: "--brand".into(),
                path: None,
            }),
        );
        request.page_rows = 1;
        let run = |request: &ImpactRequest| {
            let (load, _) = build_graph(&model(), &|_, _| None);
            super::super::impact_framework::answer(
                [9; 32], [2; 32], &[7; 32], vec![(ImpactDomain::Css, load)],
                &|spec, node| CssAdapter.seed_matches(spec, node), request, &|| false,
            )
            .expect("answer")
        };
        let first = run(&request);
        assert_eq!(first.rows.len(), 1);
        let ImpactTerminal::More { cursor } = first.terminal.clone() else { panic!("expected more") };
        request.continuation = Some(cursor);
        let second = run(&request);
        assert_eq!(second.rows.len(), 1);
        assert_eq!(second.rows[0].n, first.rows[0].n + 1);
        // A different recipe cannot reuse the cursor.
        request.depth = 1;
        let (load, _) = build_graph(&model(), &|_, _| None);
        let refused = super::super::impact_framework::answer(
            [9; 32], [2; 32], &[7; 32], vec![(ImpactDomain::Css, load)],
            &|spec, node| CssAdapter.seed_matches(spec, node), &request, &|| false,
        );
        assert!(refused.is_err());
    }
}

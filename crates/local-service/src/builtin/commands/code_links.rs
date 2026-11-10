//! Code-side style link facts: how a TypeScript, TSX or JSX source uses
//! stylesheets, classes and custom properties, kept as persisted relation rows.
//!
//! The scan is the same lexical OXC pass the impact adapter has always run at
//! query time (`impact_code::scan_module`); this module turns its result into
//! bounded rows at index time and back into the identical scan when the rows
//! are read, so the impact graph is built from published facts rather than a
//! second parse. Nothing here claims more than the scan does: a literal class
//! token is a candidate, a class list with runtime parts is counted as dynamic.
use super::impact_code::{
    ClassUse, FileScan, ImportBinding, Imported, ModuleClassUse, PropertyUse, RenderUse,
    profile_for, scan_module,
};
use backend_engine::builtin::{
    CodeFactRow, CodeHostState, CodeImported, CodeUnavailable, MAX_CODE_FACTS_PER_HOST,
    StyloAnalysisRecord, code_fact_key, code_host_key, stylo_rows_digest,
};
use backend_frontend_typescript::legacy::with_analysis;
use std::collections::BTreeMap;

/// Whether a project-relative path is a source this scan reads.
pub(in crate::builtin) fn is_code_path(path: &str) -> bool {
    profile_for(path).is_some() || is_vue_path(path)
}

fn is_vue_path(path: &str) -> bool {
    path.ends_with(".vue")
}

/// Result of scanning one source file's bytes.
pub(in crate::builtin) enum ScanOutcome {
    Scanned(FileScan),
    Unparsed,
    InvalidUtf8,
}

/// Scans the exact bytes of one code source. `None` means the path is not code.
pub(in crate::builtin) fn scan_bytes(path: &str, bytes: &[u8]) -> Option<ScanOutcome> {
    if is_vue_path(path) {
        return Some(scan_vue(path, bytes));
    }
    let profile = profile_for(path)?;
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Some(ScanOutcome::InvalidUtf8);
    };
    Some(match with_analysis(profile, text, |module| scan_module(path, text, &module)) {
        Ok(scan) => ScanOutcome::Scanned(scan),
        Err(_) => ScanOutcome::Unparsed,
    })
}

/// PascalCase component name of a single-file component, from its file stem:
/// `ProductCard.vue` and `product-card.vue` are both `ProductCard`.
fn vue_component_name(path: &str) -> String {
    let stem = path.rsplit('/').next().unwrap_or(path).trim_end_matches(".vue");
    if stem.starts_with(|c: char| c.is_ascii_uppercase()) && !stem.contains(['-', '_', '.']) {
        return stem.to_owned();
    }
    stem.split(['-', '_', '.'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            chars.next().map_or_else(String::new, |first| first.to_uppercase().chain(chars).collect())
        })
        .collect()
}

/// The scan of a Vue single-file component: the component itself, the class
/// tokens its template spells out, and its `$style` members. The `<script>` is
/// not read here, so imports and renders inside it are not links; a `$style`
/// member is recorded against a synthetic default import of the file itself,
/// which is how the graph names a module block of the same component.
fn scan_vue(path: &str, bytes: &[u8]) -> ScanOutcome {
    let Ok(text) = std::str::from_utf8(bytes) else { return ScanOutcome::InvalidUtf8 };
    let Ok(container) = backend_frontend_vue::parse(bytes) else { return ScanOutcome::Unparsed };
    let name = vue_component_name(path);
    let mut scan = FileScan { path: path.to_owned(), ..FileScan::default() };
    if name.is_empty() {
        return ScanOutcome::Scanned(scan);
    }
    scan.components.insert(name.clone(), 1);
    scan.exports.insert("default".to_owned(), name.clone());
    let Some(template) = backend_frontend_vue::template_classes(container.host()) else {
        return ScanOutcome::Scanned(scan);
    };
    let mut starts = vec![0usize];
    starts.extend(text.bytes().enumerate().filter(|(_, byte)| *byte == b'\n').map(|(index, _)| index + 1));
    let line_at = |offset: usize| u32::try_from(starts.partition_point(|start| *start <= offset)).unwrap_or(u32::MAX);
    for class in &template.classes {
        scan.class_uses.push(ClassUse {
            token: class.token.clone(),
            line: line_at(class.range.start),
            owner: Some(name.clone()),
            conditional: class.conditional,
        });
    }
    if !template.module_members.is_empty() {
        scan.imports.push(ImportBinding {
            local: "$style".to_owned(),
            spec: format!("./{}", path.rsplit('/').next().unwrap_or(path)),
            imported: Imported::Default,
            line: 1,
        });
        for member in &template.module_members {
            scan.module_class_uses.push(ModuleClassUse {
                binding: 0,
                class: member.name.clone(),
                line: line_at(member.range.start),
                owner: Some(name.clone()),
            });
        }
    }
    scan.dynamic_class_expressions = template.dynamic_bindings;
    ScanOutcome::Scanned(scan)
}

/// Flattens a scan into ordered facts. Imports come first so a module-class
/// fact can name its import by ordinal.
pub(in crate::builtin) fn facts_of(scan: &FileScan) -> Vec<CodeFactRow> {
    let mut facts = Vec::new();
    for binding in &scan.imports {
        facts.push(CodeFactRow::Import {
            local: binding.local.clone(),
            spec: binding.spec.clone(),
            imported: match &binding.imported {
                Imported::Default => CodeImported::Default,
                Imported::Named(name) => CodeImported::Named(name.clone()),
                Imported::Namespace => CodeImported::Namespace,
            },
            line: binding.line,
        });
    }
    for (spec, line) in &scan.side_effect_imports {
        facts.push(CodeFactRow::SideEffectImport { spec: spec.clone(), line: *line });
    }
    for usage in &scan.class_uses {
        facts.push(CodeFactRow::ClassUse {
            token: usage.token.clone(),
            line: usage.line,
            owner: usage.owner.clone(),
            conditional: usage.conditional,
        });
    }
    for usage in &scan.module_class_uses {
        facts.push(CodeFactRow::ModuleClassUse {
            binding: u32::try_from(usage.binding).unwrap_or(u32::MAX),
            class: usage.class.clone(),
            line: usage.line,
            owner: usage.owner.clone(),
        });
    }
    for render in &scan.renders {
        facts.push(CodeFactRow::Render {
            local: render.local.clone(),
            line: render.line,
            owner: render.owner.clone(),
        });
    }
    for usage in &scan.property_uses {
        facts.push(CodeFactRow::PropertyUse {
            name: usage.name.clone(),
            line: usage.line,
            owner: usage.owner.clone(),
            writes: usage.writes,
        });
    }
    for (name, line) in &scan.components {
        facts.push(CodeFactRow::Component { name: name.clone(), line: *line });
    }
    for (exported, local) in &scan.exports {
        facts.push(CodeFactRow::Export { exported: exported.clone(), local: local.clone() });
    }
    for (exported, (spec, imported)) in &scan.reexports {
        facts.push(CodeFactRow::Reexport {
            exported: exported.clone(),
            spec: spec.clone(),
            imported: imported.clone(),
        });
    }
    facts
}

/// Rebuilds the scan a host's facts describe. Inverse of [`facts_of`].
pub(in crate::builtin) fn scan_from_facts(path: &str, dynamic_class_expressions: u32, facts: &[CodeFactRow]) -> FileScan {
    let mut scan = FileScan { path: path.to_owned(), dynamic_class_expressions, ..FileScan::default() };
    for fact in facts {
        match fact {
            CodeFactRow::Import { local, spec, imported, line } => scan.imports.push(ImportBinding {
                local: local.clone(),
                spec: spec.clone(),
                imported: match imported {
                    CodeImported::Default => Imported::Default,
                    CodeImported::Named(name) => Imported::Named(name.clone()),
                    CodeImported::Namespace => Imported::Namespace,
                },
                line: *line,
            }),
            CodeFactRow::SideEffectImport { spec, line } => scan.side_effect_imports.push((spec.clone(), *line)),
            CodeFactRow::ClassUse { token, line, owner, conditional } => scan.class_uses.push(ClassUse {
                token: token.clone(),
                line: *line,
                owner: owner.clone(),
                conditional: *conditional,
            }),
            CodeFactRow::ModuleClassUse { binding, class, line, owner } => scan.module_class_uses.push(ModuleClassUse {
                binding: *binding as usize,
                class: class.clone(),
                line: *line,
                owner: owner.clone(),
            }),
            CodeFactRow::Render { local, line, owner } => scan.renders.push(RenderUse {
                local: local.clone(),
                line: *line,
                owner: owner.clone(),
            }),
            CodeFactRow::PropertyUse { name, line, owner, writes } => scan.property_uses.push(PropertyUse {
                name: name.clone(),
                line: *line,
                owner: owner.clone(),
                writes: *writes,
            }),
            CodeFactRow::Component { name, line } => {
                scan.components.insert(name.clone(), *line);
            }
            CodeFactRow::Export { exported, local } => {
                scan.exports.insert(exported.clone(), local.clone());
            }
            CodeFactRow::Reexport { exported, spec, imported } => {
                scan.reexports.insert(exported.clone(), (spec.clone(), imported.clone()));
            }
        }
    }
    scan
}

/// One code host and its keyed facts, ready to join the Stylo analysis relation.
pub(in crate::builtin) struct CodeHostRows {
    pub key: [u8; 32],
    pub host: StyloAnalysisRecord,
    pub facts: Vec<([u8; 32], StyloAnalysisRecord)>,
}

/// Builds the host and fact rows for scanned bytes. `source` is the typed
/// source-fact identity of exactly `bytes`.
pub(in crate::builtin) fn host_rows(
    project: [u8; 32],
    path: &str,
    source: [u8; 32],
    byte_length: u64,
    outcome: ScanOutcome,
) -> Result<CodeHostRows, String> {
    let key = code_host_key(project, path)?;
    let (state, facts) = match outcome {
        ScanOutcome::Scanned(scan) => {
            let facts = facts_of(&scan);
            if facts.len() > MAX_CODE_FACTS_PER_HOST as usize {
                (CodeHostState::Unavailable(CodeUnavailable::Limit), Vec::new())
            } else {
                (CodeHostState::Scanned { dynamic_class_expressions: scan.dynamic_class_expressions }, facts)
            }
        }
        ScanOutcome::Unparsed => (CodeHostState::Unparsed, Vec::new()),
        ScanOutcome::InvalidUtf8 => (CodeHostState::Unavailable(CodeUnavailable::InvalidUtf8), Vec::new()),
    };
    let mut rows = Vec::with_capacity(facts.len());
    for (ordinal, fact) in facts.into_iter().enumerate() {
        let ordinal = u32::try_from(ordinal).map_err(|_| "code fact ordinal overflow".to_owned())?;
        rows.push((code_fact_key(key, ordinal), StyloAnalysisRecord::CodeFact { host: key, ordinal, fact }));
    }
    rows.sort_by_key(|(fact_key, _)| *fact_key);
    let refs: Vec<_> = rows.iter().map(|(fact_key, row)| (*fact_key, row)).collect();
    let fact_digest = stylo_rows_digest(&refs)?;
    let host = StyloAnalysisRecord::CodeHost {
        project,
        path: path.to_owned(),
        source,
        byte_length,
        state,
        fact_count: u32::try_from(rows.len()).map_err(|_| "code fact count overflow".to_owned())?,
        fact_digest,
    };
    host.validate()?;
    Ok(CodeHostRows { key, host, facts: rows })
}

/// One code host as the selected relation holds it.
pub(in crate::builtin) struct PersistedHost {
    pub source: [u8; 32],
    pub byte_length: u64,
    pub state: CodeHostState,
    pub facts: Vec<CodeFactRow>,
}

/// The project's published code facts, by path.
pub(in crate::builtin) struct PersistedCode {
    pub hosts: BTreeMap<String, PersistedHost>,
}

const CODE_PAGE: usize = 64;
const MAX_CODE_ROWS: usize = 1_000_000;

/// Reads the project's published code hosts from the selected relation.
/// `Ok(None)` means the index published no code facts for this project (an
/// older index, no stylesheets, or no Stylo worker), which readers must treat as
/// "not published" and never as "no links".
pub(in crate::builtin) fn read_persisted(
    snapshot: &backend_engine::WorkspaceSnapshot,
    project: [u8; 32],
    cancelled: &dyn Fn() -> bool,
) -> Result<Option<PersistedCode>, String> {
    use backend_engine::builtin::stylo_selection_key;
    let relation = match super::stylo_query::selected_native_relation(snapshot) {
        Ok(Some(relation)) => relation,
        Ok(None) => return Ok(None),
        Err(error) => return Err(format!("code link relation: {}", error.0)),
    };
    let selection = relation.lookup(&stylo_selection_key(project)).map_err(|e| format!("{e:?}"))?;
    let Some(StyloAnalysisRecord::Selection { code: Some(_), .. }) = selection else {
        return Ok(None);
    };
    let mut hosts: BTreeMap<[u8; 32], (String, PersistedHost, Vec<(u32, CodeFactRow)>)> = BTreeMap::new();
    let mut facts: Vec<([u8; 32], u32, CodeFactRow)> = Vec::new();
    let mut after: Option<[u8; 32]> = None;
    let mut scanned = 0usize;
    loop {
        if cancelled() {
            return Err("impact cancelled".to_owned());
        }
        let page = relation.page(after.as_ref(), CODE_PAGE).map_err(|e| format!("{e:?}"))?;
        let entries = page.entries();
        if entries.is_empty() {
            break;
        }
        for (key, row) in entries {
            after = Some(*key);
            scanned += 1;
            if scanned > MAX_CODE_ROWS {
                return Err("the code link relation exceeds the impact scan budget".to_owned());
            }
            match row {
                StyloAnalysisRecord::CodeHost { project: owner, path, source, byte_length, state, .. } if *owner == project => {
                    hosts.insert(*key, (path.clone(), PersistedHost { source: *source, byte_length: *byte_length, state: *state, facts: Vec::new() }, Vec::new()));
                }
                StyloAnalysisRecord::CodeFact { host, ordinal, fact } => facts.push((*host, *ordinal, fact.clone())),
                _ => {}
            }
        }
    }
    for (host, ordinal, fact) in facts {
        if let Some(entry) = hosts.get_mut(&host) {
            entry.2.push((ordinal, fact));
        }
    }
    let mut out = BTreeMap::new();
    for (_, (path, mut host, mut rows)) in hosts {
        rows.sort_by_key(|(ordinal, _)| *ordinal);
        host.facts = rows.into_iter().map(|(_, fact)| fact).collect();
        out.insert(path, host);
    }
    Ok(Some(PersistedCode { hosts: out }))
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use backend_engine::builtin::admit_stylo_relation;

    const SOURCE: &str = r#"
import styles from "./Card.module.css";
import "./theme.css";
import clsx from "clsx";
export function Card({ on }) {
  return <div className={clsx("card", on && "card-on", styles.title)} style={{ "--gap": 4 }}><Inner /></div>;
}
export const Inner = () => <span className="shared" />;
"#;

    fn scanned(path: &str, source: &str) -> FileScan {
        match scan_bytes(path, source.as_bytes()).expect("code path") {
            ScanOutcome::Scanned(scan) => scan,
            _ => panic!("source does not scan"),
        }
    }

    #[test]
    fn facts_roundtrip_to_the_identical_scan() {
        let scan = scanned("src/Card.tsx", SOURCE);
        assert!(!scan.module_class_uses.is_empty() && !scan.class_uses.is_empty());
        let facts = facts_of(&scan);
        let back = scan_from_facts("src/Card.tsx", scan.dynamic_class_expressions, &facts);
        assert_eq!(back, scan, "persisting and reading facts must not change what the graph sees");
    }

    #[test]
    fn rows_are_keyed_ordered_and_witnessed() {
        let scan = scanned("src/Card.tsx", SOURCE);
        let rows = host_rows([7; 32], "src/Card.tsx", [9; 32], SOURCE.len() as u64, ScanOutcome::Scanned(scan))
            .expect("rows");
        let StyloAnalysisRecord::CodeHost { fact_count, state, .. } = &rows.host else { panic!("host") };
        assert_eq!(*fact_count as usize, rows.facts.len());
        assert!(matches!(state, CodeHostState::Scanned { .. }));
        assert!(rows.facts.windows(2).all(|pair| pair[0].0 < pair[1].0));
        // A bare host row admits only with a selection witness; without a
        // selection the relation refuses code rows outright.
        let mut all: Vec<([u8; 32], &StyloAnalysisRecord)> = vec![(rows.key, &rows.host)];
        all.extend(rows.facts.iter().map(|(key, row)| (*key, row)));
        all.sort_by_key(|(key, _)| *key);
        assert!(admit_stylo_relation(&all).is_err());
    }

    #[test]
    fn a_vue_component_scans_to_its_template_classes_and_module_members() {
        let source = "<template>\n  <div class=\"card\" :class=\"[$style.title, active && 'on']\" />\n</template>\n<style module>.title { color: red; }</style>\n";
        let ScanOutcome::Scanned(scan) = scan_bytes("src/product-card.vue", source.as_bytes()).expect("vue path") else {
            panic!("the component scans");
        };
        assert_eq!(scan.components.keys().collect::<Vec<_>>(), ["ProductCard"]);
        assert_eq!(scan.exports.get("default").map(String::as_str), Some("ProductCard"));
        let tokens: Vec<(&str, bool, u32)> = scan.class_uses.iter().map(|u| (u.token.as_str(), u.conditional, u.line)).collect();
        assert_eq!(tokens, [("card", false, 2), ("on", true, 2)]);
        assert_eq!(scan.module_class_uses.len(), 1);
        assert_eq!(scan.module_class_uses[0].class, "title");
        assert_eq!(scan.imports[0].spec, "./product-card.vue");
        assert_eq!(scan.dynamic_class_expressions, 1);
        let facts = facts_of(&scan);
        assert_eq!(scan_from_facts("src/product-card.vue", 1, &facts), scan);
        assert_eq!(vue_component_name("a/Hero.vue"), "Hero");
        assert_eq!(vue_component_name("a/main_nav.item.vue"), "MainNavItem");
    }

    #[test]
    fn unparsed_and_non_utf8_sources_leave_no_facts() {
        assert!(matches!(scan_bytes("a.tsx", b"export const = <<<"), Some(ScanOutcome::Unparsed)));
        assert!(matches!(scan_bytes("a.tsx", &[0xff, 0xfe, 0x00]), Some(ScanOutcome::InvalidUtf8)));
        assert!(scan_bytes("a.css", b".a{}").is_none());
        let rows = host_rows([7; 32], "a.tsx", [9; 32], 3, ScanOutcome::Unparsed).expect("rows");
        assert!(rows.facts.is_empty());
    }
}

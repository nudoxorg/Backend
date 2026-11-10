//! SQL adapter for the cross-language impact framework.
//!
//! Nodes are schema objects (tables, columns, views, indexes, triggers,
//! functions), plus the statements which use them (queries and migrations). An
//! edge points from a thing to what a change to it can break: a column to the
//! view that selects it, the trigger that updates it, the query that reads it,
//! the migration that alters it. Edges come from references the project-wide
//! binder resolved; a mention whose member is unknown (a view column) or which
//! binds to several objects is a candidate edge, never an exact one. Mentions
//! the binder could not bind are counted as a gap, not dropped.
use super::impact_framework::{
    DomainLoad, Edge, FileWitness, ImpactAdapter, ImpactContext, Key, Node, node_key,
};
use crate::builtin::sql_catalog::{ProjectSql, load_project};
use backend_engine::builtin::SqlHostTerminal;
use backend_library::impact::{
    ImpactDomain, ImpactDomainBasis, ImpactDomainState, ImpactGap, ImpactSeedSpec, ImpactStrength,
};
use backend_semantic::sql_analysis::{
    SqlDefinitionAction, SqlEntityRef, SqlFact, SqlObjectKind, SqlRefKind, SqlResolution,
    SqlStatementKind,
};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

pub(super) struct SqlAdapter;

const DOMAIN: ImpactDomain = ImpactDomain::Sql;

fn kind_str(kind: SqlObjectKind) -> &'static str {
    match kind {
        SqlObjectKind::Schema => "schema",
        SqlObjectKind::Table => "table",
        SqlObjectKind::View => "view",
        SqlObjectKind::Column => "column",
        SqlObjectKind::Index => "index",
        SqlObjectKind::Trigger => "trigger",
        SqlObjectKind::Function => "function",
        SqlObjectKind::Type => "type",
        SqlObjectKind::Sequence => "sequence",
    }
}

/// Zero-based byte offset to one-based line, from the file's own newlines.
struct Lines(Vec<usize>);
impl Lines {
    fn of(bytes: &[u8]) -> Self {
        Self(bytes.iter().enumerate().filter(|(_, b)| **b == b'\n').map(|(i, _)| i).collect())
    }
    fn line(&self, offset: u32) -> u32 {
        let offset = offset as usize;
        u32::try_from(self.0.partition_point(|newline| *newline < offset) + 1).unwrap_or(u32::MAX)
    }
}

impl ImpactAdapter for SqlAdapter {
    fn domain(&self) -> ImpactDomain {
        DOMAIN
    }

    fn load(&self, cx: &ImpactContext<'_>) -> DomainLoad {
        let unavailable = |reason: &str| {
            DomainLoad::empty(DOMAIN, ImpactDomainState::Unavailable { reason: reason.to_owned() })
        };
        let loaded = match load_project(cx.snapshot, cx.project, cx.cancelled) {
            Ok(Some(loaded)) => loaded,
            Ok(None) => {
                return unavailable(
                    "no SQL analysis is selected for this project: no .sql file was indexed, or no SQL worker is installed",
                )
            }
            Err(error) => return unavailable(&format!("the selected SQL analysis could not be read: {error}")),
        };
        build(loaded)
    }

    fn seed_matches(&self, spec: &ImpactSeedSpec, node: &Node) -> bool {
        if spec.kind != node.kind {
            return false;
        }
        if spec.path.as_deref().is_some_and(|path| node.path.as_deref() != Some(path)) {
            return false;
        }
        let wanted = spec.name.to_ascii_lowercase();
        let name = node.name.to_ascii_lowercase();
        name == wanted || (!node.scope.is_empty() && format!("{}.{name}", node.scope.to_ascii_lowercase()) == wanted)
    }
}

fn build(loaded: ProjectSql) -> DomainLoad {
    let root = PathBuf::from(&loaded.root);
    // Lines, once per file; the project capture bounds the total size.
    let mut lines: Vec<Option<Lines>> = Vec::with_capacity(loaded.hosts.len());
    for host in &loaded.hosts {
        let bytes = host.length.and_then(|length| {
            crate::builtin::ingest::read_stylo_selected_source(
                &root,
                &host.path,
                usize::try_from(length).unwrap_or(usize::MAX).min(8 * 1024 * 1024),
            )
            .ok()
            .filter(|bytes| bytes.len() as u64 == length)
        });
        lines.push(bytes.as_deref().map(Lines::of));
    }
    let line_of = |host: usize, offset: u32| lines.get(host).and_then(|l| l.as_ref()).map(|l| l.line(offset));

    // Pass 1: entities by the definition that created them.
    #[derive(Clone)]
    struct Entity {
        key: Key,
    }
    let mut entities: BTreeMap<SqlEntityRef, Entity> = BTreeMap::new();
    let mut nodes: BTreeMap<Key, Node> = BTreeMap::new();
    let mut table_columns: BTreeMap<Key, Vec<Key>> = BTreeMap::new();
    let mut entity_parent: BTreeMap<SqlEntityRef, SqlEntityRef> = BTreeMap::new();
    for (host, ordinal, fact) in &loaded.facts {
        let SqlFact::Definition(definition) = fact else { continue };
        if !matches!(definition.action, SqlDefinitionAction::Create | SqlDefinitionAction::AddColumn)
            || definition.kind == SqlObjectKind::Schema
        {
            continue;
        }
        let path = loaded.hosts[*host].path.clone();
        let entity_ref = SqlEntityRef { path: path.clone(), ordinal: *ordinal };
        let name = definition.head_name.clone().unwrap_or_else(|| definition.name.clone());
        let (display, scope) = if definition.kind == SqlObjectKind::Column {
            let parent = definition.head_parent.clone().or_else(|| definition.parent.clone()).unwrap_or_default();
            (format!("{parent}.{name}"), definition.parent_schema.clone().or_else(|| definition.schema.clone()).unwrap_or_default())
        } else {
            (name.clone(), definition.schema.clone().unwrap_or_default())
        };
        let kind = kind_str(definition.kind);
        let key = node_key(DOMAIN, kind, &scope, &display);
        let mut node = Node::new(DOMAIN, kind, &scope, &display);
        node.path = Some(path);
        node.line = line_of(*host, definition.name_extent.start);
        nodes.entry(key).or_insert(node);
        entities.insert(entity_ref.clone(), Entity { key });
        if let Some(parent) = &definition.parent_entity {
            entity_parent.insert(entity_ref, parent.clone());
        }
    }
    for (entity, parent) in &entity_parent {
        if let (Some(child), Some(parent)) = (entities.get(entity), entities.get(parent)) {
            table_columns.entry(parent.key).or_default().push(child.key);
        }
    }
    for (parent, children) in &table_columns {
        if let Some(node) = nodes.get_mut(parent) {
            node.companions = children.clone();
        }
    }

    // Pass 2: statements and the edges their references create.
    let mut edges: Vec<Edge> = Vec::new();
    let mut unresolved = 0u32;
    let mut seen_edges: BTreeSet<(Key, Key, &'static str)> = BTreeSet::new();
    let mut statement_nodes: BTreeMap<(usize, u32), Key> = BTreeMap::new();
    let mut statement_kind: BTreeMap<(usize, u32), SqlStatementKind> = BTreeMap::new();
    let mut statement_extent: BTreeMap<(usize, u32), u32> = BTreeMap::new();
    for (host, ordinal, fact) in &loaded.facts {
        if let SqlFact::Statement { class, extent, .. } = fact {
            statement_kind.insert((*host, *ordinal), *class);
            statement_extent.insert((*host, *ordinal), extent.start);
        }
    }
    for (host, _, fact) in &loaded.facts {
        let SqlFact::Reference(reference) = fact else { continue };
        let targets: Vec<(&SqlEntityRef, ImpactStrength)> = match &reference.resolution {
            SqlResolution::Resolved { target, .. } => vec![(target, ImpactStrength::Exact)],
            SqlResolution::Expands { targets } => targets.iter().map(|t| (t, ImpactStrength::Exact)).collect(),
            SqlResolution::Relation { relation, .. } => vec![(relation, ImpactStrength::Candidate)],
            SqlResolution::Ambiguous { candidates, .. } => {
                candidates.iter().map(|c| (c, ImpactStrength::Candidate)).collect()
            }
            SqlResolution::Local => continue,
            SqlResolution::Unresolved { .. } => {
                // Function calls to builtins and CTE-local names are expected to
                // be unbound; real objects the project never defines are a gap.
                if matches!(reference.kind, SqlRefKind::Relation | SqlRefKind::Column) {
                    unresolved += 1;
                }
                continue;
            }
        };
        // Who owns the mention: the object its statement defines, else the statement.
        let class = statement_kind
            .get(&(*host, reference.statement))
            .copied()
            .unwrap_or(reference.statement_kind);
        let (owner_kind, owner_name, owner_scope, relation): (&'static str, String, String, &'static str) =
            match &reference.in_object {
                Some(object) => {
                    let relation = match object.kind {
                        SqlObjectKind::View => "sql.selected-by-view",
                        SqlObjectKind::Trigger => "sql.used-by-trigger",
                        SqlObjectKind::Function => "sql.used-by-function",
                        SqlObjectKind::Index => "sql.indexed-by",
                        SqlObjectKind::Table => "sql.constrains",
                        _ => "sql.used-by",
                    };
                    (kind_str(object.kind), object.name.clone(), String::new(), relation)
                }
                None => {
                    let path = &loaded.hosts[*host].path;
                    let relation_name = match (class, reference.role) {
                        (SqlStatementKind::AlterTable | SqlStatementKind::Drop, _) => "sql.altered-by",
                        (_, backend_semantic::sql_analysis::SqlRefRole::Write) => "sql.written-by",
                        _ => "sql.read-by",
                    };
                    let kind = if matches!(class, SqlStatementKind::AlterTable | SqlStatementKind::Drop) {
                        "migration"
                    } else {
                        "query"
                    };
                    let line = statement_extent
                        .get(&(*host, reference.statement))
                        .and_then(|offset| line_of(*host, *offset))
                        .unwrap_or(0);
                    (kind, format!("{path}:{line}"), path.clone(), relation_name)
                }
            };
        let owner_key = node_key(DOMAIN, owner_kind, &owner_scope, &owner_name);
        nodes.entry(owner_key).or_insert_with(|| {
            let mut node = Node::new(DOMAIN, owner_kind, &owner_scope, &owner_name);
            if reference.in_object.is_none() {
                node.path = Some(loaded.hosts[*host].path.clone());
                node.line = statement_extent
                    .get(&(*host, reference.statement))
                    .and_then(|offset| line_of(*host, *offset));
            }
            node
        });
        statement_nodes.insert((*host, reference.statement), owner_key);
        // A mention of an object defined in the project: the relation holding the
        // member (`accounts` for `accounts.email`) is not itself changed by it.
        let why = format!(
            "{} {:?} {} in {}",
            if reference.role == backend_semantic::sql_analysis::SqlRefRole::Write { "writes" } else { "mentions" },
            reference.kind,
            reference.qualifier.as_ref().map_or_else(|| reference.name.clone(), |q| format!("{q}.{}", reference.name)),
            loaded.hosts[*host].path
        );
        for (target, strength) in targets {
            let Some(entity) = entities.get(target) else { continue };
            if entity.key == owner_key {
                continue;
            }
            if seen_edges.insert((entity.key, owner_key, relation)) {
                edges.push(Edge { from: entity.key, to: owner_key, strength, relation, why: why.clone() });
            }
        }
    }

    // Gaps: everything that could hide an affected node.
    let mut gaps = Vec::new();
    let g = loaded.gaps;
    if g.partial_statements + g.unsupported_statements > 0 {
        gaps.push(ImpactGap {
            facet: "sql.partial-statement".into(),
            note: "some statements were not fully extracted (procedural bodies, dynamic SQL or unmodeled syntax); mentions inside them may be missing".into(),
            count: Some(g.partial_statements + g.unsupported_statements),
        });
    }
    if g.unavailable_hosts > 0 {
        gaps.push(ImpactGap {
            facet: "sql.host-unavailable".into(),
            note: "some SQL files could not be analysed (no admitted worker, too large, unreadable); their mentions are unknown".into(),
            count: Some(g.unavailable_hosts),
        });
    }
    if g.native_error_hosts > 0 {
        gaps.push(ImpactGap {
            facet: "sql.unparseable-file".into(),
            note: "the native parser rejected some SQL files; their mentions are unknown".into(),
            count: Some(g.native_error_hosts),
        });
    }
    if !loaded.inventory_complete {
        gaps.push(ImpactGap {
            facet: "sql.inventory".into(),
            note: "the SQL file inventory was cut at its bound; further files are not analysed".into(),
            count: None,
        });
    }
    if unresolved > 0 {
        gaps.push(ImpactGap {
            facet: "sql.unresolved-reference".into(),
            note: "table and column mentions that match no object defined by the project's SQL (external tables, extensions, dropped objects)".into(),
            count: Some(unresolved),
        });
    }
    gaps.push(ImpactGap {
        facet: "sql.embedded-sql".into(),
        note: "SQL written inside application code (query strings, ORM models, query builders) is not analysed".into(),
        count: None,
    });
    gaps.push(ImpactGap {
        facet: "sql.schema-basis".into(),
        note: "objects come from the project's DDL in declared file order; a deployed database may differ".into(),
        count: None,
    });

    let files: Vec<FileWitness> = loaded
        .hosts
        .iter()
        .filter_map(|host| {
            Some(FileWitness {
                root: root.clone(),
                path: host.path.clone(),
                length: host.length?,
                source: host.source?,
            })
        })
        .collect();
    let analysed = loaded
        .hosts
        .iter()
        .filter(|host| matches!(host.terminal, SqlHostTerminal::Parsed { .. }))
        .count();
    let state = if g.unavailable_hosts + g.native_error_hosts == 0 && loaded.inventory_complete {
        ImpactDomainState::Analyzed
    } else if analysed == 0 {
        ImpactDomainState::Unavailable {
            reason: "no SQL file could be analysed (no admitted SQL worker, or every file was rejected)".into(),
        }
    } else {
        ImpactDomainState::Partial {
            reasons: gaps.iter().take(3).map(|gap| gap.note.clone()).collect(),
        }
    };
    DomainLoad {
        nodes,
        edges,
        gaps,
        basis: ImpactDomainBasis {
            domain: DOMAIN,
            state,
            files: u32::try_from(analysed).unwrap_or(u32::MAX),
        },
        known_paths: loaded.hosts.iter().map(|host| host.path.clone()).collect(),
        files,
    }
}

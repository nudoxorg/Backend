//! Project-wide binding of SQL outlines.
//!
//! Workers report what each file writes. This module replays every file's
//! definitions in the project's declared source order to build an ordered
//! schema, then binds each mention to the schema object it names. The result
//! is a pure function of the supplied outlines: same input, same facts.
//!
//! Basis (stated, not hidden): migration order is the declared source order
//! (path order). Queries bind to the schema as it stands after the last
//! statement; definitions (views, triggers, indexes, foreign keys, ALTERs)
//! bind to the schema as it stood at their own position, falling back to the
//! eventual definition when the object is created later in the order.
//! Nothing here claims to know what a deployed database contains.
use backend_semantic::sql_analysis::{
    SqlByteRange, SqlDefinition, SqlDefinitionAction, SqlDialect, SqlEntityRef, SqlFact, SqlGap,
    SqlObjectKind, SqlObjectRef, SqlRefKind, SqlRefRole, SqlReference, SqlResolution, SqlStatementKind,
    SqlStatementSupport, SqlUnresolved,
};
use backend_sql_contract as c;
use std::collections::{BTreeMap, HashMap};

/// One parsed file in the project's declared order.
pub(super) struct HostOutline<'a> {
    pub(super) path: &'a str,
    pub(super) dialect: SqlDialect,
    pub(super) outline: &'a c::Outline,
}

const MAX_CANDIDATES: usize = 8;

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
enum Ns {
    Relation,
    Index,
    Trigger,
    Function,
    Type,
    Sequence,
    Schema,
}
fn ns_of(kind: c::ObjectKind) -> Ns {
    match kind {
        c::ObjectKind::Table | c::ObjectKind::View | c::ObjectKind::Column => Ns::Relation,
        c::ObjectKind::Index => Ns::Index,
        c::ObjectKind::Trigger => Ns::Trigger,
        c::ObjectKind::Function => Ns::Function,
        c::ObjectKind::Type => Ns::Type,
        c::ObjectKind::Sequence => Ns::Sequence,
        c::ObjectKind::Schema => Ns::Schema,
    }
}

struct Entity {
    id: SqlEntityRef,
    kind: c::ObjectKind,
    schema: String,
    name: String,
    alive: bool,
    /// Live columns, by entity index, for relations.
    columns: Vec<usize>,
    /// False when the relation's column list is not known (view without an
    /// explicit list, `CREATE TABLE AS`, virtual tables).
    columns_known: bool,
    /// The relation a column, index or trigger belongs to.
    parent: Option<usize>,
}

type Key = (Ns, String, String);

#[derive(Default)]
struct Catalog {
    entities: Vec<Entity>,
    /// Live name to entity.
    current: HashMap<Key, Vec<usize>>,
    /// Latest dropped entity for a name, for stale-reference detection.
    dropped: HashMap<Key, usize>,
}

fn default_schema(dialect: SqlDialect) -> &'static str {
    match dialect {
        SqlDialect::Sqlite => "main",
        SqlDialect::Postgres => "public",
    }
}

impl Catalog {
    fn key(&self, ns: Ns, schema: Option<&str>, name: &str, dialect: SqlDialect) -> Key {
        (
            ns,
            schema.unwrap_or(default_schema(dialect)).to_owned(),
            name.to_owned(),
        )
    }
    fn lookup_live(&self, key: &Key) -> &[usize] {
        self.current.get(key).map_or(&[], Vec::as_slice)
    }
    fn add(&mut self, entity: Entity, dialect: SqlDialect, ns: Ns) -> usize {
        let index = self.entities.len();
        let key = (ns, entity.schema.clone(), entity.name.clone());
        let _ = dialect;
        self.entities.push(entity);
        self.current.entry(key).or_default().push(index);
        index
    }
    fn unlink(&mut self, index: usize, ns: Ns) {
        let key = (
            ns,
            self.entities[index].schema.clone(),
            self.entities[index].name.clone(),
        );
        if let Some(list) = self.current.get_mut(&key) {
            list.retain(|candidate| *candidate != index);
            if list.is_empty() {
                self.current.remove(&key);
            }
        }
    }
    fn kill(&mut self, index: usize, ns: Ns) {
        self.unlink(index, ns);
        let key = (
            ns,
            self.entities[index].schema.clone(),
            self.entities[index].name.clone(),
        );
        self.dropped.insert(key, index);
        self.entities[index].alive = false;
        // The columns stay attached so a stale mention still binds to the
        // dropped column, not to nothing; they just stop being live.
        for column in self.entities[index].columns.clone() {
            self.entities[column].alive = false;
        }
    }
    /// A live column of that name, else the most recently dropped one.
    fn column_of(&self, relation: usize, name: &str) -> Option<usize> {
        let named = |column: &usize| self.entities[*column].name == name;
        let columns = &self.entities[relation].columns;
        columns
            .iter()
            .copied()
            .find(|column| named(column) && self.entities[*column].alive)
            .or_else(|| columns.iter().rev().copied().find(named))
    }
}

/// Bind every outline. Result is indexed like `hosts`; each inner vector is
/// the host's complete fact list in ordinal order.
pub(super) fn bind_project(hosts: &[HostOutline<'_>]) -> Vec<Vec<SqlFact>> {
    // Pass 1: ordinals. Per statement: the statement fact, its definitions,
    // then its references.
    let mut statement_ordinal: Vec<Vec<u32>> = Vec::new();
    let mut definition_ordinal: Vec<Vec<Vec<u32>>> = Vec::new();
    for host in hosts {
        let mut next = 0u32;
        let mut statements = Vec::new();
        let mut definitions = Vec::new();
        for statement in &host.outline.statements {
            statements.push(next);
            next += 1;
            let mut d = Vec::new();
            for _ in &statement.definitions {
                d.push(next);
                next += 1;
            }
            let mut r = Vec::new();
            for _ in &statement.references {
                r.push(next);
                next += 1;
            }
            definitions.push(d);
            let _ = r;
        }
        statement_ordinal.push(statements);
        definition_ordinal.push(definitions);
    }
    let entity_ref = |host: usize, statement: usize, definition: usize| SqlEntityRef {
        path: hosts[host].path.to_owned(),
        ordinal: definition_ordinal[host][statement][definition],
    };

    let mut catalogs: BTreeMap<u8, Catalog> = BTreeMap::new();
    let dialect_slot = |dialect: SqlDialect| match dialect {
        SqlDialect::Sqlite => 0u8,
        SqlDialect::Postgres => 1u8,
    };
    // Entity bound to each definition, and for references the resolution.
    let mut def_entity: HashMap<(usize, usize, usize), Option<usize>> = HashMap::new();
    let mut def_parent: HashMap<(usize, usize, usize), Option<usize>> = HashMap::new();
    let mut ref_resolution: HashMap<(usize, usize, usize), Res> = HashMap::new();

    // Phase 1: DDL in order. Phase 2 (below): queries against the head.
    for (h, host) in hosts.iter().enumerate() {
        let catalog = catalogs.entry(dialect_slot(host.dialect)).or_default();
        for (s, statement) in host.outline.statements.iter().enumerate() {
            let is_query = is_query(statement.kind);
            if is_query {
                continue;
            }
            let apply_first = !matches!(
                statement.kind,
                c::StatementKind::AlterTable | c::StatementKind::Drop
            );
            if apply_first {
                apply_definitions(
                    catalog,
                    host.dialect,
                    statement,
                    (h, s),
                    &entity_ref,
                    &mut def_entity,
                    &mut def_parent,
                );
            }
            for (r, reference) in statement.references.iter().enumerate() {
                let res = resolve_reference(
                    catalog,
                    host.dialect,
                    statement,
                    reference,
                    Some((h, s, &def_entity)),
                    true,
                );
                ref_resolution.insert((h, s, r), res);
            }
            if !apply_first {
                apply_definitions(
                    catalog,
                    host.dialect,
                    statement,
                    (h, s),
                    &entity_ref,
                    &mut def_entity,
                    &mut def_parent,
                );
            }
        }
    }
    for (h, host) in hosts.iter().enumerate() {
        let catalog = catalogs.entry(dialect_slot(host.dialect)).or_default();
        for (s, statement) in host.outline.statements.iter().enumerate() {
            if !is_query(statement.kind) {
                continue;
            }
            for (r, reference) in statement.references.iter().enumerate() {
                let res = resolve_reference(catalog, host.dialect, statement, reference, None, true);
                ref_resolution.insert((h, s, r), res);
            }
        }
    }

    // Assemble facts.
    let to_range = |e: &c::Extent| SqlByteRange {
        start: e.start as u32,
        end: e.end as u32,
    };
    let entity_ref_of = |catalog: &Catalog, index: usize| catalog.entities[index].id.clone();
    let mut out = Vec::new();
    for (h, host) in hosts.iter().enumerate() {
        let catalog = &catalogs[&dialect_slot(host.dialect)];
        let mut facts = Vec::new();
        for (s, statement) in host.outline.statements.iter().enumerate() {
            facts.push(SqlFact::Statement {
                class: statement_kind(statement.kind),
                extent: to_range(&statement.extent),
                support: support(statement.support),
            });
            for (d, definition) in statement.definitions.iter().enumerate() {
                let entity = def_entity.get(&(h, s, d)).copied().flatten();
                let parent = def_parent.get(&(h, s, d)).copied().flatten();
                facts.push(SqlFact::Definition(Box::new(SqlDefinition {
                    kind: object_kind(definition.kind),
                    action: action(&definition.action, &to_range),
                    conditional: definition.conditional,
                    schema: definition.schema.clone(),
                    name: definition.name.clone(),
                    parent: definition.parent.clone(),
                    parent_schema: definition.parent_schema.clone(),
                    name_extent: to_range(&definition.name_extent),
                    extent: to_range(&definition.extent),
                    detail: definition.detail.clone(),
                    statement: statement_ordinal[h][s],
                    entity: entity.map(|index| entity_ref_of(catalog, index)),
                    parent_entity: parent.map(|index| entity_ref_of(catalog, index)),
                    live_at_head: entity.is_some_and(|index| catalog.entities[index].alive),
                    head_name: entity.map(|index| catalog.entities[index].name.clone()),
                    head_parent: entity
                        .and_then(|index| catalog.entities[index].parent)
                        .map(|parent| catalog.entities[parent].name.clone()),
                })));
            }
            for (r, reference) in statement.references.iter().enumerate() {
                let res = ref_resolution
                    .remove(&(h, s, r))
                    .unwrap_or(Res::Unresolved(SqlUnresolved::PartialStatement));
                facts.push(SqlFact::Reference(Box::new(SqlReference {
                    kind: ref_kind(reference.kind),
                    role: ref_role(reference.role),
                    schema: reference.schema.clone(),
                    qualifier: reference.qualifier.clone(),
                    name: reference.name.clone(),
                    extent: to_range(&reference.extent),
                    full_extent: to_range(&reference.full_extent),
                    statement: statement_ordinal[h][s],
                    statement_kind: statement_kind(statement.kind),
                    in_object: statement
                        .definitions
                        .iter()
                        .find(|d| {
                            matches!(d.action, c::DefinitionAction::Create)
                                && !matches!(d.kind, c::ObjectKind::Column | c::ObjectKind::Schema)
                        })
                        .map(|d| SqlObjectRef { kind: object_kind(d.kind), name: d.name.clone() }),
                    resolution: res.into_fact(catalog),
                })));
            }
        }
        out.push(facts);
    }
    out
}

fn is_query(kind: c::StatementKind) -> bool {
    matches!(
        kind,
        c::StatementKind::Select
            | c::StatementKind::Insert
            | c::StatementKind::Update
            | c::StatementKind::Delete
            | c::StatementKind::Merge
            | c::StatementKind::Other
    )
}

fn apply_definitions(
    catalog: &mut Catalog,
    dialect: SqlDialect,
    statement: &c::StatementOutline,
    (h, s): (usize, usize),
    entity_ref: &dyn Fn(usize, usize, usize) -> SqlEntityRef,
    def_entity: &mut HashMap<(usize, usize, usize), Option<usize>>,
    def_parent: &mut HashMap<(usize, usize, usize), Option<usize>>,
) {
    let statement_columns_known = matches!(statement.support, c::StatementSupport::Full);
    for (d, definition) in statement.definitions.iter().enumerate() {
        let ns = ns_of(definition.kind);
        let schema = definition
            .schema
            .clone()
            .unwrap_or_else(|| default_schema(dialect).to_owned());
        let key = (ns, schema.clone(), definition.name.clone());
        // Relation a column / index / trigger belongs to.
        let parent_relation = definition.parent.as_ref().and_then(|parent| {
            let parent_key = (
                Ns::Relation,
                definition
                    .parent_schema
                    .clone()
                    .or_else(|| definition.schema.clone())
                    .unwrap_or_else(|| default_schema(dialect).to_owned()),
                parent.clone(),
            );
            catalog.lookup_live(&parent_key).last().copied()
        });
        def_parent.insert((h, s, d), parent_relation);
        match &definition.action {
            c::DefinitionAction::Create => {
                // A column shares its spelling with no other object kind.
                let existing = if definition.kind == c::ObjectKind::Column {
                    None
                } else {
                    catalog.lookup_live(&key).last().copied()
                };
                match (existing, definition.conditional, definition.kind) {
                    // IF NOT EXISTS on an existing object, or OR REPLACE of a
                    // view/function: the entity continues.
                    (Some(index), true, kind) if kind != c::ObjectKind::Column => {
                        def_entity.insert((h, s, d), Some(index));
                    }
                    _ => {
                        if let Some(index) = existing {
                            // Re-creation without a drop: a new epoch supersedes.
                            catalog.kill(index, ns);
                        }
                        let columns_known = match definition.kind {
                            c::ObjectKind::Table => statement_columns_known,
                            c::ObjectKind::View => false,
                            _ => true,
                        };
                        let index = catalog.add(
                            Entity {
                                id: entity_ref(h, s, d),
                                kind: definition.kind,
                                schema,
                                name: definition.name.clone(),
                                alive: true,
                                columns: Vec::new(),
                                columns_known,
                                parent: parent_relation,
                            },
                            dialect,
                            ns,
                        );
                        if definition.kind == c::ObjectKind::Column {
                            // Columns are owned by their relation, not the name table.
                            catalog.unlink(index, ns);
                            if let Some(relation) = parent_relation {
                                catalog.entities[relation].columns.push(index);
                                if matches!(
                                    catalog.entities[relation].kind,
                                    c::ObjectKind::View
                                ) {
                                    catalog.entities[relation].columns_known = true;
                                }
                            }
                        }
                        def_entity.insert((h, s, d), Some(index));
                    }
                }
            }
            c::DefinitionAction::AddColumn => {
                let index = catalog.add(
                    Entity {
                        id: entity_ref(h, s, d),
                        kind: c::ObjectKind::Column,
                        schema,
                        name: definition.name.clone(),
                        alive: true,
                        columns: Vec::new(),
                        columns_known: true,
                        parent: parent_relation,
                    },
                    dialect,
                    Ns::Relation,
                );
                catalog.unlink(index, Ns::Relation);
                if let Some(relation) = parent_relation {
                    catalog.entities[relation].columns.push(index);
                }
                def_entity.insert((h, s, d), Some(index));
            }
            c::DefinitionAction::DropColumn => {
                let found = parent_relation.and_then(|relation| catalog.column_of(relation, &definition.name));
                if let Some(column) = found {
                    catalog.entities[column].alive = false;
                }
                def_entity.insert((h, s, d), found);
            }
            c::DefinitionAction::Drop => {
                let found = catalog
                    .lookup_live(&key)
                    .last()
                    .copied()
                    .or_else(|| catalog.dropped.get(&key).copied());
                if let Some(index) = found {
                    if catalog.entities[index].alive {
                        catalog.kill(index, ns);
                    }
                }
                def_entity.insert((h, s, d), found);
            }
            c::DefinitionAction::Rename { to, .. } => {
                if definition.kind == c::ObjectKind::Column {
                    let found = parent_relation.and_then(|relation| catalog.column_of(relation, &definition.name));
                    if let Some(column) = found {
                        catalog.entities[column].name = to.clone();
                    }
                    def_entity.insert((h, s, d), found);
                } else {
                    let found = catalog.lookup_live(&key).last().copied();
                    if let Some(index) = found {
                        catalog.unlink(index, ns);
                        catalog.entities[index].name = to.clone();
                        catalog.current.entry((ns, schema, to.clone())).or_default().push(index);
                    }
                    def_entity.insert((h, s, d), found);
                }
            }
            c::DefinitionAction::Alter => {
                let found = if definition.kind == c::ObjectKind::Column {
                    parent_relation.and_then(|relation| catalog.column_of(relation, &definition.name))
                } else {
                    catalog.lookup_live(&key).last().copied()
                };
                def_entity.insert((h, s, d), found);
            }
        }
    }
}

/// Internal binding result with entity indexes; converted once the catalog is final.
enum Res {
    Resolved(usize),
    Expands(Vec<usize>),
    Relation(usize),
    Ambiguous(Vec<usize>, bool),
    Local,
    Unresolved(SqlUnresolved),
}
impl Res {
    fn into_fact(self, catalog: &Catalog) -> SqlResolution {
        let id = |index: usize| catalog.entities[index].id.clone();
        let live = |index: usize| catalog.entities[index].alive;
        match self {
            Res::Resolved(index) => SqlResolution::Resolved { target: id(index), live: live(index) },
            Res::Expands(list) => SqlResolution::Expands { targets: list.into_iter().map(id).collect() },
            Res::Relation(index) => SqlResolution::Relation { relation: id(index), live: live(index) },
            Res::Ambiguous(list, more) => SqlResolution::Ambiguous {
                candidates: list.into_iter().map(id).collect(),
                more,
            },
            Res::Local => SqlResolution::Local,
            Res::Unresolved(reason) => SqlResolution::Unresolved { reason },
        }
    }
}

fn contains(outer: &c::Extent, inner: &c::Extent) -> bool {
    outer.start <= inner.start && inner.end <= outer.end
}

fn find_relation(
    catalog: &Catalog,
    dialect: SqlDialect,
    schema: Option<&str>,
    name: &str,
    at_position: bool,
) -> Option<usize> {
    let key = catalog.key(Ns::Relation, schema, name, dialect);
    let live = catalog.lookup_live(&key);
    match live.len() {
        0 => {
            // Not live now: a dropped entity is still the thing a stale
            // mention names; a forward reference binds the eventual object
            // in the head pass.
            let _ = at_position;
            catalog.dropped.get(&key).copied()
        }
        _ => live.last().copied(),
    }
}

fn resolve_reference(
    catalog: &Catalog,
    dialect: SqlDialect,
    statement: &c::StatementOutline,
    reference: &c::Reference,
    position: Option<(usize, usize, &HashMap<(usize, usize, usize), Option<usize>>)>,
    _head_fallback: bool,
) -> Res {
    let _ = position;
    match reference.kind {
        c::RefKind::Relation => {
            if reference.schema.is_none()
                && statement.scope.iter().any(|entry| {
                    entry.kind == c::ScopeKind::Cte
                        && entry.visible_as == reference.name
                        && contains(&entry.region, &reference.extent)
                })
            {
                return Res::Local;
            }
            match find_relation(catalog, dialect, reference.schema.as_deref(), &reference.name, true) {
                Some(index) => Res::Resolved(index),
                None => Res::Unresolved(SqlUnresolved::NoSuchObject),
            }
        }
        c::RefKind::Column => resolve_column(catalog, dialect, statement, reference),
        c::RefKind::Function => lookup_many(catalog, dialect, Ns::Function, reference),
        c::RefKind::Type => lookup_many(catalog, dialect, Ns::Type, reference),
        c::RefKind::Index => lookup_many(catalog, dialect, Ns::Index, reference),
        c::RefKind::Sequence => lookup_many(catalog, dialect, Ns::Sequence, reference),
        c::RefKind::Trigger => lookup_many(catalog, dialect, Ns::Trigger, reference),
        c::RefKind::Schema => {
            let key = (Ns::Schema, String::new(), reference.name.clone());
            let live = catalog.current.get(&key);
            match live.and_then(|list| list.last().copied()) {
                Some(index) => Res::Resolved(index),
                None => Res::Unresolved(SqlUnresolved::NoSuchObject),
            }
        }
    }
}

fn lookup_many(
    catalog: &Catalog,
    dialect: SqlDialect,
    ns: Ns,
    reference: &c::Reference,
) -> Res {
    let key = catalog.key(ns, reference.schema.as_deref(), &reference.name, dialect);
    let live = catalog.lookup_live(&key);
    match live.len() {
        0 => match catalog.dropped.get(&key) {
            Some(index) => Res::Resolved(*index),
            None => Res::Unresolved(SqlUnresolved::NoSuchObject),
        },
        1 => Res::Resolved(live[0]),
        n => Res::Ambiguous(
            live.iter().take(MAX_CANDIDATES).copied().collect(),
            n > MAX_CANDIDATES,
        ),
    }
}

/// Outcome of looking for a column name inside one relation-like scope entry.
enum Member {
    Found(usize),
    Missing,
    Unknown(Option<usize>),
}

fn entry_relation(
    catalog: &Catalog,
    dialect: SqlDialect,
    statement: &c::StatementOutline,
    entry: &c::ScopeEntry,
) -> Option<Result<usize, ()>> {
    match entry.kind {
        c::ScopeKind::Relation => {
            let name = entry.relation.as_deref()?;
            // A CTE of the same name shadows the catalog within its region.
            if entry.relation_schema.is_none()
                && statement.scope.iter().any(|cte| {
                    cte.kind == c::ScopeKind::Cte
                        && cte.visible_as == name
                        && contains(&cte.region, &entry.region)
                })
            {
                return Some(Err(()));
            }
            find_relation(catalog, dialect, entry.relation_schema.as_deref(), name, true).map(Ok)
        }
        c::ScopeKind::TriggerRow => {
            let table = statement
                .definitions
                .iter()
                .find(|d| d.kind == c::ObjectKind::Trigger)?
                .parent
                .as_deref()?;
            let parent_schema = statement
                .definitions
                .iter()
                .find(|d| d.kind == c::ObjectKind::Trigger)
                .and_then(|d| d.parent_schema.as_deref());
            find_relation(catalog, dialect, parent_schema, table, true).map(Ok)
        }
        _ => Some(Err(())),
    }
}

fn member(
    catalog: &Catalog,
    dialect: SqlDialect,
    statement: &c::StatementOutline,
    entry: &c::ScopeEntry,
    column: &str,
) -> Member {
    match entry.kind {
        c::ScopeKind::Cte | c::ScopeKind::Subquery | c::ScopeKind::Function => {
            match &entry.columns {
                Some(columns) if columns.iter().any(|name| name == column) => Member::Unknown(None),
                Some(_) => Member::Missing,
                None => Member::Unknown(None),
            }
        }
        _ => match entry_relation(catalog, dialect, statement, entry) {
            Some(Ok(relation)) => match catalog.column_of(relation, column) {
                Some(found) => Member::Found(found),
                None if catalog.entities[relation].columns_known => Member::Missing,
                None => Member::Unknown(Some(relation)),
            },
            Some(Err(())) => Member::Unknown(None),
            None => Member::Missing,
        },
    }
}

fn resolve_column(
    catalog: &Catalog,
    dialect: SqlDialect,
    statement: &c::StatementOutline,
    reference: &c::Reference,
) -> Res {
    // Entries that can see this reference, innermost region first.
    let mut visible: Vec<&c::ScopeEntry> = statement
        .scope
        .iter()
        .filter(|entry| contains(&entry.region, &reference.extent))
        .collect();
    visible.sort_by_key(|entry| entry.region.end - entry.region.start);
    if reference.name == "*" {
        let wanted: Vec<&c::ScopeEntry> = match &reference.qualifier {
            Some(qualifier) => visible
                .iter()
                .copied()
                .filter(|entry| &entry.visible_as == qualifier)
                .take(1)
                .collect(),
            None => {
                let innermost = visible.first().map(|entry| entry.region.clone());
                visible
                    .iter()
                    .copied()
                    .filter(|entry| Some(&entry.region) == innermost.as_ref())
                    .collect()
            }
        };
        if wanted.is_empty() {
            return qualifier_as_relation(catalog, dialect, reference)
                .map_or(Res::Unresolved(SqlUnresolved::NoScope), |relation| {
                    Res::Expands(vec![relation])
                });
        }
        let mut relations = Vec::new();
        let mut opaque = false;
        for entry in wanted {
            match entry_relation(catalog, dialect, statement, entry) {
                Some(Ok(relation)) => relations.push(relation),
                _ => opaque = true,
            }
        }
        return match (relations.is_empty(), opaque) {
            (true, _) => Res::Unresolved(SqlUnresolved::OpaqueScope),
            _ => Res::Expands(relations),
        };
    }
    if let Some(qualifier) = &reference.qualifier {
        let entry = visible
            .iter()
            .copied()
            .find(|entry| &entry.visible_as == qualifier);
        let entry_found = entry.is_some();
        let direct = match entry {
            Some(entry) => member(catalog, dialect, statement, entry, &reference.name),
            None => match qualifier_as_relation(catalog, dialect, reference) {
                Some(relation) => match catalog.column_of(relation, &reference.name) {
                    Some(found) => Member::Found(found),
                    None if catalog.entities[relation].columns_known => Member::Missing,
                    None => Member::Unknown(Some(relation)),
                },
                None => return Res::Unresolved(SqlUnresolved::NoScope),
            },
        };
        let _ = entry_found;
        return match direct {
            Member::Found(index) => Res::Resolved(index),
            Member::Missing => Res::Unresolved(SqlUnresolved::NoSuchColumn),
            Member::Unknown(Some(relation)) => Res::Relation(relation),
            Member::Unknown(None) => Res::Unresolved(SqlUnresolved::OpaqueScope),
        };
    }
    if visible.is_empty() {
        return resolve_unscoped(catalog, dialect, statement, reference);
    }
    // Unqualified: innermost group that can supply the name wins.
    let mut index = 0;
    let mut saw_scope = false;
    while index < visible.len() {
        let region = visible[index].region.clone();
        let mut group = Vec::new();
        while index < visible.len() && visible[index].region == region {
            group.push(visible[index]);
            index += 1;
        }
        saw_scope = true;
        let mut found = Vec::new();
        let mut unknown_relation = None;
        let mut opaque = false;
        for entry in &group {
            match member(catalog, dialect, statement, entry, &reference.name) {
                Member::Found(column) => found.push(column),
                Member::Missing => {}
                Member::Unknown(Some(relation)) => unknown_relation = Some(relation),
                Member::Unknown(None) => opaque = true,
            }
        }
        match found.len() {
            0 => {
                if let Some(relation) = unknown_relation {
                    return Res::Relation(relation);
                }
                if opaque {
                    return Res::Unresolved(SqlUnresolved::OpaqueScope);
                }
            }
            1 if !opaque && unknown_relation.is_none() => return Res::Resolved(found[0]),
            1 => return Res::Resolved(found[0]),
            n => {
                found.truncate(MAX_CANDIDATES);
                return Res::Ambiguous(found, n > MAX_CANDIDATES);
            }
        }
    }
    if saw_scope {
        Res::Unresolved(SqlUnresolved::NoSuchColumn)
    } else {
        Res::Unresolved(SqlUnresolved::NoScope)
    }
}

/// A written column with no FROM item around it: a key, foreign-key or index
/// column list, or a constraint expression. Syntax ties it to one relation: the
/// nearest relation mentioned before it in the statement, else the table the
/// statement itself defines.
fn resolve_unscoped(
    catalog: &Catalog,
    dialect: SqlDialect,
    statement: &c::StatementOutline,
    reference: &c::Reference,
) -> Res {
    let mentioned = statement
        .references
        .iter()
        .filter(|other| {
            other.kind == c::RefKind::Relation && other.extent.end <= reference.extent.start
        })
        .max_by_key(|other| other.extent.end)
        .and_then(|other| find_relation(catalog, dialect, other.schema.as_deref(), &other.name, true));
    let defined = || {
        statement
            .definitions
            .iter()
            .find(|d| matches!(d.kind, c::ObjectKind::Table | c::ObjectKind::View))
            .and_then(|d| find_relation(catalog, dialect, d.schema.as_deref(), &d.name, true))
    };
    let Some(relation) = mentioned.or_else(defined) else {
        return Res::Unresolved(SqlUnresolved::NoScope);
    };
    match catalog.column_of(relation, &reference.name) {
        Some(column) => Res::Resolved(column),
        None if catalog.entities[relation].columns_known => Res::Unresolved(SqlUnresolved::NoSuchColumn),
        None => Res::Relation(relation),
    }
}

fn qualifier_as_relation(
    catalog: &Catalog,
    dialect: SqlDialect,
    reference: &c::Reference,
) -> Option<usize> {
    let qualifier = reference.qualifier.as_deref().or(Some(&reference.name))?;
    let relation = find_relation(catalog, dialect, reference.schema.as_deref(), qualifier, true)?;
    Some(relation)
}

fn statement_kind(kind: c::StatementKind) -> SqlStatementKind {
    use c::StatementKind as K;
    match kind {
        K::CreateTable => SqlStatementKind::CreateTable,
        K::CreateView => SqlStatementKind::CreateView,
        K::CreateIndex => SqlStatementKind::CreateIndex,
        K::CreateTrigger => SqlStatementKind::CreateTrigger,
        K::CreateFunction => SqlStatementKind::CreateFunction,
        K::CreateType => SqlStatementKind::CreateType,
        K::CreateSchema => SqlStatementKind::CreateSchema,
        K::CreateSequence => SqlStatementKind::CreateSequence,
        K::AlterTable => SqlStatementKind::AlterTable,
        K::Drop => SqlStatementKind::Drop,
        K::Select => SqlStatementKind::Select,
        K::Insert => SqlStatementKind::Insert,
        K::Update => SqlStatementKind::Update,
        K::Delete => SqlStatementKind::Delete,
        K::Merge => SqlStatementKind::Merge,
        K::Other => SqlStatementKind::Other,
    }
}
fn gap(gap: c::Gap) -> SqlGap {
    match gap {
        c::Gap::OpaqueBody => SqlGap::OpaqueBody,
        c::Gap::DynamicSql => SqlGap::DynamicSql,
        c::Gap::UnmodeledConstruct => SqlGap::UnmodeledConstruct,
        c::Gap::UnmodeledStatement => SqlGap::UnmodeledStatement,
        c::Gap::Limit => SqlGap::Limit,
    }
}
fn support(support: c::StatementSupport) -> SqlStatementSupport {
    match support {
        c::StatementSupport::Full => SqlStatementSupport::Full,
        c::StatementSupport::Partial(g) => SqlStatementSupport::Partial(gap(g)),
        c::StatementSupport::Unsupported(g) => SqlStatementSupport::Unsupported(gap(g)),
    }
}
fn object_kind(kind: c::ObjectKind) -> SqlObjectKind {
    match kind {
        c::ObjectKind::Schema => SqlObjectKind::Schema,
        c::ObjectKind::Table => SqlObjectKind::Table,
        c::ObjectKind::View => SqlObjectKind::View,
        c::ObjectKind::Column => SqlObjectKind::Column,
        c::ObjectKind::Index => SqlObjectKind::Index,
        c::ObjectKind::Trigger => SqlObjectKind::Trigger,
        c::ObjectKind::Function => SqlObjectKind::Function,
        c::ObjectKind::Type => SqlObjectKind::Type,
        c::ObjectKind::Sequence => SqlObjectKind::Sequence,
    }
}
fn action(
    action: &c::DefinitionAction,
    range: &dyn Fn(&c::Extent) -> SqlByteRange,
) -> SqlDefinitionAction {
    match action {
        c::DefinitionAction::Create => SqlDefinitionAction::Create,
        c::DefinitionAction::Drop => SqlDefinitionAction::Drop,
        c::DefinitionAction::AddColumn => SqlDefinitionAction::AddColumn,
        c::DefinitionAction::DropColumn => SqlDefinitionAction::DropColumn,
        c::DefinitionAction::Rename { to, to_extent } => SqlDefinitionAction::Rename {
            to: to.clone(),
            to_extent: range(to_extent),
        },
        c::DefinitionAction::Alter => SqlDefinitionAction::Alter,
    }
}
fn ref_kind(kind: c::RefKind) -> SqlRefKind {
    match kind {
        c::RefKind::Relation => SqlRefKind::Relation,
        c::RefKind::Column => SqlRefKind::Column,
        c::RefKind::Function => SqlRefKind::Function,
        c::RefKind::Type => SqlRefKind::Type,
        c::RefKind::Schema => SqlRefKind::Schema,
        c::RefKind::Index => SqlRefKind::Index,
        c::RefKind::Sequence => SqlRefKind::Sequence,
        c::RefKind::Trigger => SqlRefKind::Trigger,
    }
}
fn ref_role(role: c::RefRole) -> SqlRefRole {
    match role {
        c::RefRole::Read => SqlRefRole::Read,
        c::RefRole::Write => SqlRefRole::Write,
        c::RefRole::Schema => SqlRefRole::Schema,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use c::*;

    fn ex(source: &str, needle: &str, nth: usize) -> Extent {
        let start = source.match_indices(needle).nth(nth).expect("needle").0;
        Extent { start, end: start + needle.len() }
    }
    fn def(source: &str, stmt_text: &str, kind: ObjectKind, action: DefinitionAction, name: &str, parent: Option<&str>, nth: usize) -> Definition {
        let stmt = ex(source, stmt_text, 0);
        let name_extent = {
            let e = ex(&source[stmt.start..stmt.end], name, nth);
            Extent { start: stmt.start + e.start, end: stmt.start + e.end }
        };
        Definition {
            kind, action, conditional: false, schema: None, name: name.into(),
            parent: parent.map(str::to_owned), parent_schema: None, name_extent: name_extent.clone(),
            extent: if kind == ObjectKind::Column { name_extent } else { stmt }, detail: None,
        }
    }
    fn reference(source: &str, within: &str, kind: RefKind, role: RefRole, qualifier: Option<&str>, name: &str) -> Reference {
        let stmt = ex(source, within, 0);
        let sub = &source[stmt.start..stmt.end];
        let (full, ext) = match qualifier {
            Some(q) => {
                let text = format!("{q}.{name}");
                let f = ex(sub, &text, 0);
                (Extent { start: stmt.start + f.start, end: stmt.start + f.end },
                 Extent { start: stmt.start + f.end - name.len(), end: stmt.start + f.end })
            }
            None => {
                // first whole-word occurrence
                let mut at = None;
                for (i, _) in sub.match_indices(name) {
                    let before = sub[..i].chars().last();
                    let after = sub[i + name.len()..].chars().next();
                    if before.is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == '.'))
                        && after.is_none_or(|c| !(c.is_alphanumeric() || c == '_'))
                    { at = Some(i); break; }
                }
                let i = at.expect("word");
                let e = Extent { start: stmt.start + i, end: stmt.start + i + name.len() };
                (e.clone(), e)
            }
        };
        Reference { kind, role, schema: None, qualifier: qualifier.map(str::to_owned), name: name.into(), extent: ext, full_extent: full }
    }

    #[test]
    fn binds_columns_across_files_through_rename_and_drop() {
        let schema = "CREATE TABLE users (id INTEGER, name TEXT)";
        let migrate = "ALTER TABLE users RENAME TO accounts; ALTER TABLE accounts DROP COLUMN name";
        let queries = "SELECT u.id, u.name FROM users u";
        let q_all = "SELECT u.id, u.name FROM users u";
        let schema_stmt = ex(schema, schema, 0);
        let schema_outline = Outline {
            coverage: OutlineCoverage::Complete,
            statements: vec![StatementOutline {
                kind: StatementKind::CreateTable,
                extent: schema_stmt,
                support: StatementSupport::Full,
                definitions: vec![
                    def(schema, schema, ObjectKind::Table, DefinitionAction::Create, "users", None, 0),
                    def(schema, schema, ObjectKind::Column, DefinitionAction::Create, "id", Some("users"), 0),
                    def(schema, schema, ObjectKind::Column, DefinitionAction::Create, "name", Some("users"), 0),
                ],
                references: vec![],
                scope: vec![],
            }],
        };
        let rename = ex(migrate, "ALTER TABLE users RENAME TO accounts", 0);
        let drop_c = ex(migrate, "ALTER TABLE accounts DROP COLUMN name", 0);
        let mut rename_def = def(migrate, "ALTER TABLE users RENAME TO accounts", ObjectKind::Table, DefinitionAction::Alter, "users", None, 0);
        rename_def.action = DefinitionAction::Rename { to: "accounts".into(), to_extent: ex(migrate, "accounts", 0) };
        let migrate_outline = Outline {
            coverage: OutlineCoverage::Complete,
            statements: vec![
                StatementOutline {
                    kind: StatementKind::AlterTable, extent: rename, support: StatementSupport::Full,
                    definitions: vec![rename_def], references: vec![], scope: vec![],
                },
                StatementOutline {
                    kind: StatementKind::AlterTable, extent: drop_c.clone(), support: StatementSupport::Full,
                    definitions: vec![{
                        let mut d = def(migrate, "ALTER TABLE accounts DROP COLUMN name", ObjectKind::Column, DefinitionAction::DropColumn, "name", Some("accounts"), 0);
                        d.extent = Extent { start: ex(migrate, "DROP COLUMN name", 0).start, end: drop_c.end };
                        d
                    }],
                    references: vec![], scope: vec![],
                },
            ],
        };
        let q = ex(q_all, q_all, 0);
        let query_outline = Outline {
            coverage: OutlineCoverage::Complete,
            statements: vec![StatementOutline {
                kind: StatementKind::Select, extent: q.clone(), support: StatementSupport::Full,
                definitions: vec![],
                references: vec![
                    reference(queries, queries, RefKind::Column, RefRole::Read, Some("u"), "id"),
                    reference(queries, queries, RefKind::Column, RefRole::Read, Some("u"), "name"),
                    reference(queries, queries, RefKind::Relation, RefRole::Read, None, "users"),
                ],
                scope: vec![ScopeEntry {
                    kind: ScopeKind::Relation, visible_as: "u".into(), relation: Some("users".into()),
                    relation_schema: None, columns: None, region: q,
                }],
            }],
        };
        assert!(schema_outline.validate(schema.as_bytes()), "schema outline invalid");
        assert!(migrate_outline.validate(migrate.as_bytes()), "migration outline invalid");
        assert!(query_outline.validate(queries.as_bytes()), "query outline invalid");
        let hosts = [
            HostOutline { path: "a_schema.sql", dialect: SqlDialect::Sqlite, outline: &schema_outline },
            HostOutline { path: "b_migrate.sql", dialect: SqlDialect::Sqlite, outline: &migrate_outline },
            HostOutline { path: "c_queries.sql", dialect: SqlDialect::Sqlite, outline: &query_outline },
        ];
        let facts = bind_project(&hosts);
        // The table keeps its entity across the rename; the query that still
        // spells the old name no longer binds, and the dropped column is
        // reported as the dropped column's entity... no: the query's `u.name`
        // finds no live column.
        let table = SqlEntityRef { path: "a_schema.sql".into(), ordinal: 1 };
        let SqlFact::Definition(table_def) = &facts[0][1] else { panic!("table def") };
        assert_eq!(table_def.entity.as_ref(), Some(&table));
        assert!(table_def.live_at_head);
        let SqlFact::Definition(name_def) = &facts[0][3] else { panic!("column def") };
        assert!(!name_def.live_at_head, "dropped column is not live at head");
        let refs: Vec<&SqlReference> = facts[2].iter().filter_map(|f| match f { SqlFact::Reference(r) => Some(r.as_ref()), _ => None }).collect();
        assert_eq!(refs.len(), 3);
        // Query still names the old table: stale after the rename.
        let users_ref = refs.iter().find(|r| r.kind == SqlRefKind::Relation).unwrap();
        assert!(matches!(users_ref.resolution, SqlResolution::Unresolved { reason: SqlUnresolved::NoSuchObject }), "{:?}", users_ref.resolution);
        // The alias resolves through the scope, whose relation is `users`,
        // which no longer exists: no relation, so columns are unresolved.
        let id_ref = refs.iter().find(|r| r.name == "id").unwrap();
        assert!(matches!(id_ref.resolution, SqlResolution::Unresolved { .. }), "{:?}", id_ref.resolution);
    }
}

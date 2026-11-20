//! Tag-first decoding for canonical SQL rows.
//!
//! Canonical Serde output writes each internal tag first. Deserializing the
//! remaining map directly avoids Serde's internally tagged `Content` buffer.
use super::sql_relation::{SqlAnalysisRecord, SqlAttemptOutcome, SqlHostTerminal, SqlInventory};
use backend_semantic::sql_analysis::*;
use serde::de::{self, MapAccess, SeqAccess, Visitor, value::MapAccessDeserializer};
use serde::{Deserialize, Deserializer};
use std::fmt;

macro_rules! body {
    ($name:ident { $($field:ident: $ty:ty),* $(,)? }) => {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct $name { $($field: $ty),* }
    };
}
// Convert each decoded string through Box<str>, whose allocation layout is
// exactly the UTF-8 byte length. This avoids relying on Vec growth policy.
struct DirectString(String);
impl<'de> Deserialize<'de> for DirectString {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct StringVisitor;
        impl Visitor<'_> for StringVisitor {
            type Value = DirectString;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a SQL string")
            }
            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(DirectString(String::from(Box::<str>::from(value))))
            }
        }
        deserializer.deserialize_str(StringVisitor)
    }
}
impl From<DirectString> for String {
    fn from(value: DirectString) -> Self {
        value.0
    }
}

body!(SelectionBody { project: [u8; 32], source_root: DirectString, config_input: [u8; 32], config_source: [u8; 32], config_length: u32, discovery: Option<[u8; 32]>, policy_complete: bool, implicit: bool, gaps: DirectGaps, inventory: SqlInventory, inventory_digest: [u8; 32], host_count: u32 });
body!(AttemptBody { project: [u8; 32], outcome: SqlAttemptOutcome, served_selection: Option<[u8; 32]> });
body!(HostBody { project: [u8; 32], path: DirectString, dialect: SqlDialect, input: Option<[u8; 32]>, source: Option<[u8; 32]>, length: Option<u64>, producer: Option<[u8; 32]>, settings: Option<[u8; 32]>, context: Option<[u8; 32]>, terminal: DirectTerminal, fact_count: u32, fact_digest: [u8; 32] });
body!(FactBody {
    host: [u8; 32],
    ordinal: u32,
    fact: DirectFact
});
body!(RelationBody {
    name: DirectString,
    relation_kind: SqliteRelationKind
});
body!(LineageBody { output_name: DirectString, ordinal: u32, origin: Option<DirectOrigin> });
body!(DirectOrigin {
    relation: DirectString,
    column: DirectString
});
impl From<DirectOrigin> for SqliteColumnOrigin {
    fn from(b: DirectOrigin) -> Self {
        Self {
            relation: b.relation.into(),
            column: b.column.into(),
        }
    }
}
body!(ReadBody {
    relation_oid: u32,
    attribute: u16,
    extent: SqlByteRange
});
body!(DiagnosticBody { code: SqlDiagnostic, extent: Option<SqlByteRange> });
body!(StatementBody {
    class: SqlStatementKind,
    extent: SqlByteRange,
    support: SqlStatementSupport
});

body!(DirectGaps {
    unavailable_hosts: u32,
    native_error_hosts: u32,
    partial_statements: u32,
    unsupported_statements: u32
});
impl From<DirectGaps> for SqlGapCounts {
    fn from(b: DirectGaps) -> Self {
        Self {
            unavailable_hosts: b.unavailable_hosts,
            native_error_hosts: b.native_error_hosts,
            partial_statements: b.partial_statements,
            unsupported_statements: b.unsupported_statements,
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
enum DirectTerminal {
    Parsed {
        source: SqlCoverage,
        contextual: SqlCoverage,
        sqlite_lineage: Option<SqliteLineageCoverage>,
    },
    NativeError,
    Unavailable(super::sql_relation::SqlUnavailable),
}
impl From<DirectTerminal> for SqlHostTerminal {
    fn from(value: DirectTerminal) -> Self {
        match value {
            DirectTerminal::Parsed {
                source,
                contextual,
                sqlite_lineage,
            } => Self::Parsed {
                source,
                contextual,
                sqlite_lineage,
            },
            DirectTerminal::NativeError => Self::NativeError,
            DirectTerminal::Unavailable(value) => Self::Unavailable(value),
        }
    }
}
body!(DirectEntity {
    path: DirectString,
    ordinal: u32
});
impl From<DirectEntity> for SqlEntityRef {
    fn from(b: DirectEntity) -> Self {
        Self {
            path: b.path.into(),
            ordinal: b.ordinal,
        }
    }
}
body!(DirectObject {
    kind: SqlObjectKind,
    name: DirectString
});
impl From<DirectObject> for SqlObjectRef {
    fn from(b: DirectObject) -> Self {
        Self {
            kind: b.kind,
            name: b.name.into(),
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
enum DirectAction {
    Create,
    Drop,
    AddColumn,
    DropColumn,
    Rename {
        to: DirectString,
        to_extent: SqlByteRange,
    },
    Alter,
}
impl From<DirectAction> for SqlDefinitionAction {
    fn from(value: DirectAction) -> Self {
        match value {
            DirectAction::Create => Self::Create,
            DirectAction::Drop => Self::Drop,
            DirectAction::AddColumn => Self::AddColumn,
            DirectAction::DropColumn => Self::DropColumn,
            DirectAction::Rename { to, to_extent } => Self::Rename {
                to: to.into(),
                to_extent,
            },
            DirectAction::Alter => Self::Alter,
        }
    }
}
body!(DefinitionBody {
    kind: SqlObjectKind, action: DirectAction, conditional: bool, schema: Option<DirectString>, name: DirectString,
    parent: Option<DirectString>, parent_schema: Option<DirectString>, name_extent: SqlByteRange, extent: SqlByteRange,
    detail: Option<DirectString>, statement: u32, entity: Option<DirectEntity>, parent_entity: Option<DirectEntity>,
    live_at_head: bool, head_name: Option<DirectString>, head_parent: Option<DirectString>
});
impl From<DefinitionBody> for SqlDefinition {
    fn from(b: DefinitionBody) -> Self {
        Self {
            kind: b.kind,
            action: b.action.into(),
            conditional: b.conditional,
            schema: b.schema.map(Into::into),
            name: b.name.into(),
            parent: b.parent.map(Into::into),
            parent_schema: b.parent_schema.map(Into::into),
            name_extent: b.name_extent,
            extent: b.extent,
            detail: b.detail.map(Into::into),
            statement: b.statement,
            entity: b.entity.map(Into::into),
            parent_entity: b.parent_entity.map(Into::into),
            live_at_head: b.live_at_head,
            head_name: b.head_name.map(Into::into),
            head_parent: b.head_parent.map(Into::into),
        }
    }
}

// This is the only variable-length vector in a SQL row. Allocate the
// validated schema maximum once and reject the first surplus element before
// decoding its contents. The owned vector's capacity is therefore explicit.
struct RejectExtra;
impl<'de> Deserialize<'de> for RejectExtra {
    fn deserialize<D: Deserializer<'de>>(_: D) -> Result<Self, D::Error> {
        Err(de::Error::custom("too many SQL entity references"))
    }
}
struct BoundedEntities(Vec<SqlEntityRef>);
impl<'de> Deserialize<'de> for BoundedEntities {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct EntitiesVisitor;
        impl<'de> Visitor<'de> for EntitiesVisitor {
            type Value = BoundedEntities;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("at most 64 SQL entity references")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let slots: Box<[SqlEntityRef]> =
                    Box::new(std::array::from_fn::<_, 64, _>(|_| SqlEntityRef {
                        path: String::new(),
                        ordinal: 0,
                    }));
                let mut entities = Vec::from(slots);
                entities.clear();
                while entities.len() < 64 {
                    match seq.next_element::<DirectEntity>()? {
                        Some(entity) => entities.push(entity.into()),
                        None => return Ok(BoundedEntities(entities)),
                    }
                }
                if seq.next_element::<RejectExtra>()?.is_some() {
                    return Err(de::Error::custom("too many SQL entity references"));
                }
                Ok(BoundedEntities(entities))
            }
        }
        deserializer.deserialize_seq(EntitiesVisitor)
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
enum DirectResolution {
    Resolved {
        target: DirectEntity,
        live: bool,
    },
    Expands {
        targets: BoundedEntities,
    },
    Relation {
        relation: DirectEntity,
        live: bool,
    },
    Ambiguous {
        candidates: BoundedEntities,
        more: bool,
    },
    Local,
    Unresolved {
        reason: SqlUnresolved,
    },
}
impl From<DirectResolution> for SqlResolution {
    fn from(value: DirectResolution) -> Self {
        match value {
            DirectResolution::Resolved { target, live } => Self::Resolved {
                target: target.into(),
                live,
            },
            DirectResolution::Expands { targets } => Self::Expands { targets: targets.0 },
            DirectResolution::Relation { relation, live } => Self::Relation {
                relation: relation.into(),
                live,
            },
            DirectResolution::Ambiguous { candidates, more } => Self::Ambiguous {
                candidates: candidates.0,
                more,
            },
            DirectResolution::Local => Self::Local,
            DirectResolution::Unresolved { reason } => Self::Unresolved { reason },
        }
    }
}
body!(ReferenceBody {
    kind: SqlRefKind, role: SqlRefRole, schema: Option<DirectString>, qualifier: Option<DirectString>,
    name: DirectString, extent: SqlByteRange, full_extent: SqlByteRange, statement: u32,
    statement_kind: SqlStatementKind, in_object: Option<DirectObject>, resolution: DirectResolution
});
impl From<ReferenceBody> for SqlReference {
    fn from(b: ReferenceBody) -> Self {
        Self {
            kind: b.kind,
            role: b.role,
            schema: b.schema.map(Into::into),
            qualifier: b.qualifier.map(Into::into),
            name: b.name.into(),
            extent: b.extent,
            full_extent: b.full_extent,
            statement: b.statement,
            statement_kind: b.statement_kind,
            in_object: b.in_object.map(Into::into),
            resolution: b.resolution.into(),
        }
    }
}

fn tag<'de, A: MapAccess<'de>>(map: &mut A, expected: &str) -> Result<&'de str, A::Error> {
    let key: Option<&str> = map.next_key()?;
    if key != Some(expected) {
        return Err(de::Error::custom("SQL tag must be first"));
    }
    map.next_value()
}

struct DirectFact(SqlFact);
impl<'de> Deserialize<'de> for DirectFact {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct FactVisitor;
        impl<'de> Visitor<'de> for FactVisitor {
            type Value = DirectFact;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a canonical SQL fact")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let fact = match tag(&mut map, "fact")? {
                    "SqliteRelation" => {
                        let b = RelationBody::deserialize(MapAccessDeserializer::new(map))?;
                        SqlFact::SqliteRelation {
                            name: b.name.into(),
                            relation_kind: b.relation_kind,
                        }
                    }
                    "SqliteOutputLineage" => {
                        let b = LineageBody::deserialize(MapAccessDeserializer::new(map))?;
                        SqlFact::SqliteOutputLineage {
                            output_name: b.output_name.into(),
                            ordinal: b.ordinal,
                            origin: b.origin.map(Into::into),
                        }
                    }
                    "PostgresColumnRead" => {
                        let b = ReadBody::deserialize(MapAccessDeserializer::new(map))?;
                        SqlFact::PostgresColumnRead {
                            relation_oid: b.relation_oid,
                            attribute: b.attribute,
                            extent: b.extent,
                        }
                    }
                    "Diagnostic" => {
                        let b = DiagnosticBody::deserialize(MapAccessDeserializer::new(map))?;
                        SqlFact::Diagnostic {
                            code: b.code,
                            extent: b.extent,
                        }
                    }
                    "Statement" => {
                        let b = StatementBody::deserialize(MapAccessDeserializer::new(map))?;
                        SqlFact::Statement {
                            class: b.class,
                            extent: b.extent,
                            support: b.support,
                        }
                    }
                    "Definition" => SqlFact::Definition(Box::new(
                        DefinitionBody::deserialize(MapAccessDeserializer::new(map))?.into(),
                    )),
                    "Reference" => SqlFact::Reference(Box::new(
                        ReferenceBody::deserialize(MapAccessDeserializer::new(map))?.into(),
                    )),
                    _ => return Err(de::Error::custom("unknown SQL fact")),
                };
                Ok(DirectFact(fact))
            }
        }
        deserializer.deserialize_map(FactVisitor)
    }
}

pub(super) struct DirectRow(pub SqlAnalysisRecord);
impl<'de> Deserialize<'de> for DirectRow {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct RowVisitor;
        impl<'de> Visitor<'de> for RowVisitor {
            type Value = DirectRow;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a canonical SQL row")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let row = match tag(&mut map, "kind")? {
                    "Selection" => {
                        let b = SelectionBody::deserialize(MapAccessDeserializer::new(map))?;
                        SqlAnalysisRecord::Selection {
                            project: b.project,
                            source_root: b.source_root.into(),
                            config_input: b.config_input,
                            config_source: b.config_source,
                            config_length: b.config_length,
                            discovery: b.discovery,
                            policy_complete: b.policy_complete,
                            implicit: b.implicit,
                            gaps: b.gaps.into(),
                            inventory: b.inventory,
                            inventory_digest: b.inventory_digest,
                            host_count: b.host_count,
                        }
                    }
                    "Attempt" => {
                        let b = AttemptBody::deserialize(MapAccessDeserializer::new(map))?;
                        SqlAnalysisRecord::Attempt {
                            project: b.project,
                            outcome: b.outcome,
                            served_selection: b.served_selection,
                        }
                    }
                    "Host" => {
                        let b = HostBody::deserialize(MapAccessDeserializer::new(map))?;
                        SqlAnalysisRecord::Host {
                            project: b.project,
                            path: b.path.into(),
                            dialect: b.dialect,
                            input: b.input,
                            source: b.source,
                            length: b.length,
                            producer: b.producer,
                            settings: b.settings,
                            context: b.context,
                            terminal: b.terminal.into(),
                            fact_count: b.fact_count,
                            fact_digest: b.fact_digest,
                        }
                    }
                    "Fact" => {
                        let b = FactBody::deserialize(MapAccessDeserializer::new(map))?;
                        SqlAnalysisRecord::Fact {
                            host: b.host,
                            ordinal: b.ordinal,
                            fact: b.fact.0,
                        }
                    }
                    _ => return Err(de::Error::custom("unknown SQL row")),
                };
                Ok(DirectRow(row))
            }
        }
        deserializer.deserialize_map(RowVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(bytes: &[u8]) -> Result<DirectRow, serde_json_core::de::Error> {
        let mut scratch = vec![0; bytes.len()];
        serde_json_core::from_slice_escaped(bytes, &mut scratch).map(|(row, _)| row)
    }

    #[test]
    fn canonical_selection_attempt_and_host_roundtrip() {
        let rows = [
            SqlAnalysisRecord::Selection {
                project: [1; 32],
                source_root: "/a\"b".into(),
                config_input: [2; 32],
                config_source: [3; 32],
                config_length: 1,
                discovery: None,
                policy_complete: true,
                implicit: false,
                gaps: SqlGapCounts::default(),
                inventory: SqlInventory::Complete,
                inventory_digest: [4; 32],
                host_count: 1,
            },
            SqlAnalysisRecord::Attempt {
                project: [1; 32],
                outcome: SqlAttemptOutcome::Observed,
                served_selection: Some([4; 32]),
            },
            SqlAnalysisRecord::Host {
                project: [1; 32],
                path: "x\"y.sql".into(),
                dialect: SqlDialect::Sqlite,
                input: Some([2; 32]),
                source: Some([3; 32]),
                length: Some(5),
                producer: Some([4; 32]),
                settings: Some([5; 32]),
                context: None,
                terminal: SqlHostTerminal::Parsed {
                    source: SqlCoverage::Partial,
                    contextual: SqlCoverage::Unavailable,
                    sqlite_lineage: None,
                },
                fact_count: 0,
                fact_digest: [6; 32],
            },
        ];
        for row in rows {
            let bytes = serde_json::to_vec(&row).unwrap();
            assert_eq!(decode(&bytes).unwrap().0, row);
        }
    }

    #[test]
    fn tag_first_decoder_rejects_duplicate_and_unknown_fields() {
        let row = SqlAnalysisRecord::Fact {
            host: [1; 32],
            ordinal: 0,
            fact: SqlFact::Diagnostic {
                code: SqlDiagnostic::Other,
                extent: None,
            },
        };
        let canonical = serde_json::to_string(&row).unwrap();
        let duplicate = canonical.replacen("\"ordinal\":0", "\"ordinal\":0,\"ordinal\":0", 1);
        let unknown = canonical.replacen("\"code\":", "\"extra\":0,\"code\":", 1);
        let tag_last =
            canonical
                .replacen("\"kind\":\"Fact\",", "", 1)
                .replacen('{', "{\"extra\":0,", 1);
        for input in [duplicate, unknown, tag_last] {
            assert!(decode(input.as_bytes()).is_err());
        }
    }

    #[test]
    fn externally_tagged_payload_variants_roundtrip() {
        let facts = [
            SqlFact::Definition(Box::new(SqlDefinition {
                kind: SqlObjectKind::Table,
                action: SqlDefinitionAction::Rename {
                    to: "new\"name".into(),
                    to_extent: SqlByteRange { start: 2, end: 3 },
                },
                conditional: false,
                schema: None,
                name: "old".into(),
                parent: None,
                parent_schema: None,
                name_extent: SqlByteRange { start: 0, end: 1 },
                extent: SqlByteRange { start: 0, end: 3 },
                detail: None,
                statement: 0,
                entity: None,
                parent_entity: None,
                live_at_head: true,
                head_name: None,
                head_parent: None,
            })),
            SqlFact::Reference(Box::new(SqlReference {
                kind: SqlRefKind::Column,
                role: SqlRefRole::Read,
                schema: None,
                qualifier: None,
                name: "x".into(),
                extent: SqlByteRange { start: 0, end: 1 },
                full_extent: SqlByteRange { start: 0, end: 1 },
                statement: 0,
                statement_kind: SqlStatementKind::Select,
                in_object: None,
                resolution: SqlResolution::Ambiguous {
                    candidates: vec![
                        SqlEntityRef {
                            path: "a.sql".into(),
                            ordinal: 0,
                        },
                        SqlEntityRef {
                            path: "b.sql".into(),
                            ordinal: 1,
                        },
                    ],
                    more: true,
                },
            })),
        ];
        for fact in facts {
            let row = SqlAnalysisRecord::Fact {
                host: [1; 32],
                ordinal: 0,
                fact,
            };
            let bytes = serde_json::to_vec(&row).unwrap();
            assert_eq!(decode(&bytes).unwrap().0, row);
        }
    }

    #[test]
    fn deeply_nested_unknown_and_surplus_values_are_rejected() {
        let row = SqlAnalysisRecord::Fact {
            host: [1; 32],
            ordinal: 0,
            fact: SqlFact::Reference(Box::new(SqlReference {
                kind: SqlRefKind::Column,
                role: SqlRefRole::Read,
                schema: None,
                qualifier: None,
                name: "c".into(),
                extent: SqlByteRange { start: 0, end: 1 },
                full_extent: SqlByteRange { start: 0, end: 1 },
                statement: 0,
                statement_kind: SqlStatementKind::Select,
                in_object: None,
                resolution: SqlResolution::Expands {
                    targets: vec![
                        SqlEntityRef {
                            path: "a.sql".into(),
                            ordinal: 0
                        };
                        64
                    ],
                },
            })),
        };
        let encoded = serde_json::to_string(&row).unwrap();
        let nested = format!("{}0{}", "[".repeat(4000), "]".repeat(4000));
        let unknown = encoded.replacen(
            "\"statement\":0",
            &format!("\"unexpected\":{nested},\"statement\":0"),
            1,
        );
        assert!(decode(unknown.as_bytes()).is_err());
        let marker = "}]}}";
        let surplus = encoded.replacen(marker, &format!("}},{nested}]}}}}"), 1);
        assert_ne!(surplus, encoded);
        assert!(decode(surplus.as_bytes()).is_err());
    }

    #[test]
    fn reference_entity_array_rejects_first_surplus() {
        let make = |count| SqlAnalysisRecord::Fact {
            host: [1; 32],
            ordinal: 0,
            fact: SqlFact::Reference(Box::new(SqlReference {
                kind: SqlRefKind::Column,
                role: SqlRefRole::Read,
                schema: None,
                qualifier: None,
                name: "c".into(),
                extent: SqlByteRange { start: 0, end: 1 },
                full_extent: SqlByteRange { start: 0, end: 1 },
                statement: 0,
                statement_kind: SqlStatementKind::Select,
                in_object: None,
                resolution: SqlResolution::Expands {
                    targets: vec![
                        SqlEntityRef {
                            path: "a.sql".into(),
                            ordinal: 0
                        };
                        count
                    ],
                },
            })),
        };
        let within = make(64);
        let bytes = serde_json::to_vec(&within).unwrap();
        assert_eq!(decode(&bytes).unwrap().0, within);
        let excess = serde_json::to_vec(&make(65)).unwrap();
        assert!(decode(&excess).is_err());
    }

    #[test]
    fn canonical_fact_variants_decode_without_tag_buffering() {
        let facts = vec![
            SqlFact::SqliteRelation {
                name: "a\\\"b".into(),
                relation_kind: SqliteRelationKind::Table,
            },
            SqlFact::SqliteOutputLineage {
                output_name: "col".into(),
                ordinal: 1,
                origin: Some(SqliteColumnOrigin {
                    relation: "t".into(),
                    column: "c".into(),
                }),
            },
            SqlFact::PostgresColumnRead {
                relation_oid: 1,
                attribute: 1,
                extent: SqlByteRange { start: 0, end: 1 },
            },
            SqlFact::Diagnostic {
                code: SqlDiagnostic::Other,
                extent: None,
            },
            SqlFact::Statement {
                class: SqlStatementKind::Select,
                extent: SqlByteRange { start: 0, end: 1 },
                support: SqlStatementSupport::Full,
            },
            SqlFact::Reference(Box::new(SqlReference {
                kind: SqlRefKind::Column,
                role: SqlRefRole::Read,
                schema: None,
                qualifier: None,
                name: "c".into(),
                extent: SqlByteRange { start: 0, end: 1 },
                full_extent: SqlByteRange { start: 0, end: 1 },
                statement: 0,
                statement_kind: SqlStatementKind::Select,
                in_object: None,
                resolution: SqlResolution::Expands {
                    targets: vec![SqlEntityRef {
                        path: "a.sql".into(),
                        ordinal: 0,
                    }],
                },
            })),
            SqlFact::Definition(Box::new(SqlDefinition {
                kind: SqlObjectKind::Table,
                action: SqlDefinitionAction::Create,
                conditional: false,
                schema: None,
                name: "t".into(),
                parent: None,
                parent_schema: None,
                name_extent: SqlByteRange { start: 0, end: 1 },
                extent: SqlByteRange { start: 0, end: 1 },
                detail: None,
                statement: 0,
                entity: None,
                parent_entity: None,
                live_at_head: true,
                head_name: None,
                head_parent: None,
            })),
        ];
        for (ordinal, fact) in facts.into_iter().enumerate() {
            let row = SqlAnalysisRecord::Fact {
                host: [1; 32],
                ordinal: ordinal as u32,
                fact,
            };
            let bytes = serde_json::to_vec(&row).unwrap();
            assert_eq!(decode(&bytes).unwrap().0, row);
        }
    }
}

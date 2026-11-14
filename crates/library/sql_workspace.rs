//! Explicit SQL source selection read from a verified project configuration.
//!
//! Parsing these bytes proves neither workspace membership nor catalog truth.
//! The index owner must separately authenticate the configuration path, source
//! files and the selected snapshot before publishing any results.
use serde::{Deserialize, Serialize};

/// Exact project-relative settings file understood by the SQL source profile.
pub const SQL_PROJECT_SETTINGS_PATH: &str = ".nudox/sql.json";
/// Maximum configuration bytes, independent of the number of source hosts.
pub const MAX_SQL_PROJECT_SETTINGS_BYTES: usize = 128 * 1024;
/// Maximum number of explicitly selected SQL source hosts.
pub const MAX_SQL_PROJECT_HOSTS: usize = 256;

/// A parsed configuration and the actual host identities used to read it.
/// This is a byte witness, not a selected-project or catalog authority.
#[derive(Clone, Debug)]
pub struct ParsedSqlProjectSettings {
    settings: SqlProjectSettings,
    input: backend_compile::InputContentVersion,
    source: backend_version::ContentId<backend_version::SourceFactDomain>,
}
impl ParsedSqlProjectSettings {
    /// Parse actual UTF-8 bytes and mint both content identities. The caller
    /// must bind these bytes to the exact selected project and settings path.
    pub fn parse(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() > MAX_SQL_PROJECT_SETTINGS_BYTES {
            return Err("SQL project settings limit");
        }
        let source = backend_compile::VerifiedSourceText::capture(bytes)
            .map_err(|_| "SQL settings are not UTF-8")?;
        let text = source
            .extent(0..bytes.len())
            .map_err(|_| "invalid settings extent")?;
        let settings: SqlProjectSettings =
            serde_json::from_str(text.text()).map_err(|_| "invalid SQL project settings")?;
        settings.validate()?;
        Ok(Self {
            settings,
            input: source.content(),
            source: text.source(),
        })
    }
    /// Validated settings claims; project admission remains separate.
    pub fn settings(&self) -> &SqlProjectSettings {
        &self.settings
    }
    /// Input-content identity minted from the actual settings bytes.
    pub fn input(&self) -> backend_compile::InputContentVersion {
        self.input
    }
    /// Source-fact content identity minted from the actual settings bytes.
    pub fn source(&self) -> backend_version::ContentId<backend_version::SourceFactDomain> {
        self.source
    }
}

/// Versioned explicit source selection; an extension alone never chooses SQL dialect.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SqlProjectSettings {
    /// Exact settings grammar version, currently one.
    pub version: u16,
    /// Strictly ordered, unique project-relative source selections.
    pub sources: Vec<SqlProjectSource>,
}
/// Explicit dialect and bounded context for one project source host.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SqlProjectSource {
    /// Canonical project-relative path, with forward slashes and no traversal.
    pub path: String,
    /// Selected native dialect; PostgreSQL requires separate engine intake.
    pub dialect: SqlDialect,
    /// Context for this dialect, explicitly unavailable or user-declared.
    pub context: SqlProjectContext,
}
/// Distinct native SQL dialect families.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SqlDialect {
    Sqlite,
    Postgres,
}
/// Project-proposed dialect-specific inputs; neither variant proves a
/// deployed schema or an observed database connection. SQLite entries carry
/// name presence only; PostgreSQL keeps the declared order separately.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SqlProjectContext {
    /// No catalog evidence was declared.
    Unavailable,
    /// Proposed SQLite name-presence catalog consumed by the pinned analyzer.
    SqliteProposed {
        /// Explicit source-config epoch, not a database object identity.
        epoch: u64,
        /// Sorted table-name entries with sorted known column-name lists.
        tables: Vec<SqliteProjectTable>,
    },
    /// Proposed PostgreSQL catalog and ordered search path; this is not a live database.
    PostgresProposed {
        /// Explicit configuration epoch, not a database identity.
        epoch: u64,
        /// Ordered, unique names of explicitly provided schemas.
        search_path: Vec<String>,
        /// Strictly ordered schema definitions for the finite native resolver.
        schemas: Vec<PostgresProjectSchema>,
    },
}
/// One proposed PostgreSQL schema.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostgresProjectSchema {
    /// Restricted unquoted lowercase ASCII identifier (`a-z`, digits, underscore).
    pub name: String,
    /// Strictly ordered proposed table definitions.
    pub tables: Vec<PostgresProjectTable>,
}
/// One proposed PostgreSQL table; column order is explicit and preserved.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostgresProjectTable {
    /// Restricted unquoted lowercase ASCII identifier (`a-z`, digits, underscore).
    pub name: String,
    /// Ordered, unique proposed column names; the current finite native
    /// resolver models these as int4, not observed deployed column types.
    pub columns: Vec<String>,
}
/// A proposed SQLite table and known column-name set.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SqliteProjectTable {
    /// ASCII-folded proposed table name.
    pub name: String,
    /// ASCII-folded, sorted known names; this is not physical column order.
    pub columns: Vec<String>,
}
fn valid_path(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 4096
        && !p.starts_with('/')
        && !p.contains('\\')
        && !p.bytes().any(|b| b.is_ascii_control())
        && !p.split('/').any(|x| x.is_empty() || x == "." || x == "..")
        && !p.as_bytes().get(1).is_some_and(|b| *b == b':')
}
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 256
        && !name.contains('\0')
        && name == name.to_ascii_lowercase()
}
fn valid_postgres_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    name.len() <= 63
        && bytes
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}
impl SqlProjectSettings {
    /// Check finite grammar, per-host dialect/context compatibility and uniqueness.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.version != 1
            || self.sources.len() > MAX_SQL_PROJECT_HOSTS
            || self
                .sources
                .windows(2)
                .any(|pair| pair[0].path >= pair[1].path)
        {
            return Err("invalid SQL source selection");
        }
        for selected in &self.sources {
            if !valid_path(&selected.path)
                || selected.path == SQL_PROJECT_SETTINGS_PATH
                || !selected
                    .path
                    .rsplit_once('.')
                    .is_some_and(|(_, suffix)| suffix.eq_ignore_ascii_case("sql"))
            {
                return Err("invalid SQL source path");
            }
            match (&selected.dialect, &selected.context) {
                (_, SqlProjectContext::Unavailable) => {}
                (SqlDialect::Sqlite, SqlProjectContext::SqliteProposed { tables, .. }) => {
                    if tables.len() > 64
                        || tables.windows(2).any(|pair| pair[0].name >= pair[1].name)
                        || tables.iter().any(|table| {
                            !valid_name(&table.name)
                                || table.columns.len() > 128
                                || table.columns.iter().any(|name| !valid_name(name))
                                || table.columns.windows(2).any(|pair| pair[0] >= pair[1])
                        })
                    {
                        return Err("invalid proposed SQLite catalog");
                    }
                }
                (
                    SqlDialect::Postgres,
                    SqlProjectContext::PostgresProposed {
                        search_path,
                        schemas,
                        ..
                    },
                ) => {
                    if schemas.is_empty()
                        || schemas.len() > 16
                        || search_path.is_empty()
                        || search_path.len() > 16
                        || schemas.windows(2).any(|pair| pair[0].name >= pair[1].name)
                        || search_path.iter().enumerate().any(|(i, name)| {
                            search_path[..i].contains(name)
                                || !schemas.iter().any(|schema| &schema.name == name)
                        })
                        || schemas.iter().any(|schema| {
                            !valid_postgres_name(&schema.name)
                                || schema.name == "pg_catalog"
                                || schema.tables.len() > 64
                                || schema
                                    .tables
                                    .windows(2)
                                    .any(|pair| pair[0].name >= pair[1].name)
                                || schema.tables.iter().any(|table| {
                                    !valid_postgres_name(&table.name)
                                        || table.columns.is_empty()
                                        || table.columns.len() > 128
                                        || table
                                            .columns
                                            .iter()
                                            .any(|column| !valid_postgres_name(column))
                                        || table
                                            .columns
                                            .iter()
                                            .enumerate()
                                            .any(|(i, column)| table.columns[..i].contains(column))
                                })
                        })
                    {
                        return Err("invalid proposed PostgreSQL catalog");
                    }
                }
                _ => return Err("SQL context does not match selected dialect"),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn verified_settings_preserve_explicit_dialect_and_context() {
        let bytes = br#"{"version":1,"sources":[{"path":"queries/a.sql","dialect":"sqlite","context":{"kind":"sqlite_proposed","epoch":9,"tables":[{"name":"t","columns":["id","name"]}]}}]}"#;
        let actual = backend_compile::VerifiedSourceText::capture(bytes).unwrap();
        let parsed = ParsedSqlProjectSettings::parse(bytes).unwrap();
        assert_eq!(parsed.settings().sources[0].dialect, SqlDialect::Sqlite);
        assert_eq!(parsed.input(), actual.content());
        assert_eq!(
            parsed.source(),
            actual.extent(0..bytes.len()).unwrap().source()
        );
        assert!(matches!(
            parsed.settings().sources[0].context,
            SqlProjectContext::SqliteProposed { epoch: 9, .. }
        ));
    }
    #[test]
    fn source_selection_rejects_ambiguity_and_unbounded_claims() {
        for source in [
            r#"{"version":1,"sources":[{"path":"../a.sql","dialect":"sqlite","context":{"kind":"unavailable"}}]}"#,
            r#"{"version":1,"sources":[{"path":"a.sql","dialect":"postgres","context":{"kind":"sqlite_proposed","epoch":1,"tables":[]}}]}"#,
            r#"{"version":1,"sources":[{"path":"a.sql","dialect":"sqlite","context":{"kind":"unavailable"}},{"path":"a.sql","dialect":"postgres","context":{"kind":"unavailable"}}]}"#,
        ] {
            assert!(ParsedSqlProjectSettings::parse(source.as_bytes()).is_err());
        }
    }
    #[test]
    fn postgres_context_preserves_search_path_and_column_order() {
        let bytes = br#"{"version":1,"sources":[{"path":"q.sql","dialect":"postgres","context":{"kind":"postgres_proposed","epoch":3,"search_path":["z","a"],"schemas":[{"name":"a","tables":[{"name":"t","columns":["z","a"]}]},{"name":"z","tables":[]}]}}]}"#;
        let parsed = ParsedSqlProjectSettings::parse(bytes).unwrap();
        let SqlProjectContext::PostgresProposed {
            search_path,
            schemas,
            ..
        } = &parsed.settings().sources[0].context
        else {
            panic!("expected PostgreSQL context")
        };
        assert_eq!(
            search_path.iter().map(String::as_str).collect::<Vec<_>>(),
            vec!["z", "a"]
        );
        assert_eq!(
            schemas[0].tables[0]
                .columns
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["z", "a"]
        );
        let bad = String::from_utf8(bytes.to_vec())
            .unwrap()
            .replace("[\"z\",\"a\"],\"schemas\"", "[\"z\",\"z\"],\"schemas\"");
        assert!(ParsedSqlProjectSettings::parse(bad.as_bytes()).is_err());
        let wrong = String::from_utf8(bytes.to_vec())
            .unwrap()
            .replace("\"dialect\":\"postgres\"", "\"dialect\":\"sqlite\"");
        assert!(ParsedSqlProjectSettings::parse(wrong.as_bytes()).is_err());
    }
}

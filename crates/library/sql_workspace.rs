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
/// Declared semantic inputs. SQLite entries describe name presence only, not
/// column order, deployed schema, or an observed database connection.
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
}

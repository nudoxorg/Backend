//! SQL native graph query vocabulary. A complete page does not upgrade native
//! parser, context, source-occurrence, or catalog coverage.
use crate::{
    NativeOwnerStatement, PackageReference, ProductAdmissionError, seal_native_owner_statement,
    verify_native_owner_statement,
};
pub use backend_semantic::sql_analysis::{
    SqlDefinitionAction, SqlFact, SqlObjectKind, SqlResolution, SqlStatementSupport,
};
use backend_semantic::sql_analysis::{SqlCoverage, SqlDialect, SqliteLineageCoverage};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};

/// Native fact family; this is not a common cross-dialect SQL binder.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SqlQueryRole {
    Any,
    Relation,
    OutputLineage,
    Diagnostic,
    /// Native PostgreSQL SELECT-column reads, qualified by proposed context.
    ColumnRead,
    /// Every statement of every selected file, with how completely it was modeled.
    Statement,
    /// Statements that were not fully modeled: the places where "no reference
    /// found" must not be read as "no reference exists".
    Gap,
    /// Tables, columns, views, indexes, triggers, functions, types and their changes.
    Definition,
    /// Every written mention of an object, bound to the object it names.
    Reference,
}
impl SqlQueryRole {
    fn byte(self) -> u8 {
        match self {
            Self::Any => 0,
            Self::Relation => 1,
            Self::OutputLineage => 2,
            Self::Diagnostic => 3,
            Self::ColumnRead => 4,
            Self::Statement => 5,
            Self::Gap => 6,
            Self::Definition => 7,
            Self::Reference => 8,
        }
    }
    fn from_byte(value: u8) -> Result<Self, ProductAdmissionError> {
        match value {
            0 => Ok(Self::Any),
            1 => Ok(Self::Relation),
            2 => Ok(Self::OutputLineage),
            3 => Ok(Self::Diagnostic),
            4 => Ok(Self::ColumnRead),
            5 => Ok(Self::Statement),
            6 => Ok(Self::Gap),
            7 => Ok(Self::Definition),
            8 => Ok(Self::Reference),
            _ => Err(ProductAdmissionError::SemanticVersionShape),
        }
    }
}
/// Largest relation-row scan one SQL request may ask for.
pub const SQL_MAX_SCAN_ROWS: u32 = 262_144;
/// Largest relation-byte scan one SQL request may ask for.
pub const SQL_MAX_SCAN_BYTES: u32 = 64 * 1024 * 1024;
/// One exact-project, bounded SQL native-fact page request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SqlQueryRequest {
    pub package: PackageReference,
    pub role: SqlQueryRole,
    /// Optional exact name. For `relation` and `output_lineage` the native model
    /// name; for `definition` and `reference` the dialect-normalized name (SQLite
    /// folds ASCII case, PostgreSQL folds unquoted names to lower case).
    pub name: Option<String>,
    /// `definition` / `reference` only: restrict to one kind of object.
    #[serde(default)]
    pub kind: Option<SqlObjectKind>,
    /// `definition` only: the owning table or view of a column, index or trigger.
    #[serde(default)]
    pub parent: Option<String>,
    /// `reference` only: mentions that bind to this object, written `table` or
    /// `table.column` (optionally schema-qualified). A table also matches mentions
    /// of its columns and `*` expansions that include it.
    #[serde(default)]
    pub uses: Option<String>,
    pub page_rows: u16,
    pub output_bytes: u32,
    pub scan_rows: u32,
    pub scan_bytes: u32,
    pub continuation: Option<SqlQueryCursor>,
}
impl SqlQueryRequest {
    /// Check request bounds. This does not authenticate a cursor or project.
    pub fn validate(&self) -> Result<(), ProductAdmissionError> {
        if !matches!(self.package, PackageReference::Local(_))
            || self.page_rows == 0
            || self.page_rows > 128
            || !(131_072..=4_194_304).contains(&self.output_bytes)
            || !(7..=SQL_MAX_SCAN_ROWS).contains(&self.scan_rows)
            || !(229_376..=SQL_MAX_SCAN_BYTES).contains(&self.scan_bytes)
            || [&self.name, &self.parent, &self.uses]
                .into_iter()
                .flatten()
                .any(|name| name.is_empty() || name.len() > 256 || name.contains('\0'))
            || self.name.is_some()
                && matches!(
                    self.role,
                    SqlQueryRole::Any
                        | SqlQueryRole::Diagnostic
                        | SqlQueryRole::ColumnRead
                        | SqlQueryRole::Statement
                        | SqlQueryRole::Gap
                )
            || (self.kind.is_some()
                && !matches!(self.role, SqlQueryRole::Definition | SqlQueryRole::Reference))
            || (self.parent.is_some() && self.role != SqlQueryRole::Definition)
            || (self.uses.is_some() && self.role != SqlQueryRole::Reference)
        {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        if let Some(cursor) = &self.continuation {
            cursor.decode()?;
        }
        Ok(())
    }
}
/// Owner-sealed cursor claims bound to exactly the consumed roots and request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SqlCursorClaims {
    pub manifest_root: [u8; 32],
    pub sql_root: [u8; 32],
    pub project: [u8; 32],
    pub role: SqlQueryRole,
    pub name: Option<String>,
    pub kind: Option<SqlObjectKind>,
    pub parent: Option<String>,
    pub uses: Option<String>,
    pub page_rows: u16,
    pub output_bytes: u32,
    pub scan_rows: u32,
    pub scan_bytes: u32,
    pub after: [u8; 32],
}
impl NativeOwnerStatement for SqlCursorClaims {
    const DOMAIN: &'static [u8] = b"backend.sql.query-cursor.v1\0";
}
/// Compact continuation; parsing its shape alone does not authenticate it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SqlQueryCursor(String);
impl SqlQueryCursor {
    /// Validate canonical cursor shape without owner authentication.
    pub fn admit_shape(&self) -> Result<(), ProductAdmissionError> {
        self.decode().map(|_| ())
    }
    /// Parse a bounded canonical token shape without authenticating its seal.
    pub fn parse(value: String) -> Result<Self, ProductAdmissionError> {
        let cursor = Self(value);
        cursor.decode()?;
        Ok(cursor)
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn issue(
        claims: SqlCursorClaims,
        secret: &[u8; 32],
    ) -> Result<Self, ProductAdmissionError> {
        let name = claims.name.as_deref().unwrap_or("").as_bytes();
        let parent = claims.parent.as_deref().unwrap_or("").as_bytes();
        let uses = claims.uses.as_deref().unwrap_or("").as_bytes();
        if [name, parent, uses].iter().any(|v| v.len() > 256 || v.contains(&0))
            || [&claims.name, &claims.parent, &claims.uses]
                .into_iter()
                .any(|v| v.as_ref().is_some_and(String::is_empty))
            || (claims.name.is_some()
                && matches!(
                    claims.role,
                    SqlQueryRole::Any
                        | SqlQueryRole::Diagnostic
                        | SqlQueryRole::ColumnRead
                        | SqlQueryRole::Statement
                        | SqlQueryRole::Gap
                ))
            || claims.page_rows == 0
            || claims.page_rows > 128
            || !(131_072..=4_194_304).contains(&claims.output_bytes)
            || !(7..=SQL_MAX_SCAN_ROWS).contains(&claims.scan_rows)
            || !(229_376..=SQL_MAX_SCAN_BYTES).contains(&claims.scan_bytes)
        {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        let seal = seal_native_owner_statement(secret, &claims)?;
        let mut out = vec![1, 1];
        out.extend_from_slice(&claims.manifest_root);
        out.extend_from_slice(&claims.sql_root);
        out.extend_from_slice(&claims.project);
        out.push(claims.role.byte());
        out.extend_from_slice(&(name.len() as u16).to_be_bytes());
        out.extend_from_slice(name);
        out.push(claims.kind.map_or(0, kind_byte));
        out.extend_from_slice(&(parent.len() as u16).to_be_bytes());
        out.extend_from_slice(parent);
        out.extend_from_slice(&(uses.len() as u16).to_be_bytes());
        out.extend_from_slice(uses);
        out.extend_from_slice(&claims.page_rows.to_be_bytes());
        out.extend_from_slice(&claims.output_bytes.to_be_bytes());
        out.extend_from_slice(&claims.scan_rows.to_be_bytes());
        out.extend_from_slice(&claims.scan_bytes.to_be_bytes());
        out.extend_from_slice(&claims.after);
        out.extend_from_slice(&seal);
        Ok(Self(URL_SAFE_NO_PAD.encode(out)))
    }
    fn decode(&self) -> Result<(SqlCursorClaims, [u8; 32]), ProductAdmissionError> {
        let bytes = decode(&self.0, 2048)?;
        let mut r = Reader { rest: &bytes };
        if r.array::<2>()? != [1, 1] {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        let manifest_root = r.array()?;
        let sql_root = r.array()?;
        let project = r.array()?;
        let role = SqlQueryRole::from_byte(r.array::<1>()?[0])?;
        let len = u16::from_be_bytes(r.array()?) as usize;
        if len > 256 {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        let name = if len == 0 {
            None
        } else {
            Some(
                std::str::from_utf8(r.take(len)?)
                    .map_err(|_| ProductAdmissionError::SemanticVersionShape)?
                    .to_owned(),
            )
        };
        let kind = kind_from_byte(r.array::<1>()?[0])?;
        let mut text = |r: &mut Reader<'_>| -> Result<Option<String>, ProductAdmissionError> {
            let len = u16::from_be_bytes(r.array()?) as usize;
            if len > 256 {
                return Err(ProductAdmissionError::SemanticVersionShape);
            }
            if len == 0 {
                return Ok(None);
            }
            Ok(Some(
                std::str::from_utf8(r.take(len)?)
                    .map_err(|_| ProductAdmissionError::SemanticVersionShape)?
                    .to_owned(),
            ))
        };
        let parent = text(&mut r)?;
        let uses = text(&mut r)?;
        let page_rows = u16::from_be_bytes(r.array()?);
        let output_bytes = u32::from_be_bytes(r.array()?);
        let scan_rows = u32::from_be_bytes(r.array()?);
        let scan_bytes = u32::from_be_bytes(r.array()?);
        let after = r.array()?;
        let seal = r.array()?;
        r.finish()?;
        Ok((
            SqlCursorClaims {
                manifest_root,
                sql_root,
                project,
                role,
                name,
                kind,
                parent,
                uses,
                page_rows,
                output_bytes,
                scan_rows,
                scan_bytes,
                after,
            },
            seal,
        ))
    }
    /// Authenticate the exact owner statement.
    pub fn verify(&self, secret: &[u8; 32]) -> Result<SqlCursorClaims, ProductAdmissionError> {
        let (claim, seal) = self.decode()?;
        verify_native_owner_statement(secret, &claim, &seal)?;
        Ok(claim)
    }
}
/// Exact owner-sealed selected native source-site reference.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SqlSourceClaims {
    pub manifest_root: [u8; 32],
    pub sql_root: [u8; 32],
    pub project: [u8; 32],
    pub host: [u8; 32],
    pub fact: [u8; 32],
}
impl NativeOwnerStatement for SqlSourceClaims {
    const DOMAIN: &'static [u8] = b"backend.sql.source-reference.v1\0";
}
/// Source handle; exact host and byte extent are re-admitted on every use.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SqlSourceReference(String);
impl SqlSourceReference {
    /// Validate canonical fixed-width shape without owner authentication.
    pub fn admit_shape(&self) -> Result<(), ProductAdmissionError> {
        let bytes = decode(&self.0, 260)?;
        if bytes.len() != 194 || bytes.get(..2) != Some(&[1u8, 2][..]) {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        Ok(())
    }
    /// Parse canonical fixed-width source-token shape without verifying its seal.
    pub fn parse(value: String) -> Result<Self, ProductAdmissionError> {
        let bytes = decode(&value, 260)?;
        if bytes.len() != 194 || bytes.get(..2) != Some(&[1u8, 2][..]) {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn issue(
        claims: SqlSourceClaims,
        secret: &[u8; 32],
    ) -> Result<Self, ProductAdmissionError> {
        let seal = seal_native_owner_statement(secret, &claims)?;
        let mut out = vec![1, 2];
        for value in [
            &claims.manifest_root,
            &claims.sql_root,
            &claims.project,
            &claims.host,
            &claims.fact,
            &seal,
        ] {
            out.extend_from_slice(value);
        }
        Ok(Self(URL_SAFE_NO_PAD.encode(out)))
    }
    /// Authenticate the selected root and exact fact reference.
    pub fn verify(&self, secret: &[u8; 32]) -> Result<SqlSourceClaims, ProductAdmissionError> {
        let bytes = decode(&self.0, 260)?;
        let mut r = Reader { rest: &bytes };
        if r.array::<2>()? != [1, 2] {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        let claim = SqlSourceClaims {
            manifest_root: r.array()?,
            sql_root: r.array()?,
            project: r.array()?,
            host: r.array()?,
            fact: r.array()?,
        };
        let seal = r.array()?;
        r.finish()?;
        verify_native_owner_statement(secret, &claim, &seal)?;
        Ok(claim)
    }
}
fn kind_byte(kind: SqlObjectKind) -> u8 {
    match kind {
        SqlObjectKind::Schema => 1,
        SqlObjectKind::Table => 2,
        SqlObjectKind::View => 3,
        SqlObjectKind::Column => 4,
        SqlObjectKind::Index => 5,
        SqlObjectKind::Trigger => 6,
        SqlObjectKind::Function => 7,
        SqlObjectKind::Type => 8,
        SqlObjectKind::Sequence => 9,
    }
}
fn kind_from_byte(value: u8) -> Result<Option<SqlObjectKind>, ProductAdmissionError> {
    Ok(Some(match value {
        0 => return Ok(None),
        1 => SqlObjectKind::Schema,
        2 => SqlObjectKind::Table,
        3 => SqlObjectKind::View,
        4 => SqlObjectKind::Column,
        5 => SqlObjectKind::Index,
        6 => SqlObjectKind::Trigger,
        7 => SqlObjectKind::Function,
        8 => SqlObjectKind::Type,
        9 => SqlObjectKind::Sequence,
        _ => return Err(ProductAdmissionError::SemanticVersionShape),
    }))
}
fn decode(value: &str, max: usize) -> Result<Vec<u8>, ProductAdmissionError> {
    if value.len() > max {
        return Err(ProductAdmissionError::SemanticVersionShape);
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| ProductAdmissionError::SemanticVersionShape)?;
    if URL_SAFE_NO_PAD.encode(&bytes) != value {
        return Err(ProductAdmissionError::SemanticVersionShape);
    }
    Ok(bytes)
}
struct Reader<'a> {
    rest: &'a [u8],
}
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], ProductAdmissionError> {
        let (a, b) = self
            .rest
            .split_at_checked(n)
            .ok_or(ProductAdmissionError::SemanticVersionShape)?;
        self.rest = b;
        Ok(a)
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N], ProductAdmissionError> {
        self.take(N)?
            .try_into()
            .map_err(|_| ProductAdmissionError::SemanticVersionShape)
    }
    fn finish(self) -> Result<(), ProductAdmissionError> {
        if self.rest.is_empty() {
            Ok(())
        } else {
            Err(ProductAdmissionError::SemanticVersionShape)
        }
    }
}
/// Availability of the most recent selected SQL analysis attempt.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum SqlAttemptState {
    ProjectNotSelected,
    NoSelectedAnalysis,
    LatestObserved,
    RetainedAfterUnavailable { reason: SqlFailure },
    Unavailable { reason: SqlFailure },
}
/// Closed native SQL attempt failure, never a successful empty result.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SqlFailure {
    Read,
    SelectionUnavailable,
    InvalidUtf8,
    Limit,
    EngineNotAdmitted,
    Deadline,
    Cancelled,
    WorkerFailure,
    UnsupportedDialect,
    Context,
}
/// Aggregate host health is unknown until a bounded admitted summary exists.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SqlHostHealth {
    Unknown,
}
/// Current source-policy stability is not established by the SQL capture.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SqlSourcePolicy {
    Unknown,
}
/// Shared basis for all rows in a page; coverage remains partial or unknown.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SqlQueryBasis {
    pub manifest_root: [u8; 32],
    pub sql_root: Option<[u8; 32]>,
    /// Exact requested project identity under the selected workspace.
    pub project: [u8; 32],
    pub attempt: SqlAttemptState,
    pub host_health: SqlHostHealth,
    pub inventory_complete: bool,
    pub policy_complete: bool,
    /// Source references and catalog binding are incomplete in this profile.
    pub source_occurrences_complete: bool,
    /// Where this project's analysis is known to be incomplete. Read with
    /// `inventory_complete`: a page with no hits is a known-empty answer only if
    /// the inventory is complete and these are all zero.
    #[serde(default)]
    pub gaps: backend_semantic::sql_analysis::SqlGapCounts,
    /// No settings file exists; every `.sql` file was selected and each dialect inferred.
    #[serde(default)]
    pub implicit_selection: bool,
    pub catalog_is_deployed: bool,
}
/// One native claim; source access requires a native-proven exact extent.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SqlQueryHit {
    pub id: [u8; 32],
    pub path: String,
    pub dialect: SqlDialect,
    /// Per-result policy qualification; this is not a current-policy proof.
    pub policy: SqlSourcePolicy,
    /// Native/source/configuration identities for the exact selected host.
    pub host_basis: SqlHostBasis,
    pub fact: SqlFact,
    pub source: Option<SqlSourceReference>,
}
/// Native result capabilities and exact selected host identity claims.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SqlHostBasis {
    pub input: [u8; 32],
    pub source: [u8; 32],
    pub length: u64,
    /// Worker artifact, native basis, and owner source/lock projection digest.
    /// This is not a proof of the owner executable or toolchain identity.
    pub producer: [u8; 32],
    pub settings: [u8; 32],
    /// Context is a project-proposed catalog digest, never a deployed catalog.
    pub proposed_context: Option<[u8; 32]>,
    pub terminal: SqlNativeTerminal,
}
/// Per-host terminal, separate from SQL query page completion.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum SqlNativeTerminal {
    Parsed {
        source: SqlCoverage,
        contextual: SqlCoverage,
        sqlite_lineage: Option<SqliteLineageCoverage>,
    },
    NativeError,
}
/// Page terminal; completeness covers enumeration only.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum SqlQueryTerminal {
    Complete,
    More {
        continuation: SqlQueryCursor,
        work_limited: bool,
    },
    Unavailable,
}
/// A bounded page over one selected SQL relation root.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SqlQueryPage {
    pub basis: SqlQueryBasis,
    pub hits: Vec<SqlQueryHit>,
    pub terminal: SqlQueryTerminal,
}
/// Result of rechecking actual selected host bytes for one native extent.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum SqlSourceRead {
    Available {
        /// Exact request reference; the owner, not this echo, verifies its seal and source.
        reference: SqlSourceReference,
        /// Verifying bytes does not establish current discovery policy.
        policy: SqlSourcePolicy,
        /// Canonical root of the selected source capture, not the latest project path.
        served_root: String,
        path: String,
        start: u32,
        end: u32,
        /// One-based line counted using LF bytes in the verified original host.
        line_1: u32,
        text: String,
    },
    SourceChanged,
    BytesNotRetained,
    Expired,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn postgres_column_read_does_not_guess_decoded_name_filters() {
        let mut request = SqlQueryRequest {
            package: PackageReference::parse("/tmp/sql-project").unwrap(),
            role: SqlQueryRole::ColumnRead,
            name: None,
            kind: None,
            parent: None,
            uses: None,
            page_rows: 1,
            output_bytes: 131_072,
            scan_rows: 7,
            scan_bytes: 229_376,
            continuation: None,
        };
        assert!(request.validate().is_ok());
        request.name = Some("id".into());
        assert!(request.validate().is_err());
    }
    #[test]
    fn cursors_are_compact_and_bind_roots_filter_and_owner() {
        let claims = SqlCursorClaims {
            manifest_root: [1; 32],
            sql_root: [2; 32],
            project: [3; 32],
            role: SqlQueryRole::Relation,
            name: Some("users".into()),
            kind: None,
            parent: None,
            uses: None,
            page_rows: 20,
            output_bytes: 131072,
            scan_rows: 100,
            scan_bytes: 229376,
            after: [4; 32],
        };
        let cursor = SqlQueryCursor::issue(claims.clone(), &[9; 32]).unwrap();
        assert!(cursor.as_str().len() < 320);
        assert_eq!(cursor.verify(&[9; 32]).unwrap(), claims);
        assert!(cursor.verify(&[8; 32]).is_err());
        let mut bytes = URL_SAFE_NO_PAD.decode(cursor.as_str()).unwrap();
        bytes[5] ^= 1;
        assert!(
            SqlQueryCursor(URL_SAFE_NO_PAD.encode(bytes))
                .verify(&[9; 32])
                .is_err()
        );
    }
    #[test]
    fn reply_admission_correlates_names_and_does_not_forge_selection() {
        use crate::{CommandId, SurfaceCommand, SurfaceReply, admit_surface_reply, package_key};
        use backend_semantic::sql_analysis::SqliteRelationKind;
        let request = SqlQueryRequest {
            package: PackageReference::parse("/tmp/sql-project").unwrap(),
            role: SqlQueryRole::Relation,
            name: Some("users".into()),
            kind: None,
            parent: None,
            uses: None,
            page_rows: 1,
            output_bytes: 131_072,
            scan_rows: 7,
            scan_bytes: 229_376,
            continuation: None,
        };
        let mut page = SqlQueryPage {
            basis: SqlQueryBasis {
                manifest_root: [1; 32],
                sql_root: Some([2; 32]),
                project: package_key(request.package.as_str()).to_bytes(),
                attempt: SqlAttemptState::LatestObserved,
                host_health: SqlHostHealth::Unknown,
                inventory_complete: false,
                policy_complete: false,
                source_occurrences_complete: false,
                gaps: Default::default(),
                implicit_selection: false,
                catalog_is_deployed: false,
            },
            hits: vec![SqlQueryHit {
                id: [3; 32],
                path: "query.sql".into(),
                dialect: SqlDialect::Sqlite,
                policy: SqlSourcePolicy::Unknown,
                host_basis: SqlHostBasis {
                    input: [4; 32],
                    source: [5; 32],
                    length: 100,
                    producer: [6; 32],
                    settings: [7; 32],
                    proposed_context: Some([8; 32]),
                    terminal: SqlNativeTerminal::Parsed {
                        source: SqlCoverage::Unavailable,
                        contextual: SqlCoverage::Partial,
                        sqlite_lineage: Some(SqliteLineageCoverage::NativeComplete),
                    },
                },
                fact: SqlFact::SqliteRelation {
                    name: "other".into(),
                    relation_kind: SqliteRelationKind::Table,
                },
                source: None,
            }],
            terminal: SqlQueryTerminal::Complete,
        };
        let reply = SurfaceReply::SqlQuery(page.clone());
        assert!(reply.admit(CommandId::SqlQuery).is_ok());
        assert!(
            admit_surface_reply(
                &SurfaceCommand::SqlQuery {
                    request: request.clone()
                },
                &reply
            )
            .is_err()
        );
        if let SqlFact::SqliteRelation { name, .. } = &mut page.hits[0].fact {
            *name = "users".into();
        }
        let reply = SurfaceReply::SqlQuery(page.clone());
        assert!(
            admit_surface_reply(
                &SurfaceCommand::SqlQuery {
                    request: request.clone()
                },
                &reply
            )
            .is_ok()
        );
        let envelope = crate::ReplyDto::new(u64::MAX, crate::CommandReply::Surface(reply));
        let encoded = crate::encode_reply_body_bounded(&envelope).unwrap();
        assert!(encoded.len() <= request.output_bytes as usize);
        page.hits.clear();
        page.basis.attempt = SqlAttemptState::NoSelectedAnalysis;
        assert!(
            SurfaceReply::SqlQuery(page)
                .admit(CommandId::SqlQuery)
                .is_err()
        );
    }

    #[test]
    fn available_source_reply_must_echo_the_requested_reference() {
        use crate::{CommandId, SurfaceCommand, SurfaceReply, admit_surface_reply};
        let claims = SqlSourceClaims {
            manifest_root: [1; 32],
            sql_root: [2; 32],
            project: [3; 32],
            host: [4; 32],
            fact: [5; 32],
        };
        let requested = SqlSourceReference::issue(claims.clone(), &[7; 32]).unwrap();
        let mut other_claims = claims;
        other_claims.fact = [6; 32];
        let other = SqlSourceReference::issue(other_claims, &[7; 32]).unwrap();
        let command = SurfaceCommand::SqlReadSource {
            reference: requested.clone(),
        };
        let mut source = SqlSourceRead::Available {
            reference: other,
            policy: SqlSourcePolicy::Unknown,
            served_root: "/tmp/project".into(),
            path: "query.sql".into(),
            start: 0,
            end: 1,
            line_1: 1,
            text: "x".into(),
        };
        let reply = SurfaceReply::SqlReadSource(source.clone());
        assert!(reply.admit(CommandId::SqlReadSource).is_ok());
        assert!(admit_surface_reply(&command, &reply).is_err());
        if let SqlSourceRead::Available { reference, .. } = &mut source {
            *reference = requested;
        }
        assert!(admit_surface_reply(&command, &SurfaceReply::SqlReadSource(source)).is_ok());
    }
}

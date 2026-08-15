//! Run sea-query / SeaORM-built statements on a [`CatalogEngine`].
//!
//! Store and versioning code build typed `SelectStatement` / SeaORM `Statement`
//! values; this module is the only place that hands rendered SQL to the engine.

use sea_orm::sea_query::{SelectStatement, SqliteQueryBuilder, Values};
use sea_orm::{DbBackend, Statement, Value as SeaValue};

use super::{CatalogEngine, EngineError, Row, Value};

/// Execute a SeaORM statement (INSERT/UPDATE/DELETE).
pub fn exec(engine: &dyn CatalogEngine, stmt: Statement) -> Result<usize, EngineError> {
    let params = stmt_params(&stmt);
    engine.execute(&stmt.sql, &params)
}

/// Run a SeaORM SELECT and map rows.
///
/// Generic over `E` because [`CatalogEngine::query_rows`] is not object-safe
/// (`Self: Sized`).
pub fn query<E, T>(
    engine: &E,
    stmt: Statement,
    map: &mut dyn FnMut(&dyn Row) -> Result<T, EngineError>,
) -> Result<Vec<T>, EngineError>
where
    E: CatalogEngine,
{
    let params = stmt_params(&stmt);
    engine.query_rows(&stmt.sql, &params, map)
}

/// Build + run a sea-query [`SelectStatement`].
pub fn query_select<E, T>(
    engine: &E,
    select: SelectStatement,
    map: &mut dyn FnMut(&dyn Row) -> Result<T, EngineError>,
) -> Result<Vec<T>, EngineError>
where
    E: CatalogEngine,
{
    let (sql, values) = select.build(SqliteQueryBuilder);
    query(
        engine,
        Statement {
            sql,
            values: Some(values),
            db_backend: DbBackend::Sqlite,
        },
        map,
    )
}

fn stmt_params(stmt: &Statement) -> Vec<Value> {
    stmt.values.as_ref().map_or_else(Vec::new, values_to_engine)
}

/// Convert sea-query bind values into the engine facade vocabulary.
pub fn values_to_engine(values: &Values) -> Vec<Value> {
    values.0.iter().cloned().map(sea_value).collect()
}

fn sea_value(value: SeaValue) -> Value {
    match value {
        SeaValue::Bool(Some(b)) => Value::Integer(i64::from(b)),
        SeaValue::TinyInt(Some(v)) => Value::Integer(i64::from(v)),
        SeaValue::SmallInt(Some(v)) => Value::Integer(i64::from(v)),
        SeaValue::Int(Some(v)) => Value::Integer(i64::from(v)),
        SeaValue::BigInt(Some(v)) => Value::Integer(v),
        SeaValue::TinyUnsigned(Some(v)) => Value::Integer(i64::from(v)),
        SeaValue::SmallUnsigned(Some(v)) => Value::Integer(i64::from(v)),
        SeaValue::Unsigned(Some(v)) => Value::Integer(i64::from(v)),
        SeaValue::BigUnsigned(Some(v)) => Value::Integer(v as i64),
        SeaValue::Float(Some(v)) => Value::Real(f64::from(v)),
        SeaValue::Double(Some(v)) => Value::Real(v),
        SeaValue::String(Some(s)) => Value::Text(*s),
        SeaValue::Char(Some(c)) => Value::Text(c.to_string()),
        SeaValue::Bytes(Some(b)) => Value::Blob(*b),
        SeaValue::Uuid(Some(u)) => Value::Blob(u.as_bytes().to_vec()),
        SeaValue::Json(Some(j)) => Value::Text(j.to_string()),
        _ => Value::Null,
    }
}

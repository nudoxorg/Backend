//! Transaction scoping.
//!
//! [`Transaction`] wraps a `BEGIN`/`COMMIT`/`ROLLBACK` triple with RAII: it
//! issues `BEGIN` on construction and, unless [`Transaction::commit`] is called,
//! rolls back on drop. This keeps a failed catalog write from leaving a
//! half-applied statement batch behind.

use crate::connection::Connection;
use crate::error::EngineError;
use crate::row::Row;
use crate::value::Value;

/// An in-progress transaction borrowing its [`Connection`] exclusively.
pub struct Transaction<'connection> {
    /// The borrowed connection the transaction runs on.
    connection: &'connection Connection,
    /// Whether the transaction has already been finalized (committed or rolled
    /// back), so `Drop` does not issue a second `ROLLBACK`.
    finalized: bool,
}

impl<'connection> Transaction<'connection> {
    /// Begin a transaction on `connection`.
    pub(crate) fn begin(connection: &'connection mut Connection) -> Result<Self, EngineError> {
        connection.execute("BEGIN", &[])?;
        Ok(Self {
            connection,
            finalized: false,
        })
    }

    /// Execute a non-query statement within the transaction.
    pub fn execute(&self, sql: &str, parameters: &[Value]) -> Result<usize, EngineError> {
        self.connection.execute(sql, parameters)
    }

    /// Run a query within the transaction, mapping every row into a `T`.
    pub fn query_rows<T>(
        &self,
        sql: &str,
        parameters: &[Value],
        map: impl FnMut(&Row<'_>) -> Result<T, EngineError>,
    ) -> Result<Vec<T>, EngineError> {
        self.connection.query_rows(sql, parameters, map)
    }

    /// Commit the transaction, making its writes durable.
    pub fn commit(mut self) -> Result<(), EngineError> {
        self.connection.execute("COMMIT", &[])?;
        self.finalized = true;
        Ok(())
    }

    /// Explicitly roll the transaction back, discarding its writes.
    pub fn rollback(mut self) -> Result<(), EngineError> {
        self.connection.execute("ROLLBACK", &[])?;
        self.finalized = true;
        Ok(())
    }
}

impl Drop for Transaction<'_> {
    fn drop(&mut self) {
        if !self.finalized {
            // Best-effort rollback; a failure here cannot be surfaced from `Drop`,
            // but the connection will roll back the open transaction on close too.
            let _ = self.connection.execute("ROLLBACK", &[]);
        }
    }
}

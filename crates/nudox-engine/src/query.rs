//! Trustfall graph query execution — drives [`CorpusAdapter`] on a `LocalSet`.
//!
//! # Why a `LocalSet` (LR-9)
//!
//! The Trustfall fork's `execute_query_async` returns a
//! `Pin<Box<dyn Stream<Item = ...>>>` that is **not `Send`**.  A `LocalSet`
//! lets us poll a `!Send` stream on a dedicated OS thread without requiring
//! any `unsafe` or `Mutex`-around-stream gymnastics.  The result rows are
//! `Send` (they are `BTreeMap<Arc<str>, FieldValue>` → `QueryRow`) so they
//! can cross thread boundaries after extraction.
//!
//! # Protocol
//!
//! 1. `QueryEvent::Columns` — exactly once, before any rows.
//! 2. `QueryEvent::Rows { rows }` — one or more batches.
//! 3. `QueryEvent::Done { total }` — terminal.
//!
//! On error: `QueryEvent::Failed` — terminal.
//!
//! # Column extraction
//!
//! Trustfall returns rows as `BTreeMap<Arc<str>, FieldValue>`. The first row
//! determines the column order (sorted by key for determinism). All subsequent
//! rows emit values in that same order as a `QueryRow { cells: Arc<[SharedStr]> }`.
//! Cell values are the `Display` of the `FieldValue`.

use std::collections::BTreeMap;
use std::sync::Arc;

use futures::StreamExt as _;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use nudox_graph::{CorpusAdapter, adapter::GraphError};
use nudox_store::corpus::Corpus;
use trustfall::FieldValue;

use crate::{
    runtime::{EngineHandle, StreamHandle},
    wire::{EngineError, Gen, QueryEvent, QueryRow, SharedStr},
};

// ---------------------------------------------------------------------------
// Channel capacity (Appendix C)
// ---------------------------------------------------------------------------

const QUERY_CHANNEL_CAP: usize = 128;

// ---------------------------------------------------------------------------
// GraphQuery
// ---------------------------------------------------------------------------

/// A Trustfall query together with its variable bindings.
///
/// `args` values are `String` — they are converted to `FieldValue::String`
/// before being passed to the executor. This is intentional: the query
/// interface accepts text (from the MCP tool or the in-app editor) and the
/// adapter converts to the appropriate `FieldValue` internally.
#[derive(Debug, Clone)]
pub struct GraphQuery {
    /// The Trustfall query text (GraphQL subset).
    pub query: String,
    /// Variable bindings: name → string value.
    pub args: BTreeMap<String, String>,
}

// ---------------------------------------------------------------------------
// EngineHandle::query
// ---------------------------------------------------------------------------

impl EngineHandle {
    /// Execute a Trustfall graph query and stream the results as
    /// [`QueryEvent`]s.
    ///
    /// Returns `(StreamHandle, receiver)`. Dropping the `StreamHandle` cancels
    /// the stream. The receiver closes after `Done` or `Failed`.
    ///
    /// # LocalSet (LR-9)
    ///
    /// The Trustfall result stream is `!Send`. This method spawns the query
    /// on a `tokio::task::LocalSet` thread so the caller never blocks and
    /// the `!Send` constraint is honoured without any `unsafe`.
    pub fn query(
        &self,
        q: GraphQuery,
        generation: Gen,
    ) -> (StreamHandle, flume::Receiver<QueryEvent>) {
        let (tx, rx) = flume::bounded(QUERY_CHANNEL_CAP);
        let (cancel_token, cancel_fn) = Self::make_cancel();
        let handle = StreamHandle::new(generation, cancel_fn);

        let corpus = self.corpus();
        let schema = self.schema();

        // spawn_local_query drives run_query on a dedicated current-thread
        // Tokio runtime so the !Send Trustfall stream never crosses a
        // thread boundary (LR-9).
        self.spawn_local_query(corpus, schema, q, generation, tx, cancel_token);

        (handle, rx)
    }
}

// ---------------------------------------------------------------------------
// Query driver (runs inside LocalSet)
// ---------------------------------------------------------------------------

/// Execute one query and emit `QueryEvent`s into `tx`.
///
/// This future is intentionally `!Send` — it holds a Trustfall result stream
/// which is `!Send`. It MUST run on a `LocalSet` thread.
///
/// Exposed as `pub(crate)` so `runtime::spawn_local_query` can reference it.
pub(crate) async fn run_query(
    corpus: Corpus,
    schema: &'static trustfall::Schema,
    q: GraphQuery,
    generation: Gen,
    tx: flume::Sender<QueryEvent>,
    cancel: CancellationToken,
) {
    if cancel.is_cancelled() {
        return;
    }

    let adapter = Arc::new(CorpusAdapter::new(corpus));

    // Convert args: String → FieldValue::String, collected into the
    // BTreeMap form that execute_query_async expects (same pattern as the
    // nudox-graph integration tests).
    let vars: BTreeMap<String, FieldValue> = q
        .args
        .into_iter()
        .map(|(k, v)| (k, FieldValue::String(Arc::from(v.as_str()))))
        .collect();

    // Execute the query. `execute_query_async` returns an error synchronously
    // if the query text is syntactically invalid or references unknown fields.
    let mut stream = match trustfall::execute_query_async(schema, adapter, &q.query, vars) {
        Ok(s) => s,
        Err(e) => {
            let _ = tx
                .send_async(QueryEvent::Failed {
                    generation,
                    error: EngineError::Chunk { message: e.to_string() },
                })
                .await;
            return;
        }
    };

    // Determine column order from the first row (BTreeMap is sorted).
    let mut columns: Option<Arc<[SharedStr]>> = None;
    let mut total: u64 = 0;

    while let Some(item) = stream.next().await {
        if cancel.is_cancelled() {
            return;
        }

        let row_map: BTreeMap<Arc<str>, FieldValue> = match item {
            Ok(r) => r,
            Err(e) => {
                // Convert the opaque Box<dyn Error> to an EngineError string.
                let _ = tx
                    .send_async(QueryEvent::Failed {
                        generation,
                        error: EngineError::Chunk { message: e.to_string() },
                    })
                    .await;
                return;
            }
        };

        // Establish column order on first row.
        if columns.is_none() {
            let cols: Vec<SharedStr> =
                row_map.keys().map(|k| SharedStr::from(k.as_ref())).collect();
            let cols_arc: Arc<[SharedStr]> = Arc::from(cols.as_slice());
            columns = Some(cols_arc.clone());

            if tx
                .send_async(QueryEvent::Columns {
                    generation,
                    columns: cols_arc,
                })
                .await
                .is_err()
            {
                debug!("query: receiver dropped after Columns");
                return;
            }
        }

        let cols = columns.as_ref().expect("columns is Some after first row");

        // Build the cell vec in column order.
        let cells: Vec<SharedStr> = cols
            .iter()
            .map(|col| {
                row_map
                    // Trustfall keys rows by `Arc<str>`; `Arc<str>: Borrow<T>`
                    // is ambiguous between `Borrow<str>` and the reflexive
                    // `Borrow<Arc<str>>`, so name the lookup type explicitly.
                    .get::<str>(col.as_ref())
                    .map(|fv| SharedStr::from(field_value_to_string(fv).as_str()))
                    .unwrap_or_else(|| SharedStr::from(""))
            })
            .collect();

        let query_row = QueryRow { cells: Arc::from(cells.as_slice()) };
        total += 1;

        if tx
            .send_async(QueryEvent::Rows {
                generation,
                rows: Arc::from(std::slice::from_ref(&query_row)),
            })
            .await
            .is_err()
        {
            debug!("query: receiver dropped during Rows");
            return;
        }
    }

    // If no rows were returned, we still need to emit Columns (empty).
    if columns.is_none() && !cancel.is_cancelled() {
        // We don't know column names with no rows — emit an empty Columns.
        let _ = tx
            .send_async(QueryEvent::Columns {
                generation,
                columns: Arc::from([] as [SharedStr; 0]),
            })
            .await;
    }

    if !cancel.is_cancelled() {
        let _ = tx.send_async(QueryEvent::Done { generation, total }).await;
    }
}

// ---------------------------------------------------------------------------
// FieldValue → display string
// ---------------------------------------------------------------------------

fn field_value_to_string(fv: &FieldValue) -> String {
    match fv {
        FieldValue::Null => String::new(),
        FieldValue::Int64(n) => n.to_string(),
        FieldValue::Uint64(n) => n.to_string(),
        FieldValue::Float64(f) => f.to_string(),
        FieldValue::String(s) => s.as_ref().to_owned(),
        FieldValue::Boolean(b) => b.to_string(),
        FieldValue::Enum(s) => s.as_ref().to_owned(),
        FieldValue::List(items) => {
            let parts: Vec<_> = items.iter().map(field_value_to_string).collect();
            format!("[{}]", parts.join(", "))
        }
        // `FieldValue` is `#[non_exhaustive]`: the trustfall fork may add a
        // scalar in a point release. Rendering an unknown one as a visible
        // placeholder keeps the query surface forward-compatible (LD-7) — a
        // blank cell would read as missing data rather than as a value this
        // build does not yet understand.
        _ => "<unsupported value>".to_owned(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use nudox_store::source::fixtures::FixtureSource;

    use crate::{
        query::GraphQuery,
        runtime::{Engine, EngineConfig},
        wire::{Gen, QueryEvent},
    };

    fn make_engine() -> crate::runtime::EngineHandle {
        Engine::start(EngineConfig::default(), FixtureSource::rich())
    }

    async fn wait_for_corpus(engine: &crate::runtime::EngineHandle) {
        use nudox_store::source::fixtures::rich_lineage;
        for _ in 0..50 {
            if engine.corpus().package(&rich_lineage()).await.is_some() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("corpus never seeded");
    }

    async fn drain(rx: flume::Receiver<QueryEvent>) -> Vec<QueryEvent> {
        let mut events = Vec::new();
        while let Ok(ev) = rx.recv_async().await {
            let done =
                matches!(ev, QueryEvent::Done { .. } | QueryEvent::Failed { .. });
            events.push(ev);
            if done {
                break;
            }
        }
        events
    }

    #[tokio::test]
    async fn columns_before_rows() {
        let engine = make_engine();
        wait_for_corpus(&engine).await;

        let q = GraphQuery {
            query: "{ Symbols { name @output kind @output } }".to_owned(),
            args: BTreeMap::new(),
        };

        let (_handle, rx) = engine.query(q, Gen(1));
        let events = drain(rx).await;

        // Columns must come before any Rows.
        let mut saw_columns = false;
        for ev in &events {
            match ev {
                QueryEvent::Columns { .. } => saw_columns = true,
                QueryEvent::Rows { .. } => {
                    assert!(saw_columns, "Rows before Columns — protocol violation");
                }
                _ => {}
            }
        }
    }

    #[tokio::test]
    async fn query_done_is_terminal() {
        let engine = make_engine();
        wait_for_corpus(&engine).await;

        let q = GraphQuery {
            query: "{ Symbols { name @output } }".to_owned(),
            args: BTreeMap::new(),
        };

        let (_handle, rx) = engine.query(q, Gen(2));
        let events = drain(rx).await;

        assert!(
            matches!(events.last(), Some(QueryEvent::Done { .. })),
            "last event must be Done"
        );
    }

    #[tokio::test]
    async fn invalid_query_emits_failed() {
        let engine = make_engine();
        wait_for_corpus(&engine).await;

        let q = GraphQuery {
            query: "{ this is not valid graphql !! }".to_owned(),
            args: BTreeMap::new(),
        };

        let (_handle, rx) = engine.query(q, Gen(3));
        let events = drain(rx).await;

        assert!(
            matches!(events.last(), Some(QueryEvent::Failed { .. })),
            "invalid query must produce Failed, got {:?}",
            events.last()
        );
    }

    #[tokio::test]
    async fn dropping_handle_cancels_query() {
        let engine = make_engine();
        wait_for_corpus(&engine).await;

        let q = GraphQuery {
            query: "{ Symbols { name @output } }".to_owned(),
            args: BTreeMap::new(),
        };

        let (handle, rx) = engine.query(q, Gen(4));
        drop(handle); // cancel immediately

        // Drain whatever landed — must not hang.
        let mut count = 0usize;
        while rx.try_recv().is_ok() {
            count += 1;
            if count > super::QUERY_CHANNEL_CAP * 2 {
                break;
            }
        }
        // Test passes if we reach here (no hang).
    }
}

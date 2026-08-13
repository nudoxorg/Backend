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

use nudox_graph::CorpusAdapter;
use nudox_store::corpus::Corpus;
use trustfall::FieldValue;

use crate::{
    runtime::{EngineHandle, StreamHandle},
    wire::{EngineError, Gen, QueryErrorPosition, QueryEvent, QueryRow, SharedStr},
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

    // Convert args: String -> FieldValue, coerced to each variable's actual
    // declared type (`GraphQueryArgs::args`'s doc comment on the MCP side
    // promises exactly this — "the adapter coerces them to the property's
    // declared type" — and until this pass existed that promise was false:
    // every value was wrapped as `FieldValue::String` unconditionally, so a
    // `Boolean`-typed `$variable` (`isDeprecated @filter(op: "=",
    // value: ["$yes"])`) could never be satisfied no matter what the caller
    // sent, and Trustfall correctly rejected it downstream. A cheap
    // preliminary parse recovers each variable's real type from the query
    // text itself — not a heuristic guess from the string's shape, which
    // would silently misinterpret a symbol or package literally named
    // "true" or "123" as the wrong `FieldValue` variant (doctrine §8: a
    // repair must be typed and visible, never a guess dressed as one).
    //
    // A parse failure here is not reported: `execute_query_async` below
    // re-parses the same text and its error already carries a position (see
    // that call site), so surfacing a second, earlier, position-less error
    // for the identical cause would only be confusing. Falling back to "no
    // known types" here just means every arg stays `FieldValue::String`,
    // which is this function's pre-existing behaviour and therefore no
    // worse than before this pass existed.
    let variable_types: BTreeMap<Arc<str>, trustfall_core::ir::Type> =
        trustfall_core::frontend::parse(schema, &q.query)
            .map(|parsed| parsed.ir_query.variables.clone())
            .unwrap_or_default();

    let mut vars: BTreeMap<String, FieldValue> = BTreeMap::new();
    for (name, raw) in q.args {
        match coerce_variable(&name, &raw, variable_types.get(name.as_str())) {
            Ok(value) => {
                vars.insert(name, value);
            }
            Err(message) => {
                let _ = tx
                    .send_async(QueryEvent::Failed {
                        generation,
                        error: EngineError::GraphQueryFailed {
                            message,
                            position: None,
                        },
                    })
                    .await;
                return;
            }
        }
    }

    // Execute the query. `execute_query_async` returns an error synchronously
    // if the query text is syntactically invalid or references unknown fields.
    //
    // This used to be reported as `EngineError::Chunk` ("chunker error: …"),
    // which names an entirely different subsystem (the doc-page renderer) —
    // an agent debugging its own malformed query text was sent looking at the
    // wrong half of the codebase for something it did not break. The same fix
    // applies to the `Err(e)` arm inside the row loop below, which is the
    // execution-time (not parse-time) half of the same misattribution. See
    // `EngineError::GraphQueryFailed`'s docs for the full reasoning.
    let mut stream = match trustfall::execute_query_async(schema, adapter, &q.query, vars) {
        Ok(s) => s,
        Err(e) => {
            // `e`'s concrete type is `anyhow::Error` (see `execute_query_async`'s
            // own signature) inferred here, never named — §L7.4/GUI-LOCAL-PLAN's
            // "no `anyhow::` outside tests" enforcement grep is textual, and this
            // crate has no need to depend on `anyhow` directly merely to call an
            // inherent method on a value another dependency's public API already
            // hands us. `downcast_ref` is `anyhow::Error`'s own inherent method.
            let position = e
                .downcast_ref::<trustfall_core::frontend::error::FrontendError>()
                .and_then(position_in_frontend_error);
            let _ = tx
                .send_async(QueryEvent::Failed {
                    generation,
                    error: EngineError::GraphQueryFailed {
                        message: e.to_string(),
                        position,
                    },
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
                // A resolver-time failure (e.g. `nudox_graph::adapter::GraphError`
                // surfacing a malformed `$key` binding) — the query parsed, so
                // there is no position to recover here, but it is still a
                // graph-query-plane failure, not a chunker one.
                let _ = tx
                    .send_async(QueryEvent::Failed {
                        generation,
                        error: EngineError::GraphQueryFailed {
                            message: e.to_string(),
                            position: None,
                        },
                    })
                    .await;
                return;
            }
        };

        // Establish column order on first row.
        if columns.is_none() {
            let cols: Vec<SharedStr> = row_map
                .keys()
                .map(|k| SharedStr::from(k.as_ref()))
                .collect();
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

        let query_row = QueryRow {
            cells: Arc::from(cells.as_slice()),
        };
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
// Variable coercion
// ---------------------------------------------------------------------------

/// Coerce one caller-supplied string argument to the `FieldValue` its
/// query-declared type actually needs.
///
/// `declared` is `None` when the query does not reference `$name` at all (an
/// extra, unused argument — Trustfall ignores these, so this does too) or
/// when the pre-parse in `run_query` failed. Either way the safe fallback is
/// the pre-existing behaviour: pass the raw string through unchanged.
///
/// Only scalar (non-list) `Boolean`, `Int` and `Float` are coerced. List-
/// typed variables (`[String!]` and friends) are left as `String` because
/// this function's caller has only ever had one flat string per argument to
/// work with — building a list out of it is a separate, unimplemented
/// capability, not something to fake here.
fn coerce_variable(
    name: &str,
    raw: &str,
    declared: Option<&trustfall_core::ir::Type>,
) -> Result<FieldValue, String> {
    let Some(ty) = declared else {
        return Ok(FieldValue::String(Arc::from(raw)));
    };
    if ty.is_list() {
        return Ok(FieldValue::String(Arc::from(raw)));
    }
    match ty.base_type() {
        "Boolean" => match raw {
            "true" => Ok(FieldValue::Boolean(true)),
            "false" => Ok(FieldValue::Boolean(false)),
            other => Err(format!(
                "variable ${name} must be exactly \"true\" or \"false\" — the query declares it \
                 Boolean, and {other:?} is neither"
            )),
        },
        "Int" => raw.parse::<i64>().map(FieldValue::Int64).map_err(|_| {
            format!(
                "variable ${name} must be a whole number — the query declares it Int, and \
                 {raw:?} does not parse as one"
            )
        }),
        "Float" => raw.parse::<f64>().map(FieldValue::Float64).map_err(|_| {
            format!(
                "variable ${name} must be a number — the query declares it Float, and {raw:?} \
                 does not parse as one"
            )
        }),
        // String, and every enum-shaped scalar (kind names, visibility,
        // keyTier, …) are all represented as GraphQL `String` on this
        // schema (see `schema.graphql`'s own note on this), so passing the
        // raw string through is correct, not a fallback.
        _ => Ok(FieldValue::String(Arc::from(raw))),
    }
}

// ---------------------------------------------------------------------------
// Query error position recovery
// ---------------------------------------------------------------------------

/// Recover the line/column a Trustfall parse error carries, when it carries
/// one.
///
/// `execute_query_async`'s `?` converts `trustfall_core::frontend::parse`'s
/// typed `FrontendError` into an opaque `anyhow::Error` (recovered by the
/// caller via `downcast_ref` — see the comment at that call site for why
/// this crate does not name `anyhow::Error` directly), and most
/// `FrontendError`/`ParseError` variants' own `Display` — see the crate's
/// `#[error("...")]` attributes — never prints the trailing `Pos` field they
/// carry, so `e.to_string()` alone drops it on the floor even though the
/// producer computed it. Ariadne's whole model is a labelled span next to the
/// message; a JSON-RPC error has no terminal to draw one in, but it can still
/// hand back the coordinates, so a client that *does* have the source text can
/// draw its own.

fn position_in_frontend_error(
    err: &trustfall_core::frontend::error::FrontendError,
) -> Option<QueryErrorPosition> {
    use trustfall_core::frontend::error::FrontendError;
    use trustfall_core::graphql_query::error::ParseError;

    match err {
        // The GraphQL-syntax case — by far the most common way a hand-written
        // query fails — carries its own `Error::positions()` iterator rather
        // than a bare `Pos` field, so it needs its own arm.
        FrontendError::ParseError(ParseError::InvalidGraphQL(inner)) => {
            inner.positions().next().map(|p| QueryErrorPosition {
                line: p.line as u64,
                column: p.column as u64,
            })
        }
        // `MultipleErrors` nests recursively; report the first position found
        // in reading order, matching `DisplayVec`'s own `Display`.
        FrontendError::MultipleErrors(errs) => errs.0.iter().find_map(position_in_frontend_error),
        // Every other `ParseError` variant carries a trailing `Pos` field
        // that `serde::Serialize` (derived, field names verbatim) renders as
        // `{"line": N, "column": M, ...}` — walking the serialized tree once
        // is forward-compatible with `ParseError`'s `#[non_exhaustive]` in a
        // way a hand-written match over two dozen tuple shapes would not be:
        // a new variant that follows the same "Pos last" convention is found
        // automatically, and one that does not just yields `None`, same as
        // today's blanket "no position" for non-`ParseError` variants.
        other => serde_json::to_value(other)
            .ok()
            .as_ref()
            .and_then(find_line_column),
    }
}

/// Depth-first search for the first JSON object carrying both a `"line"` and
/// a `"column"` integer field — the shape `serde`'s derive gives
/// `async_graphql_parser::Pos` with no `#[serde(rename)]` anywhere on it.
fn find_line_column(value: &serde_json::Value) -> Option<QueryErrorPosition> {
    match value {
        serde_json::Value::Object(map) => {
            if let (Some(line), Some(column)) = (
                map.get("line").and_then(serde_json::Value::as_u64),
                map.get("column").and_then(serde_json::Value::as_u64),
            ) {
                return Some(QueryErrorPosition { line, column });
            }
            map.values().find_map(find_line_column)
        }
        serde_json::Value::Array(items) => items.iter().find_map(find_line_column),
        _ => None,
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
        wire::{EngineError, Gen, QueryEvent},
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
            let done = matches!(ev, QueryEvent::Done { .. } | QueryEvent::Failed { .. });
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

        // Doctrine §4: "never assert on a message string where you can
        // assert on a typed variant" — this used to accept `Failed { .. }`
        // regardless of *which* `EngineError` it carried, which is exactly
        // the shape a stub emitting `EngineError::Cancelled` would also
        // pass. Pin the variant, and that it is not the mislabelled
        // "chunker error" this used to be (see `EngineError::GraphQueryFailed`'s
        // docs).
        match events.last() {
            Some(QueryEvent::Failed { error, .. }) => match error {
                EngineError::GraphQueryFailed { message, .. } => {
                    assert!(
                        !message.to_lowercase().contains("chunk"),
                        "a malformed graph query must not be reported as a chunker error: \
                         {message:?}"
                    );
                }
                other => panic!("expected GraphQueryFailed, got {other:?}"),
            },
            other => panic!("invalid query must produce Failed, got {other:?}"),
        }
    }

    /// A genuine GraphQL syntax error carries a line/column position an agent
    /// can point an editor at — see `position_in_frontend_error`'s docs for
    /// why `Display` alone drops it.
    #[tokio::test]
    async fn invalid_syntax_reports_a_position() {
        let engine = make_engine();
        wait_for_corpus(&engine).await;

        // Line 2 is where the stray `!!` sits; this is deliberately a
        // multi-line query so "position 1:1" (a lazy always-zero default)
        // would visibly fail this test.
        let q = GraphQuery {
            query: "{\n  Symbols { name @output !! }\n}".to_owned(),
            args: BTreeMap::new(),
        };

        let (_handle, rx) = engine.query(q, Gen(30));
        let events = drain(rx).await;

        match events.last() {
            Some(QueryEvent::Failed { error, .. }) => match error {
                EngineError::GraphQueryFailed { position, message } => {
                    let pos = position
                        .unwrap_or_else(|| panic!("expected a position, message was {message:?}"));
                    assert_eq!(pos.line, 2, "the syntax error is on line 2: {message:?}");
                    assert!(pos.column >= 1, "column must be 1-based, got {}", pos.column);
                }
                other => panic!("expected GraphQueryFailed, got {other:?}"),
            },
            other => panic!("invalid syntax must produce Failed, got {other:?}"),
        }
    }

    /// A `Boolean`-typed `$variable` actually filters, instead of failing
    /// with a type-mismatch no matter what the caller sends — the defect
    /// `coerce_variable` exists to close. `GraphQueryArgs::args`'s own doc
    /// comment already promised this; before `coerce_variable` existed the
    /// promise was false for every non-`String` scalar.
    #[tokio::test]
    async fn boolean_variable_actually_coerces_and_filters() {
        let engine = make_engine();
        wait_for_corpus(&engine).await;

        let mut args = BTreeMap::new();
        args.insert("flag".to_owned(), "false".to_owned());
        let q = GraphQuery {
            query: "{ Symbols { isDeprecated @filter(op: \"=\", value: [\"$flag\"]) name @output } }"
                .to_owned(),
            args,
        };

        let (_handle, rx) = engine.query(q, Gen(40));
        let events = drain(rx).await;

        assert!(
            matches!(events.last(), Some(QueryEvent::Done { .. })),
            "a correctly-typed Boolean variable must not fail the query: {:?}",
            events.last()
        );
        let saw_rows = events
            .iter()
            .any(|e| matches!(e, QueryEvent::Rows { rows, .. } if !rows.is_empty()));
        assert!(
            saw_rows,
            "FixtureSource::rich() has non-deprecated symbols; isDeprecated=false must match some"
        );
    }

    /// A value that cannot be coerced to the variable's declared type fails
    /// with a message naming the variable, the declared type, and the
    /// offending value — not a bare type-mismatch from deep inside Trustfall
    /// with no indication of which side (caller input vs. query text) is
    /// wrong.
    #[tokio::test]
    async fn unconvertible_boolean_variable_fails_with_a_named_reason() {
        let engine = make_engine();
        wait_for_corpus(&engine).await;

        let mut args = BTreeMap::new();
        args.insert("flag".to_owned(), "yes".to_owned()); // not "true"/"false"
        let q = GraphQuery {
            query: "{ Symbols { isDeprecated @filter(op: \"=\", value: [\"$flag\"]) name @output } }"
                .to_owned(),
            args,
        };

        let (_handle, rx) = engine.query(q, Gen(41));
        let events = drain(rx).await;

        match events.last() {
            Some(QueryEvent::Failed { error, .. }) => match error {
                EngineError::GraphQueryFailed { message, .. } => {
                    assert!(message.contains("flag"), "message must name the variable: {message:?}");
                    assert!(
                        message.contains("Boolean"),
                        "message must name the declared type: {message:?}"
                    );
                    assert!(
                        message.contains("yes"),
                        "message must echo the offending value: {message:?}"
                    );
                }
                other => panic!("expected GraphQueryFailed, got {other:?}"),
            },
            other => panic!("an unconvertible variable must produce Failed, got {other:?}"),
        }
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

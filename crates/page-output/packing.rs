//! One bounded prefix selector with one retained, un-emitted lookahead row.
//!
//! Its measurement callback uses the actual shared presentation/envelope
//! encoder and the proved fixed-width pp1 cost token. The owner authenticates
//! only the returned checkpoint, then egress remeasures the real signed token.

use backend_library::{
    PackedPageCheckpoint, PackedPageContext, PackedPageStop, PackedPrefixCommitment,
    PackedProjectedRowHasher, PagePackingError,
};
use serde::Serialize;

/// Complete canonical projected-row commitment, without another JSON buffer.
pub fn packed_projected_row_commitment<T: Serialize + ?Sized>(row: &T) -> Result<[u8; 32], String> {
    let mut writer = PackedProjectedRowHasher::default();
    serde_json::to_writer(&mut writer, row).map_err(|error| error.to_string())?;
    Ok(writer.finish().0)
}

/// Bounded rows and exact claims awaiting producer authentication.
pub struct PackedPrefixPage<T> {
    /// Exact canonical prefix, in the source's original order.
    pub rows: Vec<T>,
    /// Complete source/query/policy-bound claims for this page.
    pub checkpoint: PackedPageCheckpoint,
}

/// An affine source reader and exactly one pending row. Dropping or cancelling
/// this worker drops its source, so an owned lazy producer can retire normally.
pub struct PackedPageWorker<I: Iterator> {
    source: Option<I>,
    pending: Option<(I::Item, [u8; 32])>,
    next: u64,
    context: PackedPageContext,
}

impl<I: Iterator> PackedPageWorker<I> {
    /// Opens one exact ordered source at an already verified absolute ordinal.
    #[must_use]
    pub fn new(source: I, context: PackedPageContext, start: u64) -> Self {
        Self {
            source: Some(source),
            pending: None,
            next: start,
            context,
        }
    }

    /// Retains a source-verified lookahead at the worker's exact start.
    #[must_use]
    pub fn with_pending_row(mut self, row: I::Item, digest: [u8; 32]) -> Self {
        self.pending = Some((row, digest));
        self
    }

    /// Retires the current bounded source and retains only its one lookahead.
    #[must_use]
    pub fn into_pending(mut self) -> Option<(I::Item, [u8; 32])> {
        self.source = None;
        self.pending.take()
    }

    /// Retires the reader and its pending row under the existing cancellation
    /// lifecycle. No successful Complete page is manufactured.
    pub fn cancel(&mut self) {
        self.pending = None;
        self.source = None;
    }

    /// Whether the next page begins with the retained rejected/lookahead row.
    #[must_use]
    pub fn has_pending_row(&self) -> bool {
        self.pending.is_some()
    }

    /// Selects one prefix under the original row credit and byte policy.
    /// `commit` must project the same canonical row bytes as `measure`.
    /// `measure` receives a fixed-width checkpoint with a cost-only measured
    /// field; it must measure a complete response with the proved pp1 token
    /// encoding, not estimate a cursor reserve. Final signing and egress must
    /// remeasure the returned actual checkpoint/token before publication.
    pub fn next_page<E>(
        &mut self,
        mut commit: impl FnMut(&I::Item) -> Result<[u8; 32], E>,
        mut measure: impl FnMut(&[I::Item], &PackedPageCheckpoint) -> Result<usize, E>,
    ) -> Result<PackedPrefixPage<I::Item>, PackedPageFailure<E>> {
        if self.source.is_none() {
            return Err(PackedPageFailure::Retired);
        }
        let result = self.select_page(&mut commit, &mut measure);
        if result.is_err() {
            self.cancel();
        }
        result
    }

    fn select_page<E>(
        &mut self,
        commit: &mut impl FnMut(&I::Item) -> Result<[u8; 32], E>,
        measure: &mut impl FnMut(&[I::Item], &PackedPageCheckpoint) -> Result<usize, E>,
    ) -> Result<PackedPrefixPage<I::Item>, PackedPageFailure<E>> {
        let start = self.next;
        let credit = self.context.policy().rows().get();
        let byte_credit = self.context.policy().bytes() as usize;
        let mut rows = Vec::with_capacity(usize::from(credit));
        let mut prefix = PackedPrefixCommitment::new(self.context, start);
        let empty = prefix
            .checkpoint(PackedPageStop::Complete, 1)
            .map_err(PackedPageFailure::Contract)?;
        let empty_bytes = measure(&[], &empty).map_err(PackedPageFailure::Projection)?;
        if empty_bytes > byte_credit {
            return Err(PackedPageFailure::EnvelopeExceedsWireCredit {
                required_bytes: empty_bytes,
                credit_bytes: byte_credit,
            });
        }
        let stop;
        loop {
            if rows.len() == usize::from(credit) {
                stop = match self.peek(commit)? {
                    Some(row) => PackedPageStop::RowCreditReached(row),
                    None => PackedPageStop::Complete,
                };
                break;
            }
            let Some((row, digest)) = self.take(commit)? else {
                stop = PackedPageStop::Complete;
                break;
            };
            let mut candidate = prefix.clone();
            candidate
                .push(self.next, digest)
                .map_err(PackedPageFailure::Contract)?;
            rows.push(row);
            // The witness and authentication bytes are fixed-width hex. Their
            // values cannot change the pp1 token's escaped length. A future
            // row need not be consumed just to measure a prospective prefix.
            let prospective_stop = if rows.len() == usize::from(credit) {
                PackedPageStop::RowCreditReached([0; 32])
            } else {
                PackedPageStop::WireCreditReached([0; 32])
            };
            let prospective = candidate
                .checkpoint(prospective_stop, 1)
                .map_err(PackedPageFailure::Contract)?;
            let required = measure(&rows, &prospective).map_err(PackedPageFailure::Projection)?;
            if required <= byte_credit {
                self.next = self
                    .next
                    .checked_add(1)
                    .ok_or(PackedPageFailure::Contract(PagePackingError::Offset))?;
                prefix = candidate;
                continue;
            }
            if rows.len() == 1 {
                // A sole final row needs no continuation. Observe exhaustion
                // before calling that row oversized under a larger envelope.
                let complete = candidate
                    .checkpoint(PackedPageStop::Complete, 1)
                    .map_err(PackedPageFailure::Contract)?;
                let complete_bytes =
                    measure(&rows, &complete).map_err(PackedPageFailure::Projection)?;
                if complete_bytes <= byte_credit
                    && self.source.as_mut().and_then(Iterator::next).is_none()
                {
                    self.next = self
                        .next
                        .checked_add(1)
                        .ok_or(PackedPageFailure::Contract(PagePackingError::Offset))?;
                    self.source = None;
                    return Ok(PackedPrefixPage {
                        rows,
                        checkpoint: candidate
                            .checkpoint(PackedPageStop::Complete, complete_bytes as u32)
                            .map_err(PackedPageFailure::Contract)?,
                    });
                }
                return Err(PackedPageFailure::ItemExceedsWireCredit {
                    ordinal: start,
                    required_bytes: if complete_bytes > byte_credit {
                        complete_bytes
                    } else {
                        required
                    },
                    credit_bytes: byte_credit,
                });
            }
            let rejected = rows.pop().ok_or(PackedPageFailure::Retired)?;
            self.pending = Some((rejected, digest));
            stop = PackedPageStop::WireCreditReached(digest);
            break;
        }
        let provisional = prefix
            .checkpoint(stop, 1)
            .map_err(PackedPageFailure::Contract)?;
        let bytes = measure(&rows, &provisional).map_err(PackedPageFailure::Projection)?;
        if bytes > byte_credit {
            return Err(PackedPageFailure::EnvelopeExceedsWireCredit {
                required_bytes: bytes,
                credit_bytes: byte_credit,
            });
        }
        let checkpoint = prefix
            .checkpoint(stop, bytes as u32)
            .map_err(PackedPageFailure::Contract)?;
        if stop == PackedPageStop::Complete {
            self.source = None;
        }
        Ok(PackedPrefixPage { rows, checkpoint })
    }

    fn take<E>(
        &mut self,
        commit: &mut impl FnMut(&I::Item) -> Result<[u8; 32], E>,
    ) -> Result<Option<(I::Item, [u8; 32])>, PackedPageFailure<E>> {
        if let Some(row) = self.pending.take() {
            return Ok(Some(row));
        }
        self.source
            .as_mut()
            .and_then(Iterator::next)
            .map(|row| {
                commit(&row)
                    .map(|digest| (row, digest))
                    .map_err(PackedPageFailure::Projection)
            })
            .transpose()
    }

    fn peek<E>(
        &mut self,
        commit: &mut impl FnMut(&I::Item) -> Result<[u8; 32], E>,
    ) -> Result<Option<[u8; 32]>, PackedPageFailure<E>> {
        if self.pending.is_none() {
            self.pending = self.take(commit)?;
        }
        Ok(self.pending.as_ref().map(|(_, digest)| *digest))
    }
}

/// Closed bounded-packing failure; no failure fabricates a continuation.
#[derive(Debug, Eq, PartialEq)]
pub enum PackedPageFailure<E> {
    /// The existing source/projection/serializer failed.
    Projection(E),
    /// A stable contract or ordinal check failed.
    Contract(PagePackingError),
    /// One actual item cannot fit its complete applicable response.
    ItemExceedsWireCredit {
        /// Absolute ordinal of the actual first un-emitted item.
        ordinal: u64,
        /// Actual serialized complete response bytes required.
        required_bytes: usize,
        /// Original complete output credit.
        credit_bytes: usize,
    },
    /// This call's envelope cannot leave space for a valid page.
    EnvelopeExceedsWireCredit {
        /// Actual serialized complete response bytes required.
        required_bytes: usize,
        /// Original complete output credit.
        credit_bytes: usize,
    },
    /// The affine worker completed, failed, or was cancelled.
    Retired,
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::{
        JsonRpcFraming, ResponseEnvelopeContext, admit_jsonrpc_response, jsonrpc_response_bytes,
    };
    use backend_library::{
        Cursor, MAX_PACKED_WIRE_BYTES, PagePackingDetail, PagePackingPolicy, PagePackingProfile,
        PagePackingScope, QueryLimit,
    };
    use serde_json::{Value, json};

    fn context(credit: u16) -> PackedPageContext {
        let owner = Cursor::new();
        let policy = PagePackingPolicy::new(
            QueryLimit::new(credit).expect("rows"),
            MAX_PACKED_WIRE_BYTES,
            PagePackingDetail::Full,
            PagePackingProfile::McpToolV1,
            PagePackingScope::StdioJsonRpcLine,
        )
        .expect("policy");
        PackedPageContext::new(
            owner,
            owner.root().into(),
            backend_library::view_key(b"actual ordered query"),
            policy,
        )
        .expect("context")
    }

    fn response(rows: &[Value], checkpoint: &PackedPageCheckpoint, id: &Value) -> Value {
        let next = checkpoint
            .stop()
            .next_row()
            .map(|_| PackedPageCheckpoint::cost_token());
        let mut body = json!({ "rows": rows, "more": next.is_some() });
        if let Some(next) = next {
            body["nextCursor"] = json!(next);
        }
        let result = json!({ "content": [{"type":"text", "text":"complete structured page"}], "structuredContent": body, "isError": false });
        ResponseEnvelopeContext {
            id,
            framing: JsonRpcFraming::StdioLine,
        }
        .success(result)
    }

    fn measure(
        rows: &[Value],
        checkpoint: &PackedPageCheckpoint,
        id: &Value,
    ) -> Result<usize, String> {
        let response = crate::attach_jsonrpc_wire_budget(
            response(rows, checkpoint, id),
            JsonRpcFraming::StdioLine,
        );
        Ok(jsonrpc_response_bytes(&response, JsonRpcFraming::StdioLine))
    }

    #[test]
    fn heterogeneous_large_rows_traverse_once_with_one_retained_lookahead() {
        let all = (0..200)
            .map(|at| json!({ "ordinal":at, "text": "é\"\\\n文".repeat(80 + at % 7 * 30) }))
            .collect::<Vec<_>>();
        let mut worker = PackedPageWorker::new(all.clone().into_iter(), context(200), 0);
        let mut observed = Vec::new();
        let mut pages = 0;
        loop {
            // IDs legitimately change between calls; they affect actual cost,
            // while stable query/policy and source order remain the same.
            let id = json!(format!("call-{pages}-{}", "x".repeat(pages * 13)));
            let page = worker
                .next_page(packed_projected_row_commitment, |rows, checkpoint| {
                    measure(rows, checkpoint, &id)
                })
                .expect("packed page");
            let encoded = admit_jsonrpc_response(
                response(&page.rows, &page.checkpoint, &id),
                JsonRpcFraming::StdioLine,
            )
            .expect("real whole response fits");
            assert_eq!(
                page.checkpoint.measured_bytes() as usize,
                jsonrpc_response_bytes(&encoded, JsonRpcFraming::StdioLine)
            );
            assert!(page.rows.len() <= 200);
            assert_eq!(page.checkpoint.context().policy().rows().get(), 200);
            let next = page.checkpoint.next() as usize;
            observed.extend(page.rows);
            pages += 1;
            if page.checkpoint.stop() == PackedPageStop::Complete {
                break;
            }
            assert!(worker.has_pending_row());
            assert_eq!(
                page.checkpoint.stop().next_row(),
                Some(packed_projected_row_commitment(&all[next]).expect("lookahead"))
            );
        }
        assert!(pages > 1);
        assert_eq!(observed, all);
        assert!(matches!(
            worker.next_page(packed_projected_row_commitment, |rows, checkpoint| measure(
                rows,
                checkpoint,
                &json!(9)
            )),
            Err(PackedPageFailure::Retired)
        ));
    }

    #[test]
    fn exactly_full_final_page_is_complete_only_after_exhaustion() {
        let rows = vec![json!({"ordinal":0}), json!({"ordinal":1})];
        let mut worker = PackedPageWorker::new(rows.clone().into_iter(), context(2), 0);
        let page = worker
            .next_page(packed_projected_row_commitment, |rows, checkpoint| {
                measure(rows, checkpoint, &json!(1))
            })
            .expect("page");
        assert_eq!(page.rows, rows);
        assert_eq!(page.checkpoint.stop(), PackedPageStop::Complete);
        assert!(!worker.has_pending_row());
        let mut worker = PackedPageWorker::new(
            vec![json!(0), json!(1), json!(2)].into_iter(),
            context(2),
            0,
        );
        let page = worker
            .next_page(packed_projected_row_commitment, |rows, checkpoint| {
                measure(rows, checkpoint, &json!(1))
            })
            .expect("continued");
        assert!(matches!(
            page.checkpoint.stop(),
            PackedPageStop::RowCreditReached(_)
        ));
        let successor = worker
            .next_page(packed_projected_row_commitment, |rows, checkpoint| {
                measure(rows, checkpoint, &json!(2))
            })
            .expect("successor");
        assert_eq!(successor.rows, vec![json!(2)]);
    }

    #[test]
    fn cold_range_verification_reopens_the_witnessed_row_at_exact_next() {
        let all = (0..20)
            .map(|at| json!({"ordinal":at,"body":"x".repeat(4000)}))
            .collect::<Vec<_>>();
        let mut worker = PackedPageWorker::new(all.clone().into_iter(), context(200), 0);
        let first = worker
            .next_page(packed_projected_row_commitment, |rows, checkpoint| {
                measure(rows, checkpoint, &json!("first"))
            })
            .expect("first");
        let next = first.checkpoint.next() as usize;
        let mut actual = PackedPrefixCommitment::new(context(200), first.checkpoint.start());
        for (at, row) in all[..next].iter().enumerate() {
            actual
                .push(
                    at as u64,
                    packed_projected_row_commitment(row).expect("source row"),
                )
                .expect("ordinal");
        }
        first
            .checkpoint
            .verify_source_range(
                &actual,
                Some(packed_projected_row_commitment(&all[next]).expect("source lookahead")),
            )
            .expect("exact source admission");
        let mut cold = PackedPageWorker::new(
            all.clone().into_iter().skip(next),
            context(200),
            next as u64,
        );
        let page = cold
            .next_page(packed_projected_row_commitment, |rows, checkpoint| {
                measure(rows, checkpoint, &json!("different request ID"))
            })
            .expect("cold successor");
        assert_eq!(page.rows.first(), Some(&all[next]));
        let hot = worker
            .next_page(packed_projected_row_commitment, |rows, checkpoint| {
                measure(rows, checkpoint, &json!("different request ID"))
            })
            .expect("hot successor");
        assert_eq!(hot.rows, page.rows);
        assert_eq!(hot.checkpoint, page.checkpoint);
    }

    #[test]
    fn oversized_item_and_huge_envelope_are_closed_failures_without_cursor() {
        let mut worker =
            PackedPageWorker::new(vec![json!("x".repeat(50000))].into_iter(), context(200), 0);
        assert!(matches!(
            worker.next_page(packed_projected_row_commitment, |rows, checkpoint| measure(
                rows,
                checkpoint,
                &json!(1)
            )),
            Err(PackedPageFailure::ItemExceedsWireCredit { ordinal: 0, .. })
        ));
        assert!(!worker.has_pending_row());
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let count = calls.clone();
        let source = vec![json!(1)]
            .into_iter()
            .inspect(move |_| count.set(count.get() + 1));
        let mut worker = PackedPageWorker::new(source, context(200), 0);
        assert!(matches!(
            worker.next_page(packed_projected_row_commitment, |rows, checkpoint| measure(
                rows,
                checkpoint,
                &json!("x".repeat(50000))
            )),
            Err(PackedPageFailure::EnvelopeExceedsWireCredit { .. })
        ));
        assert_eq!(calls.get(), 0);
    }

    #[test]
    fn cancellation_retires_the_pending_slot_and_source() {
        let mut worker = PackedPageWorker::new(vec![json!(0), json!(1)].into_iter(), context(1), 0);
        let page = worker
            .next_page(packed_projected_row_commitment, |rows, checkpoint| {
                measure(rows, checkpoint, &json!(1))
            })
            .expect("first");
        assert!(page.checkpoint.stop().next_row().is_some());
        assert!(worker.has_pending_row());
        worker.cancel();
        assert!(!worker.has_pending_row());
        assert!(matches!(
            worker.next_page(packed_projected_row_commitment, |rows, checkpoint| measure(
                rows,
                checkpoint,
                &json!(2)
            )),
            Err(PackedPageFailure::Retired)
        ));
    }
}

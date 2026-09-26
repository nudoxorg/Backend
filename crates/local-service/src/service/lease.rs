//! Leased local subscriptions for one owner loop.
//!
//! Command admission and the owner loop stay with the service. This module
//! allocates leases and serves durable subscription operations against the
//! retained cursor.

use super::{
    CompletionAdmission, DurableLease, DurableSnapshot, LocaldOwner, ReplicationAdmission,
    decode_cursor_for_service, map_queue_error, subscription, wait_for_daemon_reply,
};
use crate::protocol::{EngineStatus, ProtocolError};
use backend_engine::{
    Cursor, CursorEvent, DaemonReply, LocalSubscriptionId, LocalSubscriptionOperation,
    LocalSubscriptionRequest, LocalSubscriptionResponse, SubscriptionReply, ViewPageCursor,
};
use std::time::{Duration, Instant};

impl<M, V, A, F, C, R> LocaldOwner<M, V, A, F, C, R>
where
    M: backend_engine::WorkspaceModel,
    V: backend_engine::OutputAdmissionValidator + backend_engine::SemanticCoverageValidator,
    A: backend_engine::AttestationVerifier + Send + Sync + 'static,
    C: CompletionAdmission<M, V, A>,
    R: ReplicationAdmission<M, V, A>,
{
    fn allocate_lease(&mut self, request_id: u64, cursor: &[u8]) -> LocalSubscriptionId {
        loop {
            self.next_lease_nonce = self.next_lease_nonce.wrapping_add(1);
            let mut hasher = blake3::Hasher::new();
            hasher.update(b"backend-locald-subscription-lease\0");
            hasher.update(&request_id.to_be_bytes());
            hasher.update(&self.next_lease_nonce.to_be_bytes());
            hasher.update(cursor);
            let digest = hasher.finalize();
            let mut bytes = [0_u8; 16];
            let byte_len = bytes.len();
            bytes.copy_from_slice(&digest.as_bytes()[..byte_len]);
            if bytes == [0_u8; 16] {
                bytes[0] = 1;
            }
            let lease = LocalSubscriptionId::from_bytes(bytes);
            if !self.leases.contains_key(&lease) {
                return lease;
            }
        }
    }

    fn request_subscription(
        &mut self,
        request_id: u64,
        cursor: &[u8],
        credit: usize,
    ) -> Result<SubscriptionReply, ProtocolError> {
        let receiver = self
            .daemon
            .client()
            .request(
                request_id,
                crate::Request::Subscribe {
                    cursor: cursor.to_vec().into_boxed_slice(),
                    credit,
                },
            )
            .map_err(|error| map_queue_error(&error))?;
        if !self.daemon.serve_one() {
            return Err(ProtocolError::Closed);
        }
        match wait_for_daemon_reply(&mut self.daemon, &receiver)? {
            DaemonReply::Subscribed(Ok(reply)) => Ok(reply),
            DaemonReply::Subscribed(Err(error)) => Err(ProtocolError::InvalidControl(
                subscription_error_text(&error),
            )),
            DaemonReply::Commit(_)
            | DaemonReply::Query(_)
            | DaemonReply::Replicated(_)
            | DaemonReply::Completed(_) => Err(ProtocolError::InvalidControl(
                "subscription reply used the wrong engine lane",
            )),
        }
    }

    fn encode_reply_payload(
        &mut self,
        reply: SubscriptionReply,
        requested_cursor: &[u8],
    ) -> Result<Box<[u8]>, ProtocolError> {
        match subscription::subscription_status(&self.daemon, reply, Some(requested_cursor))? {
            EngineStatus::AcceptedPayload(payload) => Ok(payload),
            EngineStatus::Accepted
            | EngineStatus::Queued { .. }
            | EngineStatus::Rejected(_)
            | EngineStatus::Subscription(_) => Err(ProtocolError::InvalidControl(
                "subscription reply omitted its typed payload",
            )),
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "open response handling preserves one lease transition"
    )]
    fn open_subscription(
        &mut self,
        request_id: u64,
        cursor: &[u8],
        credit: usize,
        lease_ms: u64,
    ) -> Result<EngineStatus, ProtocolError> {
        if credit == 0 || credit > backend_engine::MAX_SUBSCRIPTION_EVENTS || lease_ms == 0 {
            return Err(ProtocolError::InvalidControl("subscription lease bounds"));
        }
        let reply = self.request_subscription(request_id, cursor, credit)?;
        let lease = self.allocate_lease(request_id, cursor);
        let expires_at = Instant::now()
            .checked_add(Duration::from_millis(lease_ms))
            .unwrap_or_else(Instant::now);
        let response = match reply {
            SubscriptionReply::Accepted { credit } => {
                let current = subscription::owner_cursor(&self.daemon)?;
                let current_bytes = current.encode_control();
                self.leases.insert(
                    lease,
                    DurableLease {
                        cursor: current_bytes.clone(),
                        credit,
                        expires_at,
                        snapshot: None,
                    },
                );
                LocalSubscriptionResponse::Opened {
                    request_id,
                    lease,
                    cursor: current_bytes,
                    credit,
                    lease_ms,
                }
            }
            SubscriptionReply::Events {
                credit,
                cursor: target,
                events,
            } => {
                let payload = self.encode_reply_payload(
                    SubscriptionReply::Events {
                        credit,
                        cursor: target.clone(),
                        events: events.clone(),
                    },
                    cursor,
                )?;
                let root = self.daemon.engine().daemon().library().view();
                let target_cursor = Cursor::decode_control_for_root(&target, root)
                    .map_err(|_| ProtocolError::InvalidControl("subscription target cursor"))?;
                let previous = event_previous_cursor(cursor, target_cursor, &events)
                    .unwrap_or_else(|| cursor.to_vec().into_boxed_slice());
                self.leases.insert(
                    lease,
                    DurableLease {
                        cursor: target.clone(),
                        credit,
                        expires_at,
                        snapshot: None,
                    },
                );
                LocalSubscriptionResponse::Batch {
                    request_id,
                    lease,
                    previous,
                    cursor: target,
                    credit,
                    payload,
                }
            }
            SubscriptionReply::ResetWithRoot {
                credit,
                cursor: target,
                root,
                reason,
            } => {
                let target_cursor = decode_cursor_for_service(&target, &root)?;
                let page = subscription::snapshot_page(
                    &root,
                    target_cursor,
                    reason,
                    ViewPageCursor::first(&root),
                    credit.min(backend_engine::MAX_SNAPSHOT_PAGE_ROWS),
                )?;
                let next_cursor = page.page().next();
                let next_token = page
                    .next_token()
                    .map_err(|_| ProtocolError::InvalidControl("snapshot page token"))?;
                let payload = backend_engine::encode_snapshot_page_dto(&page)
                    .map_err(|_| ProtocolError::InvalidControl("snapshot page encoding"))?;
                self.leases.insert(
                    lease,
                    DurableLease {
                        cursor: target.clone(),
                        credit,
                        expires_at,
                        snapshot: next_cursor.map(|next| DurableSnapshot {
                            root,
                            cursor: target_cursor,
                            reason,
                            next: Some(next),
                            next_token: next_token.clone(),
                        }),
                    },
                );
                LocalSubscriptionResponse::SnapshotPage {
                    request_id,
                    lease,
                    page: Box::new([]),
                    next: next_token,
                    credit,
                    payload: payload.into_boxed_slice(),
                }
            }
            SubscriptionReply::Reset { .. } => {
                return Err(ProtocolError::InvalidControl(
                    "subscription reset omitted its replacement root",
                ));
            }
        };
        Ok(EngineStatus::Subscription(response))
    }

    pub(super) fn durable_subscription(
        &mut self,
        request_id: u64,
        request: LocalSubscriptionRequest,
    ) -> Result<EngineStatus, ProtocolError> {
        if request.request_id != request_id {
            return Err(ProtocolError::InvalidControl(
                "subscription request correlation mismatch",
            ));
        }
        match request.operation {
            LocalSubscriptionOperation::Open {
                cursor,
                credit,
                lease_ms,
            } => self.open_subscription(request_id, cursor.as_ref(), credit, lease_ms),
            LocalSubscriptionOperation::Resume {
                lease,
                cursor,
                credit,
                lease_ms,
            } => self.resume_subscription(request_id, lease, cursor.as_ref(), credit, lease_ms),
            LocalSubscriptionOperation::Credit { lease, credit } => {
                self.credit_subscription(request_id, lease, credit)
            }
            LocalSubscriptionOperation::Ack { lease, cursor } => {
                self.ack_subscription(request_id, lease, cursor)
            }
            LocalSubscriptionOperation::Renew {
                lease,
                cursor,
                credit,
                lease_ms,
            } => self.renew_subscription(request_id, lease, cursor, credit, lease_ms),
            LocalSubscriptionOperation::Cancel { lease } => {
                if self.leases.remove(&lease).is_some() {
                    Ok(EngineStatus::Subscription(
                        LocalSubscriptionResponse::Cancelled { request_id, lease },
                    ))
                } else {
                    Err(ProtocolError::InvalidControl("unknown subscription lease"))
                }
            }
            LocalSubscriptionOperation::Page {
                lease,
                page,
                credit,
            } => self.page_subscription(request_id, lease, page, credit),
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "resume response handling preserves one lease transition"
    )]
    fn resume_subscription(
        &mut self,
        request_id: u64,
        lease: LocalSubscriptionId,
        cursor: &[u8],
        credit: usize,
        lease_ms: u64,
    ) -> Result<EngineStatus, ProtocolError> {
        let Some(state) = self.leases.get(&lease) else {
            return Err(ProtocolError::InvalidControl("unknown subscription lease"));
        };
        if state.expires_at <= Instant::now() {
            self.leases.remove(&lease);
            return Err(ProtocolError::InvalidControl("subscription lease expired"));
        }
        if state.snapshot.is_some() || state.cursor.as_ref() != cursor {
            return Err(ProtocolError::InvalidControl(
                "subscription resume cursor is not the lease cursor",
            ));
        }
        if credit == 0 || credit > backend_engine::MAX_SUBSCRIPTION_EVENTS || lease_ms == 0 {
            return Err(ProtocolError::InvalidControl("subscription lease bounds"));
        }
        let reply = self.request_subscription(request_id, cursor, credit)?;
        let expires_at = Instant::now()
            .checked_add(Duration::from_millis(lease_ms))
            .unwrap_or_else(Instant::now);
        let response = match reply {
            SubscriptionReply::Accepted { credit } => {
                let current = subscription::owner_cursor(&self.daemon)?;
                let current_bytes = current.encode_control();
                let state = self
                    .leases
                    .get_mut(&lease)
                    .ok_or(ProtocolError::InvalidControl("unknown subscription lease"))?;
                state.cursor.clone_from(&current_bytes);
                state.credit = credit;
                state.expires_at = expires_at;
                LocalSubscriptionResponse::Resumed {
                    request_id,
                    lease,
                    cursor: current_bytes,
                    credit,
                    lease_ms,
                }
            }
            SubscriptionReply::Events {
                credit,
                cursor: target,
                events,
            } => {
                let payload = self.encode_reply_payload(
                    SubscriptionReply::Events {
                        credit,
                        cursor: target.clone(),
                        events: events.clone(),
                    },
                    cursor,
                )?;
                let root = self.daemon.engine().daemon().library().view();
                let target_cursor = Cursor::decode_control_for_root(&target, root)
                    .map_err(|_| ProtocolError::InvalidControl("subscription target cursor"))?;
                let previous = event_previous_cursor(cursor, target_cursor, &events)
                    .unwrap_or_else(|| cursor.to_vec().into_boxed_slice());
                let state = self
                    .leases
                    .get_mut(&lease)
                    .ok_or(ProtocolError::InvalidControl("unknown subscription lease"))?;
                state.cursor.clone_from(&target);
                state.credit = credit;
                state.expires_at = expires_at;
                LocalSubscriptionResponse::Batch {
                    request_id,
                    lease,
                    previous,
                    cursor: target,
                    credit,
                    payload,
                }
            }
            SubscriptionReply::ResetWithRoot {
                credit,
                cursor: target,
                root,
                reason,
            } => {
                let target_cursor = decode_cursor_for_service(&target, &root)?;
                let page = subscription::snapshot_page(
                    &root,
                    target_cursor,
                    reason,
                    ViewPageCursor::first(&root),
                    credit.min(backend_engine::MAX_SNAPSHOT_PAGE_ROWS),
                )?;
                let next_cursor = page.page().next();
                let next_token = page
                    .next_token()
                    .map_err(|_| ProtocolError::InvalidControl("snapshot page token"))?;
                let payload = backend_engine::encode_snapshot_page_dto(&page)
                    .map_err(|_| ProtocolError::InvalidControl("snapshot page encoding"))?;
                let state = self
                    .leases
                    .get_mut(&lease)
                    .ok_or(ProtocolError::InvalidControl("unknown subscription lease"))?;
                state.cursor.clone_from(&target);
                state.credit = credit;
                state.expires_at = expires_at;
                state.snapshot = next_cursor.map(|next_cursor| DurableSnapshot {
                    root,
                    cursor: target_cursor,
                    reason,
                    next: Some(next_cursor),
                    next_token: next_token.clone(),
                });
                LocalSubscriptionResponse::SnapshotPage {
                    request_id,
                    lease,
                    page: Box::new([]),
                    next: next_token,
                    credit,
                    payload: payload.into_boxed_slice(),
                }
            }
            SubscriptionReply::Reset { .. } => {
                return Err(ProtocolError::InvalidControl(
                    "subscription reset omitted its replacement root",
                ));
            }
        };
        Ok(EngineStatus::Subscription(response))
    }

    fn credit_subscription(
        &mut self,
        request_id: u64,
        lease: LocalSubscriptionId,
        credit: usize,
    ) -> Result<EngineStatus, ProtocolError> {
        if credit == 0 || credit > backend_engine::MAX_SUBSCRIPTION_EVENTS {
            return Err(ProtocolError::InvalidControl("subscription credit bounds"));
        }
        if self
            .leases
            .get(&lease)
            .is_none_or(|state| state.expires_at <= Instant::now())
        {
            self.leases.remove(&lease);
            return Err(ProtocolError::InvalidControl("subscription lease expired"));
        }
        let state = self
            .leases
            .get_mut(&lease)
            .ok_or(ProtocolError::InvalidControl("unknown subscription lease"))?;
        state.credit = state
            .credit
            .checked_add(credit)
            .ok_or(ProtocolError::InvalidControl(
                "subscription credit overflow",
            ))?;
        if state.credit > backend_engine::MAX_SUBSCRIPTION_EVENTS {
            return Err(ProtocolError::InvalidControl("subscription credit bounds"));
        }
        Ok(EngineStatus::Subscription(
            LocalSubscriptionResponse::Renewed {
                request_id,
                lease,
                cursor: state.cursor.clone(),
                credit: state.credit,
                lease_ms: state
                    .expires_at
                    .saturating_duration_since(Instant::now())
                    .as_millis()
                    .try_into()
                    .unwrap_or(u64::MAX),
            },
        ))
    }

    fn ack_subscription(
        &mut self,
        request_id: u64,
        lease: LocalSubscriptionId,
        cursor: Box<[u8]>,
    ) -> Result<EngineStatus, ProtocolError> {
        let state = self
            .leases
            .get(&lease)
            .ok_or(ProtocolError::InvalidControl("unknown subscription lease"))?;
        if state.expires_at <= Instant::now() {
            self.leases.remove(&lease);
            return Err(ProtocolError::InvalidControl("subscription lease expired"));
        }
        if state.cursor.as_ref() != cursor.as_ref() {
            return Err(ProtocolError::InvalidControl(
                "subscription acknowledgement cursor mismatch",
            ));
        }
        Ok(EngineStatus::Subscription(
            LocalSubscriptionResponse::Acked {
                request_id,
                lease,
                cursor,
            },
        ))
    }

    fn renew_subscription(
        &mut self,
        request_id: u64,
        lease: LocalSubscriptionId,
        cursor: Box<[u8]>,
        credit: usize,
        lease_ms: u64,
    ) -> Result<EngineStatus, ProtocolError> {
        if credit == 0 || credit > backend_engine::MAX_SUBSCRIPTION_EVENTS || lease_ms == 0 {
            return Err(ProtocolError::InvalidControl("subscription lease bounds"));
        }
        let state = self
            .leases
            .get_mut(&lease)
            .ok_or(ProtocolError::InvalidControl("unknown subscription lease"))?;
        if state.expires_at <= Instant::now() || state.cursor.as_ref() != cursor.as_ref() {
            return Err(ProtocolError::InvalidControl(
                "subscription renewal cursor or lease mismatch",
            ));
        }
        state.credit = credit;
        state.expires_at = Instant::now()
            .checked_add(Duration::from_millis(lease_ms))
            .unwrap_or_else(Instant::now);
        Ok(EngineStatus::Subscription(
            LocalSubscriptionResponse::Renewed {
                request_id,
                lease,
                cursor,
                credit,
                lease_ms,
            },
        ))
    }

    fn page_subscription(
        &mut self,
        request_id: u64,
        lease: LocalSubscriptionId,
        page_token: Box<[u8]>,
        credit: usize,
    ) -> Result<EngineStatus, ProtocolError> {
        if credit == 0 || credit > backend_engine::MAX_SNAPSHOT_PAGE_ROWS {
            return Err(ProtocolError::InvalidControl("snapshot page credit bounds"));
        }
        if self
            .leases
            .get(&lease)
            .is_none_or(|state| state.expires_at <= Instant::now())
        {
            self.leases.remove(&lease);
            return Err(ProtocolError::InvalidControl("subscription lease expired"));
        }
        let snapshot = self
            .leases
            .get_mut(&lease)
            .and_then(|state| state.snapshot.take())
            .ok_or(ProtocolError::InvalidControl(
                "subscription has no pending snapshot",
            ))?;
        if snapshot.next_token.as_deref() != Some(page_token.as_ref()) {
            self.leases
                .get_mut(&lease)
                .ok_or(ProtocolError::InvalidControl("unknown subscription lease"))?
                .snapshot = Some(snapshot);
            return Err(ProtocolError::InvalidControl(
                "snapshot page continuation mismatch or replay",
            ));
        }
        let next_cursor = snapshot.next.ok_or(ProtocolError::InvalidControl(
            "subscription snapshot is complete",
        ))?;
        let page = subscription::snapshot_page(
            &snapshot.root,
            snapshot.cursor,
            snapshot.reason,
            next_cursor,
            credit,
        )?;
        let next_cursor = page.page().next();
        let next_token = page
            .next_token()
            .map_err(|_| ProtocolError::InvalidControl("snapshot page token"))?;
        let payload = backend_engine::encode_snapshot_page_dto(&page)
            .map_err(|_| ProtocolError::InvalidControl("snapshot page encoding"))?;
        let state = self
            .leases
            .get_mut(&lease)
            .ok_or(ProtocolError::InvalidControl("unknown subscription lease"))?;
        state.cursor = snapshot.cursor.encode_control();
        state.credit = credit;
        state.snapshot = next_cursor.map(|next| DurableSnapshot {
            root: snapshot.root,
            cursor: snapshot.cursor,
            reason: snapshot.reason,
            next: Some(next),
            next_token: next_token.clone(),
        });
        Ok(EngineStatus::Subscription(
            LocalSubscriptionResponse::SnapshotPage {
                request_id,
                lease,
                page: page_token,
                next: next_token,
                credit,
                payload: payload.into_boxed_slice(),
            },
        ))
    }
}

fn event_previous_cursor(
    requested: &[u8],
    mut target: Cursor,
    events: &[CursorEvent],
) -> Option<Box<[u8]>> {
    if !requested.is_empty() {
        return Some(requested.to_vec().into_boxed_slice());
    }
    for event in events.iter().rev() {
        target = target.rewind_event(event).ok()?;
    }
    Some(target.encode_control())
}

fn subscription_error_text(error: &backend_engine::DaemonError) -> &'static str {
    match error {
        backend_engine::DaemonError::SubscriptionCredit => "subscription credit rejected",
        backend_engine::DaemonError::CursorInvalid => "subscription cursor rejected",
        _ => "subscription request rejected",
    }
}

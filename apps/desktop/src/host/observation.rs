//! One publication observer on the existing owner worker. The actor supplies
//! its first admitted root; subsequent reads share this latest worker root.
//! No socket, hydration, or retry sleep runs on GPUI. Socket waits are
//! interruptible; native dial/authentication filesystem syscalls and decoding
//! are not preemptible. Reset cancellation is checked between bounded frames.

use crate::runtime::owner::{OwnerGate, PublicationAdmission};
use backend_client::{ClientError, LocalSubscriptionTransport, PublicationLease};
use std::path::Path;
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(250);
const IO: Duration = Duration::from_secs(1);
const RETRIES: usize = 3;
const FIRST_ROOT: Duration = Duration::from_secs(60);

/// Keeps the host alive and observes its exact serving attachment until
/// closure, or until an explicit retry replaces a failed subscription.
pub(super) fn serve(gate: &OwnerGate, endpoint: &Path) -> bool {
    let Some(mut attachment) = gate.ready_attachment() else {
        return gate.await_close_or_restart();
    };
    let Some(mut cancel) = gate.observation_scope(attachment) else {
        return gate.await_close_or_restart();
    };
    let first_root = Instant::now();
    let mut lease: Option<PublicationLease> = None;
    let mut failures = 0;
    let mut last_renew = Instant::now();
    let mut connection = None;
    let mut replace_attachment = false;
    while !cancel.is_cancelled() {
        let Some((root, cursor)) = gate.publication(attachment) else {
            if first_root.elapsed() >= FIRST_ROOT {
                gate.observation_failed(attachment, "the owner answered, but the initial complete root was not admitted within 60 s".into());
                break;
            }
            if !gate.observation_pause(attachment, POLL) {
                break;
            }
            continue;
        };
        // Dial is timeout-bounded; once connected, the exact cloned socket
        // is interruptible even when the protocol rotates its connection.
        if connection.is_none() {
            match LocalSubscriptionTransport::connect_with_timeouts(endpoint, IO, IO) {
                Ok(transport) => connection = Some(transport),
                Err(error) => {
                    failures += 1;
                    if failures >= RETRIES {
                        gate.observation_failed(attachment, error.to_string().into());
                        break;
                    }
                    if !gate.observation_pause(attachment, POLL * failures as u32) {
                        break;
                    }
                    continue;
                }
            }
        }
        let transport = connection.as_mut().expect("connection installed above");
        let Some(interrupt) = transport.interrupt_handle() else {
            gate.observation_failed(
                attachment,
                "publication transport has no cancellation handle".into(),
            );
            break;
        };
        let _wake = cancel.on_cancel(move || interrupt.interrupt());
        let result = match &mut lease {
            Some(state) => transport.resume_publications(state, &|| cancel.is_cancelled()),
            None => transport
                .acquire_publications(root, cursor, &|| cancel.is_cancelled())
                .map(|state| {
                    lease = Some(state);
                }),
        };
        if cancel.is_cancelled() {
            break;
        }
        match result {
            Ok(()) => {
                let state = lease
                    .as_ref()
                    .expect("successful acquisition installed state");
                match gate.publish_view(attachment, state.root(), state.cursor()) {
                    PublicationAdmission::Admitted | PublicationAdmission::Obsolete => {}
                    PublicationAdmission::Withdrawn => break,
                    PublicationAdmission::Invalid => {
                        gate.observation_failed(
                            attachment,
                            "publication changed producer stream identity".into(),
                        );
                        break;
                    }
                }
                // Resume itself extends the producer lease. An explicit
                // renewal periodically validates quiet retained ownership.
                if last_renew.elapsed() >= Duration::from_secs(5) {
                    if let Err(error) = transport.renew_publications(state) {
                        connection = None;
                        failures += 1;
                        if failures >= RETRIES {
                            gate.observation_failed(attachment, error.to_string().into());
                            break;
                        }
                        if matches!(error, ClientError::Protocol(_)) {
                            lease = None;
                            let Some((next_attachment, next_cancel)) =
                                gate.replace_observation(attachment)
                            else {
                                break;
                            };
                            attachment = next_attachment;
                            cancel = next_cancel;
                            replace_attachment = true;
                        }
                        if !gate.observation_pause(attachment, POLL * failures as u32) {
                            break;
                        }
                        continue;
                    }
                    last_renew = Instant::now();
                }
                failures = 0;
                if replace_attachment {
                    if !gate.complete_observation(attachment) {
                        break;
                    }
                    replace_attachment = false;
                }
            }
            Err(error) => {
                // Transport loss resumes the exact existing lease. A
                // rejected Resume (including expiry, restart, or a lost
                // advanced reply) requires fresh acquisition and a new
                // attachment fence immediately, then reopen readiness only
                // after complete admission. The server's
                // per-owner lease nonce must prevent cross-boot ID reuse.
                connection = None;
                failures += 1;
                if failures >= RETRIES {
                    gate.observation_failed(attachment, error.to_string().into());
                    break;
                }
                if lease.is_some() && matches!(error, ClientError::Protocol(_)) {
                    lease = None;
                    let Some((next_attachment, next_cancel)) = gate.replace_observation(attachment)
                    else {
                        break;
                    };
                    attachment = next_attachment;
                    cancel = next_cancel;
                    replace_attachment = true;
                }
                if !gate.observation_pause(attachment, POLL * failures as u32) {
                    break;
                }
                continue;
            }
        }
        if !gate.observation_pause(attachment, POLL) {
            break;
        }
    }
    // A cancelled exact socket is dropped, never used for another blocking
    // teardown call. Finite producer expiry bounds its residual retention.
    drop(connection);
    gate.await_close_or_restart()
}

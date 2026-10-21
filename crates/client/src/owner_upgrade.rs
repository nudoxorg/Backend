//! A compatibility probe precedes every command submitted to an old locald.
//! Only an authenticated automatic Linux owner can be retired; no mutation is
//! retried, and a probe never publishes or indexes a project.
use super::{
    ClientError, Command, CommandDto, RequestContext, Session, UnixCommandTransport, admit_reply,
    encode_request,
};
use backend_runtime::WorkspacePaths;
use std::time::{Duration, Instant};

const PROBE_ID: u64 = u64::MAX;
const OUTLINE_CAPABILITY_PROBE_ID: u64 = u64::MAX - 1;
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProbeEnvelope {
    version: u16,
    request_id: u64,
    #[serde(rename = "reply")]
    _reply: serde::de::IgnoredAny,
    #[serde(rename = "certificate")]
    _certificate: Option<serde::de::IgnoredAny>,
    #[serde(rename = "health_cursor")]
    _health_cursor: Option<serde::de::IgnoredAny>,
    #[serde(rename = "semantic_search")]
    _semantic_search: Option<serde::de::IgnoredAny>,
}
fn version(bytes: &[u8]) -> Result<u16, ClientError> {
    let reply: ProbeEnvelope =
        serde_json::from_slice(bytes).map_err(|e| ClientError::Protocol(e.to_string()))?;
    if reply.request_id != PROBE_ID {
        return Err(ClientError::RequestMismatch {
            expected: PROBE_ID,
            observed: reply.request_id,
        });
    }
    Ok(reply.version)
}
fn update_error(found: u16, detail: impl Into<String>) -> ClientError {
    ClientError::OwnerUpdate {
        found,
        supported: backend_library::DTO_VERSION,
        detail: detail.into(),
    }
}

fn peer_reports_outline_witness_route(
    transport: &mut UnixCommandTransport,
    deadline: Instant,
    context: Option<&RequestContext>,
) -> Result<bool, ClientError> {
    let remaining = probe_budget(deadline, context)?;
    let request = CommandDto::new(
        OUTLINE_CAPABILITY_PROBE_ID,
        Command::OutlineOwnerWitnessCapability,
    );
    let body = encode_request(&request)?;
    // Use the already authenticated health connection. A new connection here
    // could race an owner replacement and bind capability to a different
    // process than the one this Session will continue using.
    let bytes = match transport.exchange_command_with_budget(&request, &body, remaining) {
        Ok(bytes) => bytes,
        Err(error) if error.is_connection_lost() => return Ok(false),
        Err(error) => return Err(error),
    };
    probe_budget(deadline, context)?;
    let reply = match transport.decode(&bytes) {
        Ok(reply) => reply,
        // A refusal or older/unknown reply grammar only disables the optional
        // route. Normal strict full-proof PC4 continuation remains available.
        Err(_) => return Ok(false),
    };
    Ok(outline_witness_route_ack(&request, reply))
}

fn outline_witness_route_ack(request: &CommandDto, reply: backend_library::ReplyDto) -> bool {
    admit_reply(request, reply).is_ok_and(|admitted| {
        matches!(
            admitted.reply,
            backend_library::CommandReply::Revision(_)
        )
    })
}
fn probe_budget(
    deadline: Instant,
    context: Option<&RequestContext>,
) -> Result<Duration, ClientError> {
    if let Some(context) = context {
        context.check()?;
    }
    deadline
        .checked_duration_since(Instant::now())
        .filter(|left| !left.is_zero())
        .ok_or(ClientError::RemoteDeadlineExceeded)
}
impl Session {
    /// Opens a product session, handing a proven old Linux automatic owner to
    /// its existing graceful SIGTERM path before starting a matched companion.
    /// Passive callers and unproven/unsupported peers receive actionable errors.
    /// No user command has been submitted at any point in this convergence.
    /// # Errors
    /// Retains cancellation, the original finite startup budget, authentication,
    /// malformed replies, and genuine startup failures. No forced kill occurs.
    pub fn connect_matched_owner(
        paths: &WorkspacePaths,
        context: Option<&RequestContext>,
        passive: bool,
    ) -> Result<Self, ClientError> {
        let started = Instant::now();
        let initial = context
            .map(RequestContext::remaining_read_budget)
            .transpose()?
            .flatten()
            .unwrap_or(Duration::from_secs(5));
        let deadline = started + initial;
        #[cfg(target_os = "linux")]
        let mut intended_image: Option<
            backend_replication::DefaultLocaldImage,
        > = None;
        for attempt in 0..2 {
            if let Some(context) = context {
                context.check()?;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(ClientError::RemoteDeadlineExceeded);
            }
            let mut transport = UnixCommandTransport::connect_before(
                paths.endpoint(),
                remaining.min(super::LOCAL_CONNECT_TIMEOUT),
                remaining,
                context,
                Some(deadline),
            )?;
            // Authentication inspects only kernel/metadata state, not a wire
            // exchange. Check the absolute clock again even for embedded peers.
            let _remaining_after_dial = probe_budget(deadline, context)?;
            // Embedded transports and arbitrary explicitly hosted same-user
            // services are not process-retirement candidates.
            #[cfg(target_os = "linux")]
            let probe = transport
                .peer
                .as_ref()
                .is_some_and(|p| p.is_locald_process(&transport.stream));
            #[cfg(not(target_os = "linux"))]
            let probe = true;
            if !probe {
                #[cfg(target_os = "linux")]
                if intended_image.is_some() {
                    return Err(update_error(
                        backend_library::DTO_VERSION,
                        "the replacement owner image could not be proven; no command was submitted",
                    ));
                }
                return Self::finish_owner_connection(paths, transport, context, deadline, false);
            }
            let remaining = probe_budget(deadline, context)?;
            let request = CommandDto::new(PROBE_ID, Command::Health);
            let bytes = transport.exchange_command_with_budget(
                &request,
                &encode_request(&request)?,
                remaining,
            )?;
            probe_budget(deadline, context)?;
            let found = version(&bytes)?;
            let same_version = found == backend_library::DTO_VERSION;
            if same_version {
                admit_reply(&request, transport.decode(&bytes)?)?;
                let outline_witness_route =
                    peer_reports_outline_witness_route(&mut transport, deadline, context)?;
                #[cfg(target_os = "linux")]
                {
                    let peer = transport.peer.as_ref().ok_or_else(|| {
                        update_error(found, "the local owner image could not be authenticated; no command was submitted")
                    })?;
                    if intended_image.is_none() {
                        let candidate = backend_runtime::installed_default_locald_executable()
                            .map_err(|e| update_error(found, format!("the installed default owner image is unavailable: {e}; no command was submitted")))?;
                        let Some(candidate) = candidate else {
                            // BACKEND_LOCALD_BIN is an intentional helper pin, not
                            // authority to replace its owner with this install's default.
                            return Self::finish_owner_connection(
                                paths,
                                transport,
                                context,
                                deadline,
                                outline_witness_route,
                            );
                        };
                        intended_image = Some(peer.observe_default_locald_image(&candidate)
                            .map_err(|e| update_error(found, format!("the installed default owner image could not be proven: {e}; no command was submitted")))?);
                    }
                    let Some(image) = intended_image.as_ref() else {
                        return Err(update_error(
                            found,
                            "the intended owner image was not retained; no command was submitted",
                        ));
                    };
                    let matches = image.matches_peer(peer, &transport.stream)
                        .map_err(|e| update_error(found, format!("the running/default owner image comparison could not be proven: {e}; no command was submitted")))?;
                    probe_budget(deadline, context)?;
                    if matches {
                        return Self::finish_owner_connection(
                            paths,
                            transport,
                            context,
                            deadline,
                            outline_witness_route,
                        );
                    }
                }
                #[cfg(not(target_os = "linux"))]
                return Self::finish_owner_connection(
                    paths,
                    transport,
                    context,
                    deadline,
                    outline_witness_route,
                );
            }
            if passive
                || attempt != 0
                || (!same_version && (found, backend_library::DTO_VERSION) != (37, 38))
            {
                return Err(update_error(
                    found,
                    if same_version {
                        "the running owner differs from the installed default image; restart the previous owner, then retry; no command was submitted"
                    } else {
                        "restart the previous NuDox owner, then retry the original command; no command was submitted"
                    },
                ));
            }
            #[cfg(target_os = "linux")]
            {
                let peer = transport.peer.as_ref().ok_or_else(|| {
                    update_error(
                        found,
                        "the local owner could not be authenticated; restart it manually",
                    )
                })?;
                let retirement_deadline = deadline.min(Instant::now() + Duration::from_secs(15));
                let retired = match intended_image.as_ref() {
                    Some(image) => peer.retire_automatic_owner_for_image(
                        &transport.stream,
                        (paths.project(), paths.data(), paths.authority_secret()),
                        image,
                        retirement_deadline,
                        || context.is_none_or(|c| c.check().is_ok()) && image.is_current(),
                    ),
                    None => peer.retire_automatic_owner(
                        &transport.stream,
                        paths.project(),
                        paths.data(),
                        paths.authority_secret(),
                        retirement_deadline,
                        || context.is_none_or(|c| c.check().is_ok()),
                    ),
                };
                if let Some(context) = context {
                    context.check()?;
                }
                retired.map_err(|e| update_error(found, e.to_string()))?;
                if intended_image
                    .as_ref()
                    .is_some_and(|image| !image.is_current())
                {
                    return Err(update_error(
                        found,
                        "the intended default owner image changed during handoff; no command was submitted",
                    ));
                }
                drop(transport);
                let remaining = deadline.saturating_duration_since(Instant::now());
                match backend_runtime::start_locald_cancellable(
                    paths,
                    remaining,
                    |_| {},
                    || {
                        context.is_none_or(|c| c.check().is_ok())
                            && intended_image
                                .as_ref()
                                .is_none_or(|image| image.is_current())
                    },
                )
                .map_err(|e| ClientError::Startup(e.into()))?
                {
                    backend_runtime::LocaldStartup::Ready(_) => {}
                    backend_runtime::LocaldStartup::Pending(pending) => {
                        return Err(ClientError::Startup(
                            backend_runtime::RuntimeError::StartupPending(pending).into(),
                        ));
                    }
                }
            }
            #[cfg(not(target_os = "linux"))]
            return Err(update_error(
                found,
                "automatic owner retirement is unavailable on this platform; restart the previous NuDox owner and retry the original command",
            ));
        }
        Err(ClientError::RemoteDeadlineExceeded)
    }
    fn finish_owner_connection(
        paths: &WorkspacePaths,
        transport: UnixCommandTransport,
        context: Option<&RequestContext>,
        deadline: Instant,
        outline_owner_witness_route: bool,
    ) -> Result<Self, ClientError> {
        probe_budget(deadline, context)?;
        let mut session = Self::from_transport(paths.endpoint(), transport);
        session.outline_owner_witness_route = outline_owner_witness_route;
        session.set_request_context(context.cloned())?;
        probe_budget(deadline, context)?;
        Ok(session)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn owner_build_update_keeps_wire_upgrade_diagnostics_distinct() {
        let build = update_error(
            backend_library::DTO_VERSION,
            "installed default image changed",
        )
        .to_string();
        assert!(build.contains("owner build update"));
        assert!(!build.contains(&format!(
            "version {} → {}",
            backend_library::DTO_VERSION,
            backend_library::DTO_VERSION
        )));
        let wire = update_error(37, "old protocol").to_string();
        assert!(wire.contains("version 37 → 38"));
        assert!(!wire.contains("build update"));
    }

    #[test]
    fn version_probe_requires_exact_correlation_and_rejects_duplicate_fields() {
        let good = format!(
            r#"{{"version":35,"request_id":{PROBE_ID},"reply":{{"kind":"error","data":{{"message":"old owner"}}}}}}"#
        );
        assert_eq!(version(good.as_bytes()).expect("bounded old envelope"), 35);
        assert!(version(good.replace(&PROBE_ID.to_string(), "1").as_bytes()).is_err());
        assert!(
            version(
                good.replace("\"version\":35", "\"version\":35,\"version\":36")
                    .as_bytes()
            )
            .is_err()
        );
        assert!(version(b"{\"version\":35}").is_err());
    }
    #[test]
    fn dial_time_never_renews_the_absolute_compatibility_probe_budget() {
        let expired = Instant::now() - Duration::from_millis(1);
        assert!(matches!(
            probe_budget(expired, None),
            Err(ClientError::RemoteDeadlineExceeded),
        ));
        let original = Duration::from_secs(2);
        let remaining = probe_budget(Instant::now() + Duration::from_millis(50), None)
            .expect("remaining original budget");
        assert!(remaining <= Duration::from_millis(50) && remaining < original);
        assert!(UnixCommandTransport::connection_budget(original, Some(expired), None).is_err());
        let post_dial = UnixCommandTransport::connection_budget(
            original,
            Some(Instant::now() + Duration::from_millis(50)),
            None,
        )
        .expect("authentication inherits the same absolute cap");
        assert!(post_dial <= Duration::from_millis(50));
        assert_eq!(
            UnixCommandTransport::connection_budget(original, None, None)
                .expect("ordinary independent timeout is unchanged"),
            original,
        );
        let error = UnixCommandTransport::connect_before(
            "/tmp/nudox-expired-owner-connect-do-not-dial.sock",
            original,
            original,
            None,
            Some(expired),
        )
        .err()
        .expect("expired compatibility dial refused before connecting");
        assert!(matches!(error, ClientError::RemoteDeadlineExceeded));
        let cancelled = RequestContext::new();
        cancelled.cancel(super::super::RequestCancellationReason::Explicit);
        assert!(matches!(
            probe_budget(expired, Some(&cancelled)),
            Err(ClientError::RequestCancelled(
                super::super::RequestCancellationReason::Explicit
            )),
        ));
    }
    #[test]
    fn incompatible_owner_has_typed_unsent_command_guidance() {
        let error = update_error(
            37,
            "restart the previous NuDox owner; no command was submitted",
        );
        assert!(matches!(
            error,
            ClientError::OwnerUpdate {
                found: 37,
                supported: 38,
                ..
            }
        ));
        assert!(!error.is_connection_lost());
        assert!(error.to_string().contains("no command was submitted"));
    }

    #[test]
    fn actual_peer_reply_policy_requires_correlated_revision_ack() {
        let request = CommandDto::new(
            OUTLINE_CAPABILITY_PROBE_ID,
            Command::OutlineOwnerWitnessCapability,
        );
        let library = backend_library::Library::new();
        let root = library.view().root();
        let revision = backend_library::RevisionReceipt::new(
            root.root(),
            library.cursor(),
            root.basis().object,
        );
        let supported = backend_library::ReplyDto::new(
            OUTLINE_CAPABILITY_PROBE_ID,
            backend_library::CommandReply::Revision(revision),
        );
        assert!(outline_witness_route_ack(&request, supported));

        let old_peer =
            backend_library::ReplyDto::error(OUTLINE_CAPABILITY_PROBE_ID, "unknown command");
        assert!(!outline_witness_route_ack(&request, old_peer));
        let unknown_success = backend_library::ReplyDto::new(
            OUTLINE_CAPABILITY_PROBE_ID,
            backend_library::CommandReply::Failed(
                backend_library::CommandFailure::InvalidQuery("unknown capability".to_owned()),
            ),
        );
        assert!(!outline_witness_route_ack(&request, unknown_success));
        let wrong_peer = backend_library::ReplyDto::new(
            OUTLINE_CAPABILITY_PROBE_ID - 1,
            backend_library::CommandReply::Revision(revision),
        );
        assert!(!outline_witness_route_ack(&request, wrong_peer));
    }
}

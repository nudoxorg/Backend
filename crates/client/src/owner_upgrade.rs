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

fn reconnect_after_unsupported_outline_probe(
    transport: &mut UnixCommandTransport,
    deadline: Instant,
    context: Option<&RequestContext>,
) -> Result<(), ClientError> {
    probe_budget(deadline, context)?;
    transport.reconnect_before(deadline, context)?;
    probe_budget(deadline, context)?;

    // An older locald writes its ordinary unknown-command error and then
    // closes the connection. Bind the session to a newly authenticated
    // connection and a correlated health reply; do not retry the capability
    // probe or any product command on this fallback path.
    let request = CommandDto::new(PROBE_ID, Command::Health);
    let body = encode_request(&request)?;
    let remaining = probe_budget(deadline, context)?;
    let bytes = transport.exchange_command_with_budget(&request, &body, remaining)?;
    probe_budget(deadline, context)?;
    let found = version(&bytes)?;
    if found != backend_library::DTO_VERSION {
        return Err(update_error(
            found,
            "the owner changed during optional capability fallback; no product command was submitted",
        ));
    }
    admit_reply(&request, transport.decode(&bytes)?)?;
    probe_budget(deadline, context)?;
    Ok(())
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
                if !outline_witness_route {
                    reconnect_after_unsupported_outline_probe(&mut transport, deadline, context)?;
                }
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
                || (!same_version && (found, backend_library::DTO_VERSION) != (38, 39))
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
    use crate::{read_body, write_body};

    fn encode_test_reply(reply: &backend_library::ReplyDto) -> Vec<u8> {
        serde_json::to_vec(reply).expect("serialize test reply DTO")
    }

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
        let wire = update_error(38, "old protocol").to_string();
        assert!(wire.contains("version 38 → 39"));
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
            38,
            "restart the previous NuDox owner; no command was submitted",
        );
        assert!(matches!(
            error,
            ClientError::OwnerUpdate {
                found: 38,
                supported: 39,
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
        let root = library.view();
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

    #[cfg(unix)]
    #[test]
    fn unsupported_outline_probe_reopens_and_health_binds_old_peer_fallback() {
        use std::os::unix::fs::PermissionsExt as _;

        let path = std::path::PathBuf::from(format!(
            "/tmp/nudox-old-pc5-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let listener = std::os::unix::net::UnixListener::bind(&path).expect("old-peer fixture");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("owner-private endpoint");
        let server = std::thread::spawn(move || {
            let library = backend_library::Library::new();
            let root = library.view();
            let readiness = || {
                backend_library::ReplyDto::new(
                    PROBE_ID,
                    backend_library::CommandReply::Readiness(
                        backend_library::HealthReport::from_root(root, library.cursor()),
                    ),
                )
            };
            let (mut old_peer, _) = listener.accept().expect("accept initial connection");
            let health = backend_library::decode_command_body(
                &read_body(&mut old_peer).expect("old peer initial health"),
            )
            .expect("valid initial health");
            assert_eq!(health.command, Command::Health);
            write_body(&mut old_peer, &encode_test_reply(&readiness()))
            .expect("write initial health reply");

            let probe = backend_library::decode_command_body(
                &read_body(&mut old_peer).expect("unsupported capability request"),
            )
            .expect("well-formed old-peer command");
            assert_eq!(probe.command, Command::OutlineOwnerWitnessCapability);
            write_body(
                &mut old_peer,
                &encode_test_reply(&backend_library::ReplyDto::error(
                    probe.request_id,
                    "unknown command",
                )),
            )
            .expect("send old peer refusal");
            old_peer
                .shutdown(std::net::Shutdown::Both)
                .expect("old owner closes the unsupported-command stream");
            drop(old_peer);

            let (mut replacement, _) = listener.accept().expect("reopened connection");
            let health = backend_library::decode_command_body(
                &read_body(&mut replacement).expect("fallback health probe"),
            )
            .expect("fallback health request");
            assert_eq!(health.request_id, PROBE_ID);
            assert_eq!(health.command, Command::Health);
            write_body(&mut replacement, &encode_test_reply(&readiness()))
            .expect("write fallback health");

            let ordinary = backend_library::decode_command_body(
                &read_body(&mut replacement).expect("first ordinary command after fallback"),
            )
            .expect("ordinary request codec");
            assert_eq!(ordinary.command, Command::Revision);
            assert_ne!(ordinary.request_id, OUTLINE_CAPABILITY_PROBE_ID);
            let revision = backend_library::RevisionReceipt::new(
                root.root(),
                library.cursor(),
                root.basis().object,
            );
            let reply = backend_library::ReplyDto::new(
                ordinary.request_id,
                backend_library::CommandReply::Revision(revision),
            );
            write_body(&mut replacement, &encode_test_reply(&reply))
            .expect("write ordinary command reply");
        });

        let deadline = Instant::now() + Duration::from_secs(3);
        let mut transport = UnixCommandTransport::connect_before(
            &path,
            Duration::from_secs(1),
            Duration::from_secs(1),
            None,
            Some(deadline),
        )
        .expect("authenticate old peer");
        let health = CommandDto::new(PROBE_ID, Command::Health);
        let health_bytes = encode_request(&health).expect("initial health request");
        let response = transport
            .exchange_command_with_budget(
                &health,
                &health_bytes,
                probe_budget(deadline, None).expect("initial health budget"),
            )
            .expect("old peer health response");
        assert_eq!(
            version(&response).expect("health version"),
            backend_library::DTO_VERSION
        );
        admit_reply(&health, transport.decode(&response).expect("health decode"))
            .expect("initial health admission");

        assert!(
            !peer_reports_outline_witness_route(&mut transport, deadline, None)
                .expect("old peer refuses optional capability")
        );
        reconnect_after_unsupported_outline_probe(&mut transport, deadline, None)
            .expect("reconnect and bind the replacement using health");
        let ordinary = CommandDto::new(PROBE_ID - 2, Command::Revision);
        let body = encode_request(&ordinary).expect("ordinary command request");
        let bytes = transport
            .exchange_command_with_budget(
                &ordinary,
                &body,
                probe_budget(deadline, None).expect("ordinary command budget"),
            )
            .expect("ordinary command succeeds on the reopened connection");
        let reply = admit_reply(
            &ordinary,
            transport.decode(&bytes).expect("ordinary reply decode"),
        )
        .expect("ordinary reply admission");
        assert!(matches!(
            reply.reply,
            backend_library::CommandReply::Revision(_)
        ));
        server
            .join()
            .expect("old-peer fixture observed one probe and one command");
        let _ = std::fs::remove_file(path);
    }
}

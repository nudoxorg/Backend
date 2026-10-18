//! Physical command-stream controls for the publication shortcut. Replies
//! deliberately refuse Revision: they prove retry/correlation and retained
//! attachment, not producer certification or complete Root admission.

use super::*;
use backend_replication::{LocalControlLimits, read_frame, write_frame};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::Instant;

struct Endpoint(PathBuf);

impl Endpoint {
    fn new() -> anyhow::Result<Self> {
        let directory = std::env::temp_dir().join(format!(
            "ndrr-{}-{:x}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos(),
        ));
        std::fs::create_dir(&directory)?;
        let endpoint = Self(directory.join("s"));
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
        Ok(endpoint)
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        if let Some(parent) = self.0.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }
}

fn revision(stream: &mut UnixStream) -> anyhow::Result<backend_library::CommandDto> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let body = read_frame(stream, LocalControlLimits::default())
        .map_err(|error| anyhow::anyhow!("physical revision request: {error:?}"))?;
    let request = backend_library::decode_command_body(&body)
        .map_err(|error| anyhow::anyhow!("revision envelope: {error:?}"))?;
    assert!(matches!(
        request.command,
        backend_library::Command::Revision
    ));
    Ok(request)
}

fn refuse(
    stream: &mut UnixStream,
    request: backend_library::CommandDto,
    message: &str,
) -> anyhow::Result<()> {
    let reply = backend_library::ReplyDto::new(
        request.request_id,
        backend_library::CommandReply::Error(message.to_owned()),
    );
    write_frame(
        stream,
        &serde_json::to_vec(&reply)?,
        LocalControlLimits::default(),
    )
    .map_err(|error| anyhow::anyhow!("correlated owner refusal: {error:?}"))?;
    Ok(())
}

#[test]
fn publication_revision_reconnects_a_retired_command_stream_but_never_retries_protocol_refusal()
-> anyhow::Result<()> {
    for retire_cached in [false, true] {
        let endpoint = Endpoint::new()?;
        let listener = UnixListener::bind(&endpoint.0)?;
        std::fs::set_permissions(&endpoint.0, std::fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let (command, mut command_owner) = UnixStream::pair()?;
        let server = std::thread::spawn(move || -> anyhow::Result<UnixListener> {
            let request = revision(&mut command_owner)?;
            if !retire_cached {
                refuse(&mut command_owner, request, "cached protocol refusal")?;
                return Ok(listener);
            }
            // The owner retires only the cached socket, after reading the
            // request. Its independent endpoint remains available.
            drop(command_owner);
            for message in ["independent probe answered", "fresh retry answered"] {
                let until = Instant::now() + Duration::from_secs(2);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < until, "missing {message}");
                            std::thread::sleep(Duration::from_millis(2));
                        }
                        Err(error) => return Err(anyhow::anyhow!("accept {message}: {error}")),
                    }
                };
                stream.set_nonblocking(false)?;
                let request = revision(&mut stream)?;
                refuse(&mut stream, request, message)?;
            }
            Ok(listener)
        });

        let view = super::super::owner::publication_tests::view();
        let cursor = backend_library::Cursor::for_view_root_at(&view, 0);
        let basis = VersionedRoot::from_revision(1, cursor, 0);
        let gate =
            super::super::owner::OwnerGate::ready(basis, crate::model::ServiceMode::Attached);
        let attachment = gate
            .ready_epoch()
            .ok_or_else(|| anyhow::anyhow!("ready attachment missing"))?;
        assert_eq!(
            gate.publish_view(attachment, view.clone(), cursor),
            super::super::owner::PublicationAdmission::Admitted
        );
        let mut client = LocalEngineClient::gated(
            &endpoint.0,
            LocalProjectId::from_path(
                endpoint
                    .0
                    .parent()
                    .ok_or_else(|| anyhow::anyhow!("project directory missing"))?,
            )?,
            gate.clone(),
        );
        client.attached_epoch = Some(attachment);
        client.session = Some(Session::from_transport(
            endpoint.0.clone(),
            backend_client::UnixCommandTransport::from_stream(command),
        ));
        let result = client.execute(&EngineRequest::Root {
            project: None,
            request: crate::navigation::RequestId::new(1),
            basis,
            cancel: CancellationToken::new(),
        });
        let listener = server
            .join()
            .map_err(|_| anyhow::anyhow!("bounded physical command owner panicked"))??;
        let expected = if retire_cached {
            "fresh retry answered"
        } else {
            "cached protocol refusal"
        };
        assert!(
            matches!(result, Err(EngineFault::Failed(error))
            if error.code() == FaultCode::Protocol && error.message().contains(expected)),
            "the exact correlated terminal reply is retained"
        );
        assert_eq!(
            gate.ready_epoch(),
            Some(attachment),
            "socket retirement cannot withdraw the answering owner"
        );
        assert!(Arc::ptr_eq(
            &gate
                .publication(attachment)
                .ok_or_else(|| anyhow::anyhow!("publication was not retained"))?
                .0,
            &view
        ));
        assert!(
            client.subscription.is_none(),
            "a failed revision cannot bypass freshness with hydration"
        );
        assert!(
            client.active_cancel.is_none(),
            "the Root request scope ended"
        );
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
            "protocol refusal and the one retry cannot open another connection"
        );
    }
    Ok(())
}

#[test]
fn index_read_reconnects_once_with_original_context_and_preserves_correlated_owner_reply()
-> anyhow::Result<()> {
    let endpoint = Endpoint::new()?;
    let listener = UnixListener::bind(&endpoint.0)?;
    std::fs::set_permissions(&endpoint.0, std::fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    let (command, mut command_owner) = UnixStream::pair()?;
    let server = std::thread::spawn(move || -> anyhow::Result<UnixListener> {
        revision(&mut command_owner)?;
        drop(command_owner);
        let until = Instant::now() + Duration::from_secs(2);
        let mut fresh = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < until, "missing index read retry");
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(error) => return Err(anyhow::anyhow!("accept index retry: {error}")),
            }
        };
        fresh.set_nonblocking(false)?;
        let request = revision(&mut fresh)?;
        refuse(&mut fresh, request, "same owner answered index retry")?;
        Ok(listener)
    });
    let view = super::super::owner::publication_tests::view();
    let cursor = backend_library::Cursor::for_view_root_at(&view, 0);
    let gate = super::super::owner::OwnerGate::ready(
        VersionedRoot::from_revision(1, cursor, 0), crate::model::ServiceMode::Attached,
    );
    let attachment = gate.ready_epoch().expect("ready attachment");
    let mut client = LocalEngineClient::gated(
        &endpoint.0, LocalProjectId::from_path(endpoint.0.parent().expect("private parent"))?, gate.clone(),
    );
    client.attached_epoch = Some(attachment);
    client.active_cancel = Some(CancellationToken::new());
    client.session = Some(Session::from_transport(
        endpoint.0.clone(), backend_client::UnixCommandTransport::from_stream(command),
    ));
    let context = backend_client::RequestContext::new();
    let deadline = context.read_deadline(Duration::from_secs(1))?;
    let mut attempts = 0;
    let result = client.with_index_read(&context, |session| {
        attempts += 1;
        session.set_request_context(Some(context.clone()))?;
        let result = session.revision();
        session.set_request_context(None)?;
        result
    });
    let listener = server.join().map_err(|_| anyhow::anyhow!("physical index owner panicked"))??;
    assert_eq!(attempts, 2);
    assert!(matches!(result, Err(EngineFault::Failed(error))
        if error.code() == FaultCode::Protocol && error.message().contains("same owner answered index retry")));
    assert_eq!(context.read_deadline(Duration::from_secs(30))?, deadline);
    assert_eq!(gate.ready_epoch(), Some(attachment));
    assert!(matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "the independent liveness probe and a third read are forbidden");
    Ok(())
}

#[test]
fn index_connection_links_desktop_cancellation_before_dial_and_removes_only_its_wake()
-> anyhow::Result<()> {
    let endpoint = Endpoint::new()?;
    let listener = UnixListener::bind(&endpoint.0)?;
    std::fs::set_permissions(&endpoint.0, std::fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    let client = LocalEngineClient::new(
        &endpoint.0, LocalProjectId::from_path(endpoint.0.parent().expect("private parent"))?,
    );
    let context = backend_client::RequestContext::new();
    context.read_deadline(Duration::from_secs(2))?;
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(matches!(client.index_session(&context, &cancelled),
        Err(ClientError::RequestCancelled(backend_client::RequestCancellationReason::Explicit))));
    assert!(matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "a desktop-cancelled request cannot open the socket before its later I/O wake");

    let context = backend_client::RequestContext::new();
    context.read_deadline(Duration::from_secs(2))?;
    let active = CancellationToken::new();
    let session = client.index_session(&context, &active)?;
    let (_owner, _) = listener.accept()?;
    context.check()?;
    // The temporary dial registration must not leak into a later request.
    // The caller installs its own exact socket wake when doing command I/O.
    active.cancel();
    context.check()?;
    drop(session);
    Ok(())
}

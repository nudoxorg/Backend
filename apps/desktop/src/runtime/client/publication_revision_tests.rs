//! Physical command-stream controls for the publication shortcut. Replies
//! deliberately refuse Revision: they prove retry/correlation and retained
//! attachment, not producer certification or complete Root admission.
#![allow(clippy::expect_used, clippy::panic)]

use super::*;
use backend_replication::{LocalControlLimits, read_frame, write_frame};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::Instant;

struct Endpoint(PathBuf);

impl Endpoint {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "ndrr-{}-{:x}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos(),
        ));
        std::fs::create_dir(&directory).expect("fresh private endpoint directory");
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
            .expect("private directory");
        Self(directory.join("s"))
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let _ = std::fs::remove_dir(self.0.parent().expect("private parent"));
    }
}

fn revision(stream: &mut UnixStream) -> backend_library::CommandDto {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("bounded read");
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .expect("bounded write");
    let body =
        read_frame(stream, LocalControlLimits::default()).expect("physical revision request");
    let request = backend_library::decode_command_body(&body).expect("revision envelope");
    assert!(matches!(
        request.command,
        backend_library::Command::Revision
    ));
    request
}

fn refuse(stream: &mut UnixStream, request: backend_library::CommandDto, message: &str) {
    let reply = backend_library::ReplyDto::new(
        request.request_id,
        backend_library::CommandReply::Error(message.to_owned()),
    );
    write_frame(
        stream,
        &serde_json::to_vec(&reply).expect("reply body"),
        LocalControlLimits::default(),
    )
    .expect("correlated owner refusal");
}

#[test]
fn publication_revision_reconnects_a_retired_command_stream_but_never_retries_protocol_refusal() {
    for retire_cached in [false, true] {
        let endpoint = Endpoint::new();
        let listener = UnixListener::bind(&endpoint.0).expect("replacement listener");
        std::fs::set_permissions(&endpoint.0, std::fs::Permissions::from_mode(0o600))
            .expect("private endpoint");
        listener.set_nonblocking(true).expect("bounded accept");
        let (command, mut command_owner) = UnixStream::pair().expect("cached command stream");
        let server = std::thread::spawn(move || {
            let request = revision(&mut command_owner);
            if !retire_cached {
                refuse(&mut command_owner, request, "cached protocol refusal");
                return listener;
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
                        Err(error) => panic!("accept {message}: {error}"),
                    }
                };
                stream
                    .set_nonblocking(false)
                    .expect("blocking accepted stream");
                let request = revision(&mut stream);
                refuse(&mut stream, request, message);
            }
            listener
        });

        let view = super::super::owner::publication_tests::view();
        let cursor = backend_library::Cursor::for_view_root_at(&view, 0);
        let basis = VersionedRoot::from_revision(1, cursor, 0);
        let gate =
            super::super::owner::OwnerGate::ready(basis, crate::model::ServiceMode::Attached);
        let attachment = gate.ready_epoch().expect("ready attachment");
        assert_eq!(
            gate.publish_view(attachment, view.clone(), cursor),
            super::super::owner::PublicationAdmission::Admitted
        );
        let mut client = LocalEngineClient::gated(
            &endpoint.0,
            LocalProjectId::from_path(endpoint.0.parent().expect("project directory"))
                .expect("project"),
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
        let listener = server.join().expect("bounded physical command owner");
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
                .expect("retained publication")
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
}

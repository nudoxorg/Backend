//! A read lease changes only after one exact, authenticated owner admission.
use super::{ClientError, Command, CommandDto, UnixCommandTransport, read_body, write_body};
use backend_replication::{
    DEFERRED_COMMAND_ACK_MAGIC, MAX_DEFERRED_COMMAND_WAIT, wrap_deferred_command,
};
use std::io::{self, Read, Write};
use std::time::Instant;

impl UnixCommandTransport {
    pub(super) fn exchange_command(
        &mut self,
        request: &CommandDto,
        original: &[u8],
    ) -> Result<Vec<u8>, ClientError> {
        self.exchange_command_bounded(request, original, None, None)
    }

    pub(super) fn exchange_command_with_budget(
        &mut self,
        request: &CommandDto,
        original: &[u8],
        remaining: std::time::Duration,
    ) -> Result<Vec<u8>, ClientError> {
        self.exchange_command_bounded(request, original, Some(remaining), None)
    }

    pub(super) fn exchange_command_with_search_admission(
        &mut self,
        request: &CommandDto,
        original: &[u8],
        remaining: std::time::Duration,
        scope: &super::RequestContext,
    ) -> Result<Vec<u8>, ClientError> {
        self.exchange_command_bounded(request, original, Some(remaining), Some(scope))
    }

    fn exchange_command_bounded(
        &mut self,
        request: &CommandDto,
        original: &[u8],
        ceiling: Option<std::time::Duration>,
        search_scope: Option<&super::RequestContext>,
    ) -> Result<Vec<u8>, ClientError> {
        // Counted before the write: a frame that fails to send may still have
        // been consumed by an owner that is retiring this connection.
        self.frames_sent = self.frames_sent.saturating_add(1);
        let started = Instant::now();
        let initial = self
            .stream
            .read_timeout()
            .map_err(|error| ClientError::Io(error.to_string()))?
            .unwrap_or(self.io_timeout);
        let initial = ceiling.map_or(initial, |remaining| initial.min(remaining));
        // Published-name continuations do only bounded current-arrangement
        // reads; they never wait for corpus preparation or owe a search ticket.
        let opted_in = self.peer.is_some() && matches!(&request.command, Command::Search(_));
        let wrapped = if opted_in {
            Some(
                wrap_deferred_command(original, backend_library::DTO_VERSION)
                    .map_err(|error| ClientError::Protocol(error.to_string()))?,
            )
        } else {
            None
        };
        let original = wrapped.as_deref().unwrap_or(original);
        if let Some(remaining) = ceiling {
            write_body(
                &mut DeadlineWrite {
                    stream: &mut self.stream,
                    deadline: started + remaining,
                },
                original,
            )?;
        } else {
            write_body(&mut self.stream, original)?;
        }
        let mut body = read_before(&mut self.stream, started + initial)
            .map_err(|error| bind_reply_basis(error, request))?;
        if body.starts_with(&DEFERRED_COMMAND_ACK_MAGIC) {
            if !opted_in {
                return Err(ClientError::Protocol(
                    "unsolicited deferred command ACK".to_owned(),
                ));
            }
            let peer = self.peer.as_ref().ok_or_else(|| {
                ClientError::Protocol("deferred ACK requires authenticated owner".to_owned())
            })?;
            let budget = peer
                .admit_deferred_command_ack(
                    &body,
                    wrapped.as_deref().unwrap_or(original),
                    backend_library::DTO_VERSION,
                )
                .map_err(|error| ClientError::Protocol(error.to_string()))?;
            // An ACK never renews its budget: both phases share the original
            // request start, and even byte-by-byte reads consume that deadline.
            let admitted = budget.min(MAX_DEFERRED_COMMAND_WAIT);
            let absolute = started + ceiling.map_or(admitted, |remaining| admitted.min(remaining));
            let deadline = match search_scope {
                Some(scope) => scope
                    .admit_deferred_search(started, admitted)?
                    .unwrap_or(absolute),
                None => absolute,
            };
            body = read_before(&mut self.stream, deadline)
                .map_err(|error| bind_reply_basis(error, request))?;
            if body.starts_with(&DEFERRED_COMMAND_ACK_MAGIC) {
                return Err(ClientError::Protocol(
                    "repeated deferred command ACK".to_owned(),
                ));
            }
        }
        Ok(body)
    }
}

fn read_before(
    stream: &mut backend_replication::LocalStream,
    deadline: Instant,
) -> Result<Vec<u8>, ClientError> {
    let result = read_body(&mut DeadlineRead { stream, deadline });
    if matches!(result, Err(ClientError::ResponseFrameTooLarge { .. })) {
        // The body remains unread. Never interpret it as a later frame.
        let _ = stream.shutdown(std::net::Shutdown::Both);
    }
    result
}

fn bind_reply_basis(error: ClientError, request: &CommandDto) -> ClientError {
    match error {
        ClientError::ResponseFrameTooLarge { .. } => ClientError::ResponseFrameTooLarge {
            basis: super::query_continuation::basis(&request.command),
        },
        other => other,
    }
}

struct DeadlineRead<'a> {
    stream: &'a mut backend_replication::LocalStream,
    deadline: Instant,
}

struct DeadlineWrite<'a> {
    stream: &'a mut backend_replication::LocalStream,
    deadline: Instant,
}
impl Write for DeadlineWrite<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        self.stream.set_write_timeout(Some(remaining))?;
        self.stream.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}
impl Read for DeadlineRead<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        self.stream.set_read_timeout(Some(remaining))?;
        self.stream.read(bytes)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::{
        CommandTransport, Query, QueryLimit, WireCertificate, WireClaim, WireSchema, encode_id,
    };
    use backend_replication::{
        DeferredCommandAck, current_local_owner_binding, unwrap_deferred_command,
    };
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    #[test]
    fn oversized_reply_header_shuts_down_the_exact_socket_before_another_read() {
        let (mut stream, mut peer) = backend_replication::LocalStream::pair().expect("pair");
        let length = u32::try_from(crate::MAX_FRAME + 1).expect("frame length");
        peer.write_all(&length.to_be_bytes())
            .expect("oversized header");
        peer.write_all(b"unconsumed body bytes")
            .expect("body prefix");
        assert!(matches!(
            read_before(&mut stream, Instant::now() + Duration::from_secs(1)),
            Err(ClientError::ResponseFrameTooLarge { basis: None })
        ));
        let mut byte = [0];
        assert!(
            matches!(stream.read(&mut byte), Ok(0) | Err(_)),
            "body bytes must never become a subsequent frame"
        );
    }

    #[test]
    fn oversized_search_reply_preserves_the_request_basis() {
        let (stream, mut peer) = backend_replication::LocalStream::pair().expect("pair");
        let length = u32::try_from(crate::MAX_FRAME + 1).expect("frame length");
        peer.write_all(&length.to_be_bytes())
            .expect("oversized header");
        peer.write_all(b"unconsumed body bytes")
            .expect("body prefix");
        peer.set_read_timeout(Some(Duration::from_secs(1)))
            .expect("bounded closed-socket check");
        let server = std::thread::spawn(move || {
            let _request = read_body(&mut peer).expect("search request");
            let mut byte = [0];
            assert_eq!(peer.read(&mut byte).expect("shutdown notification"), 0);
        });
        let mut client = UnixCommandTransport::from_stream(stream);
        let command = request();
        let expected_basis = super::super::query_continuation::basis(&command.command);
        let actual_basis = match client.request(command) {
            Err(ClientError::ResponseFrameTooLarge { basis }) => basis,
            other => panic!("oversized reply returned: {other:?}"),
        };
        assert!(expected_basis.is_some(), "fixture is a Search request");
        assert_eq!(actual_basis, expected_basis);
        // Shutdown can leave bytes already queued in the kernel, but the
        // transport cannot start another exchange on this stream.
        assert!(client.request(request()).is_err());
        server.join().expect("closed peer joined");
    }

    /// A private short-path directory for named-socket tests. `temp_dir()` can
    /// point into a deeply nested runner directory, longer than `sun_path`.
    /// Prefer the conventional short roots and reject any path that leaves a
    /// wide margin below the smallest common Unix socket-path limit.
    struct PrivateSocketDir(std::path::PathBuf);

    impl PrivateSocketDir {
        fn new() -> Self {
            use std::os::unix::fs::DirBuilderExt as _;
            use std::sync::atomic::{AtomicU64, Ordering};

            static NEXT: AtomicU64 = AtomicU64::new(0);
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos();
            let pid = std::process::id();
            let roots = [
                std::path::PathBuf::from("/tmp"),
                std::path::PathBuf::from("/var/tmp"),
                std::env::temp_dir(),
            ];
            let mut unavailable = None;
            for root in roots {
                // `/owner.sock` plus a NUL terminator must fit under 80 bytes.
                if root.as_os_str().as_bytes().len() + b"/owner.sock\0".len() >= 80 {
                    continue;
                }
                for _ in 0..64 {
                    let path = root.join(format!(
                        "mcp-{}-{nonce:x}-{:x}",
                        pid,
                        NEXT.fetch_add(1, Ordering::Relaxed)
                    ));
                    if path.join("owner.sock").as_os_str().as_bytes().len() >= 80 {
                        continue;
                    }
                    let mut builder = std::fs::DirBuilder::new();
                    builder.mode(0o700);
                    match builder.create(&path) {
                        Ok(()) => return Self(path),
                        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                        Err(error)
                            if matches!(
                                error.kind(),
                                io::ErrorKind::NotFound
                                    | io::ErrorKind::PermissionDenied
                                    | io::ErrorKind::ReadOnlyFilesystem
                            ) =>
                        {
                            unavailable = Some(error);
                            break;
                        }
                        Err(error) => panic!("create private socket directory: {error}"),
                    }
                }
            }
            panic!("no short writable Unix socket directory is available: {unavailable:?}")
        }

        fn socket_path(&self) -> std::path::PathBuf {
            self.0.join("owner.sock")
        }
    }

    impl Drop for PrivateSocketDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(self.socket_path());
            let _ = std::fs::remove_dir(&self.0);
        }
    }

    #[test]
    fn private_socket_fixture_uses_a_short_owner_only_directory() {
        use std::os::unix::fs::MetadataExt as _;

        let directory = PrivateSocketDir::new();
        assert!(directory.socket_path().as_os_str().as_bytes().len() < 80);
        let mode = std::fs::metadata(&directory.0)
            .expect("private fixture directory")
            .mode();
        assert_eq!(mode & 0o077, 0, "fixture directory is private to its owner");
    }

    #[test]
    fn budgeted_partial_frame_writes_share_one_absolute_deadline() {
        let (mut client, mut peer) =
            backend_replication::LocalStream::pair().expect("real socket pair");
        let stopped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observer = std::sync::Arc::clone(&stopped);
        peer.set_read_timeout(Some(Duration::from_millis(100)))
            .expect("bounded peer");
        let drain = std::thread::spawn(move || {
            let mut buffer = [0_u8; 256];
            while !observer.load(std::sync::atomic::Ordering::Acquire) {
                if peer.read(&mut buffer).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        let started = Instant::now();
        let result = write_body(
            &mut DeadlineWrite {
                stream: &mut client,
                deadline: started + Duration::from_millis(40),
            },
            &vec![1; backend_replication::LOCAL_CONTROL_MAX_FRAME],
        );
        let elapsed = started.elapsed();
        stopped.store(true, std::sync::atomic::Ordering::Release);
        client
            .shutdown(std::net::Shutdown::Both)
            .expect("close observer");
        drain.join().expect("peer joined");
        assert!(matches!(
            result,
            Err(ClientError::Disconnected(
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
            ))
        ));
        assert!(
            elapsed < Duration::from_millis(500),
            "partial writes renewed the deadline: {elapsed:?}"
        );
    }

    fn request() -> CommandDto {
        let root = backend_library::view_state_root(&[]);
        CommandDto::new(
            7,
            Command::Search(Query::new(
                "real cold query",
                root,
                QueryLimit::new(1).expect("limit"),
            )),
        )
        .with_certificate(
            WireCertificate::new().with_claim(WireClaim::RootCommitment {
                schema: WireSchema::ViewRelation,
                id: encode_id(root.as_bytes()),
            }),
        )
    }

    fn exchange(
        case: &str,
        wait: Duration,
    ) -> (Result<backend_library::ReplyDto, ClientError>, Duration) {
        exchange_with_scope(case, wait, None)
    }

    fn exchange_with_scope(
        case: &str,
        wait: Duration,
        scope: Option<&super::super::RequestContext>,
    ) -> (Result<backend_library::ReplyDto, ClientError>, Duration) {
        let socket_dir = PrivateSocketDir::new();
        let path = socket_dir.socket_path();
        assert!(
            path.as_os_str().as_bytes().len() < 80,
            "socket fixture path must fit sun_path"
        );
        let listener = backend_replication::LocalListener::bind(&path).expect("private listener");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("private endpoint");
        let principal = current_local_owner_binding(&path).expect("owner instance");
        let mut client =
            UnixCommandTransport::connect_with_timeouts(&path, Duration::from_secs(1), wait)
                .expect("affine authentication");
        let case = case.to_owned();
        let server_path = path.clone();
        let (release_peer, retain_peer) = std::sync::mpsc::channel();
        let budgeted = case == "budgeted";
        let server = std::thread::spawn(move || {
            let (mut peer, _) = listener.accept().expect("accept");
            let frame = read_body(&mut peer).expect("request frame");
            let original = unwrap_deferred_command(&frame, backend_library::DTO_VERSION)
                .expect("request grammar")
                .expect("explicit opt in");
            let mut ack =
                DeferredCommandAck::admitted(&frame, principal, Duration::from_millis(500))
                    .expect("finite admission")
                    .encode(backend_library::DTO_VERSION);
            let mut replacement = None;
            match case.as_str() {
                "silent" => {
                    std::thread::sleep(Duration::from_millis(250));
                    return;
                }
                "wrong-request" => ack[40] ^= 1,
                "replayed-nonce" => {
                    let prior = wrap_deferred_command(original, backend_library::DTO_VERSION)
                        .expect("previous identical DTO request");
                    assert_ne!(prior, frame);
                    ack =
                        DeferredCommandAck::admitted(&prior, principal, Duration::from_millis(500))
                            .expect("prior ack")
                            .encode(backend_library::DTO_VERSION);
                }
                "foreign-owner" => ack[8] ^= 1,
                "wrong-version" => ack[4] += 1,
                "wrong-dto" => ack[7] += 1,
                "wrong-frame" => ack[0] = b'X',
                "unbounded" => ack[72..80].copy_from_slice(&900_001u64.to_be_bytes()),
                "zero" => ack[72..80].fill(0),
                "trailing" => ack.push(0),
                "retired-owner" => {
                    std::fs::remove_file(&server_path).expect("retire endpoint");
                    replacement = Some(
                        backend_replication::LocalListener::bind(&server_path)
                            .expect("new owner endpoint"),
                    );
                    std::fs::set_permissions(&server_path, std::fs::Permissions::from_mode(0o600))
                        .expect("new private endpoint");
                }
                _ => {}
            }
            let _ = write_body(&mut peer, &ack);
            if case == "repeated" {
                let _ = write_body(&mut peer, &ack);
            }
            std::thread::sleep(Duration::from_millis(180));
            let terminal = if case == "bad-final" {
                b"{invalid final certificate}".to_vec()
            } else {
                backend_engine::encode_reply_dto(&backend_library::ReplyDto::error(
                    if case == "wrong-final-id" { 8 } else { 7 },
                    "owner terminal",
                ))
                .expect("terminal")
            };
            let _ = write_body(&mut peer, &terminal);
            // Keep the owner and stream alive until the client consumes the
            // terminal. On macOS, setting a read deadline after peer closure
            // can report InvalidInput before buffered reply admission.
            let _ = retain_peer.recv();
            drop(replacement);
            drop(peer);
            drop(listener);
        });
        let started = Instant::now();
        let result = if let Some(scope) = scope {
            client.request_with_search_admission(request(), wait, scope)
        } else if budgeted {
            client.request_with_budget(request(), wait)
        } else {
            client.request(request())
        };
        let elapsed = started.elapsed();
        let _ = release_peer.send(());
        drop(client);
        server.join().expect("owned server kernel wait");
        drop(socket_dir);
        (result, elapsed)
    }

    #[test]
    fn deferred_command_ack_allows_one_finite_authenticated_wait_and_keeps_final_admission() {
        let (result, elapsed) = exchange("valid", Duration::from_millis(60));
        let reply = result.expect("terminal beyond ordinary read lease");
        assert_eq!(reply.request_id, 7);
        assert!(elapsed >= Duration::from_millis(150));
        let (result, _) = exchange("wrong-final-id", Duration::from_millis(60));
        assert!(
            matches!(
                result,
                Err(ClientError::RequestMismatch {
                    expected: 7,
                    observed: 8
                })
            ),
            "wrong-final-id returned: {result:?}"
        );
        let (result, _) = exchange("bad-final", Duration::from_millis(60));
        assert!(matches!(result, Err(ClientError::Protocol(_))));
    }

    #[test]
    fn default_search_scope_adopts_only_exact_ack_and_explicit_budget_remains_absolute() {
        let scope = crate::RequestContext::new();
        scope
            .begin_default_search(Duration::from_millis(60))
            .expect("default initial admission");
        let (result, elapsed) =
            exchange_with_scope("valid", Duration::from_millis(60), Some(&scope));
        assert_eq!(
            result
                .expect("one ordinary search terminal past initial budget")
                .request_id,
            7
        );
        assert!(elapsed >= Duration::from_millis(150));
        scope
            .check()
            .expect("admitted finite owner lease remains active");

        let explicit = crate::RequestContext::new();
        let absolute = explicit
            .read_deadline(Duration::from_millis(60))
            .expect("explicit absolute deadline");
        explicit
            .begin_default_search(Duration::from_secs(30))
            .expect("cannot replace explicit budget");
        let (result, elapsed) =
            exchange_with_scope("valid", Duration::from_millis(60), Some(&explicit));
        assert!(matches!(result, Err(ClientError::Disconnected(_))));
        assert!(elapsed < Duration::from_millis(150));
        assert!(Instant::now() >= absolute);

        for case in [
            "wrong-request",
            "replayed-nonce",
            "foreign-owner",
            "unbounded",
            "retired-owner",
            "repeated",
        ] {
            let scope = crate::RequestContext::new();
            scope
                .begin_default_search(Duration::from_millis(60))
                .expect("initial lease");
            let (result, elapsed) =
                exchange_with_scope(case, Duration::from_millis(60), Some(&scope));
            assert!(
                matches!(result, Err(ClientError::Protocol(_))),
                "{case}: {result:?}"
            );
            assert!(elapsed < Duration::from_millis(150));
        }
    }

    #[test]
    fn preparation_read_remaining_budget_is_not_renewed_by_an_authenticated_ack() {
        let (result, elapsed) = exchange("budgeted", Duration::from_millis(60));
        assert!(matches!(result, Err(ClientError::Disconnected(_))));
        assert!(
            elapsed < Duration::from_millis(150),
            "ACK renewed the preparation read budget: {elapsed:?}"
        );
    }

    #[test]
    fn deferred_command_ack_invalid_silent_and_retired_owners_never_extend_read_lease() {
        for case in [
            "wrong-request",
            "replayed-nonce",
            "foreign-owner",
            "wrong-version",
            "wrong-dto",
            "wrong-frame",
            "unbounded",
            "zero",
            "trailing",
            "retired-owner",
            "repeated",
        ] {
            let (result, elapsed) = exchange(case, Duration::from_millis(60));
            assert!(
                matches!(result, Err(ClientError::Protocol(_))),
                "{case}: {result:?}"
            );
            assert!(
                elapsed < Duration::from_millis(150),
                "{case} extended lease: {elapsed:?}"
            );
        }
        let (result, elapsed) = exchange("silent", Duration::from_millis(60));
        assert!(matches!(result, Err(ClientError::Disconnected(_))));
        assert!(
            elapsed < Duration::from_millis(150),
            "silent endpoint changed lease: {elapsed:?}"
        );
        assert_eq!(crate::CLIENT_REQUEST_TIMEOUT, Duration::from_secs(30));
    }

    #[test]
    fn deferred_command_ack_unverified_stream_and_trickled_frame_cannot_renew() {
        let (raw, mut peer) = backend_replication::LocalStream::pair().expect("pair");
        let (release_peer, retain_peer) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let original = read_body(&mut peer).expect("legacy raw request");
            assert!(
                unwrap_deferred_command(&original, backend_library::DTO_VERSION)
                    .expect("grammar")
                    .is_none()
            );
            let wrapped = wrap_deferred_command(&original, backend_library::DTO_VERSION)
                .expect("well-formed untrusted opt-in claim");
            let ack = DeferredCommandAck::admitted(&wrapped, [7; 32], Duration::from_secs(1))
                .expect("claim")
                .encode(backend_library::DTO_VERSION);
            write_body(&mut peer, &ack).expect("untrusted ACK");
            retain_peer.recv().expect("client consumed untrusted ACK");
        });
        raw.set_read_timeout(Some(Duration::from_millis(60)))
            .expect("initial lease");
        let mut transport = UnixCommandTransport::from_stream(raw);
        let unverified = transport.request(request());
        release_peer.send(()).expect("release untrusted peer");
        worker.join().expect("retire peer");
        assert!(
            matches!(unverified, Err(ClientError::Protocol(_))),
            "unverified stream returned: {unverified:?}"
        );

        let (mut raw, mut peer) = backend_replication::LocalStream::pair().expect("trickle pair");
        let worker = std::thread::spawn(move || {
            use std::io::Write;
            let frame = backend_replication::frame(
                b"longer than deadline",
                backend_replication::LocalControlLimits::default(),
            )
            .expect("frame");
            for byte in frame {
                if peer.write_all(&[byte]).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(15));
            }
        });
        let started = Instant::now();
        assert!(read_before(&mut raw, started + Duration::from_millis(60)).is_err());
        assert!(started.elapsed() < Duration::from_millis(150));
        drop(raw);
        worker.join().expect("trickle peer retired");
    }
}

//! A read lease changes only after one exact, authenticated owner admission.
use super::{ClientError, Command, CommandDto, UnixCommandTransport, read_body, write_body};
use backend_replication::{
    DEFERRED_COMMAND_ACK_MAGIC, MAX_DEFERRED_COMMAND_WAIT, wrap_deferred_command,
};
use std::io::{self, Read};
use std::time::Instant;

impl UnixCommandTransport {
    pub(super) fn exchange_command(
        &mut self,
        request: &CommandDto,
        original: &[u8],
    ) -> Result<Vec<u8>, ClientError> {
        let started = Instant::now();
        let initial = self
            .stream
            .read_timeout()
            .map_err(|error| ClientError::Io(error.to_string()))?
            .unwrap_or(self.io_timeout);
        let opted_in = self.peer.is_some() && matches!(&request.command, Command::Search(_));
        let wrapped = if opted_in {
            Some(
                wrap_deferred_command(original, backend_library::DTO_VERSION)
                    .map_err(|error| ClientError::Protocol(error.to_string()))?,
            )
        } else {
            None
        };
        write_body(&mut self.stream, wrapped.as_deref().unwrap_or(original))?;
        let mut body = read_before(&mut self.stream, started + initial)?;
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
                .admit_deferred_command_ack(&body, original, backend_library::DTO_VERSION)
                .map_err(|error| ClientError::Protocol(error.to_string()))?;
            // An ACK never renews its budget: both phases share the original
            // request start, and even byte-by-byte reads consume that deadline.
            let deadline = started + budget.min(MAX_DEFERRED_COMMAND_WAIT);
            body = read_before(&mut self.stream, deadline)?;
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
    read_body(&mut DeadlineRead { stream, deadline })
}

struct DeadlineRead<'a> {
    stream: &'a mut backend_replication::LocalStream,
    deadline: Instant,
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
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

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
        let path = std::env::temp_dir().join(format!(
            "ack-{}-{}-{}.sock",
            std::process::id(),
            case,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let listener = backend_replication::LocalListener::bind(&path).expect("private listener");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("private endpoint");
        let principal = current_local_owner_binding(&path).expect("owner instance");
        let mut client =
            UnixCommandTransport::connect_with_timeouts(&path, Duration::from_secs(1), wait)
                .expect("affine authentication");
        let case = case.to_owned();
        let server_path = path.clone();
        let server = std::thread::spawn(move || {
            let (mut peer, _) = listener.accept().expect("accept");
            let frame = read_body(&mut peer).expect("request frame");
            let original = unwrap_deferred_command(&frame, backend_library::DTO_VERSION)
                .expect("request grammar")
                .expect("explicit opt in");
            let mut ack =
                DeferredCommandAck::admitted(original, principal, Duration::from_millis(500))
                    .expect("finite admission")
                    .encode(backend_library::DTO_VERSION);
            let mut replacement = None;
            match case.as_str() {
                "silent" => {
                    std::thread::sleep(Duration::from_millis(250));
                    return;
                }
                "wrong-request" => ack[40] ^= 1,
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
            drop(replacement);
        });
        let started = Instant::now();
        let result = client.request(request());
        let elapsed = started.elapsed();
        drop(client);
        server.join().expect("owned server kernel wait");
        let _ = std::fs::remove_file(path);
        (result, elapsed)
    }

    #[test]
    fn deferred_command_ack_allows_one_finite_authenticated_wait_and_keeps_final_admission() {
        let (result, elapsed) = exchange("valid", Duration::from_millis(60));
        let reply = result.expect("terminal beyond ordinary read lease");
        assert_eq!(reply.request_id, 7);
        assert!(elapsed >= Duration::from_millis(150));
        let (result, _) = exchange("wrong-final-id", Duration::from_millis(60));
        assert!(matches!(
            result,
            Err(ClientError::RequestMismatch {
                expected: 7,
                observed: 8
            })
        ));
        let (result, _) = exchange("bad-final", Duration::from_millis(60));
        assert!(matches!(result, Err(ClientError::Protocol(_))));
    }

    #[test]
    fn deferred_command_ack_invalid_silent_and_retired_owners_never_extend_read_lease() {
        for case in [
            "wrong-request",
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
        let (mut raw, mut peer) = backend_replication::LocalStream::pair().expect("pair");
        let worker = std::thread::spawn(move || {
            let original = read_body(&mut peer).expect("legacy raw request");
            assert!(
                unwrap_deferred_command(&original, backend_library::DTO_VERSION)
                    .expect("grammar")
                    .is_none()
            );
            let ack = DeferredCommandAck::admitted(&original, [7; 32], Duration::from_secs(1))
                .expect("claim")
                .encode(backend_library::DTO_VERSION);
            write_body(&mut peer, &ack).expect("untrusted ACK");
        });
        raw.set_read_timeout(Some(Duration::from_millis(60)))
            .expect("initial lease");
        let mut transport = UnixCommandTransport::from_stream(raw);
        assert!(matches!(
            transport.request(request()),
            Err(ClientError::Protocol(_))
        ));
        worker.join().expect("retire peer");

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

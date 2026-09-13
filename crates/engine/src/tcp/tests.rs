use super::*;
use std::io::{self, Cursor, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug)]
struct ScriptedStream {
    readable: Cursor<Vec<u8>>,
    written: Vec<u8>,
}

impl ScriptedStream {
    fn from_readable(bytes: Vec<u8>) -> Self {
        Self {
            readable: Cursor::new(bytes),
            written: Vec::new(),
        }
    }
}

impl Read for ScriptedStream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.readable.read(bytes)
    }
}

impl Write for ScriptedStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.written.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
struct RecordingStream<S> {
    inner: S,
    writes: Vec<u8>,
}

impl<S> RecordingStream<S> {
    fn new(inner: S) -> Self {
        Self {
            inner,
            writes: Vec::new(),
        }
    }
}

impl<S: Read> Read for RecordingStream<S> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.inner.read(bytes)
    }
}

impl<S: Write> Write for RecordingStream<S> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(bytes)?;
        self.writes.extend_from_slice(&bytes[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn challenge(server_nonce: [u8; NONCE_BYTES]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(MAGIC.len() + 1 + NONCE_BYTES);
    bytes.extend_from_slice(&MAGIC);
    bytes.push(VERSION);
    bytes.extend_from_slice(&server_nonce);
    bytes
}

fn client_envelope(
    secret: &[u8; 32],
    server_nonce: [u8; NONCE_BYTES],
    client_nonce: [u8; NONCE_BYTES],
) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(MAGIC.len() + 1 + (NONCE_BYTES * 2) + TOKEN_BYTES);
    bytes.extend_from_slice(&MAGIC);
    bytes.push(VERSION);
    bytes.extend_from_slice(&server_nonce);
    bytes.extend_from_slice(&client_nonce);
    bytes.extend_from_slice(&token(secret, &server_nonce, &client_nonce, b"client"));
    bytes
}

fn configure_loopback(stream: &TcpStream) {
    stream
        .set_read_timeout(Some(HANDSHAKE_TIMEOUT))
        .expect("read timeout");
    stream
        .set_write_timeout(Some(HANDSHAKE_TIMEOUT))
        .expect("write timeout");
}

#[test]
fn mutual_handshake_rejects_wrong_secret() {
    let secret = [7_u8; 32];
    let wrong = [8_u8; 32];
    let server_nonce = [3_u8; NONCE_BYTES];
    let client_nonce = [4_u8; NONCE_BYTES];
    let mut server =
        ScriptedStream::from_readable(client_envelope(&secret, server_nonce, client_nonce));
    assert_eq!(
        TcpAuthority::new(wrong).server_handshake_after_challenge(&mut server, server_nonce),
        Err(TcpHandshakeError::AuthenticationFailed)
    );
    assert_eq!(server.written, vec![STATUS_REJECTED]);
}

#[test]
fn captured_response_is_bound_to_fresh_server_challenge() {
    let secret = [6_u8; 32];
    let server_nonce = [9_u8; NONCE_BYTES];
    let client_nonce = [10_u8; NONCE_BYTES];
    let envelope = client_envelope(&secret, server_nonce, client_nonce);
    let authority = TcpAuthority::new(secret);
    let mut first = ScriptedStream::from_readable(envelope.clone());
    assert!(
        authority
            .server_handshake_after_challenge(&mut first, server_nonce)
            .is_ok()
    );
    assert_eq!(first.written.len(), 1 + TOKEN_BYTES);

    // A captured envelope remains invalid no matter how many fresh
    // challenges the authority issues. There is no bounded replay cache
    // whose eviction could make an old response valid again.
    for attempt in 0_u64..1024 {
        let mut fresh_nonce = [0_u8; NONCE_BYTES];
        fresh_nonce[..8].copy_from_slice(&(attempt + 1).to_be_bytes());
        if fresh_nonce == server_nonce {
            fresh_nonce[NONCE_BYTES - 1] = 1;
        }
        let mut replay = ScriptedStream::from_readable(envelope.clone());
        assert_eq!(
            authority.server_handshake_after_challenge(&mut replay, fresh_nonce),
            Err(TcpHandshakeError::AuthenticationFailed)
        );
    }
}

#[test]
fn malformed_and_truncated_handshakes_fail_closed() {
    let secret = [6_u8; 32];
    let server_nonce = [9_u8; NONCE_BYTES];
    let mut truncated = ScriptedStream::from_readable(MAGIC.to_vec());
    assert_eq!(
        TcpAuthority::new(secret).server_handshake_after_challenge(&mut truncated, server_nonce,),
        Err(TcpHandshakeError::Truncated)
    );

    let mut invalid =
        ScriptedStream::from_readable(vec![
            0_u8;
            MAGIC.len() + 1 + (NONCE_BYTES * 2) + TOKEN_BYTES
        ]);
    assert_eq!(
        TcpAuthority::new(secret).server_handshake_after_challenge(&mut invalid, server_nonce),
        Err(TcpHandshakeError::InvalidEnvelope)
    );
    assert_eq!(invalid.written, vec![STATUS_REJECTED]);

    let mut truncated_challenge = ScriptedStream::from_readable(MAGIC.to_vec());
    assert_eq!(
        TcpAuthority::new(secret)
            .client_handshake_with_nonce(&mut truncated_challenge, [2_u8; NONCE_BYTES],),
        Err(TcpHandshakeError::Truncated)
    );
}

#[test]
fn loopback_tcp_handshake_succeeds_with_matching_key() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("loopback listener");
    let address = listener.local_addr().expect("listener address");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accepted client");
        configure_loopback(&stream);
        TcpAuthority::new([4_u8; 32]).server_handshake(&mut stream)
    });
    let mut client =
        TcpStream::connect_timeout(&address, HANDSHAKE_TIMEOUT).expect("connected client");
    configure_loopback(&client);
    assert!(
        TcpAuthority::new([4_u8; 32])
            .client_handshake(&mut client)
            .is_ok()
    );
    assert!(server.join().expect("server joined").is_ok());
}

#[test]
fn loopback_tcp_handshake_authenticates_and_rejects_wrong_key() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("loopback listener");
    let address = listener.local_addr().expect("listener address");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accepted client");
        configure_loopback(&stream);
        TcpAuthority::new([4_u8; 32]).server_handshake(&mut stream)
    });
    let mut client =
        TcpStream::connect_timeout(&address, HANDSHAKE_TIMEOUT).expect("connected client");
    configure_loopback(&client);
    assert_eq!(
        TcpAuthority::new([5_u8; 32]).client_handshake(&mut client),
        Err(TcpHandshakeError::AuthenticationFailed)
    );
    assert_eq!(
        server.join().expect("server joined"),
        Err(TcpHandshakeError::AuthenticationFailed)
    );
}

#[test]
fn loopback_authenticated_stream_protects_post_handshake_bytes() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("loopback listener");
    let address = listener.local_addr().expect("listener address");
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accepted client");
        configure_loopback(&stream);
        let mut stream = TcpAuthority::new([31_u8; 32])
            .server_handshake_authenticated(stream, 256)
            .expect("authenticated server");
        let mut request = [0_u8; 7];
        stream.read_exact(&mut request).expect("request");
        assert_eq!(&request, b"request");
        stream.write_all(b"response").expect("response");
        stream.flush().expect("response flush");
    });
    let stream = TcpStream::connect_timeout(&address, HANDSHAKE_TIMEOUT).expect("connected");
    configure_loopback(&stream);
    let mut stream = TcpAuthority::new([31_u8; 32])
        .client_handshake_authenticated(stream, 256)
        .expect("authenticated client");
    stream.write_all(b"request").expect("request");
    stream.flush().expect("request flush");
    let mut response = [0_u8; 8];
    stream.read_exact(&mut response).expect("response");
    assert_eq!(&response, b"response");
    server.join().expect("server joined");
}

#[test]
fn loopback_authenticated_stream_splits_into_concurrent_directions() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("loopback listener");
    let address = listener.local_addr().expect("listener address");
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accepted client");
        configure_loopback(&stream);
        let stream = TcpAuthority::new([32_u8; 32])
            .server_handshake_authenticated(stream, 256)
            .expect("authenticated server");
        let (mut reader, mut writer) = stream
            .try_split_with(|inner| Ok((inner.try_clone()?, inner.try_clone()?)))
            .expect("split server stream");
        let reader_thread = thread::spawn(move || {
            let mut request = [0_u8; 7];
            reader.read_exact(&mut request).expect("request");
            assert_eq!(&request, b"request");
        });
        writer.write_all(b"response").expect("response");
        writer.flush().expect("response flush");
        reader_thread.join().expect("reader joined");
    });
    let stream = TcpStream::connect_timeout(&address, HANDSHAKE_TIMEOUT).expect("connected");
    configure_loopback(&stream);
    let stream = TcpAuthority::new([32_u8; 32])
        .client_handshake_authenticated(stream, 256)
        .expect("authenticated client");
    let (mut reader, mut writer) = stream
        .try_split_with(|inner| Ok((inner.try_clone()?, inner.try_clone()?)))
        .expect("split client stream");
    writer.write_all(b"request").expect("request");
    writer.flush().expect("request flush");
    let mut response = [0_u8; 8];
    reader.read_exact(&mut response).expect("response");
    assert_eq!(&response, b"response");
    server.join().expect("server joined");
}

#[test]
fn loopback_replay_is_rejected_after_a_fresh_challenge() {
    let secret = [11_u8; 32];
    let first_nonce = [12_u8; NONCE_BYTES];
    let second_nonce = [13_u8; NONCE_BYTES];
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("loopback listener");
    let address = listener.local_addr().expect("listener address");
    let (captured_sender, captured_receiver) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut first, _) = listener.accept().expect("first client");
        configure_loopback(&first);
        first
            .write_all(&challenge(first_nonce))
            .expect("first challenge");
        first.flush().expect("first challenge flush");
        let first_result =
            TcpAuthority::new(secret).server_handshake_after_challenge(&mut first, first_nonce);
        let _captured = captured_receiver.recv().expect("captured client response");

        let (mut second, _) = listener.accept().expect("replay client");
        configure_loopback(&second);
        second
            .write_all(&challenge(second_nonce))
            .expect("second challenge");
        second.flush().expect("second challenge flush");
        let second_result =
            TcpAuthority::new(secret).server_handshake_after_challenge(&mut second, second_nonce);
        (first_result, second_result)
    });

    let mut first = RecordingStream::new(
        TcpStream::connect_timeout(&address, HANDSHAKE_TIMEOUT).expect("first connection"),
    );
    configure_loopback(&first.inner);
    assert!(
        TcpAuthority::new(secret)
            .client_handshake_with_nonce(&mut first, [14_u8; NONCE_BYTES])
            .is_ok()
    );
    let captured = first.writes.clone();
    captured_sender
        .send(std::mem::take(&mut first.writes))
        .expect("captured response receiver");
    drop(first);

    let mut second =
        TcpStream::connect_timeout(&address, HANDSHAKE_TIMEOUT).expect("second connection");
    configure_loopback(&second);
    let mut challenge_bytes = [0_u8; MAGIC.len() + 1 + NONCE_BYTES];
    second
        .read_exact(&mut challenge_bytes)
        .expect("second challenge read");
    second
        .write_all(&captured)
        .expect("captured response write");
    second.flush().expect("captured response flush");
    let mut status = [0_u8; 1];
    second.read_exact(&mut status).expect("replay status");
    assert_eq!(status, [STATUS_REJECTED]);

    let (first_result, second_result) = server.join().expect("server joined");
    assert!(first_result.is_ok());
    assert_eq!(second_result, Err(TcpHandshakeError::AuthenticationFailed));
}

fn test_sessions() -> (TcpSession, TcpSession) {
    let server_nonce = [21_u8; NONCE_BYTES];
    let client_nonce = [22_u8; NONCE_BYTES];
    let secret = [23_u8; 32];
    (
        session_for_client(&secret, &server_nonce, &client_nonce),
        session_for_server(&secret, &server_nonce, &client_nonce),
    )
}

fn record_size(bytes: &[u8]) -> usize {
    let length = u32::from_be_bytes(bytes[14..18].try_into().expect("record header"));
    RECORD_HEADER_BYTES + length as usize + RECORD_MAC_BYTES
}

#[test]
fn authenticated_records_round_trip_and_bind_direction() {
    let (client_session, server_session) = test_sessions();
    let mut sender =
        AuthenticatedTcpStream::new(Cursor::new(Vec::new()), &client_session, 256).expect("sender");
    sender.write_all(b"capability frame").expect("write");
    sender.flush().expect("flush");
    let encoded = sender.into_inner().into_inner();
    let mut receiver =
        AuthenticatedTcpStream::new(Cursor::new(encoded.clone()), &server_session, 256)
            .expect("receiver");
    let mut decoded = Vec::new();
    receiver.read_to_end(&mut decoded).expect("read");
    assert_eq!(decoded, b"capability frame");

    let (client_session, _) = test_sessions();
    let mut wrong_direction =
        AuthenticatedTcpStream::new(Cursor::new(encoded), &client_session, 256)
            .expect("wrong-direction receiver");
    let error = wrong_direction
        .read(&mut [0_u8; 1])
        .expect_err("wrong direction must fail");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(
        error.to_string(),
        "authenticated TCP record direction mismatch"
    );
}

#[test]
fn authenticated_records_reject_tamper_replay_and_reorder() {
    let (client_session, _) = test_sessions();
    let mut sender =
        AuthenticatedTcpStream::new(Cursor::new(Vec::new()), &client_session, 256).expect("sender");
    sender.write_all(b"one").expect("first write");
    sender.flush().expect("first flush");
    sender.write_all(b"two").expect("second write");
    sender.flush().expect("second flush");
    let encoded = sender.into_inner().into_inner();
    let first_len = record_size(&encoded);
    let second_len = record_size(&encoded[first_len..]);
    assert_eq!(first_len + second_len, encoded.len());

    let mut tampered = encoded.clone();
    tampered[RECORD_HEADER_BYTES] ^= 1;
    let (_, tampered_server_session) = test_sessions();
    let mut tampered_receiver =
        AuthenticatedTcpStream::new(Cursor::new(tampered), &tampered_server_session, 256)
            .expect("tampered receiver");
    let error = tampered_receiver
        .read(&mut [0_u8; 3])
        .expect_err("tampered record must fail");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(error.to_string(), "authenticated TCP record MAC mismatch");

    let mut replayed = encoded[..first_len].to_vec();
    replayed.extend_from_slice(&encoded[..first_len]);
    let (_, replay_server_session) = test_sessions();
    let mut replay_receiver =
        AuthenticatedTcpStream::new(Cursor::new(replayed), &replay_server_session, 256)
            .expect("replay receiver");
    let mut bytes = [0_u8; 3];
    replay_receiver
        .read_exact(&mut bytes)
        .expect("first record");
    let error = replay_receiver
        .read(&mut [0_u8; 1])
        .expect_err("replayed record must fail");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(
        error.to_string(),
        "authenticated TCP record replay or reorder"
    );

    let mut reordered = encoded[first_len..].to_vec();
    reordered.extend_from_slice(&encoded[..first_len]);
    let (_, reorder_server_session) = test_sessions();
    let mut reorder_receiver =
        AuthenticatedTcpStream::new(Cursor::new(reordered), &reorder_server_session, 256)
            .expect("reorder receiver");
    let error = reorder_receiver
        .read(&mut [0_u8; 3])
        .expect_err("reordered record must fail");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(
        error.to_string(),
        "authenticated TCP record replay or reorder"
    );
}

#[derive(Debug)]
struct PartialReader {
    readable: Cursor<Vec<u8>>,
    reads: usize,
}

impl Read for PartialReader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.reads = self.reads.saturating_add(1);
        if self.reads == 2 {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "short read timeout",
            ));
        }
        let length = bytes.len().min(2);
        self.readable.read(&mut bytes[..length])
    }
}

#[test]
fn authenticated_record_reader_resumes_after_partial_timeout() {
    let (client_session, server_session) = test_sessions();
    let mut sender =
        AuthenticatedTcpStream::new(Cursor::new(Vec::new()), &client_session, 256).expect("sender");
    sender.write_all(b"partial frame").expect("write");
    sender.flush().expect("flush");
    let encoded = sender.into_inner().into_inner();
    let mut receiver = AuthenticatedTcpStream::new(
        PartialReader {
            readable: Cursor::new(encoded),
            reads: 0,
        },
        &server_session,
        256,
    )
    .expect("receiver");
    let error = receiver
        .read(&mut [0_u8; 1])
        .expect_err("first partial read should time out");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    let mut decoded = Vec::new();
    receiver.read_to_end(&mut decoded).expect("resumed read");
    assert_eq!(decoded, b"partial frame");
}

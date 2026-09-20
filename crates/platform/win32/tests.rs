//! Real Windows behaviour of the local socket and identity seams.
//! Each test binds an actual AF_UNIX endpoint, because the facts shared code depends on are kernel facts.
//! Error kinds and addresses are asserted exactly as the Unix call sites match on them.

use super::identity::{current_user, file_owner, is_owned_by_current_user, owner_of, peer_user};
use super::random;
use super::security::{is_endpoint_metadata, restrict_to_current_user};
use super::socket::{LocalAddr, LocalListener, LocalStream};
use std::io::{ErrorKind, Read as _, Write as _};
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn endpoint(label: &str) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    std::env::temp_dir().join(format!("bp-{label}-{}-{nonce}.sock", std::process::id()))
}

fn accept_within(listener: &LocalListener) -> (LocalStream, LocalAddr) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match listener.accept() {
            Ok(accepted) => return accepted,
            Err(error) if error.kind() == ErrorKind::WouldBlock && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(error) => panic!("accept: {error}"),
        }
    }
}

#[test]
fn a_stream_round_trips_and_reports_both_addresses_like_unix() {
    let path = endpoint("roundtrip");
    let listener = LocalListener::bind(&path).expect("bind");
    listener
        .set_nonblocking(true)
        .expect("nonblocking listener");
    let mut client = LocalStream::connect(&path).expect("connect");
    let (server, address) = accept_within(&listener);
    server.set_nonblocking(false).expect("blocking stream");

    assert!(
        address.is_unnamed(),
        "an accepted client never bound a path"
    );
    let peer = client.peer_addr().expect("client peer address");
    assert!(!peer.is_unnamed());
    assert_eq!(peer.as_pathname(), Some(path.as_path()));

    client.write_all(b"ping").expect("write");
    let mut received = [0_u8; 4];
    (&server).read_exact(&mut received).expect("read");
    assert_eq!(&received, b"ping");

    drop(client);
    drop(server);
    drop(listener);
    std::fs::remove_file(&path).expect("remove endpoint");
}

#[test]
fn both_ends_of_a_connection_identify_the_current_user() {
    let path = endpoint("peer-user");
    let listener = LocalListener::bind(&path).expect("bind");
    listener
        .set_nonblocking(true)
        .expect("nonblocking listener");
    let client = LocalStream::connect(&path).expect("connect");
    let (server, _) = accept_within(&listener);

    let me = current_user().expect("current user");
    assert_eq!(peer_user(&client).expect("server identity"), me);
    assert_eq!(peer_user(&server).expect("client identity"), me);

    drop(client);
    drop(server);
    drop(listener);
    std::fs::remove_file(&path).expect("remove endpoint");
}

#[test]
fn an_endpoint_is_classified_owned_restricted_and_still_connectable() {
    let path = endpoint("restrict");
    let listener = LocalListener::bind(&path).expect("bind");
    let metadata = std::fs::symlink_metadata(&path).expect("endpoint metadata");
    assert!(is_endpoint_metadata(&metadata), "{metadata:?}");

    restrict_to_current_user(&path).expect("restrict endpoint");
    let owner = file_owner(&path).expect("endpoint owner");
    assert!(
        is_owned_by_current_user(&owner).expect("owner check"),
        "{owner:?}"
    );
    let _client = LocalStream::connect(&path).expect("connect after restricting");

    drop(listener);
    std::fs::remove_file(&path).expect("remove endpoint");
}

#[test]
fn a_regular_file_is_not_an_endpoint_and_can_be_made_private() {
    let path = endpoint("regular").with_extension("secret");
    std::fs::write(&path, [7_u8; 32]).expect("write file");
    let metadata = std::fs::symlink_metadata(&path).expect("file metadata");
    assert!(!is_endpoint_metadata(&metadata));
    restrict_to_current_user(&path).expect("restrict file");
    let file = std::fs::File::open(&path).expect("open for reading");
    let owner = owner_of(&file).expect("owner through a read handle");
    assert!(
        is_owned_by_current_user(&owner).expect("owner check"),
        "{owner:?}"
    );
    drop(file);
    assert_eq!(std::fs::read(&path).expect("read back").len(), 32);
    std::fs::remove_file(&path).expect("remove file");
}

#[test]
fn a_dead_endpoint_refuses_like_unix_and_can_be_removed() {
    let path = endpoint("dead");
    let listener = LocalListener::bind(&path).expect("bind");
    drop(listener);
    assert!(path.exists(), "closing a listener leaves its socket file");
    let error = LocalStream::connect(&path).expect_err("nothing is listening");
    assert!(
        matches!(
            error.kind(),
            ErrorKind::ConnectionRefused | ErrorKind::NotFound | ErrorKind::ConnectionReset
        ),
        "unexpected dead-endpoint error: {error:?} ({:?})",
        error.kind()
    );
    std::fs::remove_file(&path).expect("remove dead endpoint");
    let missing = LocalStream::connect(&path).expect_err("no endpoint file");
    assert!(
        matches!(
            missing.kind(),
            ErrorKind::ConnectionRefused | ErrorKind::NotFound | ErrorKind::ConnectionReset
        ),
        "unexpected missing-endpoint error: {missing:?} ({:?})",
        missing.kind()
    );
}

#[test]
fn a_read_deadline_expires_instead_of_blocking() {
    let path = endpoint("deadline");
    let listener = LocalListener::bind(&path).expect("bind");
    listener
        .set_nonblocking(true)
        .expect("nonblocking listener");
    let mut client = LocalStream::connect(&path).expect("connect");
    let (_server, _) = accept_within(&listener);
    client
        .set_read_timeout(Some(Duration::from_millis(50)))
        .expect("read timeout");
    let started = Instant::now();
    let mut byte = [0_u8; 1];
    let error = client.read(&mut byte).expect_err("no data was sent");
    assert!(
        matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut),
        "{error:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    drop(listener);
    std::fs::remove_file(&path).expect("remove endpoint");
}

#[test]
fn the_system_generator_fills_a_buffer() {
    let mut bytes = [0_u8; 32];
    random::fill(&mut bytes).expect("random bytes");
    assert_ne!(bytes, [0_u8; 32]);
}

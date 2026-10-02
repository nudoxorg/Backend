//! Connected local stream pairs for tests, on every platform with local
//! sockets.
//!
//! `UnixStream::pair` does not exist on Windows. A named AF_UNIX listener
//! does, and the platform's `LocalStream`/`LocalListener` expose it with the
//! same method names on Unix and Windows, so a pair is a listener, a connect
//! and an accept.

use backend_replication::{LocalListener, LocalStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Returns the two ends of one connected local stream.
#[allow(clippy::expect_used)]
pub(crate) fn local_pair() -> (LocalStream, LocalStream) {
    static PAIRS: AtomicUsize = AtomicUsize::new(0);
    let path: PathBuf = std::env::temp_dir().join(format!(
        "nxc-{}-{}.sock",
        std::process::id(),
        PAIRS.fetch_add(1, Ordering::Relaxed)
    ));
    let listener = LocalListener::bind(&path).expect("bind a pair endpoint");
    let client = LocalStream::connect(&path).expect("connect to the pair endpoint");
    let (server, _) = listener.accept().expect("accept the pair connection");
    drop(listener);
    let _ = std::fs::remove_file(&path);
    (client, server)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn a_pair_carries_bytes_both_ways() {
        let (mut client, mut server) = local_pair();
        client.write_all(b"ping").expect("client write");
        let mut received = [0_u8; 4];
        server.read_exact(&mut received).expect("server read");
        assert_eq!(&received, b"ping");
        server.write_all(b"pong").expect("server write");
        client.read_exact(&mut received).expect("client read");
        assert_eq!(&received, b"pong");
    }

    #[test]
    fn shutting_down_one_end_wakes_a_blocked_read_on_the_other() {
        let (mut client, server) = local_pair();
        let reader = std::thread::spawn(move || {
            let mut byte = [0_u8; 1];
            client.read(&mut byte)
        });
        std::thread::sleep(std::time::Duration::from_millis(50));
        server
            .shutdown(std::net::Shutdown::Both)
            .expect("shutdown the server end");
        assert!(matches!(
            reader.join().expect("reader thread"),
            Ok(0) | Err(_)
        ));
    }

    /// The contract `TransportInterrupt` relies on: shutting a socket down from
    /// another thread releases a read already blocked on it. The reader is
    /// given time to block first, so this cannot pass by racing the shutdown.
    #[test]
    fn shutting_down_a_clone_releases_a_read_already_blocked_on_the_socket() {
        let (mut client, _server) = local_pair();
        client
            .set_read_timeout(Some(std::time::Duration::from_secs(8)))
            .expect("read deadline");
        let clone = client.try_clone().expect("clone the exact socket");
        let reader = std::thread::spawn(move || {
            let mut byte = [0_u8; 1];
            let started = std::time::Instant::now();
            let result = client.read(&mut byte);
            (result, started.elapsed())
        });
        std::thread::sleep(std::time::Duration::from_millis(300));
        clone
            .shutdown(std::net::Shutdown::Both)
            .expect("shut the clone down");
        let (result, waited) = reader.join().expect("reader thread");
        assert!(matches!(result, Ok(0) | Err(_)));
        assert!(
            waited < std::time::Duration::from_secs(4),
            "the blocked read was not released by shutdown; it waited {waited:?} for its 8 s deadline"
        );
    }
}

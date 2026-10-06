//! One local stream, listener, and address type for every process-boundary crate.
//! On Unix these are the standard library's Unix socket types. On Windows they are AF_UNIX
//! sockets with the same method names, so call sites need no platform branches.

#[cfg(unix)]
pub use std::os::unix::net::{
    SocketAddr as LocalAddr, UnixListener as LocalListener, UnixStream as LocalStream,
};

#[cfg(windows)]
pub use crate::win32::socket::{LocalAddr, LocalListener, LocalStream};

/// What a nonconsuming probe establishes about delivery to a local peer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplyPeerState {
    /// The kernel established that the peer cannot receive a response.
    Closed,
    /// No closed response direction was established. A full send buffer or an
    /// unsupported platform probe is not evidence that the caller abandoned.
    NotKnownClosed,
}

/// Inspects response-write readiness without reading request bytes, sending
/// protocol bytes, or changing the stream's blocking and deadline options.
///
/// Only the write direction is selected: Darwin reports read-side HUP for a
/// valid peer `shutdown(Write)`, which must still be allowed to receive replies.
/// Windows currently retains its bounded owner-response deadline when this
/// stronger kernel witness is unavailable.
///
/// # Errors
/// Returns the kernel error if the descriptor cannot be inspected.
pub fn reply_peer_state(stream: &LocalStream) -> std::io::Result<ReplyPeerState> {
    #[cfg(unix)]
    {
        use rustix::event::{PollFd, PollFlags, Timespec, poll};
        let mut descriptors = [PollFd::new(stream, PollFlags::OUT)];
        poll(
            &mut descriptors,
            Some(&Timespec {
                tv_sec: 0,
                tv_nsec: 0,
            }),
        )?;
        if descriptors[0].revents().contains(PollFlags::NVAL) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "local response peer descriptor is invalid",
            ));
        }
        Ok(if descriptors[0].revents().contains(PollFlags::HUP) {
            ReplyPeerState::Closed
        } else {
            ReplyPeerState::NotKnownClosed
        })
    }
    #[cfg(windows)]
    {
        let _ = stream;
        Ok(ReplyPeerState::NotKnownClosed)
    }
}

/// Connects to one local socket endpoint with a bounded dial time.
///
/// # Errors
/// Returns an I/O error when the endpoint cannot be reached before `timeout` or is invalid.
pub fn connect_timeout(
    path: impl AsRef<std::path::Path>,
    timeout: std::time::Duration,
) -> std::io::Result<LocalStream> {
    #[cfg(unix)]
    {
        use socket2::{Domain, SockAddr, Socket, Type};
        use std::os::fd::OwnedFd;

        let address = SockAddr::unix(path.as_ref())?;
        let socket = Socket::new(Domain::UNIX, Type::STREAM, None)?;
        socket.connect_timeout(&address, timeout)?;
        let descriptor: OwnedFd = socket.into();
        Ok(std::os::unix::net::UnixStream::from(descriptor))
    }
    #[cfg(windows)]
    {
        LocalStream::connect_timeout(path, timeout)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::{LocalListener, LocalStream, ReplyPeerState, connect_timeout, reply_peer_state};
    use std::io::{Read as _, Write as _};
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn reply_peer_probe_preserves_half_close_and_buffered_request_bytes() {
        let (mut owner, mut peer) = LocalStream::pair().expect("local stream pair");
        peer.write_all(b"next frame").expect("buffer request bytes");
        peer.shutdown(std::net::Shutdown::Write)
            .expect("half-close request direction");
        assert_eq!(
            reply_peer_state(&owner).expect("probe half-close"),
            ReplyPeerState::NotKnownClosed
        );
        let mut bytes = [0_u8; 10];
        owner
            .read_exact(&mut bytes)
            .expect("probe did not consume request bytes");
        assert_eq!(&bytes, b"next frame");
        owner
            .write_all(b"reply")
            .expect("half-closed peer can receive");
        let mut reply = [0_u8; 5];
        peer.read_exact(&mut reply).expect("receive owner response");
        assert_eq!(&reply, b"reply");
    }

    #[test]
    fn reply_peer_probe_detects_full_close_with_unread_bytes() {
        let (owner, mut peer) = LocalStream::pair().expect("local stream pair");
        peer.write_all(b"buffered").expect("buffer request bytes");
        drop(peer);
        assert_eq!(
            reply_peer_state(&owner).expect("probe full-close"),
            ReplyPeerState::Closed
        );
    }

    #[test]
    fn reply_peer_probe_does_not_charge_an_idle_connected_peer_as_closed() {
        let (owner, _peer) = LocalStream::pair().expect("local stream pair");
        assert_eq!(
            reply_peer_state(&owner).expect("probe idle"),
            ReplyPeerState::NotKnownClosed
        );
    }

    #[test]
    fn local_socket_connect_timeout_reaches_the_named_listener() {
        // The name stays short: macOS refuses a socket path of 104 bytes or more, and a
        // per-session temporary directory already spends about half of them.
        let path = std::env::temp_dir().join(format!(
            "bplc-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_nanos()),
        ));
        let listener = LocalListener::bind(&path).expect("bind local socket");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("make local socket private");
        let stream = connect_timeout(&path, std::time::Duration::from_secs(1))
            .expect("connect within timeout");
        let (_accepted, _) = listener.accept().expect("accept timed connection");
        assert_eq!(
            stream.peer_addr().expect("peer address").as_pathname(),
            Some(path.as_path())
        );
        drop(stream);
        drop(listener);
        std::fs::remove_file(path).expect("remove local socket");
    }
}

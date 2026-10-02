//! One local stream, listener, and address type for every process-boundary crate.
//! On Unix these are the standard library's Unix socket types. On Windows they are AF_UNIX
//! sockets with the same method names, so call sites need no platform branches.

#[cfg(unix)]
pub use std::os::unix::net::{
    SocketAddr as LocalAddr, UnixListener as LocalListener, UnixStream as LocalStream,
};

#[cfg(windows)]
pub use crate::win32::socket::{LocalAddr, LocalListener, LocalStream};

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
    use super::{LocalListener, connect_timeout};
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn local_socket_connect_timeout_reaches_the_named_listener() {
        let path = std::env::temp_dir().join(format!(
            "backend-platform-local-connect-{}-{}",
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

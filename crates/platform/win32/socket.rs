//! AF_UNIX streams and listeners for Windows 10 1803 and later.
//! Method names mirror `std::os::unix::net`, so shared code compiles against either platform unchanged.
//! Addresses are decoded from the kernel's `sockaddr_un`, never inferred from the path a caller asked for.
#![allow(
    unsafe_code,
    reason = "decoding a kernel-filled sockaddr_un is the single reviewed unsafe read in this module"
)]

use socket2::{Domain, SockAddr, Socket, Type};
use std::fmt;
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::windows::io::{AsRawSocket, RawSocket};
use std::path::{Path, PathBuf};
use std::time::Duration;
use windows_sys::Win32::Networking::WinSock::{ADDRESS_FAMILY, AF_UNIX, SOCKADDR_UN};

/// Listen backlog, matching the value the Rust standard library uses for
/// `UnixListener` on Unix.
const BACKLOG: i32 = 128;

/// The address of one end of a local AF_UNIX connection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalAddr {
    named: bool,
    path: Option<PathBuf>,
}

impl LocalAddr {
    /// Returns whether this end never bound a path, which is how an accepted
    /// client connection is reported.
    #[must_use]
    pub const fn is_unnamed(&self) -> bool {
        !self.named
    }

    /// Returns the bound path, when the kernel reported one that is valid UTF-8.
    ///
    /// A named address whose bytes are not UTF-8 reports `None` here while
    /// still reporting `false` from [`Self::is_unnamed`], so a comparison
    /// against an expected endpoint fails closed.
    #[must_use]
    pub fn as_pathname(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    fn from_socket(address: SockAddr) -> Self {
        if address.family() != AF_UNIX {
            return Self {
                named: true,
                path: None,
            };
        }
        let length = usize::try_from(address.len()).unwrap_or(0);
        let mut storage = address.as_storage();
        // SAFETY: `SockAddrStorage` wraps `SOCKADDR_STORAGE`, which is larger
        // than `SOCKADDR_UN` and has a stricter alignment. The family was
        // checked above, and both fields read are plain integers, so every
        // bit pattern is a valid value.
        let unix = unsafe { storage.view_as::<SOCKADDR_UN>() };
        let available = length
            .saturating_sub(size_of::<ADDRESS_FAMILY>())
            .min(unix.sun_path.len());
        let bytes: Vec<u8> = unix.sun_path[..available]
            .iter()
            .map(|byte| byte.cast_unsigned())
            .take_while(|byte| *byte != 0)
            .collect();
        if bytes.is_empty() {
            return Self {
                named: false,
                path: None,
            };
        }
        Self {
            named: true,
            path: String::from_utf8(bytes).ok().map(PathBuf::from),
        }
    }
}

/// A connected AF_UNIX stream.
pub struct LocalStream(Socket);

impl LocalStream {
    /// Connects to the listener bound at `path`.
    ///
    /// # Errors
    /// Returns an error when the path is not valid UTF-8, is too long for
    /// `sockaddr_un`, or nothing is listening there.
    pub fn connect(path: impl AsRef<Path>) -> io::Result<Self> {
        let address = SockAddr::unix(path.as_ref())?;
        let socket = Socket::new(Domain::UNIX, Type::STREAM, None)?;
        socket.connect(&address)?;
        Ok(Self(socket))
    }

    /// Returns the address of the other end of this connection.
    ///
    /// # Errors
    /// Returns an error when the socket is not connected.
    pub fn peer_addr(&self) -> io::Result<LocalAddr> {
        self.0.peer_addr().map(LocalAddr::from_socket)
    }

    /// Sets the read deadline for blocking reads.
    ///
    /// # Errors
    /// Returns an error when the socket option cannot be set.
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.0.set_read_timeout(timeout)
    }

    /// Sets the write deadline for blocking writes.
    ///
    /// # Errors
    /// Returns an error when the socket option cannot be set.
    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.0.set_write_timeout(timeout)
    }

    /// Switches the stream between blocking and nonblocking I/O.
    ///
    /// # Errors
    /// Returns an error when the socket mode cannot be changed.
    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.0.set_nonblocking(nonblocking)
    }

    /// Returns a second handle to the same connection.
    ///
    /// # Errors
    /// Returns an error when the socket cannot be duplicated.
    pub fn try_clone(&self) -> io::Result<Self> {
        self.0.try_clone().map(Self)
    }

    /// Shuts down one or both directions of the connection.
    ///
    /// # Errors
    /// Returns an error when the socket is not connected.
    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        self.0.shutdown(how)
    }
}

impl fmt::Debug for LocalStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalStream")
            .field("socket", &self.0.as_raw_socket())
            .finish()
    }
}

impl AsRawSocket for LocalStream {
    fn as_raw_socket(&self) -> RawSocket {
        self.0.as_raw_socket()
    }
}

impl Read for LocalStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        (&self.0).read(buffer)
    }
}

impl Read for &LocalStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        (&self.0).read(buffer)
    }
}

impl Write for LocalStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        (&self.0).write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        (&self.0).flush()
    }
}

impl Write for &LocalStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        (&self.0).write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        (&self.0).flush()
    }
}

/// A listening AF_UNIX socket bound to a filesystem path.
pub struct LocalListener(Socket);

impl LocalListener {
    /// Binds `path` and starts listening. The path must not already exist.
    ///
    /// # Errors
    /// Returns an error when the path is invalid, already exists, or the
    /// socket cannot be created.
    pub fn bind(path: impl AsRef<Path>) -> io::Result<Self> {
        let address = SockAddr::unix(path.as_ref())?;
        let socket = Socket::new(Domain::UNIX, Type::STREAM, None)?;
        socket.bind(&address)?;
        socket.listen(BACKLOG)?;
        Ok(Self(socket))
    }

    /// Accepts one pending connection.
    ///
    /// # Errors
    /// Returns [`io::ErrorKind::WouldBlock`] on a nonblocking listener with no
    /// pending client, or the accept failure.
    pub fn accept(&self) -> io::Result<(LocalStream, LocalAddr)> {
        let (socket, address) = self.0.accept()?;
        Ok((LocalStream(socket), LocalAddr::from_socket(address)))
    }

    /// Switches the listener between blocking and nonblocking accept.
    ///
    /// # Errors
    /// Returns an error when the socket mode cannot be changed.
    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.0.set_nonblocking(nonblocking)
    }
}

impl fmt::Debug for LocalListener {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalListener")
            .field("socket", &self.0.as_raw_socket())
            .finish()
    }
}

impl AsRawSocket for LocalListener {
    fn as_raw_socket(&self) -> RawSocket {
        self.0.as_raw_socket()
    }
}

//! Bounded authenticated handshake for cross-host worker transports.
//!
//! Unix sockets use peer credentials and filesystem permissions.  A TCP
//! socket has neither property, so a process must prove possession of the
//! same owner-supplied credential before the canonical replication stream is
//! opened.  The handshake is deliberately separate from the replication
//! codec: after it succeeds, callers wrap the stream in
//! [`crate::dispatch::StreamTransport`].  TCP callers must use the
//! authenticated stream constructor so every subsequent replication frame is
//! protected by a transcript-derived, direction-bound record MAC.
#![deny(unsafe_code)]
const MAGIC: [u8; 4] = *b"BTCP";
const VERSION: u8 = 1;
const NONCE_BYTES: usize = 32;
const TOKEN_BYTES: usize = 32;
const STATUS_OK: u8 = 0;
const STATUS_REJECTED: u8 = 1;
const RECORD_MAGIC: [u8; 4] = *b"BTS1";
const RECORD_VERSION: u8 = 1;
const CLIENT_TO_SERVER: u8 = 0;
const SERVER_TO_CLIENT: u8 = 1;
const RECORD_HEADER_BYTES: usize = 4 + 1 + 1 + 8 + 4;
const RECORD_MAC_BYTES: usize = 32;

fn token(
    secret: &[u8; 32],
    server_nonce: &[u8; NONCE_BYTES],
    client_nonce: &[u8; NONCE_BYTES],
    role: &[u8],
) -> [u8; 32] {
    let mut material = Vec::with_capacity(
        MAGIC.len() + 1 + 2 + server_nonce.len() + client_nonce.len() + role.len(),
    );
    material.extend_from_slice(&MAGIC);
    material.push(VERSION);
    material.extend_from_slice(b"backend.tcp.authority.transcript.v1\0");
    material.extend_from_slice(&u16::try_from(role.len()).unwrap_or(u16::MAX).to_be_bytes());
    material.extend_from_slice(server_nonce);
    material.extend_from_slice(client_nonce);
    material.extend_from_slice(role);
    *blake3::keyed_hash(secret, &material).as_bytes()
}

fn session_key(
    secret: &[u8; 32],
    server_nonce: &[u8; NONCE_BYTES],
    client_nonce: &[u8; NONCE_BYTES],
    direction: &[u8],
) -> [u8; 32] {
    let mut material = Vec::with_capacity(
        MAGIC.len() + 1 + server_nonce.len() + client_nonce.len() + direction.len() + 2,
    );
    material.extend_from_slice(&MAGIC);
    material.push(VERSION);
    material.extend_from_slice(b"backend.tcp.stream.v1\0");
    material.extend_from_slice(
        &u16::try_from(direction.len())
            .unwrap_or(u16::MAX)
            .to_be_bytes(),
    );
    material.extend_from_slice(server_nonce);
    material.extend_from_slice(client_nonce);
    material.extend_from_slice(direction);
    *blake3::keyed_hash(secret, &material).as_bytes()
}

fn session_for_client(
    secret: &[u8; 32],
    server_nonce: &[u8; NONCE_BYTES],
    client_nonce: &[u8; NONCE_BYTES],
) -> TcpSession {
    TcpSession {
        send_key: session_key(secret, server_nonce, client_nonce, b"client-to-server"),
        recv_key: session_key(secret, server_nonce, client_nonce, b"server-to-client"),
        send_direction: CLIENT_TO_SERVER,
        recv_direction: SERVER_TO_CLIENT,
        send_sequence: 0,
        recv_sequence: 0,
    }
}

fn session_for_server(
    secret: &[u8; 32],
    server_nonce: &[u8; NONCE_BYTES],
    client_nonce: &[u8; NONCE_BYTES],
) -> TcpSession {
    TcpSession {
        send_key: session_key(secret, server_nonce, client_nonce, b"server-to-client"),
        recv_key: session_key(secret, server_nonce, client_nonce, b"client-to-server"),
        send_direction: SERVER_TO_CLIENT,
        recv_direction: CLIENT_TO_SERVER,
        send_sequence: 0,
        recv_sequence: 0,
    }
}

struct TcpSession {
    send_key: [u8; 32],
    recv_key: [u8; 32],
    send_direction: u8,
    recv_direction: u8,
    send_sequence: u64,
    recv_sequence: u64,
}

impl Drop for TcpSession {
    fn drop(&mut self) {
        self.send_key = [0_u8; 32];
        self.recv_key = [0_u8; 32];
    }
}

fn constant_time_eq(left: &[u8; 32], right: &[u8; 32]) -> bool {
    let mut difference = 0_u8;
    for (a, b) in left.iter().zip(right.iter()) {
        difference |= a ^ b;
    }
    difference == 0
}

mod handshake;
mod stream;
#[cfg(test)]
mod tests;

pub use handshake::{TcpAuthority, TcpHandshakeError};
pub use stream::{AuthenticatedReadHalf, AuthenticatedTcpStream, AuthenticatedWriteHalf};

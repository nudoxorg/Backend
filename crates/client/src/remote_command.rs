//! Revision-aware product queries through the owner's signed remote-index grant.

use crate::{ClientError, CommandTransport, admit_reply};
use backend_engine::cluster_transport::{
    Endpoint, EndpointAddr, MAX_REMOTE_INDEX_BODY_BYTES, RemoteIndexCapability, RemoteIndexChannel,
    RemoteIndexOutcome, RemoteIndexRequest, RemoteIndexSession, RemoteIndexSessionHello, SecretKey,
    TransportError, bind_direct, connect_remote_index, remote_index_now,
};
use backend_library::{CommandDto, ReplyDto, decode_reply_body, encode_command_body};
use std::net::SocketAddr;

/// Direct, no-relay product command transport with an owner-signed query grant.
pub struct RemoteIndexCommandTransport {
    runtime: tokio::runtime::Runtime,
    endpoint: Endpoint,
    owner_address: EndpointAddr,
    capability: RemoteIndexCapability,
    session: Option<RemoteIndexSession>,
}

impl RemoteIndexCommandTransport {
    /// Creates a client endpoint whose Iroh identity must match the grant.
    pub fn connect(
        client_secret: SecretKey,
        owner: backend_engine::cluster_transport::EndpointId,
        owner_address: SocketAddr,
        capability: RemoteIndexCapability,
    ) -> Result<Self, ClientError> {
        if capability.claims.product.is_none() {
            return Err(ClientError::Protocol(
                "remote product transport needs a product-query capability".to_owned(),
            ));
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| ClientError::Io(error.to_string()))?;
        let bind_address = match owner_address {
            SocketAddr::V4(_) => SocketAddr::from(([0, 0, 0, 0], 0)),
            SocketAddr::V6(_) => SocketAddr::from(([0_u16; 8], 0)),
        };
        let endpoint = runtime
            .block_on(bind_direct(client_secret, bind_address))
            .map_err(|error| ClientError::Io(error.to_string()))?;
        capability
            .verify(
                owner,
                endpoint.id(),
                remote_index_now().map_err(map_transport)?,
            )
            .map_err(|error| ClientError::Protocol(error.to_string()))?;
        Ok(Self {
            runtime,
            endpoint,
            owner_address: EndpointAddr::new(owner).with_ip_addr(owner_address),
            capability,
            session: None,
        })
    }

    fn open_session(&mut self) -> Result<(), ClientError> {
        let hello =
            RemoteIndexSessionHello::new(self.capability.clone(), RemoteIndexChannel::ProductQuery)
                .map_err(|error| ClientError::Protocol(error.to_string()))?;
        let session = self
            .runtime
            .block_on(connect_remote_index(
                &self.endpoint,
                self.owner_address.clone(),
                hello,
            ))
            .map_err(map_session_transport)?;
        self.session = Some(session);
        Ok(())
    }

    fn request_once(
        &mut self,
        request_id: u64,
        body: Box<[u8]>,
    ) -> Result<RemoteIndexOutcome, ClientError> {
        if self.session.is_none() {
            self.open_session()?;
        }
        let Some(session) = self.session.as_mut() else {
            return Err(ClientError::Io("remote session was not opened".to_owned()));
        };
        let request = RemoteIndexRequest { request_id, body };
        self.runtime
            .block_on(async {
                session.send_request(&request).await?;
                session.receive_response(request_id).await
            })
            .map(|response| response.outcome)
            .map_err(map_session_transport)
    }
}

impl CommandTransport for RemoteIndexCommandTransport {
    fn request(&mut self, request: CommandDto) -> Result<ReplyDto, ClientError> {
        let body = encode_command_body(&request).map_err(ClientError::Protocol)?;
        let outcome = match self.request_once(request.request_id, body.into_boxed_slice()) {
            Ok(outcome) => outcome,
            Err(error) if matches!(error, ClientError::Disconnected(_)) => {
                self.session = None;
                self.open_session()?;
                let body = encode_command_body(&request).map_err(ClientError::Protocol)?;
                self.request_once(request.request_id, body.into_boxed_slice())?
            }
            Err(error) => return Err(error),
        };
        match outcome {
            RemoteIndexOutcome::Payload(body) => {
                if body.len() > MAX_REMOTE_INDEX_BODY_BYTES {
                    return Err(ClientError::Transport(
                        backend_replication::ReplicationError::MessageTooLarge,
                    ));
                }
                let reply = decode_reply_body(&body).map_err(ClientError::Protocol)?;
                admit_reply(&request, reply)
            }
            RemoteIndexOutcome::StaleProductRoot { expected, observed } => {
                Err(ClientError::StaleRemoteRoot { expected, observed })
            }
            RemoteIndexOutcome::StaleProductSnapshot { .. } => {
                Err(ClientError::StaleRemoteCapability)
            }
            RemoteIndexOutcome::StaleSemanticSelection => Err(ClientError::StaleSelection),
            RemoteIndexOutcome::Rejected(
                backend_engine::cluster_transport::RemoteIndexReject::StaleCapability,
            ) => Err(ClientError::StaleRemoteCapability),
            RemoteIndexOutcome::Rejected(
                backend_engine::cluster_transport::RemoteIndexReject::CapabilityRevoked,
            ) => Err(ClientError::RemoteCapabilityRevoked),
            RemoteIndexOutcome::Rejected(reason) => Err(ClientError::Protocol(format!(
                "remote index request was rejected: {reason:?}"
            ))),
        }
    }

    fn reconnect(&mut self) -> Result<(), ClientError> {
        self.session = None;
        self.open_session()
    }
}

fn map_transport(error: impl std::fmt::Display) -> ClientError {
    ClientError::Io(error.to_string())
}

fn map_session_transport(error: TransportError) -> ClientError {
    match error {
        TransportError::Iroh(_) | TransportError::Io(_) => {
            ClientError::Disconnected(std::io::ErrorKind::ConnectionReset)
        }
        TransportError::Frame(message) => ClientError::Protocol(message),
        other => ClientError::Protocol(other.to_string()),
    }
}

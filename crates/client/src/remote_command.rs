//! Revision-aware product queries through the owner's signed remote-index grant.

use crate::{ClientError, CommandTransport, admit_reply};
use backend_engine::cluster_transport::{
    Endpoint, EndpointAddr, MAX_REMOTE_INDEX_BODY_BYTES, RemoteIndexAuthenticatedPeer,
    RemoteIndexCapability, RemoteIndexChannel, RemoteIndexOutcome,
    RemoteIndexProductCapabilityReceipt, RemoteIndexRequest, RemoteIndexSession,
    RemoteIndexSessionHello, SecretKey, TransportError, bind_direct, connect_remote_index,
    remote_index_now,
};
use backend_engine::{
    ProducerObservationClaims, ProducerObservationVerifier, ScopeRoot, UntrustedProducerObservation,
};
use backend_library::{
    Command, CommandDto, CommandReply, ReplyDto, RevisionReceipt, encode_command_body,
};
use std::cell::Cell;
use std::net::SocketAddr;

/// The producer tuple admitted from the first owner-certified revision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RemoteProductProducerProof {
    scope: [u8; 32],
    producer: [u8; 32],
    context: [u8; 32],
    evidence_digest: [u8; 32],
}

/// Exact product state bootstrapped from the owner's signed grant and revision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RemoteProductRevisionBinding {
    root: [u8; 32],
    source: [u8; 32],
    producer: RemoteProductProducerProof,
}

/// Admits bounded producer observations only with proof obtained from an
/// authenticated ProductQuery session and its owner-signed capability.
struct RemoteProductCoverageVerifier {
    expected: Option<RemoteProductRevisionBinding>,
    observed: Cell<Option<RemoteProductProducerProof>>,
}

impl RemoteProductCoverageVerifier {
    fn bootstrap_revision(
        peer: &RemoteIndexAuthenticatedPeer,
        receipt: &RemoteIndexProductCapabilityReceipt,
    ) -> Result<Self, &'static str> {
        Self::from_authenticated_grant(peer, receipt, None)
    }

    fn pinned_query(
        peer: &RemoteIndexAuthenticatedPeer,
        receipt: &RemoteIndexProductCapabilityReceipt,
        expected: RemoteProductRevisionBinding,
    ) -> Result<Self, &'static str> {
        Self::from_authenticated_grant(peer, receipt, Some(expected))
    }

    fn from_authenticated_grant(
        peer: &RemoteIndexAuthenticatedPeer,
        receipt: &RemoteIndexProductCapabilityReceipt,
        expected: Option<RemoteProductRevisionBinding>,
    ) -> Result<Self, &'static str> {
        if peer.channel() != RemoteIndexChannel::ProductQuery
            || peer.peer() != receipt.server()
            || peer.client() != receipt.client()
            || peer.grant_id() != receipt.grant_id()
            || receipt.view_root() == [0; 32]
            || expected.is_some_and(|binding| {
                binding.root != receipt.view_root()
                    || binding.source == [0; 32]
                    || binding.producer.scope != binding.source
            })
        {
            return Err("remote producer verifier needs the authenticated owner grant");
        }
        Ok(Self {
            expected,
            observed: Cell::new(None),
        })
    }

    fn observed_proof(&self) -> Option<RemoteProductProducerProof> {
        self.observed.get()
    }
}

impl ProducerObservationVerifier for RemoteProductCoverageVerifier {
    type Error = &'static str;

    fn verify(
        &self,
        observation: &UntrustedProducerObservation,
    ) -> Result<ProducerObservationClaims, Self::Error> {
        let proof = validate_remote_product_observation(observation, self.expected)?;
        if self
            .observed
            .get()
            .is_some_and(|existing| existing != proof)
        {
            return Err("authenticated remote producer observation is invalid");
        }
        self.observed.set(Some(proof));
        Ok(ProducerObservationClaims::new(
            observation.producer_identity(),
            observation.scope_root(),
            observation.context(),
            *blake3::hash(observation.evidence()).as_bytes(),
        ))
    }
}

fn remote_product_producer_proof(
    observation: &UntrustedProducerObservation,
) -> Result<RemoteProductProducerProof, &'static str> {
    if observation.scope_root().as_bytes() == &[0; 32]
        || observation.producer_identity() == [0; 32]
        || observation.context() == [0; 32]
        || observation.evidence().is_empty()
        || observation.evidence().len() > backend_library::MAX_COVERAGE_EVIDENCE
    {
        return Err("authenticated remote producer observation is invalid");
    }
    Ok(RemoteProductProducerProof {
        scope: *observation.scope_root().as_bytes(),
        producer: observation.producer_identity(),
        context: observation.context(),
        evidence_digest: *blake3::hash(observation.evidence()).as_bytes(),
    })
}

fn validate_remote_product_observation(
    observation: &UntrustedProducerObservation,
    expected: Option<RemoteProductRevisionBinding>,
) -> Result<RemoteProductProducerProof, &'static str> {
    let proof = remote_product_producer_proof(observation)?;
    if expected.is_some_and(|binding| proof.scope != binding.source || proof != binding.producer) {
        return Err("authenticated remote producer observation is invalid");
    }
    Ok(proof)
}

fn bind_remote_product_revision(
    expected_root: [u8; 32],
    revision: RevisionReceipt,
    producer: RemoteProductProducerProof,
) -> Result<RemoteProductRevisionBinding, ClientError> {
    let root = revision.root().to_bytes();
    validate_revision_root(expected_root, root)?;
    let source = revision.source().to_bytes();
    if source == [0; 32] || producer.scope != source {
        return Err(ClientError::Protocol(
            "remote revision proof does not match its producer source".to_owned(),
        ));
    }
    Ok(RemoteProductRevisionBinding {
        root,
        source,
        producer,
    })
}

fn require_remote_product_revision(
    has_revision: bool,
    is_revision: bool,
) -> Result<(), ClientError> {
    if has_revision || is_revision {
        Ok(())
    } else {
        Err(ClientError::Protocol(
            "remote product queries require an admitted Revision first".to_owned(),
        ))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RevisionBootstrapEnvelope {
    Revision,
    IdentityFreeFailure,
}

fn classify_revision_bootstrap(body: &[u8]) -> Result<RevisionBootstrapEnvelope, ClientError> {
    let envelope: serde_json::Value =
        serde_json::from_slice(body).map_err(|error| ClientError::Protocol(error.to_string()))?;
    let kind = envelope
        .get("reply")
        .and_then(|reply| reply.get("kind"))
        .and_then(serde_json::Value::as_str);
    match kind {
        Some("revision") => Ok(RevisionBootstrapEnvelope::Revision),
        Some("error" | "failed")
            if envelope
                .get("certificate")
                .map_or(true, serde_json::Value::is_null) =>
        {
            Ok(RevisionBootstrapEnvelope::IdentityFreeFailure)
        }
        _ => Err(ClientError::Protocol(
            "remote product session must establish its Revision before queries".to_owned(),
        )),
    }
}

/// Direct, no-relay product command transport with an owner-signed query grant.
pub struct RemoteIndexCommandTransport {
    runtime: tokio::runtime::Runtime,
    endpoint: Endpoint,
    owner_address: EndpointAddr,
    capability: RemoteIndexCapability,
    active: Option<ActiveProductSession>,
}

/// All authority attached to one accepted Iroh session is stored atomically.
struct ActiveProductSession {
    session: RemoteIndexSession,
    authenticated_peer: RemoteIndexAuthenticatedPeer,
    capability_receipt: RemoteIndexProductCapabilityReceipt,
    revision: Option<RemoteProductRevisionBinding>,
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
            active: None,
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
        let (authenticated_peer, capability_receipt) = session
            .product_coverage_authentication()
            .ok_or_else(|| {
                ClientError::Protocol(
                    "remote product session did not prove its owner-signed grant".to_owned(),
                )
            })?;
        self.active = Some(ActiveProductSession {
            session,
            authenticated_peer,
            capability_receipt,
            revision: None,
        });
        Ok(())
    }

    fn request_once(
        &mut self,
        request_id: u64,
        body: Box<[u8]>,
    ) -> Result<RemoteIndexOutcome, ClientError> {
        if self.active.is_none() {
            self.open_session()?;
        }
        let Some(active) = self.active.as_mut() else {
            return Err(ClientError::Io("remote session was not opened".to_owned()));
        };
        let request = RemoteIndexRequest { request_id, body };
        self.runtime
            .block_on(async {
                active.session.send_request(&request).await?;
                active.session.receive_response(request_id).await
            })
            .map(|response| response.outcome)
            .map_err(map_session_transport)
    }
}

impl CommandTransport for RemoteIndexCommandTransport {
    fn request(&mut self, request: CommandDto) -> Result<ReplyDto, ClientError> {
        let is_revision = matches!(&request.command, Command::Revision);
        if self.active.is_none() {
            self.open_session()?;
        }
        require_remote_product_revision(
            self.active
                .as_ref()
                .is_some_and(|active| active.revision.is_some()),
            is_revision,
        )?;
        let body = encode_command_body(&request).map_err(ClientError::Protocol)?;
        let outcome = match self.request_once(request.request_id, body.into_boxed_slice()) {
            Ok(outcome) => outcome,
            Err(error) if matches!(error, ClientError::Disconnected(_)) => {
                self.active = None;
                if !is_revision {
                    return Err(error);
                }
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
                let scope = self.capability.claims.product.as_ref().ok_or_else(|| {
                    ClientError::Protocol(
                        "remote product transport lost its signed product scope".to_owned(),
                    )
                })?;
                let active = self.active.as_mut().ok_or_else(|| {
                    ClientError::Protocol("remote product session was not opened".to_owned())
                })?;
                if is_revision
                    && classify_revision_bootstrap(&body)?
                        == RevisionBootstrapEnvelope::IdentityFreeFailure
                {
                    let reply = backend_library::decode_reply_body(&body)
                        .map_err(ClientError::Protocol)?;
                    return admit_reply(&request, reply);
                }
                let verifier = if is_revision {
                    RemoteProductCoverageVerifier::bootstrap_revision(
                        &active.authenticated_peer,
                        &active.capability_receipt,
                    )
                } else {
                    let expected = active.revision.ok_or_else(|| {
                        ClientError::Protocol(
                            "remote product query has no admitted revision".to_owned(),
                        )
                    })?;
                    RemoteProductCoverageVerifier::pinned_query(
                        &active.authenticated_peer,
                        &active.capability_receipt,
                        expected,
                    )
                }
                .map_err(|error| ClientError::Protocol(error.to_owned()))?;
                let reply = backend_library::decode_reply_body_with_verifier(&body, &verifier)
                    .map_err(ClientError::Protocol)?;
                if is_revision {
                    let CommandReply::Revision(revision) = &reply.reply else {
                        return Err(ClientError::Protocol(
                            "remote product session must establish its Revision before queries"
                                .to_owned(),
                        ));
                    };
                    let producer = verifier.observed_proof().ok_or_else(|| {
                        ClientError::Protocol(
                            "remote revision omitted its producer observation".to_owned(),
                        )
                    })?;
                    let binding = bind_remote_product_revision(scope.view_root, *revision, producer)?;
                    if active.revision.is_some_and(|previous| previous != binding) {
                        return Err(ClientError::StaleRemoteCapability);
                    }
                    let admitted = admit_reply(&request, reply)?;
                    active.revision = Some(binding);
                    return Ok(admitted);
                }
                admit_reply(&request, reply)
            }
            RemoteIndexOutcome::StaleProductRoot { expected, observed } => {
                Err(ClientError::StaleRemoteRoot { expected, observed })
            }
            RemoteIndexOutcome::StaleProductSource { .. } => {
                Err(ClientError::StaleRemoteCapability)
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
        self.active = None;
        self.open_session()
    }
}

fn validate_revision_root(expected: [u8; 32], observed: [u8; 32]) -> Result<(), ClientError> {
    if observed == expected {
        Ok(())
    } else {
        Err(ClientError::StaleRemoteRoot { expected, observed })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_engine::cluster_transport::{
        RemoteIndexCapabilityClaims, RemoteIndexCapabilityIssuer, RemoteIndexPermission,
        RemoteIndexProductScope, RemoteIndexQueryOperation, RemoteIndexResponse,
        accept_remote_index,
    };
    use backend_library::canonical::{object_version, view_key, view_state_root, view_version_preimage};
    use backend_library::{
        AuthorityScopeClaim, Basis, CoverageCapability, Cursor, Freshness, Frontier, Query,
        QueryLimit, ViewRoot, ViewSnapshot, WireCertificate, WireClaim, WireSchema,
        admit_complete_scope, admit_producer_observation, encode_id,
    };
    use std::net::UdpSocket;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;
    use tokio::sync::oneshot;

    const TEST_IO_TIMEOUT: Duration = Duration::from_secs(10);
    const TEST_SERVER_LIFETIME: Duration = Duration::from_secs(30);
    const TEST_READY_TIMEOUT: Duration = Duration::from_secs(30);

    struct ProductTestServer {
        shutdown: Option<oneshot::Sender<()>>,
        worker: Option<thread::JoinHandle<()>>,
    }

    impl ProductTestServer {
        fn stop(&mut self) {
            if let Some(shutdown) = self.shutdown.take() {
                let _ = shutdown.send(());
            }
        }

        fn finish(mut self) {
            self.stop();
            if let Some(worker) = self.worker.take() {
                worker.join().expect("join bounded remote-index test server");
            }
        }
    }

    impl Drop for ProductTestServer {
        fn drop(&mut self) {
            self.stop();
            if let Some(worker) = self.worker.take() {
                let result = worker.join();
                if !thread::panicking() {
                    assert!(result.is_ok(), "remote-index test server panicked");
                }
            }
        }
    }

    fn observation(scope: ScopeRoot) -> UntrustedProducerObservation {
        UntrustedProducerObservation::new([1; 32], scope, [2; 32], b"owner-proof".to_vec())
    }

    #[test]
    fn remote_product_proof_must_match_the_revision_source_and_producer() {
        let observed = observation(ScopeRoot::from_bytes([3; 32]));
        let proof = remote_product_producer_proof(&observed).expect("bounded producer proof");
        let expected = RemoteProductRevisionBinding {
            root: [7; 32],
            source: [3; 32],
            producer: proof,
        };
        assert_eq!(
            validate_remote_product_observation(&observed, Some(expected)),
            Ok(proof)
        );
        assert!(validate_remote_product_observation(
            &observation(ScopeRoot::from_bytes([4; 32])),
            Some(expected),
        )
        .is_err());
        let wrong_context = UntrustedProducerObservation::new(
            [1; 32],
            ScopeRoot::from_bytes([3; 32]),
            [9; 32],
            b"owner-proof".to_vec(),
        );
        assert!(validate_remote_product_observation(&wrong_context, Some(expected)).is_err());
        let wrong_evidence = UntrustedProducerObservation::new(
            [1; 32],
            ScopeRoot::from_bytes([3; 32]),
            [2; 32],
            b"changed-owner-proof".to_vec(),
        );
        assert!(validate_remote_product_observation(&wrong_evidence, Some(expected)).is_err());
    }

    #[test]
    fn remote_product_verifier_rejects_unbounded_or_ambiguous_observations() {
        assert!(remote_product_producer_proof(&UntrustedProducerObservation::new(
            [1; 32],
            ScopeRoot::from_bytes([3; 32]),
            [2; 32],
            Vec::new(),
        ))
        .is_err());
        assert!(remote_product_producer_proof(&UntrustedProducerObservation::new(
            [1; 32],
            ScopeRoot::from_bytes([0; 32]),
            [2; 32],
            b"owner-proof".to_vec(),
        ))
        .is_err());
        assert!(remote_product_producer_proof(&UntrustedProducerObservation::new(
            [0; 32],
            ScopeRoot::from_bytes([3; 32]),
            [2; 32],
            b"owner-proof".to_vec(),
        ))
        .is_err());
        assert!(remote_product_producer_proof(&UntrustedProducerObservation::new(
            [1; 32],
            ScopeRoot::from_bytes([3; 32]),
            [2; 32],
            vec![0; backend_library::MAX_COVERAGE_EVIDENCE + 1],
        ))
        .is_err());
    }

    #[test]
    fn non_revision_commands_cannot_bootstrap_a_remote_product_session() {
        assert!(require_remote_product_revision(false, true).is_ok());
        assert!(require_remote_product_revision(true, false).is_ok());
        assert!(require_remote_product_revision(false, false).is_err());
    }

    #[test]
    fn non_revision_reply_is_rejected_before_coverage_admission() {
        assert!(classify_revision_bootstrap(
            br#"{"reply":{"kind":"search"}}"#
        )
        .is_err());
        assert_eq!(classify_revision_bootstrap(
            br#"{"reply":{"kind":"revision"}}"#
        ), Ok(RevisionBootstrapEnvelope::Revision));
    }

    #[test]
    fn identity_free_owner_failure_is_not_misreported_as_a_revision_protocol_error() {
        for kind in ["error", "failed"] {
            let body = serde_json::json!({
                "version": backend_library::DTO_VERSION,
                "request_id": 1,
                "reply": if kind == "error" {
                    serde_json::json!({ "kind": "error", "data": { "message": "owner unavailable" } })
                } else {
                    serde_json::json!({ "kind": "failed", "data": { "kind": "not_found", "data": {} } })
                },
            });
            assert_eq!(
                classify_revision_bootstrap(&serde_json::to_vec(&body).expect("error body")),
                Ok(RevisionBootstrapEnvelope::IdentityFreeFailure),
            );
        }
        let certified_error = serde_json::json!({
            "version": backend_library::DTO_VERSION,
            "request_id": 1,
            "reply": { "kind": "error", "data": { "message": "owner unavailable" } },
            "certificate": {},
        });
        assert!(classify_revision_bootstrap(
            &serde_json::to_vec(&certified_error).expect("certified error body")
        )
        .is_err());
    }

    #[test]
    fn remote_revision_must_match_the_signed_view_root() {
        assert!(validate_revision_root([7; 32], [7; 32]).is_ok());
        assert_eq!(
            validate_revision_root([7; 32], [8; 32]),
            Err(ClientError::StaleRemoteRoot {
                expected: [7; 32],
                observed: [8; 32],
            })
        );
    }

    struct OwnerCoverageProof {
        source: [u8; 32],
        producer: [u8; 32],
        context: [u8; 32],
        evidence: Vec<u8>,
    }

    impl ProducerObservationVerifier for OwnerCoverageProof {
        type Error = &'static str;

        fn verify(
            &self,
            observation: &UntrustedProducerObservation,
        ) -> Result<ProducerObservationClaims, Self::Error> {
            if observation.scope_root().as_bytes() != &self.source
                || observation.producer_identity() != self.producer
                || observation.context() != self.context
                || observation.evidence() != self.evidence
            {
                return Err("unexpected owner producer proof");
            }
            Ok(ProducerObservationClaims::new(
                observation.producer_identity(),
                observation.scope_root(),
                observation.context(),
                *blake3::hash(observation.evidence()).as_bytes(),
            ))
        }
    }

    fn owner_proof(source: [u8; 32], context: [u8; 32]) -> OwnerCoverageProof {
        OwnerCoverageProof {
            source,
            producer: [0x41; 32],
            context,
            evidence: b"authenticated-owner-source-proof".to_vec(),
        }
    }

    fn owner_view() -> ViewRoot {
        let source = object_version(b"source");
        let source_bytes = source.to_bytes();
        let proof = owner_proof(source_bytes, [0x42; 32]);
        let observation = UntrustedProducerObservation::new(
            proof.producer,
            ScopeRoot::from_bytes(source_bytes),
            proof.context,
            proof.evidence.clone(),
        );
        let admitted = admit_producer_observation(observation, &proof).expect("owner proof");
        let complete = admit_complete_scope(
            AuthorityScopeClaim::from_object_version(source),
            admitted,
        )
        .expect("complete source coverage");
        let coverage = CoverageCapability::from_authorized_with_evidence(
            complete,
            proof.evidence,
        )
        .expect("coverage evidence");
        let source_root = view_state_root(&[]);
        let basis = Basis::new(source_root, source);
        ViewRoot::empty_checked(
            view_key(b"remote-product-test-view"),
            basis,
            Frontier::new(basis.branch, basis.log, basis.schema, source_root, 0),
            coverage,
        )
        .expect("owner view root")
    }

    fn owner_certificate(
        root: &ViewRoot,
        context: [u8; 32],
        evidence: &[u8],
    ) -> WireCertificate {
        let source = root.basis().object;
        let source_bytes = source.to_bytes();
        let producer = [0x41; 32];
        let cursor = Cursor::for_view_root(root);
        WireCertificate::new()
            .with_claim(WireClaim::KeyBytes {
                schema: WireSchema::ViewRecipe,
                id: encode_id(root.recipe().as_bytes()),
                value: b"remote-product-test-view".to_vec().into_boxed_slice(),
            })
            .with_claim(WireClaim::Version {
                schema: WireSchema::ViewVersion,
                id: encode_id(root.version().as_bytes()),
                value: view_version_preimage(
                    root.recipe(),
                    root.basis(),
                    root.frontier(),
                    root.root(),
                    root.coverage(),
                )
                .into_boxed_slice(),
            })
            .with_claim(WireClaim::Root {
                schema: WireSchema::ViewRelation,
                id: encode_id(root.root().as_bytes()),
                canonical: root
                    .canonical_relation_bytes()
                    .expect("canonical view bytes")
                    .into_boxed_slice(),
            })
            .with_claim(WireClaim::RootCommitment {
                schema: WireSchema::ViewRelation,
                id: encode_id(root.root().as_bytes()),
            })
            .with_claim(WireClaim::Version {
                schema: WireSchema::Object,
                id: encode_id(&source_bytes),
                value: b"source".to_vec().into_boxed_slice(),
            })
            .with_claim(WireClaim::Coverage {
                scope: encode_id(&source_bytes),
                observed: encode_id(&source_bytes),
                producer: encode_id(&producer),
                context: encode_id(&context),
                evidence: evidence.to_vec().into_boxed_slice(),
            })
            .with_claim(WireClaim::Key {
                schema: WireSchema::Branch,
                id: encode_id(root.basis().branch.as_bytes()),
                value: "main".to_owned(),
            })
            .with_claim(WireClaim::Key {
                schema: WireSchema::Log,
                id: encode_id(root.basis().log.as_bytes()),
                value: "library".to_owned(),
            })
            .with_claim(WireClaim::Cursor {
                recipe: encode_id(cursor.recipe().as_bytes()),
                version: encode_id(cursor.version().as_bytes()),
                branch: encode_id(cursor.branch().as_bytes()),
                log: encode_id(cursor.log().as_bytes()),
                schema: cursor.schema(),
                root: encode_id(cursor.root().as_bytes()),
                sequence: cursor.sequence(),
            })
    }

    fn product_capability(
        owner: &SecretKey,
        client: backend_engine::cluster_transport::EndpointId,
        root: [u8; 32],
    ) -> RemoteIndexCapability {
        let now = remote_index_now().expect("clock");
        RemoteIndexCapabilityIssuer::new(owner.clone())
            .issue(
                RemoteIndexCapabilityClaims {
                    version: 2,
                    server: owner.public(),
                    client,
                    grant_id: [0x55; 16],
                    issued_at_unix_ms: now,
                    expires_at_unix_ms: now + 60_000,
                    request_budget: 16,
                    byte_budget: 64 * 1024,
                    permissions: vec![RemoteIndexPermission::ProductRead],
                    product: Some(RemoteIndexProductScope {
                        view_root: root,
                        operations: vec![RemoteIndexQueryOperation::Search],
                        index_search_snapshot: None,
                    }),
                    semantic: None,
                },
                now,
            )
            .expect("signed product capability")
    }

    fn unused_loopback_addresses() -> (SocketAddr, SocketAddr) {
        let first = UdpSocket::bind("127.0.0.1:0").expect("reserve first loopback port");
        let second = UdpSocket::bind("127.0.0.1:0").expect("reserve second loopback port");
        (
            first.local_addr().expect("first loopback address"),
            second.local_addr().expect("second loopback address"),
        )
    }

    fn view_reply(
        request_id: u64,
        root: ViewRoot,
        context: [u8; 32],
        evidence: Vec<u8>,
        revision: bool,
    ) -> Vec<u8> {
        let cursor = Cursor::for_view_root(&root);
        let reply = if revision {
            CommandReply::Revision(RevisionReceipt::new(root.root(), cursor, root.basis().object))
        } else {
            CommandReply::Search(ViewSnapshot {
                root: root.clone(),
                freshness: Freshness::Current,
                next: None,
                graph_relations: None,
                rich_graph: None,
            })
        };
        serde_json::to_vec(
            &ReplyDto::new(request_id, reply)
                .with_certificate(owner_certificate(&root, context, &evidence)),
        )
        .expect("encode owner reply")
    }

    #[test]
    fn remote_product_admission_pins_one_authenticated_session_across_real_wire_replies() {
        let root = owner_view();
        let query_root = root.root();
        let root_id = query_root.to_bytes();
        let context = [0x42; 32];
        let evidence = b"authenticated-owner-source-proof".to_vec();
        let owner_secret = SecretKey::generate();
        let signing_secret = owner_secret.clone();
        let client_secret = SecretKey::generate();
        let impostor_client_secret = SecretKey::generate();
        let wrong_peer_client_secret = SecretKey::generate();
        let error_client_secret = SecretKey::generate();
        let (owner_address, impostor_address) = unused_loopback_addresses();
        let (ready_sender, ready_receiver) = mpsc::channel();
        let (shutdown_sender, mut shutdown_receiver) = oneshot::channel();
        let owner_thread = thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("owner runtime");
            runtime.block_on(async move {
                let owner = tokio::time::timeout(
                    TEST_IO_TIMEOUT,
                    bind_direct(owner_secret.clone(), owner_address),
                )
                .await
                .expect("owner endpoint bind timed out")
                .expect("owner endpoint");
                let impostor = tokio::time::timeout(
                    TEST_IO_TIMEOUT,
                    bind_direct(SecretKey::generate(), impostor_address),
                )
                .await
                .expect("impostor endpoint bind timed out")
                .expect("impostor endpoint");
                let impostor_accept = tokio::spawn(async move {
                    while let Some(incoming) = impostor.accept().await {
                        if let Ok(connecting) = incoming.accept() {
                            let _ = connecting.await;
                        }
                    }
                });
                ready_sender
                    .send(owner.id())
                    .expect("send owner address");
                let serve_sessions = async {
                    for session_index in 0..3 {
                        let incoming = tokio::select! {
                            _ = &mut shutdown_receiver => return false,
                            result = tokio::time::timeout(TEST_IO_TIMEOUT, owner.accept()) => {
                                result.expect("owner accept timed out")
                                    .expect("owner accepts connection")
                            }
                        };
                        let connecting = incoming
                            .accept()
                            .expect("accept remote product connection");
                        let connection = tokio::select! {
                            _ = &mut shutdown_receiver => return false,
                            result = tokio::time::timeout(TEST_IO_TIMEOUT, connecting) => {
                                result.expect("owner handshake timed out")
                                    .expect("complete owner connection")
                            }
                        };
                        let mut session = tokio::select! {
                            _ = &mut shutdown_receiver => return false,
                            result = tokio::time::timeout(
                                TEST_IO_TIMEOUT,
                                accept_remote_index(connection, owner.id()),
                            ) => {
                                result.expect("remote-index admission timed out")
                                    .expect("admit signed product grant")
                            }
                        };
                        let request_count = match session_index {
                            0 => 3,
                            1 => 2,
                            _ => 1,
                        };
                        for _ in 0..request_count {
                            let request = tokio::select! {
                                _ = &mut shutdown_receiver => return false,
                                result = tokio::time::timeout(
                                    TEST_IO_TIMEOUT,
                                    session.receive_request(),
                                ) => {
                                    result.expect("product request timed out")
                                        .expect("product request")
                                }
                            };
                            let command = backend_library::decode_command_body(&request.body)
                                .expect("decode command request");
                            let body = if session_index == 2 {
                                assert!(matches!(command.command, Command::Revision));
                                serde_json::to_vec(&ReplyDto::error(
                                    request.request_id,
                                    "owner unavailable",
                                ))
                                .expect("encode identity-free owner error")
                            } else {
                                let (reply, reply_context, reply_evidence) =
                                    match command.command {
                                        Command::Revision => (true, context, evidence.clone()),
                                        Command::Search(_) if request.request_id == 3 => {
                                            (false, [0x99; 32], evidence.clone())
                                        }
                                        Command::Search(_) => {
                                            (false, context, evidence.clone())
                                        }
                                        other => panic!(
                                            "unexpected product command: {other:?}"
                                        ),
                                    };
                                view_reply(
                                    request.request_id,
                                    root.clone(),
                                    reply_context,
                                    reply_evidence,
                                    reply,
                                )
                            };
                            tokio::select! {
                                _ = &mut shutdown_receiver => return false,
                                result = tokio::time::timeout(
                                    TEST_IO_TIMEOUT,
                                    session.send_response(&RemoteIndexResponse {
                                        request_id: request.request_id,
                                        outcome: RemoteIndexOutcome::Payload(body.into_boxed_slice()),
                                    }),
                                ) => {
                                    result.expect("owner reply send timed out")
                                        .expect("send owner product reply");
                                }
                            }
                        }
                    }
                    true
                };
                let completed = tokio::time::timeout(TEST_SERVER_LIFETIME, serve_sessions)
                    .await
                    .unwrap_or(false);
                impostor_accept.abort();
                let _ = impostor_accept.await;
                assert!(completed, "remote-index test server stopped before all sessions");
            });
        });
        let mut test_server = ProductTestServer {
            shutdown: Some(shutdown_sender),
            worker: Some(owner_thread),
        };

        let owner_id = ready_receiver
            .recv_timeout(TEST_READY_TIMEOUT)
            .expect("owner endpoint became ready before timeout");
        let capability = product_capability(&signing_secret, client_secret.public(), root_id);
        let mut transport = RemoteIndexCommandTransport::connect(
            client_secret.clone(),
            owner_id,
            owner_address,
            capability.clone(),
        )
        .expect("open owner-signed product transport");
        let wrong_peer_grant = product_capability(
            &signing_secret,
            wrong_peer_client_secret.public(),
            root_id,
        );
        let mut wrong_peer_transport = RemoteIndexCommandTransport::connect(
            wrong_peer_client_secret,
            owner_id,
            impostor_address,
            wrong_peer_grant,
        )
        .expect("locally valid grant for the wrong network peer");
        assert!(wrong_peer_transport
            .request(CommandDto::new(1, Command::Revision))
            .is_err());
        let revision = transport
            .request(CommandDto::new(1, Command::Revision))
            .expect("admit owner revision reply");
        assert!(matches!(revision.reply, CommandReply::Revision(_)));
        let query = || {
            Command::Search(Query::new(
                "needle",
                query_root,
                QueryLimit::new(10).expect("valid query limit"),
            ))
        };
        assert!(matches!(
            transport
                .request(CommandDto::new(2, query()))
                .expect("admit source-pinned search reply")
                .reply,
            CommandReply::Search(_)
        ));
        assert!(transport.request(CommandDto::new(3, query())).is_err());

        transport.reconnect().expect("open a fresh accepted session");
        assert!(transport.request(CommandDto::new(4, query())).is_err());
        assert!(matches!(
            transport
                .request(CommandDto::new(4, Command::Revision))
                .expect("rebootstrap fresh session revision")
                .reply,
            CommandReply::Revision(_)
        ));
        assert!(matches!(
            transport
                .request(CommandDto::new(5, query()))
                .expect("admit query after fresh revision")
                .reply,
            CommandReply::Search(_)
        ));

        let wrong_client_grant = product_capability(
            &signing_secret,
            impostor_client_secret.public(),
            root_id,
        );
        assert!(RemoteIndexCommandTransport::connect(
            client_secret.clone(),
            owner_id,
            owner_address,
            wrong_client_grant,
        )
        .is_err());
        let mut forged_grant = capability.clone();
        forged_grant.claims.grant_id[0] ^= 0xff;
        assert!(RemoteIndexCommandTransport::connect(
            client_secret,
            owner_id,
            owner_address,
            forged_grant,
        )
        .is_err());

        let error_capability = product_capability(
            &signing_secret,
            error_client_secret.public(),
            root_id,
        );
        let mut error_transport = RemoteIndexCommandTransport::connect(
            error_client_secret,
            owner_id,
            owner_address,
            error_capability,
        )
        .expect("open second owner's product session");
        assert!(error_transport.request(CommandDto::new(1, query())).is_err());
        assert!(matches!(
            error_transport
                .request(CommandDto::new(1, Command::Revision))
                .expect("preserve identity-free owner failure")
                .reply,
            CommandReply::Error(message) if message == "owner unavailable"
        ));

        test_server.finish();
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

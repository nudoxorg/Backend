//! Worker listener integration tests.

#![allow(clippy::expect_used, clippy::panic)]

use super::*;
use crate::protocol::{read_message, write_message};
use crate::service::{NoJobAdmission, WorkerService};
use backend_engine::{
    CapabilityManifest, NoAttestationSigner, PureRecipeExecutor, RecipeId, Relation,
    ResourceEnvelope, SchemaDescriptor, TransportMessage, VersionRange, WireIdentity,
    WorkerCapabilities, WorkerError,
};
use std::io::{Cursor, Read, Write};

#[derive(Debug)]
struct DummyExecutor;

#[derive(Debug)]
struct TestRelation;

impl Relation for TestRelation {
    const DOMAIN: u8 = 0x93;
    const TYPE: u16 = 1;
    type Key = u64;
    type Value = u64;

    fn encode_key(value: &Self::Key, output: &mut Vec<u8>) {
        output.extend_from_slice(&value.to_be_bytes());
    }

    fn encode_value(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(&value.to_be_bytes());
    }
}

impl PureRecipeExecutor for DummyExecutor {
    fn recipe_id(&self) -> RecipeId {
        RecipeId::from_value(b"worker-listener-test")
    }

    fn execute(
        &self,
        _invocation: &backend_engine::AdmittedInvocation,
        _context: &mut backend_engine::PureWorkContext<'_>,
    ) -> Result<backend_engine::PreparedOutput, WorkerError> {
        Err(WorkerError::Capability)
    }
}

fn limits() -> crate::protocol::WorkerLimits {
    crate::protocol::WorkerLimits {
        transport: backend_engine::TransportLimits {
            max_frame: 4096,
            max_chunk: 4096,
            ..backend_engine::TransportLimits::default()
        },
        max_frames_per_connection: 2,
        io_timeout: Duration::from_millis(100),
    }
}

fn manifest() -> CapabilityManifest {
    let recipe = RecipeId::from_value(b"worker-listener-test");
    CapabilityManifest {
        protocol: VersionRange { min: 1, max: 1 },
        schemas: vec![SchemaDescriptor {
            domain: 1,
            type_id: 1,
            versions: VersionRange { min: 1, max: 1 },
        }],
        recipes: vec![backend_engine::RecipeCapability {
            recipe: WireIdentity::from_typed(&recipe),
            versions: VersionRange { min: 1, max: 1 },
        }],
        max_object: 1024,
        max_chunk: 512,
        max_frame: 4096,
        max_ranges: 4,
        max_resources: listener_resource_limit(),
    }
}

fn listener_resource_limit() -> ResourceEnvelope {
    ResourceEnvelope {
        cpu_millis: 100,
        memory_bytes: 1024,
        network_bytes: 1024,
        storage_bytes: 1024,
        output_bytes: 1024,
        processes: 1,
        wall_millis: 100,
    }
}

fn path(label: &str) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    PathBuf::from("/tmp").join(format!("bw-{label}-{nonce:x}.sock"))
}

#[derive(Debug)]
struct RejectPeers;

impl PeerPolicy for RejectPeers {
    fn authorize(&self, _stream: &std::os::unix::net::UnixStream) -> Result<(), PeerPolicyError> {
        Err(PeerPolicyError::Rejected)
    }
}

#[test]
fn worker_listener_rejects_a_peer_before_recipe_admission() {
    let socket = path("peer");
    let config = WorkerListenerConfig {
        path: socket.clone(),
        limits: limits(),
    };
    let capabilities = WorkerCapabilities {
        recipes: vec![RecipeId::from_value(b"worker-listener-test")],
        max_scope: 1,
        max_resources: ResourceEnvelope {
            cpu_millis: 100,
            memory_bytes: 1024,
            network_bytes: 1024,
            storage_bytes: 1024,
            output_bytes: 1024,
            processes: 1,
            wall_millis: 100,
        },
    };
    let worker = WorkerService::new(capabilities, DummyExecutor, manifest(), config.limits)
        .unwrap_or_else(|error| panic!("worker: {error}"));
    let mut listener = UnixWorkerListener::<
        DummyExecutor,
        NoAttestationSigner,
        TestRelation,
        NoJobAdmission,
    >::bind_with_peer_policy(
        worker, NoJobAdmission, config, Arc::new(RejectPeers)
    )
    .unwrap_or_else(|error| panic!("bind: {error}"));
    let client_socket = socket.clone();
    let client = std::thread::spawn(move || {
        let _ = std::os::unix::net::UnixStream::connect(client_socket);
    });
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && listener.report().failures == 0 {
        let _ = listener
            .run_once()
            .unwrap_or_else(|error| panic!("run once: {error}"));
        std::thread::sleep(Duration::from_millis(1));
    }
    client
        .join()
        .unwrap_or_else(|_| panic!("client thread panicked"));
    assert_eq!(listener.report().connections, 0);
    assert_eq!(listener.report().failures, 1);
    drop(listener);
}

#[test]
fn worker_listener_sets_private_mode_and_handles_canonical_capabilities() {
    let socket = path("capabilities");
    let config = WorkerListenerConfig {
        path: socket.clone(),
        limits: limits(),
    };
    let capabilities = WorkerCapabilities {
        recipes: vec![RecipeId::from_value(b"worker-listener-test")],
        max_scope: 1,
        max_resources: ResourceEnvelope {
            cpu_millis: 100,
            memory_bytes: 1024,
            network_bytes: 1024,
            storage_bytes: 1024,
            output_bytes: 1024,
            processes: 1,
            wall_millis: 100,
        },
    };
    let worker = WorkerService::new(capabilities, DummyExecutor, manifest(), config.limits)
        .unwrap_or_else(|error| panic!("worker: {error}"));
    let telemetry = backend_engine::Telemetry::enabled();
    let mut listener = UnixWorkerListener::<
        DummyExecutor,
        NoAttestationSigner,
        TestRelation,
        NoJobAdmission,
    >::bind(worker, NoJobAdmission, config)
    .unwrap_or_else(|error| panic!("bind: {error}"))
    .with_telemetry(telemetry.clone());
    let client_socket = socket.clone();
    let client = std::thread::spawn(move || {
        let mut stream = std::os::unix::net::UnixStream::connect(client_socket)
            .unwrap_or_else(|error| panic!("connect: {error}"));
        let message = TransportMessage::Capabilities(manifest());
        let request = crate::protocol::frame(&message, limits())
            .unwrap_or_else(|error| panic!("frame: {error}"));
        stream
            .write_all(&request)
            .unwrap_or_else(|error| panic!("write: {error}"));
        let mut header = [0_u8; 4];
        stream
            .read_exact(&mut header)
            .unwrap_or_else(|error| panic!("read header: {error}"));
        let length = u32::from_be_bytes(header) as usize;
        let mut response = Vec::with_capacity(length.saturating_add(4));
        response.extend_from_slice(&header);
        let mut body = vec![0_u8; length];
        stream
            .read_exact(&mut body)
            .unwrap_or_else(|error| panic!("read body: {error}"));
        response.extend_from_slice(&body);
        response
    });
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && listener.report().frames == 0 {
        let _ = listener
            .run_once()
            .unwrap_or_else(|error| panic!("run once: {error}"));
        std::thread::sleep(Duration::from_millis(1));
    }
    listener.shutdown();
    let response = client
        .join()
        .unwrap_or_else(|_| panic!("client thread panicked"));
    let decoded = read_message(&mut Cursor::new(response), limits())
        .unwrap_or_else(|error| panic!("decode response: {error}"));
    assert!(matches!(decoded, TransportMessage::Capabilities(_)));
    assert_eq!(listener.report().frames, 1);
    let transport = telemetry
        .snapshot()
        .family(backend_engine::MetricFamily::Transport);
    assert_eq!(transport.completed, 1);
    assert_eq!(transport.failed, 0);
    assert_eq!(transport.units, 1);
    drop(listener);
    assert!(!socket.exists());
}

#[test]
fn tcp_listener_authenticates_capabilities_and_rejects_wrong_key() {
    let limits = limits();
    let secret = [0x29_u8; 32];
    let config = TcpWorkerListenerConfig {
        address: "127.0.0.1:0".parse().expect("loopback address"),
        limits,
        authority: backend_engine::TcpAuthority::new(secret),
        exposure: TcpExposure::LoopbackOnly,
    };
    let capabilities = WorkerCapabilities {
        recipes: vec![RecipeId::from_value(b"worker-listener-test")],
        max_scope: 1,
        max_resources: ResourceEnvelope {
            cpu_millis: 100,
            memory_bytes: 1024,
            network_bytes: 1024,
            storage_bytes: 1024,
            output_bytes: 1024,
            processes: 1,
            wall_millis: 100,
        },
    };
    let worker = WorkerService::new(capabilities, DummyExecutor, manifest(), limits)
        .unwrap_or_else(|error| panic!("worker: {error}"));
    let mut listener = TcpWorkerListener::<
        DummyExecutor,
        NoAttestationSigner,
        TestRelation,
        NoJobAdmission,
    >::bind(worker, NoJobAdmission, config)
    .unwrap_or_else(|error| panic!("bind: {error}"));
    let address = listener.local_addr().expect("listener address");
    let client = std::thread::spawn(move || {
        let stream = TcpStream::connect(address).expect("connect");
        configure_tcp_stream(&stream, Duration::from_secs(1)).expect("timeouts");
        let mut stream = backend_engine::TcpAuthority::new(secret)
            .client_handshake_authenticated(stream, limits.transport.max_frame + 4)
            .expect("authority handshake");
        write_message(
            &mut stream,
            &TransportMessage::Capabilities(manifest()),
            limits,
        )
        .expect("capability request");
        read_message(&mut stream, limits).expect("capability response")
    });
    let deadline = Instant::now() + Duration::from_secs(2);
    while listener.report().frames == 0 && Instant::now() < deadline {
        let _ = listener.run_once().expect("run once");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(matches!(
        client.join().expect("client thread"),
        TransportMessage::Capabilities(_)
    ));
    assert_eq!(listener.report().frames, 1);

    let mut wrong = TcpStream::connect(address).expect("wrong-key connect");
    configure_tcp_stream(&wrong, Duration::from_secs(1)).expect("wrong-key timeouts");
    let wrong_client = std::thread::spawn(move || {
        backend_engine::TcpAuthority::new([0x2a_u8; 32]).client_handshake(&mut wrong)
    });
    let deadline = Instant::now() + Duration::from_secs(2);
    while listener.report().failures == 0 && Instant::now() < deadline {
        let _ = listener.run_once().expect("run wrong-key once");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        wrong_client.join().expect("wrong-key client thread"),
        Err(backend_engine::TcpHandshakeError::AuthenticationFailed)
    );
    assert_eq!(listener.report().failures, 1);

    let reconnect = std::thread::spawn(move || {
        let stream = TcpStream::connect(address).expect("reconnect");
        configure_tcp_stream(&stream, Duration::from_secs(1)).expect("reconnect timeouts");
        let mut stream = backend_engine::TcpAuthority::new(secret)
            .client_handshake_authenticated(stream, limits.transport.max_frame + 4)
            .expect("reconnect authority handshake");
        write_message(
            &mut stream,
            &TransportMessage::Capabilities(manifest()),
            limits,
        )
        .expect("reconnect capability request");
        read_message(&mut stream, limits).expect("reconnect capability response")
    });
    let deadline = Instant::now() + Duration::from_secs(2);
    while listener.report().frames < 2 && Instant::now() < deadline {
        let _ = listener.run_once().expect("run reconnect once");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(matches!(
        reconnect.join().expect("reconnect client thread"),
        TransportMessage::Capabilities(_)
    ));
    assert_eq!(listener.report().frames, 2);
    listener.shutdown();
}

#[test]
fn routable_tcp_requires_explicit_outer_confidentiality() {
    let config = TcpWorkerListenerConfig {
        address: "0.0.0.0:0".parse().expect("routable address"),
        limits: limits(),
        authority: backend_engine::TcpAuthority::new([7_u8; 32]),
        exposure: TcpExposure::LoopbackOnly,
    };
    assert!(matches!(
        config.validate(),
        Err(WorkerListenerError::ConfidentialityRequired)
    ));
}

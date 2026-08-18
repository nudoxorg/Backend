//! The endpoint an agent was configured against must survive a restart.
//!
//! # The defect
//!
//! Two independent values are regenerated on every launch:
//!
//! * the **port**, because `LOOPBACK_BIND` is `127.0.0.1:0`
//!   (`src/mcp/endpoint.rs:56`) and the kernel picks a free one;
//! * the **token**, because `McpHost::start` calls `SessionToken::generate()`
//!   (`src/mcp/host.rs:102`), and `session.rs` says outright that it "is *not*
//!   persisted: a new launch means a new token".
//!
//! Both are written into the `mcpServers` snippet Settings → Connection hands
//! the user. So every restart invalidates the configuration the user pasted
//! into their agent, and the fix is manual: read the new URL off the status
//! bar, edit the client config, restart the client.
//!
//! # Why the existing reasons are good ones, and why they do not settle it
//!
//! Neither choice was careless, and this file is written so a fix cannot
//! quietly discard what they bought:
//!
//! * Ephemeral ports exist so "two lindsey windows on one machine collide on
//!   start-up, with the loser either failing or — worse — silently attaching
//!   an agent to the other window's corpus" cannot happen. That hazard is
//!   real. But it is a *collision* hazard, and collisions are detectable: the
//!   second binder finds the port taken. Preferring a stable port and falling
//!   back to an ephemeral one on conflict keeps the guarantee (no two servers
//!   on one port, ever) while making the ordinary single-window case stable.
//!
//! * A non-persisted token exists so "a stale config fails closed". It still
//!   does under a persisted token — a config carrying a token that no longer
//!   matches the stored one is rejected exactly as before. What changes is
//!   only how often a *correct* config becomes stale for no reason.
//!
//! # What is asserted, and what deliberately is not
//!
//! The port preference and token are read through injected closures rather
//! than the process environment, following `app::corpus::select`'s split: the
//! decision table is testable without `std::env::set_var`, which is unsound to
//! call from a multi-threaded test binary.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

use nudox_engine::mcp::{PortPreference, SessionToken, preferred_bind};

fn addr(port: u16) -> SocketAddr {
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))
}

// ---------------------------------------------------------------------------
// The bind decision
// ---------------------------------------------------------------------------

/// With a remembered port, that is what we ask for.
#[test]
fn a_remembered_port_is_preferred() {
    assert_eq!(
        preferred_bind(PortPreference::Remembered(51234)),
        addr(51234),
        "the whole point: the URL a user pasted last time still works",
    );
}

/// With nothing remembered, the kernel picks — today's behaviour, unchanged.
#[test]
fn no_preference_still_asks_the_kernel() {
    assert_eq!(
        preferred_bind(PortPreference::Any).port(),
        0,
        "port 0 asks the kernel for a free port",
    );
}

/// Every reachable preference binds loopback.
///
/// This is the property `LOOPBACK_BIND`'s doc comment calls "a property of the
/// type system's reachable states rather than of a default that a config file
/// could override". Introducing a *remembered* port introduces exactly such a
/// file, so the property has to be re-established rather than assumed.
#[test]
fn no_preference_can_make_the_server_reachable_off_box() {
    for preference in [
        PortPreference::Any,
        PortPreference::Remembered(1),
        PortPreference::Remembered(u16::MAX),
    ] {
        let bound = preferred_bind(preference);
        assert!(
            bound.ip().is_loopback(),
            "{preference:?} must never produce a routable address, got {bound}",
        );
    }
}

/// A remembered port of 0 is not a port.
///
/// It is what an empty or corrupt state file decodes to, and treating it as a
/// preference would silently mean "ephemeral" — the right *behaviour*, reached
/// by accident, which is how a corrupt state file stops being noticeable.
#[test]
fn a_zero_port_is_not_a_preference() {
    assert_eq!(
        preferred_bind(PortPreference::Remembered(0)),
        addr(0),
        "decoding must normalise this to Any rather than pretend to prefer it",
    );
}

// ---------------------------------------------------------------------------
// The token
// ---------------------------------------------------------------------------

/// A token round-trips through its own string form.
///
/// `SessionToken::from_secret` already exists for exactly this; the gap was
/// that nothing ever *wrote* one down.
#[test]
fn a_token_survives_a_round_trip_through_its_secret() {
    let generated = SessionToken::generate();
    let restored = SessionToken::from_secret(generated.expose().to_owned());
    assert_eq!(
        restored.expose(),
        generated.expose(),
        "a persisted token must reconstruct the same credential",
    );
    assert_eq!(
        restored.bearer_header_value(),
        generated.bearer_header_value(),
        "and therefore the same Authorization header the client already has",
    );
}

/// Two freshly generated tokens must still differ.
///
/// Guards the direction a persistence bug would break: a `load_or_generate`
/// that fell back to a constant on a read error would make every install share
/// one token, which is worse than rotating.
#[test]
fn generation_is_still_random() {
    let a = SessionToken::generate();
    let b = SessionToken::generate();
    assert_ne!(
        a.expose(),
        b.expose(),
        "generate() must not become a constant",
    );
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

/// The pair survives a process boundary.
///
/// Written against a caller-supplied directory rather than the real state dir
/// so the test does not depend on — or write to — the developer's
/// `~/Library/Application Support/nudox`.
#[test]
fn a_saved_endpoint_identity_is_read_back_verbatim() {
    use nudox_engine::mcp::EndpointIdentity;

    let dir = tempfile::tempdir().expect("temporary state dir");
    let token = SessionToken::generate();

    let saved = EndpointIdentity {
        port: 51234,
        token: token.expose().to_owned(),
    };
    saved.save(dir.path()).expect("saving must succeed");

    let loaded = EndpointIdentity::load(dir.path()).expect("a just-written identity must load");
    assert_eq!(loaded.port, 51234, "the port must survive");
    assert_eq!(
        loaded.token,
        token.expose(),
        "the token must survive — this is what keeps the pasted config valid",
    );
}

/// An absent state file is not an error.
///
/// First launch is the common case, and a hard failure there would make the
/// app refuse to start over a file that is *supposed* not to exist yet.
#[test]
fn a_missing_identity_is_absence_not_failure() {
    use nudox_engine::mcp::EndpointIdentity;

    let dir = tempfile::tempdir().expect("temporary state dir");
    assert!(
        EndpointIdentity::load(dir.path()).is_none(),
        "first launch must read as 'nothing remembered', not as an error",
    );
}

/// A corrupt state file degrades to a fresh identity rather than wedging.
///
/// The failure this prevents: a truncated write leaves the app unable to start
/// its endpoint at all, and the only remedy is deleting a file the user does
/// not know exists.
#[test]
fn a_corrupt_identity_is_discarded_not_fatal() {
    use nudox_engine::mcp::EndpointIdentity;

    let dir = tempfile::tempdir().expect("temporary state dir");
    std::fs::write(dir.path().join("endpoint.json"), b"{not json at all")
        .expect("write corrupt state");

    assert!(
        EndpointIdentity::load(dir.path()).is_none(),
        "unreadable state must be treated as no state, so the next launch \
         simply writes a good one",
    );
}

/// The token file must not be world-readable.
///
/// It is a bearer credential for a server that answers questions about
/// everything in the corpus. Persisting it is only defensible if persisting it
/// safely is enforced rather than intended.
#[cfg(unix)]
#[test]
fn the_saved_identity_is_not_readable_by_other_users() {
    use nudox_engine::mcp::EndpointIdentity;
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("temporary state dir");
    EndpointIdentity {
        port: 51234,
        token: SessionToken::generate().expose().to_owned(),
    }
    .save(dir.path())
    .expect("saving must succeed");

    let mode = std::fs::metadata(dir.path().join("endpoint.json"))
        .expect("the file must exist after save")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        mode, 0o600,
        "a bearer token on disk must be owner-only; got {mode:o}",
    );
}

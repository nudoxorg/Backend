//! The capability handshake wire shape and its compatibility helpers, pinned.
//!
//! The handshake is the input to a client's routing policy (the "minimum work
//! when connected, standalone when not" decision), so its wire form is a
//! contract a server and an independently-built client must agree on exactly.
//! These tests pin that form and the forward-compatibility rule the `#[serde(
//! default)]` fields promise: an older peer that omits `generation`/`semantic`
//! must still decode, defaulting rather than failing.

use heart::surface::{CAPABILITIES_PATH, Capabilities, GenerationId, PROTOCOL_VERSION, SurfaceId};
use serde_json::json;

#[test]
fn capabilities_full_wire() {
    let caps = Capabilities {
        protocol: 1,
        surfaces: vec![SurfaceId::Symbols, SurfaceId::Packages, SurfaceId::Usages],
        generation: Some(GenerationId(7)),
        semantic: true,
    };
    let expected = json!({
        "protocol": 1,
        // Surfaces render as their `Surface::NAME` tokens.
        "surfaces": ["symbols", "packages", "usages"],
        // `GenerationId` is a newtype over u64: it serializes as the bare number.
        "generation": 7,
        "semantic": true,
    });
    assert_eq!(serde_json::to_value(&caps).unwrap(), expected);
    let back: Capabilities = serde_json::from_value(expected).unwrap();
    assert_eq!(back, caps);
}

#[test]
fn capabilities_optional_fields_default() {
    // A minimal node — older, or one with no vector plane and no generation
    // tracking — sends only the required fields. Forward-compat: this must
    // decode, defaulting `generation` to `None` and `semantic` to `false`,
    // never erroring. This is what lets the contract add fields without a
    // protocol bump.
    let caps: Capabilities = serde_json::from_value(json!({
        "protocol": 1,
        "surfaces": ["symbols"],
    }))
    .expect("optional fields may be omitted");
    assert_eq!(caps.generation, None);
    assert!(!caps.semantic);
    assert_eq!(caps.surfaces, vec![SurfaceId::Symbols]);
}

#[test]
fn compatibility_is_exact_protocol_match() {
    let current = Capabilities {
        protocol: PROTOCOL_VERSION,
        surfaces: SurfaceId::ALL.to_vec(),
        generation: None,
        semantic: false,
    };
    assert!(current.is_compatible(), "current protocol must be compatible");

    let future = Capabilities {
        protocol: PROTOCOL_VERSION + 1,
        ..current.clone()
    };
    assert!(
        !future.is_compatible(),
        "an unknown future protocol must not be delegated to"
    );
}

#[test]
fn serves_reports_advertised_surfaces() {
    let precise_only = Capabilities {
        protocol: PROTOCOL_VERSION,
        surfaces: vec![SurfaceId::Symbols, SurfaceId::Packages],
        generation: None,
        semantic: false,
    };
    assert!(precise_only.serves(SurfaceId::Symbols));
    assert!(precise_only.serves(SurfaceId::Packages));
    assert!(
        !precise_only.serves(SurfaceId::Usages),
        "a surface not advertised must not be reported as served"
    );
}

#[test]
fn surface_id_name_round_trips() {
    for id in SurfaceId::ALL {
        assert_eq!(
            SurfaceId::from_name(id.as_str()),
            Some(id),
            "{} must round-trip through its name",
            id.as_str()
        );
    }
    assert_eq!(SurfaceId::from_name("widgets"), None);
}

#[test]
fn capabilities_path_is_stable() {
    // Pinned so a server route and a client probe cannot silently diverge.
    assert_eq!(CAPABILITIES_PATH, "/capabilities");
}

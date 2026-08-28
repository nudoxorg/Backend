#![cfg(feature = "server")]
//! `GET /capabilities` — the server end of the capability handshake.
//!
//! The client-side decode is pinned in `heart/tests/{capabilities,remote_client}`;
//! this is the server end: the route is mounted, and its body decodes into the
//! **same** [`heart::surface::Capabilities`] type the client reads — so server
//! and client cannot advertise and expect two different shapes. It runs over the
//! full router via `tower::ServiceExt`, opt-in via `SERVER_TEST_BACKENDS` (it
//! needs an assembled server); it soft-skips when the live stack is absent.

mod server_common;

use heart::surface::{Capabilities, PROTOCOL_VERSION, SurfaceId};
use index::server::http::router::router;

#[tokio::test]
async fn capabilities_route_advertises_the_served_surfaces() {
    let Some((server, _data_directory)) =
        server_common::assembled_server("capabilities_route_advertises_the_served_surfaces").await
    else {
        return; // no live backends: soft-skip, like every other assembled-server test
    };

    let (status, body) =
        server_common::call_json(router(server), server_common::get("/capabilities"))
            .await
            .expect("GET /capabilities returns a JSON body");
    assert_eq!(status, axum::http::StatusCode::OK);

    // The load-bearing assertion: the server's response decodes into the exact
    // type the client decodes. A drift between the two would be a compile or a
    // decode error right here, not a silent mismatch in the field.
    let caps: Capabilities =
        serde_json::from_value(body).expect("body decodes as heart::surface::Capabilities");

    assert_eq!(caps.protocol, PROTOCOL_VERSION);
    assert!(caps.is_compatible(), "a node must advertise a version it speaks");
    assert!(
        caps.serves(SurfaceId::Symbols) && caps.serves(SurfaceId::Packages),
        "symbol and package search are served: {:?}",
        caps.surfaces
    );
    assert!(
        !caps.serves(SurfaceId::Usages),
        "usages still answers 501, so it must not be advertised as authoritative"
    );
}

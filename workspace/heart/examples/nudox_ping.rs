//! Smoke-exercises the connecting client against a running `nudox-serve`.
//!
//!   cargo run -p heart --features client --example nudox_ping -- http://127.0.0.1:8080
//!
//! Prints the server's readiness and runs one symbol search, proving
//! `heart::client::NudoxClient` reaches the folded `index::server` composition.

use heart::client::NudoxClient;
use heart::query::{Query, QueryMode, Target};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let base = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "http://127.0.0.1:8080".to_owned());
    let client = NudoxClient::connect(&base)?;

    let health = client.readyz().await?;
    println!(
        "readyz: ready={} degraded={:?}",
        health.ready, health.degraded
    );

    let query = Query {
        target: Target::Symbols,
        text: std::env::args()
            .nth(2)
            .unwrap_or_else(|| "serde".to_owned()),
        scope: Default::default(),
        rank: Default::default(),
        mode: QueryMode::default(),
        routing: Default::default(),
        session: None,
        at: None,
        page: Default::default(),
        query_id: None,
    };
    let hits = client.search(&query).await?;
    println!("search '{}': {} hit(s)", query.text, hits.len());
    for hit in hits.iter().take(5) {
        println!("  - {:?}", hit);
    }
    Ok(())
}

use std::{
    io::{self, Read},
    net::SocketAddr,
    path::PathBuf,
    time::Instant,
};

use backend_cluster_transport::{Capability, TransferScope, VerifiedCoverage, bind_direct};
use iroh::{EndpointAddr, EndpointId, SecretKey};
use serde::Deserialize;

#[derive(Deserialize)]
struct FetchInput {
    server_addr: EndpointAddr,
    bind_addr: SocketAddr,
    client_secret: [u8; 32],
    trusted_issuer: EndpointId,
    capabilities: Vec<Capability>,
    expected_scope: TransferScope,
    checkpoint: PathBuf,
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("cluster peer failed: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut input = Vec::new();
    io::stdin().read_to_end(&mut input)?;
    let input: FetchInput = serde_json::from_slice(&input)?;
    let endpoint =
        bind_direct(SecretKey::from_bytes(&input.client_secret), input.bind_addr).await?;
    let report_timings = std::env::var_os("BACKEND_CLUSTER_MEASURE_TIMINGS").is_some();
    let had_checkpoint = input.checkpoint.exists();
    let open_started = Instant::now();
    let first = input
        .capabilities
        .first()
        .ok_or("at least one range capability is required")?;
    let mut verified = VerifiedCoverage::open(
        &endpoint,
        &input.server_addr,
        input.trusted_issuer,
        first,
        input.expected_scope,
        &input.checkpoint,
    )
    .await?;
    if report_timings {
        eprintln!(
            "cluster-peer-open existing_checkpoint={} elapsed_us={}",
            had_checkpoint,
            open_started.elapsed().as_micros()
        );
    }
    for capability in input.capabilities {
        let range = capability.claims.range;
        let range_started = Instant::now();
        verified
            .fetch_range(&endpoint, input.server_addr.clone(), capability)
            .await?;
        if report_timings {
            eprintln!(
                "cluster-peer-range start={} end={} elapsed_us={}",
                range.start,
                range.end,
                range_started.elapsed().as_micros()
            );
        }
    }
    let state = verified.finish();
    println!("complete={}", state.is_complete());
    endpoint.close().await;
    Ok(())
}

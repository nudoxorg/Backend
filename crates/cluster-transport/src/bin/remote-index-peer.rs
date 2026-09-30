use std::{
    io::{self, Read},
    net::SocketAddr,
};

use backend_cluster_transport::{
    RemoteIndexChannel, RemoteIndexOutcome, RemoteIndexRequest, RemoteIndexSessionHello,
    bind_direct, connect_remote_index,
};
use iroh::{EndpointAddr, EndpointId, SecretKey};
use serde::Deserialize;

#[derive(Deserialize)]
struct Input {
    owner: EndpointId,
    address: SocketAddr,
    bind: SocketAddr,
    secret: [u8; 32],
    capability: backend_cluster_transport::RemoteIndexCapability,
    body: Vec<u8>,
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("remote index peer failed: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut input_bytes = Vec::new();
    io::stdin().read_to_end(&mut input_bytes)?;
    let input: Input = serde_json::from_slice(&input_bytes)?;
    let endpoint = bind_direct(SecretKey::from_bytes(&input.secret), input.bind).await?;
    let hello = RemoteIndexSessionHello::new(input.capability, RemoteIndexChannel::ProductQuery)?;
    let mut session = connect_remote_index(
        &endpoint,
        EndpointAddr::new(input.owner).with_ip_addr(input.address),
        hello,
    )
    .await?;
    let request = RemoteIndexRequest {
        request_id: 1,
        body: input.body.into_boxed_slice(),
    };
    session.send_request(&request).await?;
    let response = session.receive_response(request.request_id).await?;
    match response.outcome {
        RemoteIndexOutcome::Payload(body) if body.as_ref() == request.body.as_ref() => {
            println!("remote-index-echo-bytes={}", body.len());
        }
        _ => return Err("remote index peer received a mismatched response".into()),
    }
    endpoint.close().await;
    Ok(())
}

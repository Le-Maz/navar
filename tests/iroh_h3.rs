use anyhow::Result;
use axum::{Router, routing::get};
use iroh::{Endpoint, endpoint::presets::N0};
use iroh_h3_axum::IrohAxum;
use navar::{Client, http_body_util::BodyExt};
use navar_h3::H3App;
use navar_iroh::IrohTransport;
use navar_tokio::TokioRuntime;

const ALPN: &[u8] = b"iroh+h3";

#[tokio::test]
async fn hello_iroh_h3() -> Result<()> {
    let server_endpoint = Endpoint::bind(N0).await?;
    server_endpoint.online().await;
    let server_id = server_endpoint.id();

    let app = Router::new().route("/", get(|| async { "Hello from Iroh H3!" }));

    // The iroh-h3-axum magic happens here:
    // It attaches the Axum app directly to the Iroh protocol router
    let _server_router = iroh::protocol::Router::builder(server_endpoint)
        .accept(ALPN, IrohAxum::new(app))
        .spawn();

    // 3. Setup Client (Using Navar)
    let client_endpoint = Endpoint::bind(N0).await?;
    client_endpoint.online().await;

    // Ensure client uses the same ALPN as server
    let transport = IrohTransport::new(client_endpoint, vec![ALPN.to_vec()]);
    let runtime = TokioRuntime::default();

    // Initialize Navar Client with the H3 Plugin
    let client = Client::new(transport, H3App::new(), runtime);

    // 4. Perform Request
    // We target the server by NodeID. Navar Iroh transport handles the resolution.
    let url = format!("iroh+h3://{}/", server_id);
    println!("Client sending request to: {}", url);

    let response = client.get(&url).build()?.send().await?;

    assert!(response.status().is_success());

    // 5. Verify Content
    let body_bytes = response.into_body().collect().await?.to_bytes();
    assert_eq!(body_bytes, "Hello from Iroh H3!");

    Ok(())
}

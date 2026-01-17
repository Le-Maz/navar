use anyhow::Result;
use axum::{Router, routing::get};
use iroh_h3_axum::IrohAxum;
use navar::{Client, http_body_util::BodyExt};
use navar_h3::H3App;
use navar_iroh::IrohTransport;
use navar_tokio::TokioRuntime;

use crate::mock_discovery::MockDiscoveryMap;

const ALPN: &[u8] = b"iroh+h3";

#[tokio::test]
async fn hello_iroh_h3() -> Result<()> {
    // 1. Setup Mock Discovery
    let discovery = MockDiscoveryMap::new();

    // 2. Setup Server (Using iroh-h3-axum)
    let server_endpoint = discovery.spawn_endpoint().await;
    let server_id = server_endpoint.id();

    let app = Router::new().route("/", get(|| async { "Hello from Iroh H3!" }));

    // The iroh-h3-axum magic happens here:
    // It attaches the Axum app directly to the Iroh protocol router
    let _server_router = iroh::protocol::Router::builder(server_endpoint)
        .accept(ALPN, IrohAxum::new(app))
        .spawn();

    // 3. Setup Client (Using Navar)
    let client_endpoint = discovery.spawn_endpoint().await;

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

mod mock_discovery {
    use std::{
        collections::BTreeMap,
        sync::{Arc, RwLock},
    };

    use futures_lite::{StreamExt, stream::Boxed};
    use iroh::{
        Endpoint, EndpointId,
        discovery::{Discovery, DiscoveryItem, EndpointData, EndpointInfo, IntoDiscovery},
    };

    use crate::ALPN;

    #[derive(Debug, Default, Clone)]
    pub struct MockDiscoveryMap {
        peers: Arc<RwLock<BTreeMap<EndpointId, Arc<EndpointData>>>>,
    }

    impl MockDiscoveryMap {
        pub fn new() -> Self {
            Default::default()
        }

        pub async fn spawn_endpoint(&self) -> Endpoint {
            Endpoint::builder()
                .discovery(self.clone())
                .alpns(vec![ALPN.to_vec()])
                .bind()
                .await
                .unwrap()
        }
    }

    impl IntoDiscovery for MockDiscoveryMap {
        fn into_discovery(
            self,
            endpoint: &Endpoint,
        ) -> Result<impl Discovery, iroh::discovery::IntoDiscoveryError> {
            Ok(MockDiscovery {
                id: endpoint.id(),
                map: self,
            })
        }
    }

    #[derive(Debug, Clone)]
    pub struct MockDiscovery {
        id: EndpointId,
        map: MockDiscoveryMap,
    }

    impl Discovery for MockDiscovery {
        fn publish(&self, data: &EndpointData) {
            self.map
                .peers
                .write()
                .unwrap()
                .insert(self.id, Arc::new(data.clone()));
        }

        fn resolve(
            &self,
            endpoint_id: EndpointId,
        ) -> Option<Boxed<Result<DiscoveryItem, iroh::discovery::DiscoveryError>>> {
            let data = self.map.peers.read().unwrap().get(&endpoint_id).cloned()?;
            let ip_addrs = data.ip_addrs().cloned().collect();
            let info = EndpointInfo::new(endpoint_id).with_ip_addrs(ip_addrs);
            let discovery_item = DiscoveryItem::new(info, "mock", None);
            Some(navar::futures_lite::stream::once(Ok(discovery_item)).boxed())
        }
    }
}

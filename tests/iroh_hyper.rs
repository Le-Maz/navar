use axum::routing::get;
use navar::{Client, application::ApplicationPlugin, http_body_util::BodyExt};
use navar_hyper::{HyperApp, Protocol};
use navar_iroh::IrohTransport;
use navar_tokio::TokioRuntime;

// Hyper/Axum glue
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::service::TowerToHyperService;
use iroh::endpoint::Endpoint;

// Import the mock discovery logic
use mock_discovery::MockDiscoveryMap;

// Defines the ALPN protocol used for the test
const ALPN: &[u8] = b"iroh+h2";

/// Spawns an Iroh Endpoint acting as an HTTP server.
///
/// Instead of a TCP Listener, this accepts QUIC connections, opens a stream,
/// and pipes that stream into Axum via Hyper.
async fn with_iroh_server(app: axum::Router, run: impl AsyncFnOnce(&str, MockDiscoveryMap)) {
    // Create the discovery map
    let discovery = MockDiscoveryMap::new();

    // 1. Create Iroh Endpoint (Server)
    // We bind to 0 to let OS pick port, strict alpn ensures we only accept our protocol
    let endpoint = Endpoint::builder()
        .discovery(discovery.clone()) // CHANGED: Register server
        .alpns(vec![ALPN.to_vec()])
        .bind()
        .await
        .unwrap();

    let node_id = endpoint.id().to_string();

    // 2. Spawn the Server Loop
    let server_endpoint = endpoint.clone();
    let server_task = tokio::spawn(async move {
        // Accept incoming QUIC connections
        while let Some(incoming) = server_endpoint.accept().await {
            let app = app.clone();

            tokio::spawn(async move {
                // Perform the QUIC handshake
                if let Ok(connection) = incoming.await {
                    // Accept bidirectional streams on this connection
                    // (An HTTP request usually corresponds to one stream in this simple model)
                    while let Ok((send, recv)) = connection.accept_bi().await {
                        let app = app.clone();
                        tokio::spawn(async move {
                            // 3. Combine Hyper IO into a single object
                            let stream = tokio::io::join(recv, send);
                            let io = TokioIo::new(stream);

                            // 4. Serve using Hyper
                            let hyper_service = TowerToHyperService::new(app);
                            let _ =
                                hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                                    .serve_connection(io, hyper_service)
                                    .await;
                        });
                    }
                }
            });
        }
    });

    // 3. Run the Test Logic
    run(&node_id, discovery).await;

    // 4. Cleanup
    server_task.abort();
    let _ = server_task.await;
}

async fn hello_iroh_world(app_plugin: impl ApplicationPlugin) {
    let app = axum::Router::new().route("/", get(async || "Hello, Iroh!"));

    with_iroh_server(app, async |node_id, discovery| {
        // 1. Setup Client Endpoint
        let client_endpoint = Endpoint::builder()
            .discovery(discovery) // CHANGED: Use discovery to find server
            .alpns(vec![ALPN.to_vec()])
            .bind()
            .await
            .unwrap();

        // 2. Configure Navar Client
        let transport = IrohTransport::new(client_endpoint, vec![ALPN.to_vec()]);
        let runtime = TokioRuntime::default();
        let client = Client::new(transport, app_plugin, runtime);

        // 3. Send Request
        // We use the Node ID as the "host" in the URI
        let req = client.get(format!("http://{}/", node_id)).build().unwrap();

        let res = req.send().await.expect("Iroh Request failed");

        let bytes = res.into_body().collect().await.unwrap();
        assert_eq!(bytes.to_bytes(), b"Hello, Iroh!".as_slice());
    })
    .await;
}

#[tokio::test]
async fn hello_iroh_hyper_h1() {
    // Even though it's over QUIC/Iroh, Hyper will speak HTTP/1.1 framing
    // INSIDE the QUIC stream if we configure it to do so.
    hello_iroh_world(HyperApp::new().with_protocol(Protocol::Http1)).await;
}

#[tokio::test]
async fn hello_iroh_hyper_h2() {
    // Navar negotiates HTTP/2 framing over the Iroh stream
    hello_iroh_world(HyperApp::new().with_protocol(Protocol::Http2)).await;
}

pub mod mock_discovery {
    use std::{
        collections::BTreeMap,
        sync::{Arc, RwLock},
    };

    use iroh::{
        Endpoint, EndpointId,
        discovery::{Discovery, DiscoveryItem, EndpointData, EndpointInfo, IntoDiscovery},
    };
    use navar::futures_lite::StreamExt;

    #[derive(Debug, Default, Clone)]
    pub struct MockDiscoveryMap {
        peers: Arc<RwLock<BTreeMap<EndpointId, Arc<EndpointData>>>>,
    }

    impl MockDiscoveryMap {
        #[inline]
        pub fn new() -> Self {
            Default::default()
        }

        pub async fn spawn_endpoint(&self) -> Endpoint {
            Endpoint::builder()
                .discovery(self.clone())
                .bind()
                .await
                .unwrap()
        }
    }

    impl IntoDiscovery for MockDiscoveryMap {
        fn into_discovery(
            self,
            endpoint: &iroh::Endpoint,
        ) -> Result<impl Discovery, iroh::discovery::IntoDiscoveryError> {
            Ok(MockDiscovery {
                id: endpoint.id(),
                map: self.clone(),
            })
        }
    }

    #[derive(Debug, Clone)]
    pub struct MockDiscovery {
        id: EndpointId,
        map: MockDiscoveryMap,
    }

    impl MockDiscovery {
        pub fn new(id: EndpointId, map: MockDiscoveryMap) -> Self {
            Self { id, map }
        }
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
        ) -> Option<
            navar::futures_lite::stream::Boxed<
                Result<iroh::discovery::DiscoveryItem, iroh::discovery::DiscoveryError>,
            >,
        > {
            let data = self.map.peers.read().unwrap().get(&endpoint_id).cloned()?;

            let ip_addrs = data.ip_addrs().cloned().collect();

            let info = EndpointInfo::new(endpoint_id).with_ip_addrs(ip_addrs);

            let discovery_item = DiscoveryItem::new(info, "mock", None);

            Some(navar::futures_lite::stream::once(Ok(discovery_item)).boxed())
        }
    }
}

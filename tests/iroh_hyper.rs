use axum::routing::get;
use iroh::endpoint::presets::N0;
use navar::{Client, application::ApplicationPlugin, http_body_util::BodyExt};
use navar_hyper::{HyperApp, Protocol};
use navar_iroh::{IrohConnection, IrohTransport};
use navar_tokio::TokioRuntime;

// Hyper/Axum glue
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::service::TowerToHyperService;
use iroh::endpoint::Endpoint;

const ALPN: &[u8] = b"iroh+http";

async fn with_iroh_server(app: axum::Router, run: impl AsyncFnOnce(&str)) {
    let endpoint = Endpoint::builder(N0)
        .alpns(vec![ALPN.to_vec()])
        .bind()
        .await
        .unwrap();
    endpoint.online().await;

    let node_id = endpoint.id().to_string();
    let server_endpoint = endpoint.clone();

    let server_task = tokio::spawn(async move {
        while let Some(incoming) = server_endpoint.accept().await {
            let app = app.clone();
            tokio::spawn(async move {
                if let Ok(connection) = incoming.await {
                    while let Ok((send, recv)) = connection.accept_bi().await {
                        let app = app.clone();
                        tokio::spawn(async move {
                            // Manual stream joining for server-side Axum
                            let stream = tokio::io::join(recv, send);
                            let io = TokioIo::new(stream);
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

    run(&node_id).await;

    server_task.abort();
    let _ = server_task.await;
}

async fn hello_iroh_world(app_plugin: impl ApplicationPlugin<IrohConnection>) {
    let app = axum::Router::new().route("/", get(async || "Hello, Iroh!"));

    with_iroh_server(app, async |node_id| {
        let client_endpoint = Endpoint::builder(N0)
            .alpns(vec![ALPN.to_vec()])
            .bind()
            .await
            .unwrap();
        client_endpoint.online().await;

        let transport = IrohTransport::new(client_endpoint, vec![ALPN.to_vec()]);
        let runtime = TokioRuntime::default();

        let client = Client::new(transport, app_plugin, runtime);

        let req = client
            .get(format!("iroh+http://{}/", node_id))
            .build()
            .unwrap();
        let res = req.send().await.expect("Iroh Request failed");

        let bytes = res.into_body().collect().await.unwrap();
        assert_eq!(bytes.to_bytes(), b"Hello, Iroh!".as_slice());
    })
    .await;
}

#[tokio::test]
async fn hello_iroh_hyper_h1() {
    hello_iroh_world(HyperApp::new().with_protocol(Protocol::Http1)).await;
}

#[tokio::test]
async fn hello_iroh_hyper_h2() {
    hello_iroh_world(HyperApp::new().with_protocol(Protocol::Http2)).await;
}

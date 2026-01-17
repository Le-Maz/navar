use axum::routing::get;
use navar::{Client, application::ApplicationPlugin, http_body_util::BodyExt};
use navar_hyper::{HyperH1App, HyperH2App};
use navar_tokio::{TokioRuntime, TokioTcpTransport};
use tokio::net::TcpListener;

async fn with_server(app: axum::Router, run: impl AsyncFnOnce(&str)) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let server = tokio::spawn(axum::serve(listener, app).into_future());

    run(&address).await;

    server.abort();
    let _ = server.await;
}

async fn hello_world(app_plugin: impl ApplicationPlugin) {
    let app = axum::Router::new().route("/", get(async || "Hello, World!"));
    with_server(app, async |address| {
        let transport = TokioTcpTransport::default();
        let runtime = TokioRuntime::default();
        let client = Client::new(transport, app_plugin, runtime);

        let req = client.get(format!("http://{address}/")).build().unwrap();
        let res = req.send().await.unwrap();

        let bytes = res.into_body().collect().await.unwrap();
        assert_eq!(bytes.to_bytes(), b"Hello, World!".as_slice());
    })
    .await;
}

#[tokio::test]
async fn hello_h1() {
    hello_world(HyperH1App::default()).await;
}

#[tokio::test]
async fn hello_h2() {
    hello_world(HyperH2App::default()).await;
}

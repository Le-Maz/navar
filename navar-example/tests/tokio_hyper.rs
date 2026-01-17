use axum::{http::Request, routing::get};
use navar::{
    HttpClient,
    application::ApplicationPlugin,
    bytes::Bytes,
    http::{Method, Uri},
    http_body_util::{BodyExt, Empty},
};
use navar_hyper::HyperH1App;
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
        let client = HttpClient {
            transport: TokioTcpTransport::default(),
            app: app_plugin,
            runtime: TokioRuntime::default(),
        };

        let uri = Uri::builder()
            .scheme("http")
            .authority(address)
            .path_and_query("/")
            .build()
            .unwrap();

        let req = Request::builder()
            .method(Method::GET)
            .uri(uri)
            .body(Empty::<Bytes>::new())
            .unwrap();

        let res = client.send(req).await.unwrap();
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
    hello_world(HyperH1App::default()).await;
}

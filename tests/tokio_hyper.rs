use axum::routing::get;
use navar::{Client, application::ApplicationPlugin, http_body_util::BodyExt};
use navar_hyper::{HyperApp, Protocol};
use navar_tokio::{TokioConnection, TokioRuntime, TokioTransport};
use std::sync::Arc;
use tokio::net::TcpListener;

// Imports for TLS and Hyper glue
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::service::TowerToHyperService;
use tokio_rustls::{TlsAcceptor, rustls};

/// Generates a self-signed certificate for localhost
/// Compatible with rcgen 0.14+ and rustls 0.23+
fn generate_self_signed_cert() -> (Vec<u8>, rustls::ServerConfig) {
    let subject_alt_names = vec!["localhost".to_string(), "127.0.0.1".to_string()];
    let certified_key = rcgen::generate_simple_self_signed(subject_alt_names).unwrap();
    let cert_der = certified_key.cert.der().to_vec();
    let key_der = certified_key.signing_key.serialize_der();
    let certs = vec![rustls::pki_types::CertificateDer::from(cert_der.clone())];
    let key = rustls::pki_types::PrivateKeyDer::Pkcs8(key_der.into());

    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap();

    (cert_der, config)
}

// Note: Usage of `async |...|` implies nightly Async Closures feature is enabled
async fn with_server(app: axum::Router, use_tls: bool, run: impl AsyncFnOnce(&str, Option<&[u8]>)) {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();

    let (tls_acceptor, ca_cert) = if use_tls {
        let (cert, config) = generate_self_signed_cert();
        (Some(TlsAcceptor::from(Arc::new(config))), Some(cert))
    } else {
        (None, None)
    };

    let server = tokio::spawn(async move {
        if let Some(acceptor) = tls_acceptor {
            loop {
                let (socket, _) = match listener.accept().await {
                    Ok(v) => v,
                    Err(_) => break,
                };
                let acceptor = acceptor.clone();
                let app = app.clone();

                tokio::spawn(async move {
                    if let Ok(stream) = acceptor.accept(socket).await {
                        let io = TokioIo::new(stream);
                        let hyper_service = TowerToHyperService::new(app);
                        let _ = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                            .serve_connection(io, hyper_service)
                            .await;
                    }
                });
            }
        } else {
            axum::serve(listener, app).await.unwrap();
        }
    });

    run(&address, ca_cert.as_deref()).await;

    server.abort();
    let _ = server.await;
}

// UPDATED: Now generic over ApplicationPlugin<TokioConnection>
async fn hello_world(app_plugin: impl ApplicationPlugin<TokioConnection>, use_tls: bool) {
    let app = axum::Router::new().route("/", get(async || "Hello, World!"));

    with_server(app, use_tls, async |address, ca_cert| {
        let transport = if let Some(cert) = ca_cert {
            TokioTransport::new(
                vec![b"h2".to_vec(), b"http/1.1".to_vec()],
                Some(cert.to_vec()),
            )
        } else {
            TokioTransport::default()
        };

        let runtime = TokioRuntime::default();
        // Client::new now correctly infers that T=TokioTransport and A=AppPlugin<TokioConnection>
        let client = Client::new(transport, app_plugin, runtime);

        let scheme = if use_tls { "https" } else { "http" };
        let req = client
            .get(format!("{}://{}/", scheme, address))
            .build()
            .unwrap();

        let res = req.send().await.expect("Request failed");

        let bytes = res.into_body().collect().await.unwrap();
        assert_eq!(bytes.to_bytes(), b"Hello, World!".as_slice());
    })
    .await;
}

#[tokio::test]
async fn hello_h1() {
    hello_world(HyperApp::new().with_protocol(Protocol::Http1), false).await;
}

#[tokio::test]
async fn hello_h2() {
    hello_world(HyperApp::new().with_protocol(Protocol::Http2), false).await;
}

#[tokio::test]
async fn hello_h1_tls() {
    hello_world(HyperApp::new().with_protocol(Protocol::Http1), true).await;
}

#[tokio::test]
async fn hello_h2_tls() {
    hello_world(HyperApp::new().with_protocol(Protocol::Http2), true).await;
}

#[tokio::test]
async fn hello_auto_tls() {
    hello_world(HyperApp::new().with_protocol(Protocol::Auto), true).await;
}

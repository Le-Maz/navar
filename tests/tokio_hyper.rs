use axum::routing::get;
use navar::{Client, application::ApplicationPlugin, http_body_util::BodyExt};
use navar_hyper::{HyperApp, Protocol};
use navar_tokio::{TokioRuntime, TokioTransport};
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

    // rcgen 0.14+ returns a CertifiedKey
    let certified_key = rcgen::generate_simple_self_signed(subject_alt_names).unwrap();

    // Extract the raw DER bytes for the certificate (to pass to the client later)
    let cert_der = certified_key.cert.der().to_vec();

    // Extract the private key
    let key_der = certified_key.signing_key.serialize_der();

    // Convert to rustls pki_types
    let certs = vec![rustls::pki_types::CertificateDer::from(cert_der.clone())];
    let key = rustls::pki_types::PrivateKeyDer::Pkcs8(key_der.into());

    // Rustls 0.22+ builder (safe defaults are now implicit/standard)
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap();

    (cert_der, config)
}

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
                    Err(_) => break, // Listener closed
                };
                let acceptor = acceptor.clone();
                let app = app.clone();

                tokio::spawn(async move {
                    if let Ok(stream) = acceptor.accept(socket).await {
                        let io = TokioIo::new(stream);
                        // Wrap axum router in TowerToHyperService
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

async fn hello_world(app_plugin: impl ApplicationPlugin, use_tls: bool) {
    let app = axum::Router::new().route("/", get(async || "Hello, World!"));

    with_server(app, use_tls, async |address, ca_cert| {
        // Construct the transport based on whether we have a CA cert (TLS mode)
        let transport = if let Some(cert) = ca_cert {
            // TLS: Pass the self-signed CA cert and default ALPNs
            TokioTransport::new(
                vec![b"h2".to_vec(), b"http/1.1".to_vec()],
                Some(cert.to_vec()),
            )
        } else {
            // Plaintext: Use defaults
            TokioTransport::default()
        };

        let runtime = TokioRuntime::default();
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
    // Should negotiate H2 over TLS if server supports it, or H1 otherwise
    hello_world(HyperApp::new().with_protocol(Protocol::Auto), true).await;
}

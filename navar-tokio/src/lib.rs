use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use anyhow::Context as _;
use tokio::net::TcpStream;
use tokio_util::compat::{Compat, TokioAsyncReadCompatExt};

use navar::AsyncRuntime;
use navar::futures_lite::{AsyncRead, AsyncWrite};
use navar::http::Uri;
use navar::transport::{TransportIo, TransportPlugin};

#[cfg(feature = "tls")]
use {
    rustls::pki_types::{CertificateDer, ServerName},
    tokio_rustls::{TlsConnector, client::TlsStream},
};

/// Tokio runtime adapter
#[derive(Clone, Copy, Default, Debug)]
pub struct TokioRuntime;

impl AsyncRuntime for TokioRuntime {
    fn spawn<F>(&self, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        tokio::spawn(future);
    }
}

/// Unified Tokio transport (TCP + optional TLS)
#[derive(Clone)]
pub struct TokioTransport {
    #[cfg(feature = "tls")]
    tls: Option<TlsConnector>,
}

impl TokioTransport {
    /// Create a new transport with explicit ALPNs and an optional custom CA cert.
    ///
    /// - `alpns`: Protocol names (e.g., "h2", "http/1.1").
    /// - `ca_cert`: Optional DER-encoded root certificate to trust (e.g., for self-signed tests).
    pub fn new(alpns: Vec<Vec<u8>>, ca_cert: Option<Vec<u8>>) -> Self {
        #[cfg(feature = "tls")]
        {
            use rustls::{ClientConfig, RootCertStore};
            use std::sync::Arc;

            let mut roots = RootCertStore::empty();

            // 1. Load native/system roots
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

            // 2. Load custom CA if provided
            if let Some(cert_bytes) = ca_cert {
                let cert = CertificateDer::from(cert_bytes);
                if let Err(e) = roots.add(cert) {
                    eprintln!("Failed to add custom CA to root store: {}", e);
                }
            }

            let mut config = ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth();

            config.alpn_protocols = alpns;

            Self {
                tls: Some(TlsConnector::from(Arc::new(config))),
            }
        }

        #[cfg(not(feature = "tls"))]
        {
            let _ = alpns;
            let _ = ca_cert;
            Self {}
        }
    }
}

impl Default for TokioTransport {
    fn default() -> Self {
        // Default: No custom CA, standard ALPNs
        Self::new(vec![b"h2".to_vec(), b"http/1.1".to_vec()], None)
    }
}

/// Unified IO type returned by the transport
#[derive(Debug)]
pub enum TokioIo {
    Plain(Compat<TcpStream>),

    #[cfg(feature = "tls")]
    Tls(Compat<TlsStream<TcpStream>>),
}

impl AsyncRead for TokioIo {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        unsafe {
            match self.get_unchecked_mut() {
                TokioIo::Plain(io) => Pin::new_unchecked(io).poll_read(cx, buf),
                #[cfg(feature = "tls")]
                TokioIo::Tls(io) => Pin::new_unchecked(io).poll_read(cx, buf),
            }
        }
    }
}

impl AsyncWrite for TokioIo {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        unsafe {
            match self.get_unchecked_mut() {
                TokioIo::Plain(io) => Pin::new_unchecked(io).poll_write(cx, buf),
                #[cfg(feature = "tls")]
                TokioIo::Tls(io) => Pin::new_unchecked(io).poll_write(cx, buf),
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        unsafe {
            match self.get_unchecked_mut() {
                TokioIo::Plain(io) => Pin::new_unchecked(io).poll_flush(cx),
                #[cfg(feature = "tls")]
                TokioIo::Tls(io) => Pin::new_unchecked(io).poll_flush(cx),
            }
        }
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        unsafe {
            match self.get_unchecked_mut() {
                TokioIo::Plain(io) => Pin::new_unchecked(io).poll_close(cx),
                #[cfg(feature = "tls")]
                TokioIo::Tls(io) => Pin::new_unchecked(io).poll_close(cx),
            }
        }
    }
}

impl TransportIo for TokioIo {
    fn alpn_protocol(&self) -> Option<&[u8]> {
        #[cfg(feature = "tls")]
        if let TokioIo::Tls(stream) = self {
            // Access the inner TlsStream to get alpn
            // Note: You might need to adjust depending on how deep the Compat layer is
            return stream.get_ref().get_ref().1.alpn_protocol();
        }
        None
    }
}

impl TransportPlugin for TokioTransport {
    type Io = TokioIo;

    async fn connect(&self, uri: &Uri) -> anyhow::Result<Self::Io> {
        let host = uri.host().context("URI missing host")?;

        let port = uri.port_u16().unwrap_or_else(|| {
            if uri.scheme_str() == Some("https") {
                443
            } else {
                80
            }
        });

        let addr = format!("{host}:{port}");

        let tcp = TcpStream::connect(addr).await?;
        let _ = tcp.set_nodelay(true);

        #[cfg(feature = "tls")]
        if uri.scheme_str() == Some("https") {
            let tls = self.tls.as_ref().context("TLS support not configured")?;

            let server_name = ServerName::try_from(host.to_string()).context("invalid DNS name")?;

            let stream = tls.connect(server_name, tcp).await?;
            return Ok(TokioIo::Tls(stream.compat()));
        }

        Ok(TokioIo::Plain(tcp.compat()))
    }
}

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};

use anyhow::Context as _;
use tokio::io::{ReadHalf, WriteHalf};
use tokio::net::{
    TcpStream,
    tcp::{OwnedReadHalf, OwnedWriteHalf},
};
use tokio_util::compat::{Compat, TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use navar::AsyncRuntime;
use navar::futures_lite::{AsyncRead, AsyncWrite};
use navar::http::Uri;
use navar::transport::{BidiStream, Connection, Stream, TransportPlugin};

#[cfg(feature = "tls")]
use {
    rustls::pki_types::{CertificateDer, ServerName},
    tokio_rustls::{TlsConnector, client::TlsStream},
};

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

#[derive(Clone)]
pub struct TokioTransport {
    #[cfg(feature = "tls")]
    tls: Option<TlsConnector>,
}

impl TokioTransport {
    pub fn new(alpns: Vec<Vec<u8>>, ca_cert: Option<Vec<u8>>) -> Self {
        #[cfg(feature = "tls")]
        {
            use rustls::{ClientConfig, RootCertStore};
            use std::sync::Arc;
            let mut roots = RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            if let Some(cert_bytes) = ca_cert {
                let cert = CertificateDer::from(cert_bytes);
                let _ = roots.add(cert);
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
        Self::new(vec![b"h2".to_vec(), b"http/1.1".to_vec()], None)
    }
}

// 1. Bidi Stream (TokioIo)
#[derive(Debug)]
pub enum TokioIo {
    Plain(Compat<TcpStream>),
    #[cfg(feature = "tls")]
    Tls(Compat<TlsStream<TcpStream>>),
}

// Implement base Stream trait
impl Stream for TokioIo {
    fn stream_id(&self) -> u64 {
        0
    } // TCP effectively has one stream
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

impl BidiStream for TokioIo {
    type Send = TokioSendStream;
    type Recv = TokioRecvStream;

    fn split(self) -> (Self::Send, Self::Recv) {
        match self {
            TokioIo::Plain(compat) => {
                let (read, write) = compat.into_inner().into_split();
                (
                    TokioSendStream::Plain(write.compat_write()),
                    TokioRecvStream::Plain(read.compat()),
                )
            }
            #[cfg(feature = "tls")]
            TokioIo::Tls(compat) => {
                let (read, write) = tokio::io::split(compat.into_inner());
                (
                    TokioSendStream::Tls(write.compat_write()),
                    TokioRecvStream::Tls(read.compat()),
                )
            }
        }
    }

    fn alpn_protocol(&self) -> Option<&[u8]> {
        #[cfg(feature = "tls")]
        if let TokioIo::Tls(stream) = self {
            return stream.get_ref().get_ref().1.alpn_protocol();
        }
        None
    }
}

// 2. Send Stream Halves
pub enum TokioSendStream {
    Plain(Compat<OwnedWriteHalf>),
    #[cfg(feature = "tls")]
    Tls(Compat<WriteHalf<TlsStream<TcpStream>>>),
}

impl Stream for TokioSendStream {
    fn stream_id(&self) -> u64 {
        0
    }
}

impl AsyncWrite for TokioSendStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        unsafe {
            match self.get_unchecked_mut() {
                TokioSendStream::Plain(io) => Pin::new_unchecked(io).poll_write(cx, buf),
                #[cfg(feature = "tls")]
                TokioSendStream::Tls(io) => Pin::new_unchecked(io).poll_write(cx, buf),
            }
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        unsafe {
            match self.get_unchecked_mut() {
                TokioSendStream::Plain(io) => Pin::new_unchecked(io).poll_flush(cx),
                #[cfg(feature = "tls")]
                TokioSendStream::Tls(io) => Pin::new_unchecked(io).poll_flush(cx),
            }
        }
    }
    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        unsafe {
            match self.get_unchecked_mut() {
                TokioSendStream::Plain(io) => Pin::new_unchecked(io).poll_close(cx),
                #[cfg(feature = "tls")]
                TokioSendStream::Tls(io) => Pin::new_unchecked(io).poll_close(cx),
            }
        }
    }
}

// 3. Recv Stream Halves
pub enum TokioRecvStream {
    Plain(Compat<OwnedReadHalf>),
    #[cfg(feature = "tls")]
    Tls(Compat<ReadHalf<TlsStream<TcpStream>>>),
}

impl Stream for TokioRecvStream {
    fn stream_id(&self) -> u64 {
        0
    }
}

impl AsyncRead for TokioRecvStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        unsafe {
            match self.get_unchecked_mut() {
                TokioRecvStream::Plain(io) => Pin::new_unchecked(io).poll_read(cx, buf),
                #[cfg(feature = "tls")]
                TokioRecvStream::Tls(io) => Pin::new_unchecked(io).poll_read(cx, buf),
            }
        }
    }
}

// --- Connection Implementation ---

#[derive(Clone)]
pub struct TokioConnection {
    addr: SocketAddr,
    #[cfg(feature = "tls")]
    tls_config: Option<(TlsConnector, ServerName<'static>)>,
}

impl Connection for TokioConnection {
    type Bidi = TokioIo;
    type Send = TokioSendStream;
    type Recv = TokioRecvStream;

    async fn open_bidirectional(&self) -> anyhow::Result<Self::Bidi> {
        let tcp = TcpStream::connect(self.addr).await?;
        let _ = tcp.set_nodelay(true);
        #[cfg(feature = "tls")]
        if let Some((connector, domain)) = &self.tls_config {
            let stream = connector.connect(domain.clone(), tcp).await?;
            return Ok(TokioIo::Tls(stream.compat()));
        }
        Ok(TokioIo::Plain(tcp.compat()))
    }

    async fn open_unidirectional(&self) -> anyhow::Result<Self::Send> {
        Err(anyhow::anyhow!(
            "TokioTransport: Unidirectional streams not supported"
        ))
    }
    async fn accept_bidirectional(&self) -> anyhow::Result<Self::Bidi> {
        Err(anyhow::anyhow!(
            "TokioTransport: Accept not supported on client"
        ))
    }
    async fn accept_unidirectional(&self) -> anyhow::Result<Self::Recv> {
        Err(anyhow::anyhow!(
            "TokioTransport: Accept not supported on client"
        ))
    }
}

impl TransportPlugin for TokioTransport {
    type Conn = TokioConnection;
    async fn connect(&self, uri: &Uri) -> anyhow::Result<Self::Conn> {
        let host = uri.host().context("URI missing host")?;
        let port = uri.port_u16().unwrap_or_else(|| {
            if uri.scheme_str() == Some("https") {
                443
            } else {
                80
            }
        });
        let addr_str = format!("{host}:{port}");
        let mut addrs = tokio::net::lookup_host(&addr_str).await?;
        let addr = addrs.next().context("DNS resolution failed")?;

        #[cfg(feature = "tls")]
        let tls_config = if uri.scheme_str() == Some("https") {
            let tls = self
                .tls
                .as_ref()
                .context("TLS support not configured")?
                .clone();
            let domain = ServerName::try_from(host.to_string())
                .context("invalid DNS name")?
                .to_owned();
            Some((tls, domain))
        } else {
            None
        };

        Ok(TokioConnection {
            addr,
            #[cfg(feature = "tls")]
            tls_config,
        })
    }
}

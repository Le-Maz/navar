use std::{
    pin::Pin,
    str::FromStr,
    task::{Context, Poll},
};

use anyhow::Context as _;
use iroh::{EndpointAddr, EndpointId, endpoint::Endpoint};
use navar::{
    futures_lite::{AsyncRead, AsyncWrite},
    http::Uri,
    transport::{TransportIo, TransportPlugin},
};
use tokio_util::compat::{Compat, TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

/// A Transport Plugin that establishes Iroh QUIC connections and returns
/// bidirectional streams as the transport IO.
#[derive(Clone, Debug)]
pub struct IrohTransport {
    endpoint: Endpoint,
    alpns: Vec<Vec<u8>>,
}

impl IrohTransport {
    /// Create a new Iroh transport using an existing Endpoint.
    ///
    /// - `endpoint`: The Iroh endpoint to use for connecting.
    /// - `alpns`: ALPN protocols to negotiate (e.g., b"n0/navar").
    pub fn new(endpoint: Endpoint, alpns: Vec<Vec<u8>>) -> Self {
        Self { endpoint, alpns }
    }
}

impl TransportPlugin for IrohTransport {
    type Io = IrohStream;

    async fn connect(&self, uri: &Uri) -> anyhow::Result<Self::Io> {
        // 1. Parse the destination from the URI
        let pubkey_str = uri.host().context("URI missing host (Iroh Public Key)")?;
        let public_key = EndpointId::from_str(pubkey_str).context("Invalid Iroh Public Key")?;

        // Construct EndpointAddr.
        // NOTE: This assumes direct connectivity or DERP assistance via the key alone.
        // If specific relay URLs are needed, they would need to be parsed from the URI path or config.
        let addr = EndpointAddr::from_parts(public_key, vec![]);

        // 2. Select ALPN
        let alpn = self
            .alpns
            .first()
            .map(|v| v.as_slice())
            .unwrap_or(b"n0/navar");

        // 3. Connect to the peer (Establish QUIC Connection)
        // TODO: In a production environment, you likely want to cache/pool this connection
        // struct to reuse it for multiple streams, rather than performing a full
        // QUIC handshake for every single request.
        let connection = self
            .endpoint
            .connect(addr, alpn)
            .await
            .context("Iroh connect failed")?;

        // 4. Open a Bidirectional Stream (The "IO")
        let (send, recv) = connection
            .open_bi()
            .await
            .context("Failed to open stream")?;

        // 5. Wrap in our compatibility struct
        Ok(IrohStream::new(send, recv))
    }
}

/// A wrapper around Iroh's SendStream and RecvStream that implements
/// the standard AsyncRead/AsyncWrite traits required by Navar.
pub struct IrohStream {
    send: Compat<iroh::endpoint::SendStream>,
    recv: Compat<iroh::endpoint::RecvStream>,
}

impl IrohStream {
    pub fn new(send: iroh::endpoint::SendStream, recv: iroh::endpoint::RecvStream) -> Self {
        Self {
            // Convert tokio::io traits to futures_lite::io traits via Compat
            send: send.compat_write(),
            recv: recv.compat(),
        }
    }
}

// Implement AsyncRead by delegating to the receive half
impl AsyncRead for IrohStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.get_mut().recv).poll_read(cx, buf)
    }
}

// Implement AsyncWrite by delegating to the send half
impl AsyncWrite for IrohStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.get_mut().send).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().send).poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().send).poll_close(cx)
    }
}

impl TransportIo for IrohStream {
    // Iroh streams don't strictly have an ALPN themselves (the connection does),
    // but typically TransportIo doesn't strictly require this method unless used
    // for protocol negotiation logic higher up.
    fn alpn_protocol(&self) -> Option<&[u8]> {
        None
    }
}

//! # Iroh Transport Implementation (No Pooling)
//!
//! This module provides an implementation of the `navar` transport abstractions
//! backed by the `iroh` QUIC endpoint.

use std::{
    pin::Pin,
    str::FromStr,
    task::{Context, Poll},
};

use anyhow::Context as _;
use iroh::{
    EndpointId,
    endpoint::{Connection as QuicConnection, Endpoint},
};
use navar::{
    futures_lite::{AsyncRead, AsyncWrite},
    http::Uri,
    transport::{BidiStream, Connection, Stream, TransportPlugin},
};
use tokio_util::compat::{Compat, TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

/// Iroh-based transport plugin.
///
/// This transport uses an [`iroh::Endpoint`] to establish QUIC connections
/// identified by peer public keys.
#[derive(Clone, Debug)]
pub struct IrohTransport {
    endpoint: Endpoint,
    alpns: Vec<Vec<u8>>,
}

impl IrohTransport {
    /// Creates a new `IrohTransport`.
    pub fn new(endpoint: Endpoint, alpns: Vec<Vec<u8>>) -> Self {
        Self { endpoint, alpns }
    }
}

impl TransportPlugin for IrohTransport {
    type Conn = IrohConnection;

    /// Connects to a remote peer identified by an Iroh public key.
    ///
    /// Every call to this method establishes a new QUIC connection.
    async fn connect(&self, uri: &Uri) -> anyhow::Result<Self::Conn> {
        let pubkey_str = uri.host().context("URI missing host")?;
        let remote_id = EndpointId::from_str(pubkey_str).context("Invalid Iroh Public Key")?;

        // Use the first ALPN or a default
        let alpn = self
            .alpns
            .first()
            .map(|v| v.as_slice())
            .unwrap_or(b"n0/navar");

        // Establish a fresh connection directly
        let connection = self
            .endpoint
            .connect(remote_id, alpn)
            .await
            .context("Failed to connect to iroh endpoint")?;

        Ok(IrohConnection { inner: connection })
    }
}

/// An established Iroh QUIC connection.
#[derive(Clone, Debug)]
pub struct IrohConnection {
    inner: QuicConnection,
}

impl Connection for IrohConnection {
    type Bidi = IrohBidiStream;
    type Send = IrohSendStream;
    type Recv = IrohRecvStream;

    async fn open_bidirectional(&self) -> anyhow::Result<Self::Bidi> {
        let (send, recv) = self.inner.open_bi().await?;
        Ok(IrohBidiStream {
            send: IrohSendStream(send.compat_write()),
            recv: IrohRecvStream(recv.compat()),
        })
    }

    async fn open_unidirectional(&self) -> anyhow::Result<Self::Send> {
        let send = self.inner.open_uni().await?;
        Ok(IrohSendStream(send.compat_write()))
    }

    async fn accept_bidirectional(&self) -> anyhow::Result<Self::Bidi> {
        let (send, recv) = self.inner.accept_bi().await?;
        Ok(IrohBidiStream {
            send: IrohSendStream(send.compat_write()),
            recv: IrohRecvStream(recv.compat()),
        })
    }

    async fn accept_unidirectional(&self) -> anyhow::Result<Self::Recv> {
        let recv = self.inner.accept_uni().await?;
        Ok(IrohRecvStream(recv.compat()))
    }

    fn alpn_protocol(&self) -> Option<&[u8]> {
        Some(self.inner.alpn())
    }
}

/// Send-only QUIC stream.
pub struct IrohSendStream(Compat<iroh::endpoint::SendStream>);

impl Stream for IrohSendStream {
    fn stream_id(&self) -> u64 {
        self.0.get_ref().id().index()
    }
}

impl AsyncWrite for IrohSendStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_close(cx)
    }
}

/// Receive-only QUIC stream.
pub struct IrohRecvStream(Compat<iroh::endpoint::RecvStream>);

impl Stream for IrohRecvStream {
    fn stream_id(&self) -> u64 {
        self.0.get_ref().id().index()
    }
}

impl AsyncRead for IrohRecvStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

/// Bidirectional QUIC stream composed of send and receive halves.
pub struct IrohBidiStream {
    send: IrohSendStream,
    recv: IrohRecvStream,
}

impl Stream for IrohBidiStream {
    fn stream_id(&self) -> u64 {
        self.send.stream_id()
    }
}

impl AsyncRead for IrohBidiStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.recv).poll_read(cx, buf)
    }
}

impl AsyncWrite for IrohBidiStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.send).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.send).poll_flush(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.send).poll_close(cx)
    }
}

impl BidiStream for IrohBidiStream {
    type Send = IrohSendStream;
    type Recv = IrohRecvStream;

    fn split(self) -> (Self::Send, Self::Recv) {
        (self.send, self.recv)
    }

    fn alpn_protocol(&self) -> Option<&[u8]> {
        None
    }
}

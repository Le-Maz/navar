//! # Iroh Transport Implementation
//!
//! This module provides an implementation of the `navar` transport abstractions
//! backed by the `iroh` QUIC endpoint.
//!
//! Features:
//!
//! - QUIC-based multiplexed transport
//! - Connection reuse via an internal connection cache
//! - Support for bidirectional and unidirectional streams
//! - ALPN-based protocol negotiation
//!
//! Design notes:
//!
//! - Connections are keyed by `EndpointId` and shared across requests
//! - Concurrent connection attempts are deduplicated using shared futures
//! - Connection entries are automatically evicted when closed

use std::{
    ops::ControlFlow,
    pin::Pin,
    str::FromStr,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use anyhow::Context as _;
use dashmap::{DashMap, mapref::entry::Entry};
use futures::{Future, FutureExt, future::Shared};
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

/// Boxed future used internally for connection establishment.
type DynFuture = Pin<Box<dyn Future<Output = Result<QuicConnection, Arc<anyhow::Error>>> + Send>>;

/// Shared future representing an in-flight connection attempt.
type DialFuture = Shared<DynFuture>;

/// Iroh-based transport plugin.
///
/// This transport uses an [`iroh::Endpoint`] to establish QUIC connections
/// identified by peer public keys.
#[derive(Clone, Debug)]
pub struct IrohTransport {
    endpoint: Endpoint,
    alpns: Vec<Vec<u8>>,
    connection_cache: Arc<DashMap<EndpointId, DialFuture>>,
}

impl IrohTransport {
    /// Creates a new `IrohTransport`.
    ///
    /// - `endpoint`: The local Iroh endpoint
    /// - `alpns`: Supported ALPN protocol identifiers
    pub fn new(endpoint: Endpoint, alpns: Vec<Vec<u8>>) -> Self {
        Self {
            endpoint,
            alpns,
            connection_cache: Arc::new(DashMap::new()),
        }
    }

    /// Retrieves or establishes a connection to the given peer.
    ///
    /// Concurrent connection attempts to the same peer are deduplicated
    /// using a shared future stored in the connection cache.
    async fn get_connection(&self, peer_id: EndpointId) -> anyhow::Result<QuicConnection> {
        loop {
            let action = {
                let entry = self.connection_cache.entry(peer_id);
                match entry {
                    Entry::Occupied(entry) => ControlFlow::Continue(entry.get().clone()),
                    Entry::Vacant(entry) => {
                        let future = self.create_dial_future(peer_id);
                        entry.insert(future.clone());
                        ControlFlow::Break(future)
                    }
                }
            };

            match action {
                ControlFlow::Continue(shared_future) => match shared_future.await {
                    Ok(conn) => return Ok(conn),
                    Err(_) => {
                        tokio::time::sleep(Duration::from_millis(1)).await;
                        continue;
                    }
                },
                ControlFlow::Break(shared_future) => {
                    return match shared_future.await {
                        Ok(conn) => Ok(conn),
                        Err(err) => {
                            self.connection_cache.remove(&peer_id);
                            Err(anyhow::anyhow!(err))
                        }
                    };
                }
            }
        }
    }

    /// Creates a shared dialing future for a peer.
    ///
    /// The resulting connection is monitored, and the cache entry is
    /// removed once the connection closes.
    fn create_dial_future(&self, peer_id: EndpointId) -> DialFuture {
        let endpoint = self.endpoint.clone();
        let alpn = self
            .alpns
            .first()
            .map(|v| v.as_slice())
            .unwrap_or(b"n0/navar")
            .to_vec();
        let cache = self.connection_cache.clone();

        let fut = async move {
            let conn = endpoint
                .connect(peer_id, &alpn)
                .await
                .map_err(|e| Arc::new(e.into()))?;

            let monitor_conn = conn.clone();
            tokio::spawn(async move {
                let _ = monitor_conn.closed().await;
                cache.remove(&peer_id);
            });

            Ok(conn)
        };

        let dyn_fut: DynFuture = Box::pin(fut);
        dyn_fut.shared()
    }
}

impl TransportPlugin for IrohTransport {
    type Conn = IrohConnection;

    /// Connects to a remote peer identified by an Iroh public key.
    ///
    /// The public key is extracted from the URI host component.
    async fn connect(&self, uri: &Uri) -> anyhow::Result<Self::Conn> {
        let pubkey_str = uri.host().context("URI missing host")?;
        let remote_id = EndpointId::from_str(pubkey_str).context("Invalid Iroh Public Key")?;
        let connection = self.get_connection(remote_id).await?;
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

    /// Returns the negotiated ALPN protocol.
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

    /// Splits the bidirectional stream into send and receive halves.
    fn split(self) -> (Self::Send, Self::Recv) {
        (self.send, self.recv)
    }

    /// ALPN is negotiated at the connection level.
    fn alpn_protocol(&self) -> Option<&[u8]> {
        None
    }
}

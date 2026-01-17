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
use iroh::{EndpointId, endpoint::Connection, endpoint::Endpoint};
use navar::{
    futures_lite::{AsyncRead, AsyncWrite},
    http::Uri,
    transport::{TransportIo, TransportPlugin},
};
use tokio_util::compat::{Compat, TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

// Define the inner future type explicitly to help the compiler
type DynFuture = Pin<Box<dyn Future<Output = Result<Connection, Arc<anyhow::Error>>> + Send>>;
type DialFuture = Shared<DynFuture>;

#[derive(Clone, Debug)]
pub struct IrohTransport {
    endpoint: Endpoint,
    alpns: Vec<Vec<u8>>,
    connection_cache: Arc<DashMap<EndpointId, DialFuture>>,
}

impl IrohTransport {
    pub fn new(endpoint: Endpoint, alpns: Vec<Vec<u8>>) -> Self {
        Self {
            endpoint,
            alpns,
            connection_cache: Arc::new(DashMap::new()),
        }
    }

    async fn get_connection(&self, peer_id: EndpointId) -> anyhow::Result<Connection> {
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

    fn create_dial_future(&self, peer_id: EndpointId) -> DialFuture {
        let endpoint = self.endpoint.clone();
        // Ensure ALPN is owned for the 'move' closure
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

        // Explicitly cast to the specific DynFuture type BEFORE calling .shared()
        let dyn_fut: DynFuture = Box::pin(fut);
        dyn_fut.shared()
    }
}

impl TransportPlugin for IrohTransport {
    type Io = IrohStream;

    async fn connect(&self, uri: &Uri) -> anyhow::Result<Self::Io> {
        let pubkey_str = uri.host().context("URI missing host (Iroh Public Key)")?;
        let remote_id = EndpointId::from_str(pubkey_str).context("Invalid Iroh Public Key")?;

        let connection = self.get_connection(remote_id).await?;

        let (send, recv) = connection
            .open_bi()
            .await
            .context("Failed to open stream")?;

        Ok(IrohStream::new(send, recv))
    }
}

pub struct IrohStream {
    send: Compat<iroh::endpoint::SendStream>,
    recv: Compat<iroh::endpoint::RecvStream>,
}

impl IrohStream {
    pub fn new(send: iroh::endpoint::SendStream, recv: iroh::endpoint::RecvStream) -> Self {
        Self {
            send: send.compat_write(),
            recv: recv.compat(),
        }
    }
}

impl AsyncRead for IrohStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.get_mut().recv).poll_read(cx, buf)
    }
}

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

impl TransportIo for IrohStream {}

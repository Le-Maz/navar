use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

use navar::{
    AsyncRuntime,
    application::ApplicationPlugin,
    http::Uri,
    http_body_util::{BodyExt, Empty},
};
use navar::{
    NormalizedBody,
    application::Session,
    http::{Request, Response},
    http_body::Body,
    transport::{BidiStream, Connection, Stream, TransportPlugin},
};
use navar::{
    anyhow,
    futures_lite::{AsyncRead, AsyncWrite},
};

#[derive(Clone)]
pub struct MockStream;
impl Stream for MockStream {
    fn stream_id(&self) -> u64 {
        0
    }
}
impl AsyncRead for MockStream {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        Poll::Ready(Ok(0))
    }
}
impl AsyncWrite for MockStream {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Poll::Ready(Ok(buf.len()))
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
impl BidiStream for MockStream {
    type Send = MockStream;
    type Recv = MockStream;
    fn split(self) -> (Self::Send, Self::Recv) {
        (self.clone(), self.clone())
    }
}

#[derive(Clone)]
pub struct MockConnection;
impl Connection for MockConnection {
    type Bidi = MockStream;
    type Send = MockStream;
    type Recv = MockStream;

    async fn open_bidirectional(&self) -> anyhow::Result<Self::Bidi> {
        Ok(MockStream)
    }
    async fn open_unidirectional(&self) -> anyhow::Result<Self::Send> {
        Ok(MockStream)
    }
    async fn accept_bidirectional(&self) -> anyhow::Result<Self::Bidi> {
        std::future::pending().await
    }
    async fn accept_unidirectional(&self) -> anyhow::Result<Self::Recv> {
        std::future::pending().await
    }
}

#[derive(Clone)]
pub struct MockTransport;
impl TransportPlugin for MockTransport {
    type Conn = MockConnection;
    async fn connect(&self, _uri: &Uri) -> anyhow::Result<Self::Conn> {
        Ok(MockConnection)
    }
}

#[derive(Clone)]
pub struct MockSession;
impl Session for MockSession {
    type ResBody = NormalizedBody;

    async fn send_request<B>(
        &mut self,
        _request: Request<B>,
    ) -> anyhow::Result<Response<Self::ResBody>>
    where
        B: Body + Send + Sync + 'static + Unpin,
        B::Data: Send + Sync,
        B::Error: Into<anyhow::Error>,
    {
        Ok(Response::new(
            Empty::new().map_err(|e| anyhow::anyhow!(e)).boxed(),
        ))
    }
}

#[derive(Clone)]
pub struct MockApp;
impl ApplicationPlugin<MockConnection> for MockApp {
    type Session = MockSession;
    async fn handshake(
        &self,
        _conn: MockConnection,
    ) -> anyhow::Result<(Self::Session, impl Future<Output = ()> + Send + 'static)> {
        Ok((MockSession, async {}))
    }
}

pub struct MockRuntime;
impl AsyncRuntime for MockRuntime {
    fn spawn<F>(&self, _future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
    }
}

use std::{
    convert::TryFrom,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use h3::quic::{self, ConnectionErrorIncoming, StreamErrorIncoming};

use navar::bytes::{Buf, Bytes, BytesMut};

use navar::transport::{BidiStream, Connection, RecvStream, SendStream};

pub(crate) type BoxFut<T> = Pin<Box<dyn Future<Output = anyhow::Result<T>> + Send>>;

pub(crate) fn conn_closed() -> ConnectionErrorIncoming {
    // Matches the compiler hint: ApplicationClose { error_code: ... }
    ConnectionErrorIncoming::ApplicationClose {
        error_code: h3::error::Code::H3_NO_ERROR.value(),
    }
}

pub(crate) fn stream_error() -> StreamErrorIncoming {
    // Wraps the connection error since StreamClosed is not available
    StreamErrorIncoming::ConnectionErrorIncoming {
        connection_error: conn_closed(),
    }
}

/// Adapts a generic Navar `Connection` to the `h3::quic::Connection` trait.
pub struct H3Connection<C: Connection> {
    pub(crate) inner: C,
    pub(crate) accept_bidi_fut: Option<BoxFut<C::Bidi>>,
    pub(crate) accept_uni_fut: Option<BoxFut<C::Recv>>,
    pub(crate) open_bidi_fut: Option<BoxFut<C::Bidi>>,
    pub(crate) open_uni_fut: Option<BoxFut<C::Send>>,
}

impl<C: Connection> H3Connection<C> {
    pub fn new(inner: C) -> Self {
        Self {
            inner,
            accept_bidi_fut: None,
            accept_uni_fut: None,
            open_bidi_fut: None,
            open_uni_fut: None,
        }
    }
}

impl<C: Connection> Clone for H3Connection<C> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            accept_bidi_fut: None,
            accept_uni_fut: None,
            open_bidi_fut: None,
            open_uni_fut: None,
        }
    }
}

// 1. Implement OpenStreams
impl<C: Connection> quic::OpenStreams<Bytes> for H3Connection<C> {
    type BidiStream = H3BidiStream<C::Bidi>;
    type SendStream = H3SendStream<C::Send>;

    // Removed RecvStream type definition here as per error

    fn poll_open_bidi(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Self::BidiStream, StreamErrorIncoming>> {
        loop {
            if self.open_bidi_fut.is_none() {
                let conn = self.inner.clone();
                self.open_bidi_fut = Some(Box::pin(async move { conn.open_bidirectional().await }));
            }

            if let Some(fut) = self.open_bidi_fut.as_mut() {
                match fut.as_mut().poll(cx) {
                    Poll::Ready(Ok(stream)) => {
                        self.open_bidi_fut = None;
                        return Poll::Ready(Ok(H3BidiStream::new(stream)));
                    }
                    Poll::Ready(Err(_)) => {
                        self.open_bidi_fut = None;
                        return Poll::Ready(Err(stream_error()));
                    }
                    Poll::Pending => return Poll::Pending,
                }
            }
        }
    }

    fn poll_open_send(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Self::SendStream, StreamErrorIncoming>> {
        loop {
            if self.open_uni_fut.is_none() {
                let conn = self.inner.clone();
                self.open_uni_fut = Some(Box::pin(async move { conn.open_unidirectional().await }));
            }

            if let Some(fut) = self.open_uni_fut.as_mut() {
                match fut.as_mut().poll(cx) {
                    Poll::Ready(Ok(stream)) => {
                        self.open_uni_fut = None;
                        return Poll::Ready(Ok(H3SendStream::new(stream)));
                    }
                    Poll::Ready(Err(_)) => {
                        self.open_uni_fut = None;
                        return Poll::Ready(Err(stream_error()));
                    }
                    Poll::Pending => return Poll::Pending,
                }
            }
        }
    }

    fn close(&mut self, _code: h3::error::Code, _reason: &[u8]) {}
}

// 2. Implement Connection
impl<C: Connection> quic::Connection<Bytes> for H3Connection<C> {
    type RecvStream = H3RecvStream<C::Recv>;
    type OpenStreams = Self;

    fn poll_accept_bidi(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Self::BidiStream, ConnectionErrorIncoming>> {
        loop {
            if self.accept_bidi_fut.is_none() {
                let conn = self.inner.clone();
                self.accept_bidi_fut =
                    Some(Box::pin(async move { conn.accept_bidirectional().await }));
            }

            if let Some(fut) = self.accept_bidi_fut.as_mut() {
                match fut.as_mut().poll(cx) {
                    Poll::Ready(Ok(stream)) => {
                        self.accept_bidi_fut = None;
                        return Poll::Ready(Ok(H3BidiStream::new(stream)));
                    }
                    Poll::Ready(Err(_)) => {
                        self.accept_bidi_fut = None;
                        return Poll::Ready(Err(conn_closed()));
                    }
                    Poll::Pending => return Poll::Pending,
                }
            }
        }
    }

    fn poll_accept_recv(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Self::RecvStream, ConnectionErrorIncoming>> {
        loop {
            if self.accept_uni_fut.is_none() {
                let conn = self.inner.clone();
                self.accept_uni_fut =
                    Some(Box::pin(async move { conn.accept_unidirectional().await }));
            }

            if let Some(fut) = self.accept_uni_fut.as_mut() {
                match fut.as_mut().poll(cx) {
                    Poll::Ready(Ok(stream)) => {
                        self.accept_uni_fut = None;
                        return Poll::Ready(Ok(H3RecvStream::new(stream)));
                    }
                    Poll::Ready(Err(_)) => {
                        self.accept_uni_fut = None;
                        return Poll::Ready(Err(conn_closed()));
                    }
                    Poll::Pending => return Poll::Pending,
                }
            }
        }
    }

    fn opener(&self) -> Self::OpenStreams {
        self.clone()
    }
}

pub struct H3RecvStream<S> {
    pub(crate) inner: S,
}

impl<S> H3RecvStream<S> {
    pub fn new(inner: S) -> Self {
        Self { inner }
    }
}

impl<S: RecvStream> quic::RecvStream for H3RecvStream<S> {
    type Buf = Bytes;

    fn poll_data(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Option<Self::Buf>, StreamErrorIncoming>> {
        let mut buf = vec![0u8; 4096];
        match Pin::new(&mut self.inner).poll_read(cx, &mut buf) {
            Poll::Ready(Ok(0)) => Poll::Ready(Ok(None)),
            Poll::Ready(Ok(n)) => {
                buf.truncate(n);
                Poll::Ready(Ok(Some(Bytes::from(buf))))
            }
            Poll::Ready(Err(_)) => Poll::Ready(Err(stream_error())),
            Poll::Pending => Poll::Pending,
        }
    }

    fn stop_sending(&mut self, _error_code: u64) {}

    fn recv_id(&self) -> h3::quic::StreamId {
        h3::quic::StreamId::try_from(self.inner.stream_id()).unwrap_or_else(|_| {
            // Should not happen unless ID exceeds 62 bits
            h3::quic::StreamId::try_from(0u64).unwrap()
        })
    }
}

pub struct H3SendStream<S> {
    pub(crate) inner: S,
    pub(crate) buf: BytesMut,
}

impl<S> H3SendStream<S> {
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            buf: BytesMut::new(),
        }
    }
}

impl<S: SendStream> quic::SendStream<Bytes> for H3SendStream<S> {
    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), StreamErrorIncoming>> {
        while !self.buf.is_empty() {
            match Pin::new(&mut self.inner).poll_write(cx, &self.buf) {
                Poll::Ready(Ok(n)) => {
                    self.buf.advance(n);
                }
                Poll::Ready(Err(_)) => return Poll::Ready(Err(stream_error())),
                Poll::Pending => return Poll::Pending,
            }
        }
        Poll::Ready(Ok(()))
    }

    fn send_data<D: Into<quic::WriteBuf<Bytes>>>(
        &mut self,
        data: D,
    ) -> Result<(), StreamErrorIncoming> {
        let mut data = data.into();
        while data.has_remaining() {
            let chunk = data.copy_to_bytes(data.remaining());
            self.buf.extend_from_slice(&chunk);
        }
        Ok(())
    }

    fn poll_finish(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), StreamErrorIncoming>> {
        match self.poll_ready(cx) {
            Poll::Ready(Ok(())) => {}
            other => return other,
        }
        match Pin::new(&mut self.inner).poll_flush(cx) {
            Poll::Ready(Ok(())) => Poll::Ready(Ok(())),
            Poll::Ready(Err(_)) => Poll::Ready(Err(stream_error())),
            Poll::Pending => Poll::Pending,
        }
    }

    fn reset(&mut self, _reset_code: u64) {}

    fn send_id(&self) -> h3::quic::StreamId {
        h3::quic::StreamId::try_from(self.inner.stream_id())
            .unwrap_or_else(|_| h3::quic::StreamId::try_from(0u64).unwrap())
    }
}

pub struct H3BidiStream<S> {
    pub(crate) inner: S,
    pub(crate) send_buf: BytesMut,
}

impl<S> H3BidiStream<S> {
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            send_buf: BytesMut::new(),
        }
    }
}

impl<S: BidiStream> quic::RecvStream for H3BidiStream<S> {
    type Buf = Bytes;
    fn poll_data(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Option<Self::Buf>, StreamErrorIncoming>> {
        let mut buf = vec![0u8; 4096];
        match Pin::new(&mut self.inner).poll_read(cx, &mut buf) {
            Poll::Ready(Ok(0)) => Poll::Ready(Ok(None)),
            Poll::Ready(Ok(n)) => {
                buf.truncate(n);
                Poll::Ready(Ok(Some(Bytes::from(buf))))
            }
            Poll::Ready(Err(_)) => Poll::Ready(Err(stream_error())),
            Poll::Pending => Poll::Pending,
        }
    }

    fn stop_sending(&mut self, _error_code: u64) {}

    fn recv_id(&self) -> h3::quic::StreamId {
        h3::quic::StreamId::try_from(self.inner.stream_id()).unwrap()
    }
}

impl<S: BidiStream> quic::SendStream<Bytes> for H3BidiStream<S> {
    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), StreamErrorIncoming>> {
        while !self.send_buf.is_empty() {
            match Pin::new(&mut self.inner).poll_write(cx, &self.send_buf) {
                Poll::Ready(Ok(n)) => {
                    self.send_buf.advance(n);
                }
                Poll::Ready(Err(_)) => return Poll::Ready(Err(stream_error())),
                Poll::Pending => return Poll::Pending,
            }
        }
        Poll::Ready(Ok(()))
    }

    fn send_data<D: Into<quic::WriteBuf<Bytes>>>(
        &mut self,
        data: D,
    ) -> Result<(), StreamErrorIncoming> {
        let mut data = data.into();
        while data.has_remaining() {
            let chunk = data.copy_to_bytes(data.remaining());
            self.send_buf.extend_from_slice(&chunk);
        }
        Ok(())
    }

    fn poll_finish(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), StreamErrorIncoming>> {
        match self.poll_ready(cx) {
            Poll::Ready(Ok(())) => {}
            other => return other,
        }
        match Pin::new(&mut self.inner).poll_flush(cx) {
            Poll::Ready(Ok(())) => Poll::Ready(Ok(())),
            Poll::Ready(Err(_)) => Poll::Ready(Err(stream_error())),
            Poll::Pending => Poll::Pending,
        }
    }

    fn reset(&mut self, _reset_code: u64) {}

    fn send_id(&self) -> h3::quic::StreamId {
        h3::quic::StreamId::try_from(self.inner.stream_id()).unwrap()
    }
}

impl<S: BidiStream> quic::BidiStream<Bytes> for H3BidiStream<S> {
    type SendStream = H3SendStream<S::Send>;
    type RecvStream = H3RecvStream<S::Recv>;
    fn split(self) -> (Self::SendStream, Self::RecvStream) {
        let (send, recv) = self.inner.split();
        (H3SendStream::new(send), H3RecvStream::new(recv))
    }
}

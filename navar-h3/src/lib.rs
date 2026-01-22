//! # HTTP/3 Application Plugin
//!
//! This module provides an [`ApplicationPlugin`] implementation for HTTP/3,
//! built on top of the `h3` crate.
//!
//! It integrates with `navar` by adapting a generic [`Connection`] into an
//! HTTP/3-capable QUIC transport using the adapter layer.

mod adapter;

use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use adapter::{H3BidiStream, H3Connection};
use navar::{
    application::{ApplicationPlugin, Session},
    bytes::{Buf, Bytes},
    http::{Request, Response},
    http_body::{Body, Frame},
    http_body_util::BodyExt,
    transport::Connection,
};

/// HTTP/3 application plugin.
#[derive(Clone, Default)]
pub struct H3App;

impl H3App {
    /// Creates a new `H3App` instance.
    pub fn new() -> Self {
        Self
    }
}

impl<C> ApplicationPlugin<C> for H3App
where
    C: Connection,
{
    type Session = H3Session<C>;

    async fn handshake(
        &self,
        conn: C,
    ) -> anyhow::Result<(Self::Session, impl Future<Output = ()> + Send + 'static)> {
        let h3_conn = H3Connection::new(conn);
        let (mut driver, sender) = h3::client::new(h3_conn).await?;
        let session = H3Session { sender };

        let driver_fut = async move {
            let err = driver.wait_idle().await;
            if !err.is_h3_no_error() {
                eprintln!("H3 Client Driver Error: {}", err);
            }
        };

        Ok((session, driver_fut))
    }
}

/// Active HTTP/3 session.
pub struct H3Session<C: Connection> {
    sender: h3::client::SendRequest<H3Connection<C>, Bytes>,
}

impl<C: Connection> Session for H3Session<C> {
    type ResBody = H3ResponseBody<C>;

    async fn send_request<B>(
        &mut self,
        request: Request<B>,
    ) -> anyhow::Result<Response<Self::ResBody>>
    where
        B: Body + Send + Sync + 'static + Unpin,
        B::Data: Send + Sync,
        B::Error: Into<anyhow::Error>,
    {
        let (parts, body) = request.into_parts();
        let req = Request::from_parts(parts, ());
        let mut stream = self.sender.send_request(req).await?;
        let mut body = body.map_err(|err| err.into());

        while let Some(frame_res) = body.frame().await {
            match frame_res {
                Ok(frame) => {
                    if let Ok(data) = frame.into_data() {
                        let chunk = data.chunk();
                        let bytes = Bytes::copy_from_slice(chunk);
                        stream.send_data(bytes).await?;
                    }
                }
                Err(err) => {
                    return Err(err.into());
                }
            };
        }

        stream.finish().await?;
        let response = stream.recv_response().await?;

        Ok(response.map(|_| H3ResponseBody { stream }))
    }
}

/// A wrapper error type that implements `std::error::Error`.
///
/// This is required because `Box<dyn Error ...>` does not automatically
/// implement `Error`, and `anyhow::Error` does not implement `Error`.
#[derive(Debug)]
pub struct H3Error(anyhow::Error);

impl std::fmt::Display for H3Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl std::error::Error for H3Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.0.source()
    }
}

/// HTTP response body backed by an HTTP/3 request stream.
pub struct H3ResponseBody<C: Connection> {
    stream: h3::client::RequestStream<H3BidiStream<C::Bidi>, Bytes>,
}

impl<C: Connection> Body for H3ResponseBody<C> {
    type Data = Bytes;
    type Error = H3Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        match self.stream.poll_recv_data(cx) {
            Poll::Ready(Ok(Some(mut bytes))) => {
                let chunk = bytes.copy_to_bytes(bytes.remaining());
                Poll::Ready(Some(Ok(Frame::data(chunk))))
            }
            Poll::Ready(Ok(None)) => Poll::Ready(None),
            // Map the h3 error into our wrapper type
            Poll::Ready(Err(e)) => Poll::Ready(Some(Err(H3Error(e.into())))),
            Poll::Pending => Poll::Pending,
        }
    }
}

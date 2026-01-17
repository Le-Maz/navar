//! # HTTP/3 Application Plugin
//!
//! This module provides an [`ApplicationPlugin`] implementation for HTTP/3,
//! built on top of the `h3` crate.
//!
//! It integrates with `navar` by adapting a generic [`Connection`] into an
//! HTTP/3-capable QUIC transport using the adapter layer.
//!
//! ## Responsibilities
//!
//! - Perform the HTTP/3 client handshake
//! - Drive the H3 connection lifecycle
//! - Expose an HTTP [`Session`] abstraction
//! - Convert between `h3` streams and `http_body::Body`
//!
//! ## Execution model
//!
//! The plugin returns:
//!
//! - A [`Session`] for sending requests
//! - A **driver future** that must be spawned by the runtime to
//!   drive the HTTP/3 state machine
//!
//! Failure to poll the driver future will stall the connection.

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
///
/// This plugin establishes an HTTP/3 client connection over a `navar`
/// [`Connection`] and produces an active [`Session`] capable of
/// sending HTTP requests.
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
        // 1. Wrap the raw connection using the HTTP/3 adapter layer
        let h3_conn = H3Connection::new(conn);

        // 2. Perform the HTTP/3 client handshake
        let (mut driver, sender) = h3::client::new(h3_conn).await?;

        // 3. Create the session handle
        let session = H3Session { sender };

        // 4. Driver future responsible for advancing the H3 connection
        let driver_fut = async move {
            // wait_idle resolves when the connection is closed
            let err = driver.wait_idle().await;

            // Ignore graceful shutdowns
            if !err.is_h3_no_error() {
                eprintln!("H3 Client Driver Error: {}", err);
            }
        };

        Ok((session, driver_fut))
    }
}

/// Active HTTP/3 session.
///
/// This type implements [`Session`] and allows sending HTTP requests
/// over an established HTTP/3 connection.
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
        B::Data: Send,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        // 1. Split request into parts and body
        let (parts, body) = request.into_parts();
        let req = Request::from_parts(parts, ());

        // 2. Open an HTTP/3 request stream
        let mut stream = self.sender.send_request(req).await?;

        // 3. Stream request body frames
        let mut body = body.map_err(Into::into);

        while let Some(frame_res) = body.frame().await {
            match frame_res {
                Ok(frame) => {
                    if let Ok(data) = frame.into_data() {
                        // Convert `Buf` into `Bytes`
                        let chunk = data.chunk();
                        let bytes = Bytes::copy_from_slice(chunk);
                        stream.send_data(bytes).await?;
                    }
                }
                Err(err) => {
                    return Err(anyhow::Error::from_boxed(err));
                }
            }
        }

        // Signal end-of-stream to the peer
        stream.finish().await?;

        // 4. Await response headers
        let response = stream.recv_response().await?;

        Ok(response.map(|_| H3ResponseBody { stream }))
    }
}

/// HTTP response body backed by an HTTP/3 request stream.
///
/// This type adapts an `h3::client::RequestStream` into the standard
/// [`http_body::Body`] interface.
pub struct H3ResponseBody<C: Connection> {
    /// Underlying HTTP/3 request stream.
    stream: h3::client::RequestStream<H3BidiStream<C::Bidi>, Bytes>,
}

impl<C: Connection> Body for H3ResponseBody<C> {
    type Data = Bytes;
    type Error = anyhow::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        match self.stream.poll_recv_data(cx) {
            Poll::Ready(Ok(Some(mut bytes))) => {
                // Convert h3's buffer into `Bytes`
                let chunk = bytes.copy_to_bytes(bytes.remaining());
                Poll::Ready(Some(Ok(Frame::data(chunk))))
            }
            Poll::Ready(Ok(None)) => Poll::Ready(None), // EOF
            Poll::Ready(Err(e)) => Poll::Ready(Some(Err(e.into()))),
            Poll::Pending => Poll::Pending,
        }
    }
}

//! # Hyper Application Plugin
//!
//! This module provides an `ApplicationPlugin` implementation built on top of
//! the `hyper` HTTP client connection machinery.
//!
//! It supports:
//!
//! - HTTP/1.1 and HTTP/2
//! - Automatic protocol selection via ALPN
//! - Integration with `navar` transports through `BidiStream`
//! - Runtime-agnostic operation using Tokio compatibility layers
//!
//! This plugin is intended to be used with transports such as:
//!
//! - Tokio TCP/TLS transport
//! - Iroh QUIC transport
//!
//! The plugin produces a `Session` that can send HTTP requests and receive
//! responses using Hyper’s client implementation.

use std::pin::Pin;
use std::{future::Future, sync::Arc};

use hyper::client::conn::{http1, http2};
use hyper_util::rt::{TokioExecutor, TokioIo};
use navar::{NormalizedBody, NormalizedData};
use tokio::sync::Mutex;
use tokio_util::compat::{Compat, FuturesAsyncReadCompatExt};

use navar::{
    application::{ApplicationPlugin, Session},
    http::{Request, Response},
    http_body::Body,
    http_body_util::BodyExt,
    transport::{BidiStream, Connection},
};

/// Response body type returned by Hyper.
type HyperResBody = hyper::body::Incoming;

/// Boxed background driver future returned by the handshake.
type BoxedFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// Supported HTTP protocol versions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Protocol {
    /// Automatically select the protocol (default).
    ///
    /// HTTP/2 is preferred when ALPN negotiation indicates support,
    /// otherwise HTTP/1.1 is used.
    #[default]
    Auto,

    /// Force HTTP/1.1.
    Http1,

    /// Force HTTP/2.
    Http2,
}

/// Bridges a `navar` bidirectional stream into a Tokio-compatible I/O object.
///
/// This allows Hyper to operate over arbitrary transports implementing
/// [`BidiStream`].
fn prepare_io<I: BidiStream>(io: I) -> TokioIo<Compat<I>> {
    TokioIo::new(io.compat())
}

/// Converts an arbitrary request body into Hyper’s boxed body type.
///
/// This ensures a uniform body representation regardless of the original
/// body type used by the caller.
fn convert_body<B>(body: B) -> NormalizedBody
where
    B: Body + Send + Sync + Unpin + 'static,
    B::Data: Send + Sync,
    B::Error: Into<anyhow::Error>,
{
    body.map_frame(|frame| frame.map_data(|data| Box::new(data) as NormalizedData))
        .map_err(|e| e.into())
        .boxed()
}

/// A unified HTTP sender wrapping either an HTTP/1.1 or HTTP/2 Hyper connection.
#[derive(Clone)]
pub enum HyperSender {
    /// HTTP/1.1 sender.
    H1(Arc<Mutex<http1::SendRequest<NormalizedBody>>>),

    /// HTTP/2 sender.
    H2(http2::SendRequest<NormalizedBody>),
}

impl Session for HyperSender {
    type ResBody = HyperResBody;

    async fn send_request<B>(
        &mut self,
        request: Request<B>,
    ) -> anyhow::Result<Response<Self::ResBody>>
    where
        B: Body + Send + Sync + Unpin + 'static,
        B::Data: Send + Sync,
        B::Error: Into<anyhow::Error>,
    {
        // Convert the request body into the boxed Hyper body type
        let (parts, body) = request.into_parts();
        let boxed_body = convert_body(body);
        let req = Request::from_parts(parts, boxed_body);

        // Dispatch based on the negotiated protocol
        match self {
            HyperSender::H1(sender) => {
                let mut sender = sender.lock().await;
                sender.ready().await?;
                Ok(sender.send_request(req).await?)
            }
            HyperSender::H2(sender) => {
                sender.ready().await?;
                Ok(sender.send_request(req).await?)
            }
        }
    }
}

/// Hyper-based application plugin.
///
/// This plugin performs an HTTP handshake over a `navar` connection and
/// produces a [`Session`] capable of sending HTTP requests.
#[derive(Clone, Default)]
pub struct HyperApp {
    protocol: Protocol,
}

impl HyperApp {
    /// Creates a new `HyperApp` using automatic protocol selection.
    pub fn new() -> Self {
        Self::default()
    }

    /// Configures the HTTP protocol version to use.
    pub fn with_protocol(mut self, protocol: Protocol) -> Self {
        self.protocol = protocol;
        self
    }
}

impl<C> ApplicationPlugin<C> for HyperApp
where
    C: Connection,
    C::Bidi: BidiStream + Unpin,
{
    type Session = HyperSender;

    async fn handshake(
        &self,
        conn: C,
    ) -> anyhow::Result<(Self::Session, impl Future<Output = ()> + Send + 'static)> {
        // 1. Open a bidirectional stream on the connection
        let io = conn.open_bidirectional().await?;

        // 2. Determine the protocol based on configuration and ALPN
        let alpn = io.alpn_protocol();
        let use_h2 = match (self.protocol, alpn) {
            (Protocol::Http2, _) => true,
            (Protocol::Http1, _) => false,
            (Protocol::Auto, Some(b"h2")) => true,
            (Protocol::Auto, _) => false,
        };

        // 3. Adapt the stream for Tokio/Hyper
        let hyper_io = prepare_io(io);

        // 4. Perform the Hyper handshake
        if use_h2 {
            let executor = TokioExecutor::new();
            let (sender, conn) = http2::Builder::new(executor).handshake(hyper_io).await?;

            let session = HyperSender::H2(sender);
            let driver = Box::pin(async move {
                if let Err(e) = conn.await {
                    eprintln!("Hyper HTTP/2 connection error: {:?}", e);
                }
            });

            Ok((session, driver as BoxedFuture))
        } else {
            let (sender, conn) = http1::handshake(hyper_io).await?;

            let session = HyperSender::H1(Arc::new(Mutex::new(sender)));
            let driver = Box::pin(async move {
                if let Err(e) = conn.await {
                    eprintln!("Hyper HTTP/1.1 connection error: {:?}", e);
                }
            });

            Ok((session, driver as BoxedFuture))
        }
    }
}

use std::future::Future;
use std::{error::Error as StdError, pin::Pin};

use hyper::{
    body::Buf,
    client::conn::{http1, http2},
};
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio_util::compat::{Compat, FuturesAsyncReadCompatExt};

use navar::{
    application::{ApplicationPlugin, Session},
    http::{Request, Response},
    http_body::Body,
    http_body_util::BodyExt,
    transport::{BidiStream, Connection},
};

type DynBody = navar::http_body_util::combinators::BoxBody<
    navar::bytes::Bytes,
    Box<dyn StdError + Send + Sync>,
>;

type HyperResBody = hyper::body::Incoming;

type BoxedFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Protocol {
    #[default]
    Auto,
    Http1,
    Http2,
}

/// Helper to bridge navar/futures-lite IO to Tokio IO
fn prepare_io<I: BidiStream>(io: I) -> TokioIo<Compat<I>> {
    TokioIo::new(io.compat())
}

/// Helper to box any valid body into our DynBody type
fn convert_body<B>(body: B) -> DynBody
where
    B: Body + Send + Sync + Unpin + 'static,
    B::Data: Send,
    B::Error: Into<Box<dyn StdError + Send + Sync + 'static>>,
{
    body.map_frame(|frame| frame.map_data(|mut data| data.copy_to_bytes(data.remaining())))
        .map_err(|e| e.into())
        .boxed()
}

/// A unified sender that wraps either an H1 or H2 connection.
pub enum HyperSender {
    H1(http1::SendRequest<DynBody>),
    H2(http2::SendRequest<DynBody>),
}

impl Session for HyperSender {
    type ResBody = HyperResBody;

    async fn send_request<B>(
        &mut self,
        request: Request<B>,
    ) -> anyhow::Result<Response<Self::ResBody>>
    where
        B: Body + Send + Sync + Unpin + 'static,
        B::Data: Send,
        B::Error: Into<Box<dyn StdError + Send + Sync + 'static>>,
    {
        // 1. Convert the body to the DynBody type expected by Hyper
        let (parts, body) = request.into_parts();
        let boxed_body = convert_body(body);
        let req = Request::from_parts(parts, boxed_body);

        // 2. Dispatch based on the active protocol
        match self {
            HyperSender::H1(sender) => {
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

#[derive(Clone, Default)]
pub struct HyperApp {
    protocol: Protocol,
}

impl HyperApp {
    /// Create a new HyperApp with default configuration (Auto)
    pub fn new() -> Self {
        Self::default()
    }

    /// Configure the expected protocol version
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
        // 1. Obtain the stream from the connection handle.
        // For TCP, this performs the actual socket dial.
        // For Iroh/QUIC, this opens a bidirectional stream.
        let io = conn.open_bidirectional().await?;

        // 2. Determine which protocol to use based on ALPN (if available)
        let alpn = io.alpn_protocol();

        let use_h2 = match (self.protocol, alpn) {
            (Protocol::Http2, _) => true,
            (Protocol::Http1, _) => false,
            // If Auto, prefer H2 if ALPN says so, otherwise fallback to H1
            (Protocol::Auto, Some(b"h2")) => true,
            (Protocol::Auto, _) => false,
        };

        // 3. Wrap the stream in the Tokio compatibility layer
        let hyper_io = prepare_io(io);

        // 4. Perform the Hyper handshake
        if use_h2 {
            let executor = TokioExecutor::new();
            let (sender, conn) = http2::Builder::new(executor).handshake(hyper_io).await?;

            let session = HyperSender::H2(sender);

            let driver = Box::pin(async move {
                if let Err(e) = conn.await {
                    eprintln!("Hyper H2 connection error: {:?}", e);
                }
            });

            Ok((session, driver as BoxedFuture))
        } else {
            let (sender, conn) = http1::handshake(hyper_io).await?;

            let session = HyperSender::H1(sender);

            let driver = Box::pin(async move {
                if let Err(e) = conn.await {
                    eprintln!("Hyper H1 connection error: {:?}", e);
                }
            });

            Ok((session, driver as BoxedFuture))
        }
    }
}

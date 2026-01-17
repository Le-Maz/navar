use std::error::Error as StdError;
use std::future::Future;

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
    transport::TransportIo,
};

type DynBody = navar::http_body_util::combinators::BoxBody<
    navar::bytes::Bytes,
    Box<dyn StdError + Send + Sync>,
>;

type HyperResBody = hyper::body::Incoming;

/// Centralizes the logic for mapping any valid Body to our DynBody
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

/// Centralizes the IO compatibility layer
fn prepare_io<I: TransportIo>(io: I) -> TokioIo<Compat<I>> {
    TokioIo::new(io.compat())
}

/// A helper trait to unify http1::SendRequest and http2::SendRequest.
trait HyperSender: Send + 'static {
    fn ready(&mut self) -> impl Future<Output = hyper::Result<()>> + Send;
    fn send_request(
        &mut self,
        req: Request<DynBody>,
    ) -> impl Future<Output = hyper::Result<Response<HyperResBody>>> + Send;
}

impl HyperSender for http1::SendRequest<DynBody> {
    async fn ready(&mut self) -> hyper::Result<()> {
        http1::SendRequest::ready(self).await
    }

    async fn send_request(
        &mut self,
        req: Request<DynBody>,
    ) -> hyper::Result<Response<HyperResBody>> {
        http1::SendRequest::send_request(self, req).await
    }
}

impl HyperSender for http2::SendRequest<DynBody> {
    async fn ready(&mut self) -> hyper::Result<()> {
        http2::SendRequest::ready(self).await
    }

    async fn send_request(
        &mut self,
        req: Request<DynBody>,
    ) -> hyper::Result<Response<HyperResBody>> {
        http2::SendRequest::send_request(self, req).await
    }
}

pub struct HyperSession<S> {
    sender: S,
}

impl<S: HyperSender> Session for HyperSession<S> {
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
        let (parts, body) = request.into_parts();
        let boxed_body = convert_body(body);
        let req = Request::from_parts(parts, boxed_body);

        self.sender.ready().await?;
        let resp = self.sender.send_request(req).await?;
        Ok(resp)
    }
}

#[derive(Clone, Default)]
pub struct HyperH1App;

impl ApplicationPlugin for HyperH1App {
    type Session = HyperSession<http1::SendRequest<DynBody>>;

    async fn handshake(
        &self,
        io: impl TransportIo,
    ) -> anyhow::Result<(Self::Session, impl Future<Output = ()> + Send + 'static)> {
        let hyper_io = prepare_io(io);
        let (sender, conn) = http1::handshake(hyper_io).await?;

        let session = HyperSession { sender };

        let driver = async move {
            if let Err(err) = conn.await {
                eprintln!("H1 Connection driver error: {:?}", err);
            }
        };

        Ok((session, driver))
    }
}

#[derive(Clone, Default)]
pub struct HyperH2App;

impl ApplicationPlugin for HyperH2App {
    type Session = HyperSession<http2::SendRequest<DynBody>>;

    async fn handshake(
        &self,
        io: impl TransportIo,
    ) -> anyhow::Result<(Self::Session, impl Future<Output = ()> + Send + 'static)> {
        let hyper_io = prepare_io(io);
        let executor = TokioExecutor::new();

        let (sender, conn) = http2::Builder::new(executor).handshake(hyper_io).await?;

        let session = HyperSession { sender };

        let driver = async move {
            if let Err(err) = conn.await {
                eprintln!("H2 Connection driver error: {:?}", err);
            }
        };

        Ok((session, driver))
    }
}

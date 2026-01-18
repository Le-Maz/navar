use crate::{BoxError, Dispatch, ResponseBody, transport::TransportPlugin};
use bytes::Buf;
use http::{Method, Request, Response, Uri, request::Builder as HttpBuilder};
use http_body::Body;
use http_body_util::Empty;
use std::future::Future;

/// A composite trait defining the requirements for HTTP request bodies.
///
/// This trait unifies the bounds needed by the client and application layers,
/// allowing generic request bodies while preserving thread-safety.
pub trait RequestBody:
    Body<Data = <Self as RequestBody>::Data, Error = <Self as RequestBody>::Error>
    + Send
    + Sync
    + Unpin
    + 'static
{
    /// The data chunk type produced by the body.
    type Data: Send;

    /// The error type produced by the body.
    type Error: Into<BoxError>;
}

/// Blanket implementation for all compatible body types.
impl<B> RequestBody for B
where
    B: Body + Send + Sync + Unpin + 'static,
    B::Data: Send,
    B::Error: Into<BoxError>,
{
    type Data = B::Data;
    type Error = B::Error;
}

/// Result type returned when sending an HTTP request.
#[allow(type_alias_bounds)]
pub type SendRequestResult<D: Dispatch> =
    anyhow::Result<Response<ResponseBody<D::App, <D::Transport as TransportPlugin>::Conn>>>;

/// A future returned by [`Dispatch::send`].
///
/// This trait exists to provide a named abstraction over the returned future
/// without boxing.
pub trait SendRequestFuture<D: Dispatch>: Future<Output = SendRequestResult<D>> + Send {}

impl<F, D: Dispatch> SendRequestFuture<D> for F where F: Future<Output = SendRequestResult<D>> + Send
{}

pub enum NeverBuf {}

impl Buf for NeverBuf {
    fn remaining(&self) -> usize {
        unreachable!()
    }

    fn chunk(&self) -> &[u8] {
        unreachable!()
    }

    fn advance(&mut self, _cnt: usize) {
        unreachable!()
    }
}

/// A builder for constructing an HTTP request bound to a dispatcher.
pub struct BoundRequestBuilder<D: Dispatch> {
    inner: HttpBuilder,
    client: D,
}

impl<D: Dispatch> BoundRequestBuilder<D> {
    pub fn new<U>(client: D, method: Method, uri: U) -> Self
    where
        U: TryInto<Uri>,
        http::Error: From<<U as TryInto<Uri>>::Error>,
    {
        let inner = Request::builder().method(method).uri(uri);
        Self { inner, client }
    }

    /// Appends a header to the request.
    ///
    /// This method allows adding headers to the request before the body is set.
    /// It wraps the standard [`http::request::Builder::header`] functionality.
    pub fn header<K, V>(mut self, key: K, value: V) -> Self
    where
        http::header::HeaderName: TryFrom<K>,
        <http::header::HeaderName as TryFrom<K>>::Error: Into<http::Error>,
        http::header::HeaderValue: TryFrom<V>,
        <http::header::HeaderValue as TryFrom<V>>::Error: Into<http::Error>,
    {
        self.inner = self.inner.header(key, value);
        self
    }

    /// Adds an extension to the request's type map.
    ///
    /// Extensions are used to pass extra data (like open telemetry spans,
    /// authentication tokens, or protocol-specific configuration) through
    /// the request lifecycle.
    pub fn extension<T>(mut self, extension: T) -> Self
    where
        T: Clone + Send + Sync + 'static,
    {
        self.inner = self.inner.extension(extension);
        self
    }

    /// Attaches a body to the request.
    pub fn body<B>(self, body: B) -> anyhow::Result<BoundRequest<B, D>>
    where
        B: RequestBody,
    {
        Ok(BoundRequest {
            request: self.inner.body(body)?,
            client: self.client,
        })
    }

    /// Builds the request with an empty body.
    pub fn build(self) -> anyhow::Result<BoundRequest<Empty<NeverBuf>, D>> {
        Ok(BoundRequest {
            request: self.inner.body(Empty::new())?,
            client: self.client,
        })
    }

    /// Builds and immediately sends the request.
    pub async fn send(self) -> SendRequestResult<D> {
        self.build()?.send().await
    }
}

/// A fully constructed HTTP request awaiting execution.
pub struct BoundRequest<B, D: Dispatch>
where
    B: RequestBody,
{
    request: Request<B>,
    client: D,
}

impl<B, D> BoundRequest<B, D>
where
    D: Dispatch,
    B: RequestBody,
{
    /// Executes the HTTP request.
    pub async fn send(self) -> SendRequestResult<D> {
        self.client.send(self.request).await
    }
}

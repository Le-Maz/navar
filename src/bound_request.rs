use crate::{Client, service::ResponseResult};
use bytes::Buf;
use http::{Method, Request, Uri, request::Builder as HttpBuilder};
use http_body::Body;
use http_body_util::Empty;

#[cfg(feature = "json")]
pub mod json;

/// A composite trait defining the requirements for HTTP request bodies.
pub trait RequestBody:
    Body<Data = <Self as RequestBody>::Data, Error = <Self as RequestBody>::Error>
    + Send
    + Sync
    + Unpin
    + 'static
{
    type Data: Buf + Send + Sync;
    type Error: Into<anyhow::Error>;
}

impl<B> RequestBody for B
where
    B: Body + Send + Sync + Unpin + 'static,
    B::Data: Buf + Send + Sync,
    B::Error: Into<anyhow::Error>,
{
    type Data = B::Data;
    type Error = B::Error;
}

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

/// A builder for constructing an HTTP request bound to a Client.
pub struct BoundRequestBuilder {
    inner: HttpBuilder,
    client: Client,
}

impl BoundRequestBuilder {
    pub fn new<U>(client: Client, method: Method, uri: U) -> Self
    where
        U: TryInto<Uri>,
        http::Error: From<<U as TryInto<Uri>>::Error>,
    {
        let inner = Request::builder().method(method).uri(uri);
        Self { inner, client }
    }

    /// Appends a header to the request.
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
    pub fn extension<T>(mut self, extension: T) -> Self
    where
        T: Clone + Send + Sync + 'static,
    {
        self.inner = self.inner.extension(extension);
        self
    }

    /// Attaches a body to the request.
    pub fn body<B>(self, body: B) -> anyhow::Result<BoundRequest<B>>
    where
        B: RequestBody,
    {
        Ok(BoundRequest {
            request: self.inner.body(body)?,
            client: self.client,
        })
    }

    /// Builds the request with an empty body.
    pub fn build(self) -> anyhow::Result<BoundRequest<Empty<NeverBuf>>> {
        Ok(BoundRequest {
            request: self.inner.body(Empty::new())?,
            client: self.client,
        })
    }

    /// Builds and immediately sends the request.
    pub async fn send(self) -> ResponseResult {
        self.build()?.send().await
    }
}

/// A fully constructed HTTP request awaiting execution.
pub struct BoundRequest<B>
where
    B: RequestBody,
{
    request: Request<B>,
    client: Client,
}

impl<B> BoundRequest<B>
where
    B: RequestBody,
{
    /// Executes the HTTP request via the bound Client.
    pub async fn send(self) -> ResponseResult {
        self.client.send(self.request).await
    }
}

#![doc = include_str!("../README.md")]

pub use anyhow;
pub use bytes;
pub use futures_lite;
pub use http;
pub use http_body;
pub use http_body_util;

use crate::{
    application::{ApplicationPlugin, Session},
    transport::TransportPlugin,
};
use bytes::Bytes;
use http::{Method, Request, Response, Uri, request::Builder as HttpBuilder};
use http_body::Body;
use http_body_util::Empty;
use std::future::Future;
use std::sync::Arc;

pub mod application;
pub mod transport;

/// A standard boxed error type used throughout the client.
///
/// This type is used as the common error representation for request bodies
/// and protocol layers.
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Helper type alias to extract the response body type from an application plugin.
///
/// This resolves to the concrete response body returned by the application
/// session associated with the given transport.
pub type ResponseBody<A, C> = <<A as ApplicationPlugin<C>>::Session as Session>::ResBody;

/// Defines the async runtime capabilities required by the client.
///
/// This abstraction allows the client to remain runtime-agnostic
/// (e.g. compatible with Tokio, async-std, or custom executors).
pub trait AsyncRuntime: Send + Sync + 'static {
    /// Spawns a future onto the background runtime.
    ///
    /// The spawned future is expected to run independently for the
    /// lifetime of the session or connection.
    fn spawn<F>(&self, future: F)
    where
        F: Future<Output = ()> + Send + 'static;
}

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

/// Defines the capability to dispatch HTTP requests.
///
/// This trait ties together:
///
/// - A transport (connection establishment)
/// - An application protocol (handshake + session)
/// - A runtime (background task execution)
pub trait Dispatch: Send + Sync + Clone {
    /// The transport mechanism (e.g. TCP, TLS, QUIC).
    type Transport: TransportPlugin;

    /// The application protocol (e.g. HTTP/1.1, HTTP/2, HTTP/3).
    ///
    /// The application is parameterized by the connection type produced
    /// by the transport.
    type App: ApplicationPlugin<<Self::Transport as TransportPlugin>::Conn>;

    /// The async runtime used for background tasks.
    type Runtime: AsyncRuntime;

    /// Connects to the remote peer, performs the application handshake,
    /// and sends a single HTTP request.
    fn send<B>(&self, request: Request<B>) -> impl SendRequestFuture<Self>
    where
        B: RequestBody;
}

struct ClientInner<T, A, R> {
    transport: T,
    app: A,
    runtime: R,
}

/// The primary HTTP client.
///
/// `Client` is a lightweight, clonable handle that shares an internal
/// transport, application plugin, and runtime.
pub struct Client<T, A, R> {
    inner: Arc<ClientInner<T, A, R>>,
}

impl<T, A, R> Clone for Client<T, A, R> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

macro_rules! http_method {
    ($name:ident, $variant:expr) => {
        #[doc = concat!("Initiates a `", stringify!($variant), "` request to the given URI.")]
        #[inline]
        pub fn $name<U>(&self, uri: U) -> BoundRequestBuilder<Self>
        where
            U: TryInto<Uri>,
            http::Error: From<<U as TryInto<Uri>>::Error>,
        {
            self.request($variant, uri)
        }
    };
}

impl<T, A, R> Client<T, A, R>
where
    T: TransportPlugin,
    A: ApplicationPlugin<T::Conn>,
    R: AsyncRuntime,
{
    /// Creates a new `Client`.
    ///
    /// The client is cheap to clone and may be reused for multiple requests.
    pub fn new(transport: T, app: A, runtime: R) -> Self {
        Self {
            inner: Arc::new(ClientInner {
                transport,
                app,
                runtime,
            }),
        }
    }

    /// Creates a request builder with the specified HTTP method and URI.
    pub fn request<U>(&self, method: Method, uri: U) -> BoundRequestBuilder<Self>
    where
        U: TryInto<Uri>,
        http::Error: From<<U as TryInto<Uri>>::Error>,
    {
        BoundRequestBuilder {
            inner: Request::builder().method(method).uri(uri),
            client: self.clone(),
        }
    }

    http_method!(head, Method::HEAD);
    http_method!(get, Method::GET);
    http_method!(post, Method::POST);
    http_method!(put, Method::PUT);
    http_method!(patch, Method::PATCH);
    http_method!(delete, Method::DELETE);
}

impl<T, A, R> Dispatch for Client<T, A, R>
where
    T: TransportPlugin,
    A: ApplicationPlugin<T::Conn>,
    R: AsyncRuntime,
{
    type Transport = T;
    type App = A;
    type Runtime = R;

    async fn send<B>(&self, req: Request<B>) -> SendRequestResult<Self>
    where
        B: RequestBody,
    {
        // Establish a transport-level connection
        let conn = self.inner.transport.connect(req.uri()).await?;

        // Perform the application-layer handshake
        let (mut session, driver) = self.inner.app.handshake(conn).await?;

        // Drive the protocol in the background
        self.inner.runtime.spawn(driver);

        // Send the request through the session
        session.send_request(req).await
    }
}

/// A builder for constructing an HTTP request bound to a dispatcher.
pub struct BoundRequestBuilder<D: Dispatch> {
    inner: HttpBuilder,
    client: D,
}

impl<D: Dispatch> BoundRequestBuilder<D> {
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
    pub fn build(self) -> anyhow::Result<BoundRequest<Empty<Bytes>, D>> {
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

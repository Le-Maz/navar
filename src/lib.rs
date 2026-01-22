#![doc = include_str!("../README.md")]

pub use anyhow;
pub use bytes;
pub use futures_lite;
pub use http;
pub use http_body;
pub use http_body_util;

use crate::{
    application::{ApplicationPlugin, Session},
    bound_request::{BoundRequestBuilder, RequestBody, SendRequestFuture, SendRequestResult},
    transport::TransportPlugin,
};
use bytes::Buf;
use http::{Method, Request, Response, Uri};
use http_body_util::{BodyExt, combinators::BoxBody};
use std::future::Future;
use std::sync::Arc;

pub mod application;
pub mod bound_request;
pub mod transport;

/// Helper type alias to extract the response body type from an application plugin.
///
/// This resolves to the concrete response body returned by the application
/// session associated with the given transport.
pub type ResponseBody<A, C> = <<A as ApplicationPlugin<C>>::Session as Session>::ResBody;

pub type NormalizedData = Box<dyn Buf + Send + Sync>;
pub type NormalizedBody = BoxBody<NormalizedData, anyhow::Error>;

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
    #[inline]
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
    #[inline]
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
    #[inline]
    pub fn request<U>(&self, method: Method, uri: U) -> BoundRequestBuilder<Self>
    where
        U: TryInto<Uri>,
        http::Error: From<<U as TryInto<Uri>>::Error>,
    {
        BoundRequestBuilder::new(self.clone(), method, uri)
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

    async fn send<B>(&self, req: Request<B>) -> SendRequestResult
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
        let (res_parts, res_body) = session.send_request(req).await?.into_parts();

        let res_body = res_body
            .map_frame(|frame| frame.map_data(|data| Box::new(data) as NormalizedData))
            .map_err(|err| err.into())
            .boxed();

        Ok(Response::from_parts(res_parts, res_body))
    }
}

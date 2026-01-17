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
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// A helper type alias to extract the response body type from the Application plugin.
pub type ResponseBody<A> = <<A as ApplicationPlugin>::Session as Session>::ResBody;

/// Defines the runtime capabilities required by the client.
///
/// This allows the client to be runtime-agnostic (e.g., Tokio, async-std), provided
/// the runtime can spawn background tasks.
pub trait AsyncRuntime: Send + Sync + 'static {
    /// Spawns a future onto the background runtime.
    fn spawn<F>(&self, future: F)
    where
        F: Future<Output = ()> + Send + 'static;
}

/// A composite trait that bundles all requirements for a request body.
///
/// This trait utilizes "lifted associated types" to ensure that any type implementing it
/// automatically satisfies the `Send`, `Sync`, `Unpin`, and `'static` bounds, as well
/// as ensuring the associated `Data` is `Send` and `Error` is convertible to `BoxError`.
///
/// This prevents the need to repeat these complex bounds on every function signature.
pub trait RequestBody:
    Body<Data = <Self as RequestBody>::Data, Error = <Self as RequestBody>::Error>
    + Send
    + Sync
    + Unpin
    + 'static
{
    type Data: Send;
    type Error: Into<BoxError>;
}

impl<B> RequestBody for B
where
    B: Body + Send + Sync + Unpin + 'static,
    B::Data: Send,
    B::Error: Into<BoxError>,
{
    type Data = B::Data;
    type Error = B::Error;
}

/// Defines the capability to dispatch HTTP requests.
///
/// This trait acts as the abstraction layer between the high-level `Client` interface
/// and the low-level logic of connecting transports and performing application handshakes.
pub trait Dispatch: Send + Sync + Clone {
    /// The transport mechanism (e.g., TCP, TLS).
    type Transport: TransportPlugin;

    /// The application protocol (e.g., HTTP/1.1, HTTP/2).
    type App: ApplicationPlugin;

    /// The runtime environment.
    type Runtime: AsyncRuntime;

    /// Connects to the remote, performs the handshake, and sends the request.
    fn send<B>(
        &self,
        request: Request<B>,
    ) -> impl Future<Output = anyhow::Result<Response<ResponseBody<Self::App>>>> + Send
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
/// This struct is the main entry point for users. It holds the configuration for
/// transport, application protocol, and runtime. It is designed to be shared
/// across threads and is cheaply clonable (wrapping its state in an `Arc`).
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
    A: ApplicationPlugin,
    R: AsyncRuntime,
{
    /// Creates a new `Client` instance.
    pub fn new(transport: T, app: A, runtime: R) -> Self {
        Self {
            inner: Arc::new(ClientInner {
                transport,
                app,
                runtime,
            }),
        }
    }

    /// Creates a request builder with a specific HTTP method and URI.
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
    A: ApplicationPlugin,
    R: AsyncRuntime,
{
    type Transport = T;
    type App = A;
    type Runtime = R;

    async fn send<B>(&self, req: Request<B>) -> anyhow::Result<Response<ResponseBody<Self::App>>>
    where
        B: RequestBody,
    {
        let io = self.inner.transport.connect(req.uri()).await?;
        let (mut session, driver) = self.inner.app.handshake(io).await?;

        self.inner.runtime.spawn(driver);

        session.send_request(req).await
    }
}

/// A builder for constructing an HTTP request.
///
/// This struct holds the partial request state (Method, URI, Headers) and the client,
/// but waits for a body to be provided before the request can be finalized.
pub struct BoundRequestBuilder<D: Dispatch> {
    inner: HttpBuilder,
    client: D,
}

impl<D: Dispatch> BoundRequestBuilder<D> {
    /// Attaches a body to the request.
    ///
    /// Consumes the builder and returns a `BoundRequest` ready for execution.
    pub fn body<B>(self, body: B) -> anyhow::Result<BoundRequest<B, D>>
    where
        B: RequestBody,
    {
        Ok(BoundRequest {
            request: self.inner.body(body)?,
            client: self.client,
        })
    }

    /// Attaches an empty body to the request.
    ///
    /// Consumes the builder and returns a `BoundRequest` ready for execution.
    pub fn build(self) -> anyhow::Result<BoundRequest<Empty<Bytes>, D>> {
        Ok(BoundRequest {
            request: self.inner.body(Empty::new())?,
            client: self.client,
        })
    }
}

/// A fully formed HTTP request awaiting execution.
///
/// This struct combines the valid `http::Request` with the `Client` capable of sending it.
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
    ///
    /// This operation is asynchronous and will return the response (or an error)
    /// once the round-trip is complete.
    pub async fn send(self) -> anyhow::Result<Response<ResponseBody<D::App>>> {
        self.client.send(self.request).await
    }
}

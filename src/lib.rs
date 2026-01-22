#![doc = include_str!("../README.md")]

pub use anyhow;
pub use bytes;
pub use futures_lite;
pub use http;
pub use http_body;
pub use http_body_util;

use crate::{
    application::{ApplicationPlugin, Session},
    bound_request::{BoundRequestBuilder, RequestBody},
    service::{Pipeline, ResponseResult, Service},
    transport::TransportPlugin,
};
use bytes::Buf;
use http::{Method, Request, Response, Uri};
use http_body_util::{BodyExt, combinators::BoxBody};

pub mod application;
pub mod bound_request;
pub mod service;
pub mod transport;

/// Helper type alias to extract the response body type from an application plugin.
pub type ResponseBody<A, C> = <<A as ApplicationPlugin<C>>::Session as Session>::ResBody;

pub type NormalizedData = Box<dyn Buf + Send + Sync>;
pub type NormalizedBody = BoxBody<NormalizedData, anyhow::Error>;

/// Defines the async runtime capabilities required by the client.
pub trait AsyncRuntime: Send + Sync + 'static {
    /// Spawns a future onto the background runtime.
    fn spawn<F>(&self, future: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static;
}

/// The core internal service that performs the actual network I/O.
/// This implements the protocol handshake and request dispatching.
struct BaseService<T, A, R> {
    transport: T,
    app: A,
    runtime: R,
}

impl<T, A, R> Service for BaseService<T, A, R>
where
    T: TransportPlugin,
    A: ApplicationPlugin<T::Conn>,
    R: AsyncRuntime,
{
    async fn handle(
        &self,
        req: Request<NormalizedBody>,
    ) -> anyhow::Result<Response<NormalizedBody>> {
        // Establish a transport-level connection
        let conn = self.transport.connect(req.uri()).await?;

        // Perform the application-layer handshake
        let (mut session, driver) = self.app.handshake(conn).await?;

        // Drive the protocol in the background
        self.runtime.spawn(driver);

        // Send the request through the session
        let (res_parts, res_body) = session.send_request(req).await?.into_parts();

        // Normalize the response body to match the Service trait expectation
        let res_body = res_body
            .map_frame(|frame| frame.map_data(|data| Box::new(data) as NormalizedData))
            .map_err(|err| err.into())
            .boxed();

        Ok(Response::from_parts(res_parts, res_body))
    }
}

/// The primary HTTP client.
///
/// `Client` is a lightweight handle that wraps a `Pipeline`.
/// It is type-erased and does not require generic parameters.
#[derive(Clone)]
pub struct Client {
    pipeline: Pipeline,
}

macro_rules! http_method {
    ($name:ident, $variant:expr) => {
        #[doc = concat!("Initiates a `", stringify!($variant), "` request to the given URI.")]
        #[inline]
        pub fn $name<U>(&self, uri: U) -> BoundRequestBuilder
        where
            U: TryInto<Uri>,
            http::Error: From<<U as TryInto<Uri>>::Error>,
        {
            self.request($variant, uri)
        }
    };
}

impl Client {
    /// Creates a new `Client` wrapping a `BaseService` inside a `Pipeline`.
    #[inline]
    pub fn new<T, A, R>(transport: T, app: A, runtime: R) -> Self
    where
        T: TransportPlugin,
        A: ApplicationPlugin<T::Conn>,
        R: AsyncRuntime,
    {
        let base = BaseService {
            transport,
            app,
            runtime,
        };
        Self {
            pipeline: Pipeline::new(base),
        }
    }

    /// Creates a request builder with the specified HTTP method and URI.
    #[inline]
    pub fn request<U>(&self, method: Method, uri: U) -> BoundRequestBuilder
    where
        U: TryInto<Uri>,
        http::Error: From<<U as TryInto<Uri>>::Error>,
    {
        BoundRequestBuilder::new(self.clone(), method, uri)
    }

    /// Internal execution logic used by the request builders.
    /// This replaces the Dispatch trait functionality.
    pub async fn send<B>(&self, req: Request<B>) -> ResponseResult
    where
        B: RequestBody,
    {
        let (req_parts, req_body) = req.into_parts();

        // Convert input body to NormalizedBody for the pipeline
        let req_body = req_body
            .map_frame(|frame| frame.map_data(|data| Box::new(data) as NormalizedData))
            .map_err(|err| err.into())
            .boxed();

        let request = Request::from_parts(req_parts, req_body);

        // Execute via the pipeline
        self.pipeline.handle(request).await
    }

    http_method!(head, Method::HEAD);
    http_method!(get, Method::GET);
    http_method!(post, Method::POST);
    http_method!(put, Method::PUT);
    http_method!(patch, Method::PATCH);
    http_method!(delete, Method::DELETE);
}

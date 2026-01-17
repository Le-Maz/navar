pub use bytes;
pub use futures_lite;
pub use http;
pub use http_body;
pub use http_body_util;

use http::{Request, Response};
use http_body::Body;
use std::future::Future;

use crate::{
    application::{ApplicationPlugin, Session},
    transport::TransportPlugin,
};

pub mod application;
pub mod transport;

pub trait AsyncRuntime: Send + Sync + 'static {
    fn spawn<F>(&self, future: F)
    where
        F: Future<Output = ()> + Send + 'static;
}

pub struct HttpClient<TransportImpl, ApplicationImpl, AsyncRuntimeImpl> {
    pub transport: TransportImpl,
    pub app: ApplicationImpl,
    pub runtime: AsyncRuntimeImpl,
}

impl<T, A, R> HttpClient<T, A, R>
where
    T: TransportPlugin,
    A: ApplicationPlugin,
    R: AsyncRuntime,
{
    pub fn new(transport: T, app: A, runtime: R) -> Self {
        Self {
            transport,
            app,
            runtime,
        }
    }

    pub async fn send<BodyImpl>(
        &self,
        request: Request<BodyImpl>,
    ) -> anyhow::Result<Response<<A::Session as Session>::ResBody>>
    where
        BodyImpl: Body + Send + Sync + Unpin + 'static,
        BodyImpl::Data: Send,
        BodyImpl::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        let io = self.transport.connect(request.uri()).await?;

        let (mut session, driver) = self.app.handshake(io).await?;

        self.runtime.spawn(driver);

        session.send_request(request).await
    }
}

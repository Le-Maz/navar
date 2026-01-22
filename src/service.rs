use std::sync::Arc;

use futures_lite::future::Boxed;
use http::{Request, Response};

use crate::NormalizedBody;

pub type ResponseResult = anyhow::Result<Response<NormalizedBody>>;

pub trait Service: Send + Sync + 'static {
    fn handle(
        &self,
        request: Request<NormalizedBody>,
    ) -> impl Future<Output = ResponseResult> + Send;
}

pub trait IntoService: Send + Sync + 'static {
    fn into_service(self) -> impl Service;
}

impl<S: Service> IntoService for S {
    #[inline]
    fn into_service(self) -> impl Service {
        self
    }
}

pub trait Middleware: Send + Sync + 'static {
    fn wrap(self, next: impl Service) -> impl Service;
}

impl<M: Middleware, S: Service> IntoService for (M, S) {
    #[inline]
    fn into_service(self) -> impl Service {
        self.0.wrap(self.1)
    }
}

impl<M1: Middleware, M2: Middleware> Middleware for (M1, M2) {
    #[inline]
    fn wrap(self, next: impl Service) -> impl Service {
        self.0.wrap(self.1.wrap(next))
    }
}

#[derive(Clone)]
pub struct Pipeline {
    handler: Arc<dyn Fn(Request<NormalizedBody>) -> Boxed<ResponseResult> + Send + Sync>,
}

impl Pipeline {
    pub fn new(service: impl IntoService) -> Self {
        let service = Arc::new(service.into_service());
        Self {
            handler: Arc::new({
                move |request| {
                    let service = service.clone();
                    Box::pin(async move { service.handle(request).await })
                }
            }),
        }
    }
}

impl Service for Pipeline {
    #[inline]
    async fn handle(&self, request: Request<NormalizedBody>) -> ResponseResult {
        (self.handler)(request).await
    }
}

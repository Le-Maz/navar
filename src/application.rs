use http::{Request, Response};
use http_body::Body;
use std::{fmt::Debug, future::Future};

pub trait Session: Send + 'static
where
    <Self::ResBody as Body>::Error: Debug + Into<anyhow::Error>,
{
    type ResBody: Body + Send + 'static;

    fn send_request<B>(
        &mut self,
        request: Request<B>,
    ) -> impl Future<Output = anyhow::Result<Response<Self::ResBody>>> + Send
    where
        B: Body + Send + Sync + 'static + Unpin,
        B::Data: Send,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>;
}

/// A generic Application Plugin.
pub trait ApplicationPlugin<C>: Send + Sync + 'static {
    type Session: Session;

    fn handshake(
        &self,
        conn: C,
    ) -> impl Future<
        Output = anyhow::Result<(Self::Session, impl Future<Output = ()> + Send + 'static)>,
    > + Send;
}

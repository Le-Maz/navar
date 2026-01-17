use futures_lite::io::{AsyncRead, AsyncWrite};
use http::Uri;
use std::future::Future;

pub trait TransportIo: AsyncRead + AsyncWrite + Unpin + Send + Sync + 'static {}
impl<T> TransportIo for T where T: AsyncRead + AsyncWrite + Unpin + Send + Sync + 'static {}

pub trait TransportPlugin: Send + Sync + 'static {
    type Io: TransportIo;

    fn connect(&self, uri: &Uri) -> impl Future<Output = anyhow::Result<Self::Io>> + Send;
}

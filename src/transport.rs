use futures_lite::io::{AsyncRead, AsyncWrite};
use http::Uri;
use std::future::Future;

pub trait TransportIo: AsyncRead + AsyncWrite + Unpin + Send + Sync + 'static {
    /// Returns the negotiated ALPN protocol, if any.
    fn alpn_protocol(&self) -> Option<&[u8]> {
        None
    }
}

pub trait TransportPlugin: Send + Sync + 'static {
    type Io: TransportIo;

    fn connect(&self, uri: &Uri) -> impl Future<Output = anyhow::Result<Self::Io>> + Send;
}

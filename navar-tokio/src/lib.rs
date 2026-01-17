use std::future::Future;
use tokio::net::TcpStream;
use tokio_util::compat::{Compat, TokioAsyncReadCompatExt};

use navar::AsyncRuntime;
use navar::http::Uri;
use navar::transport::TransportPlugin;

#[derive(Clone, Copy, Default, Debug)]
pub struct TokioRuntime;

impl AsyncRuntime for TokioRuntime {
    fn spawn<F>(&self, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        tokio::spawn(future);
    }
}

#[derive(Clone, Default, Debug)]
pub struct TokioTcpTransport;

impl TransportPlugin for TokioTcpTransport {
    type Io = Compat<TcpStream>;

    async fn connect(&self, uri: &Uri) -> anyhow::Result<Self::Io> {
        let host = uri
            .host()
            .ok_or_else(|| anyhow::anyhow!("URI missing host"))?;

        let port = uri.port_u16().unwrap_or_else(|| {
            if uri.scheme_str() == Some("https") {
                443
            } else {
                80
            }
        });

        let addr = format!("{}:{}", host, port);

        let stream = TcpStream::connect(addr).await?;

        let _ = stream.set_nodelay(true);

        Ok(stream.compat())
    }
}

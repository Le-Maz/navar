pub mod adapter;

use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use adapter::{H3BidiStream, H3Connection};
use navar::{
    application::{ApplicationPlugin, Session},
    bytes::{Buf, Bytes},
    http::{Request, Response},
    http_body::{Body, Frame},
    http_body_util::BodyExt, // Import BodyExt for map_err
    transport::Connection,
};

/// The HTTP/3 Application Plugin.
#[derive(Clone, Default)]
pub struct H3App;

impl H3App {
    pub fn new() -> Self {
        Self
    }
}

impl<C> ApplicationPlugin<C> for H3App
where
    C: Connection,
{
    type Session = H3Session<C>;

    async fn handshake(
        &self,
        conn: C,
    ) -> anyhow::Result<(Self::Session, impl Future<Output = ()> + Send + 'static)> {
        // 1. Wrap the raw connection in the Adapter
        let h3_conn = H3Connection::new(conn);

        // 2. Perform the H3 client handshake
        let (mut driver, sender) = h3::client::new(h3_conn).await?;

        // 3. Create the session
        let session = H3Session { sender };

        // 4. Driver future (must be spawned by the runtime)
        let driver_fut = async move {
            // wait_idle returns the Error directly when the connection closes
            let err = driver.wait_idle().await;

            // Check if it was a graceful close (H3_NO_ERROR)
            if !err.is_h3_no_error() {
                eprintln!("H3 Client Driver Error: {}", err);
            }
        };

        Ok((session, driver_fut))
    }
}

/// The active HTTP/3 Session.
pub struct H3Session<C: Connection> {
    sender: h3::client::SendRequest<H3Connection<C>, Bytes>,
}

impl<C: Connection> Session for H3Session<C> {
    type ResBody = H3ResponseBody<C>;

    async fn send_request<B>(
        &mut self,
        request: Request<B>,
    ) -> anyhow::Result<Response<Self::ResBody>>
    where
        B: Body + Send + Sync + 'static + Unpin,
        B::Data: Send,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        // 1. Prepare request
        let (parts, body) = request.into_parts();
        let req = Request::from_parts(parts, ());

        // 2. Open stream
        let mut stream = self.sender.send_request(req).await?;

        // 3. Send body
        let mut body = body.map_err(Into::into);

        while let Some(frame_res) = body.frame().await {
            match frame_res {
                Ok(frame) => {
                    if let Ok(data) = frame.into_data() {
                        // explicitly use Buf::chunk() to get &[u8]
                        let chunk = data.chunk();
                        let bytes = Bytes::copy_from_slice(chunk);
                        stream.send_data(bytes).await?;
                    }
                }
                Err(err) => {
                    return Err(anyhow::Error::from_boxed(err));
                }
            }
        }
        // Signal EOF to peer
        stream.finish().await?;

        // 4. Await response
        let response = stream.recv_response().await?;

        Ok(response.map(|_| H3ResponseBody { stream }))
    }
}

/// Wraps the H3 RequestStream into a standard http_body::Body.
pub struct H3ResponseBody<C: Connection> {
    // The stream type uses the BidiStream provided by the Connection adapter
    stream: h3::client::RequestStream<H3BidiStream<C::Bidi>, Bytes>,
}

impl<C: Connection> Body for H3ResponseBody<C> {
    type Data = Bytes;
    type Error = anyhow::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        match self.stream.poll_recv_data(cx) {
            Poll::Ready(Ok(Some(mut bytes))) => {
                // Convert h3's opaque `impl Buf` to `Bytes`
                let chunk = bytes.copy_to_bytes(bytes.remaining());
                Poll::Ready(Some(Ok(Frame::data(chunk))))
            }
            Poll::Ready(Ok(None)) => Poll::Ready(None), // EOF
            Poll::Ready(Err(e)) => {
                // Return h3::Error directly
                Poll::Ready(Some(Err(e.into())))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

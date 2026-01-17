use futures_lite::io::{AsyncRead, AsyncWrite};
use http::Uri;
use std::future::Future;

pub trait Stream: Unpin + Send + Sync + 'static {
    /// Returns the stream identifier.
    fn stream_id(&self) -> u64;
}

/// A unidirectional stream capable of sending data (Write-only).
pub trait SendStream: Stream + AsyncWrite {}
impl<T> SendStream for T where T: Stream + AsyncWrite {}

/// A unidirectional stream capable of receiving data (Read-only).
pub trait RecvStream: Stream + AsyncRead {}
impl<T> RecvStream for T where T: Stream + AsyncRead {}

/// A bidirectional stream (Read + Write) that can be split into unidirectional halves.
pub trait BidiStream: Stream + AsyncRead + AsyncWrite {
    type Send: SendStream;
    type Recv: RecvStream;

    /// Splits the bidirectional stream into independent Send and Recv handles.
    fn split(self) -> (Self::Send, Self::Recv);

    fn alpn_protocol(&self) -> Option<&[u8]> {
        None
    }
}

/// Represents a handle capable of creating or accepting specific stream types.
pub trait Connection: Clone + Send + Sync + 'static {
    type Bidi: BidiStream;
    type Send: SendStream;
    type Recv: RecvStream;

    /// Opens a new bidirectional byte stream.
    fn open_bidirectional(&self) -> impl Future<Output = anyhow::Result<Self::Bidi>> + Send;

    /// Opens a new unidirectional byte stream (outbound).
    fn open_unidirectional(&self) -> impl Future<Output = anyhow::Result<Self::Send>> + Send;

    /// Accepts the next incoming bidirectional stream.
    fn accept_bidirectional(&self) -> impl Future<Output = anyhow::Result<Self::Bidi>> + Send;

    /// Accepts the next incoming unidirectional stream (inbound).
    fn accept_unidirectional(&self) -> impl Future<Output = anyhow::Result<Self::Recv>> + Send;

    fn alpn_protocol(&self) -> Option<&[u8]> {
        None
    }
}

pub trait TransportPlugin: Send + Sync + 'static {
    type Conn: Connection;

    fn connect(&self, uri: &Uri) -> impl Future<Output = anyhow::Result<Self::Conn>> + Send;
}

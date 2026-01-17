//! # Transport Abstraction Traits
//!
//! This module defines a set of async traits that abstract over
//! stream-oriented transports (e.g. QUIC, TCP-based protocols, or custom
//! multiplexed transports).
//!
//! The design is centered around:
//!
//! - **Streams**: Unidirectional or bidirectional byte streams
//! - **Connections**: Objects that open or accept streams
//! - **Transport plugins**: Factories that establish connections to remote peers
//!
//! These traits are intended to be implemented by transport backends and
//! consumed by higher-level protocol implementations without tying them to
//! a specific transport technology.

use futures_lite::io::{AsyncRead, AsyncWrite};
use http::Uri;
use std::future::Future;

/// Base trait for all stream types.
///
/// A stream represents a logical byte channel within a connection.
/// All streams are required to be:
///
/// - [`Unpin`] to simplify async usage
/// - [`Send`] and [`Sync`] for use across tasks and threads
/// - `'static` so they can be owned by long-lived async tasks
pub trait Stream: Unpin + Send + Sync + 'static {
    /// Returns a transport-specific identifier for this stream.
    ///
    /// The identifier is unique within the scope of a single connection
    /// and may be used for logging, debugging, or protocol bookkeeping.
    fn stream_id(&self) -> u64;
}

/// A unidirectional stream capable of sending data (write-only).
///
/// This trait represents outbound-only streams. It combines [`Stream`]
/// with [`AsyncWrite`].
pub trait SendStream: Stream + AsyncWrite {}

/// Blanket implementation for all compatible types.
impl<T> SendStream for T where T: Stream + AsyncWrite {}

/// A unidirectional stream capable of receiving data (read-only).
///
/// This trait represents inbound-only streams. It combines [`Stream`]
/// with [`AsyncRead`].
pub trait RecvStream: Stream + AsyncRead {}

/// Blanket implementation for all compatible types.
impl<T> RecvStream for T where T: Stream + AsyncRead {}

/// A bidirectional stream capable of both sending and receiving data.
///
/// Bidirectional streams can be split into independent send and receive
/// halves, allowing concurrent read and write operations.
pub trait BidiStream: Stream + AsyncRead + AsyncWrite {
    /// The send-only half produced by [`split`](Self::split).
    type Send: SendStream;

    /// The receive-only half produced by [`split`](Self::split).
    type Recv: RecvStream;

    /// Splits the bidirectional stream into independent send and receive handles.
    ///
    /// After splitting, the returned halves may be moved to separate tasks
    /// and used concurrently.
    fn split(self) -> (Self::Send, Self::Recv);

    /// Returns the negotiated ALPN protocol for this stream, if available.
    ///
    /// This is typically meaningful for transports layered over TLS
    /// (e.g. QUIC). The default implementation returns `None`.
    fn alpn_protocol(&self) -> Option<&[u8]> {
        None
    }
}

/// Represents an established connection capable of opening or accepting streams.
///
/// A connection is responsible for multiplexing multiple streams and
/// providing async access to them.
pub trait Connection: Clone + Send + Sync + 'static {
    /// Bidirectional stream type associated with this connection.
    type Bidi: BidiStream;

    /// Outbound unidirectional stream type.
    type Send: SendStream;

    /// Inbound unidirectional stream type.
    type Recv: RecvStream;

    /// Opens a new outbound bidirectional byte stream.
    ///
    /// The returned future resolves once the stream has been successfully
    /// created by the transport.
    fn open_bidirectional(&self) -> impl Future<Output = anyhow::Result<Self::Bidi>> + Send;

    /// Opens a new outbound unidirectional byte stream.
    fn open_unidirectional(&self) -> impl Future<Output = anyhow::Result<Self::Send>> + Send;

    /// Accepts the next incoming bidirectional stream.
    ///
    /// This future resolves when a remote peer opens a new bidirectional stream.
    fn accept_bidirectional(&self) -> impl Future<Output = anyhow::Result<Self::Bidi>> + Send;

    /// Accepts the next incoming unidirectional stream.
    ///
    /// This future resolves when a remote peer opens a new inbound stream.
    fn accept_unidirectional(&self) -> impl Future<Output = anyhow::Result<Self::Recv>> + Send;

    /// Returns the negotiated ALPN protocol for this connection, if available.
    ///
    /// The default implementation returns `None`.
    fn alpn_protocol(&self) -> Option<&[u8]> {
        None
    }
}

/// A transport plugin capable of establishing connections.
///
/// This trait serves as the entry point for a transport implementation.
/// Higher-level code uses a `TransportPlugin` to create connections without
/// depending on a concrete transport backend.
pub trait TransportPlugin: Send + Sync + 'static {
    /// Connection type produced by this transport.
    type Conn: Connection;

    /// Establishes a connection to the given URI.
    ///
    /// The interpretation of the URI (scheme, authority, path, etc.)
    /// is transport-specific.
    fn connect(&self, uri: &Uri) -> impl Future<Output = anyhow::Result<Self::Conn>> + Send;
}

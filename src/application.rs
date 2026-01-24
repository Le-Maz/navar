//! # Application Session Abstractions
//!
//! This module defines traits for application-layer protocol handling
//! built on top of an established transport connection.
//!
//! The core concepts are:
//!
//! - **Sessions**: Stateful request/response handlers bound to a connection
//! - **Application plugins**: Protocol implementations that perform a
//!   handshake and produce a session plus a background driver task
//!
//! These abstractions are intended to support protocols such as HTTP/1,
//! HTTP/2, HTTP/3, or custom request/response protocols while remaining
//! transport-agnostic.

use http::{Request, Response};
use http_body::Body;
use std::future::Future;

/// Represents an active application-layer session.
///
/// A session is responsible for sending requests and receiving responses
/// over an already-established connection. Implementations typically
/// encapsulate protocol state (e.g. stream management, flow control,
/// or header compression).
///
/// # Error Handling
///
/// The response body error type must implement [`std::error::Error`], [`Send`],
/// and [`Sync`] to ensure it can be propagated safely across threads and
/// converted into common error types like [`anyhow::Error`].
pub trait Session: Clone + Send + 'static
where
    <Self::ResBody as Body>::Error: Into<anyhow::Error> + Send + Sync + 'static,
    <Self::ResBody as Body>::Data: Send + Sync,
{
    /// The response body type produced by this session.
    ///
    /// This body represents the payload of responses returned by
    /// [`send_request`](Self::send_request).
    type ResBody: Body + Send + Sync + 'static;

    /// Sends a request and asynchronously returns the corresponding response.
    ///
    /// Implementations may multiplex multiple concurrent requests depending
    /// on the underlying protocol (e.g. HTTP/2 or HTTP/3), or may serialize
    /// requests (e.g. HTTP/1).
    ///
    /// # Type Parameters
    ///
    /// - `B`: The request body type
    ///
    /// # Requirements
    ///
    /// - The request body must be sendable across tasks
    /// - The body error must be convertible into a boxed dynamic error
    fn send_request<B>(
        &mut self,
        request: Request<B>,
    ) -> impl Future<Output = anyhow::Result<Response<Self::ResBody>>> + Send
    where
        B: Body + Send + Sync + 'static + Unpin,
        B::Data: Send + Sync,
        B::Error: Into<anyhow::Error>;
}

/// A generic application-layer plugin.
///
/// An `ApplicationPlugin` performs protocol-specific setup (e.g. exchanging
/// settings, negotiating features, spawning background tasks) and produces
/// a [`Session`] that can be used by higher-level application code.
pub trait ApplicationPlugin<C>: Send + Sync + 'static {
    /// The session type produced after a successful handshake.
    type Session: Session;

    /// Performs the application-layer handshake on top of an established connection.
    ///
    /// On success, this returns:
    ///
    /// - A [`Session`] used to send requests
    /// - A background future that must be polled to drive the protocol
    ///   (e.g. reading incoming frames, handling control streams, etc.)
    ///
    /// The background future is expected to run for the lifetime of the
    /// session and typically terminates when the connection closes.
    fn handshake(
        &self,
        conn: C,
    ) -> impl Future<
        Output = anyhow::Result<(Self::Session, impl Future<Output = ()> + Send + 'static)>,
    > + Send;
}

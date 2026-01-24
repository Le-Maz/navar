//! # NDJSON Streaming Parser
//!
//! This module provides [`NdjsonStream`], a [`futures_lite::Stream`] of values
//! obtained from parsing newline-delimited JSON (NDJSON) received through an
//! HTTP response body.

use std::{
    collections::VecDeque,
    pin::{Pin, pin},
    task::{Context, Poll, ready},
};

use bytes::{Buf, Bytes};
use futures_lite::Stream;
use http_body::Body;
use http_body_util::{BodyExt, combinators::BoxBody};
use serde::de::DeserializeOwned;
use serde_json::from_slice;
use tracing::instrument;

use crate::NormalizedBody;

/// A streaming **NDJSON (newline-delimited JSON)** parser.
///
/// Reconstructs `\n`-terminated lines across arbitrary frame boundaries
/// and deserializes each line into `T`.
pub struct NdjsonStream<T> {
    /// The underlying HTTP body stream adapted to produce `Bytes`.
    body: BoxBody<Bytes, anyhow::Error>,

    /// Accumulates incoming bytes.
    buffer: VecDeque<u8>,

    /// Scratch buffer for a single line.
    line_buf: Vec<u8>,

    /// A queue of parsed results.
    queue: VecDeque<Result<T, anyhow::Error>>,

    _marker: std::marker::PhantomData<T>,
}

impl<T> NdjsonStream<T>
where
    T: DeserializeOwned,
{
    /// Creates a new NDJSON stream from a `NormalizedBody`.
    pub fn new(body: NormalizedBody) -> Self {
        // Adapt NormalizedBody (Box<dyn Buf>) -> BoxBody<Bytes>
        let bytes_body =
            body.map_frame(|frame| frame.map_data(|mut data| data.copy_to_bytes(data.remaining())));

        Self {
            body: BoxBody::new(bytes_body),
            buffer: VecDeque::with_capacity(256),
            line_buf: Vec::with_capacity(256),
            queue: VecDeque::with_capacity(16),
            _marker: std::marker::PhantomData,
        }
    }

    /// Appends raw bytes and extracts/parses complete lines.
    fn append_frame(&mut self, mut frame: impl Buf) {
        let old_len = self.buffer.len();

        while frame.has_remaining() {
            let chunk = frame.chunk();
            self.buffer.extend(chunk);
            frame.advance(chunk.len());
        }

        let mut processed = 0;

        for pos in old_len..self.buffer.len() {
            if self.buffer[pos] == b'\n' {
                let start = processed;
                let end = pos;

                let mut buf = std::mem::take(&mut self.line_buf);
                buf.clear();
                buf.reserve(end - start);

                let (s1, s2) = self.buffer.as_slices();
                if end <= s1.len() {
                    buf.extend_from_slice(&s1[start..end]);
                } else if start >= s1.len() {
                    let s2_start = start - s1.len();
                    let s2_end = end - s1.len();
                    buf.extend_from_slice(&s2[s2_start..s2_end]);
                } else {
                    buf.extend_from_slice(&s1[start..]);
                    buf.extend_from_slice(&s2[..(end - s1.len())]);
                }

                // Attempt JSON deserialization
                let result = from_slice::<T>(&buf).map_err(anyhow::Error::from);
                self.queue.push_back(result);

                buf.clear();
                self.line_buf = buf;

                processed = pos + 1;
            }
        }

        self.buffer.drain(0..processed);
    }
}

impl<T> Unpin for NdjsonStream<T> {}

impl<T> Stream for NdjsonStream<T>
where
    T: DeserializeOwned,
{
    type Item = Result<T, anyhow::Error>;

    #[instrument(skip(self, cx))]
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if let Some(item) = self.queue.pop_front() {
            return Poll::Ready(Some(item));
        }

        let body = pin!(&mut self.body);
        let frame_result = ready!(body.poll_frame(cx));

        match frame_result {
            Some(Ok(frame)) => {
                if let Ok(data) = frame.into_data() {
                    self.append_frame(data);
                    if let Some(item) = self.queue.pop_front() {
                        return Poll::Ready(Some(item));
                    }
                }
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            Some(Err(e)) => Poll::Ready(Some(Err(e))),
            None => Poll::Ready(None),
        }
    }
}

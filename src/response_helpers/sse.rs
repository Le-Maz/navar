//! Server-Sent Events (SSE) stream parser.
//!
//! This module provides [`SseStream`], a [`futures_lite::Stream`] implementation
//! that incrementally parses an HTTP response body into individual
//! Server-Sent Events according to the SSE specification.

use std::{
    collections::VecDeque,
    pin::{Pin, pin},
    task::{Context, Poll, ready},
};

use bytes::{Buf, Bytes};
use futures_lite::Stream;
use http_body::Body;
use http_body_util::{BodyExt, combinators::BoxBody};
use tracing::instrument;

use crate::NormalizedBody;

/// A streaming Server-Sent Events (SSE) parser.
///
/// `SseStream` consumes an HTTP response body, handles arbitrary frame boundaries,
/// reconstructs `\n`-delimited lines, and builds [`SseEvent`]s.
pub struct SseStream {
    /// The HTTP response body being streamed.
    /// Adapted to yield `Bytes` instead of `NormalizedData`.
    body: BoxBody<Bytes, anyhow::Error>,

    /// A deque of unprocessed raw bytes.
    buffer: VecDeque<u8>,

    /// Scratch buffer used for assembling a single line.
    line_buf: Vec<u8>,

    /// The SSE event currently being constructed.
    pending_event: SseEvent,

    /// A queue of fully constructed SSE events ready to be emitted.
    ready_events: VecDeque<SseEvent>,

    /// The most recent `id:` field received.
    last_event_id: Option<String>,
}

impl SseStream {
    /// Create a new SSE stream from a `NormalizedBody`.
    pub fn new(body: NormalizedBody) -> Self {
        // Adapt NormalizedBody (Box<dyn Buf>) -> BoxBody<Bytes>
        let bytes_body =
            body.map_frame(|frame| frame.map_data(|mut data| data.copy_to_bytes(data.remaining())));

        Self {
            body: BoxBody::new(bytes_body),
            buffer: VecDeque::with_capacity(256),
            line_buf: Vec::with_capacity(256),
            pending_event: SseEvent::default(),
            ready_events: VecDeque::new(),
            last_event_id: None,
        }
    }

    /// Retrieve the most recently received SSE event ID, if any.
    pub fn last_event_id(&self) -> Option<&str> {
        self.last_event_id.as_deref()
    }

    /// Appends raw bytes from a frame and processes lines.
    fn append_frame(&mut self, mut frame: impl Buf) {
        let old_len = self.buffer.len();

        while frame.has_remaining() {
            let chunk = frame.chunk();
            for &b in chunk {
                self.buffer.push_back(b);
            }
            frame.advance(chunk.len());
        }

        let mut processed = 0;
        for position in old_len..self.buffer.len() {
            if self.buffer[position] == b'\n' {
                let start = processed;
                let end = position;

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

                let line = String::from_utf8_lossy(&buf);
                self.process_line(line.trim_end());

                buf.clear();
                self.line_buf = buf;

                processed = position + 1;
            }
        }

        for _ in 0..processed {
            self.buffer.pop_front();
        }
    }

    fn process_line(&mut self, line: &str) {
        if line.is_empty() {
            self.ready_events
                .push_back(std::mem::take(&mut self.pending_event));
            return;
        }

        let mut parts = line.splitn(2, ':');
        let name = parts.next().unwrap_or("");
        let value = parts.next().map(|v| v.trim_start()).unwrap_or("");

        match name {
            "data" => {
                if !self.pending_event.data.is_empty() {
                    self.pending_event.data.push('\n');
                }
                self.pending_event.data.push_str(value);
            }
            "id" => {
                self.pending_event.id = Some(value.to_owned());
                self.last_event_id = Some(value.to_owned());
            }
            "event" => {
                self.pending_event.event = Some(value.to_owned());
            }
            _ => {}
        }
    }
}

impl Stream for SseStream {
    type Item = Result<SseEvent, anyhow::Error>;

    #[instrument(skip(self, cx))]
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if let Some(ev) = self.ready_events.pop_front() {
            return Poll::Ready(Some(Ok(ev)));
        }

        let body = pin!(&mut self.body);
        let frame_result = ready!(body.poll_frame(cx));

        match frame_result {
            Some(Ok(frame)) => {
                if let Ok(data) = frame.into_data() {
                    self.append_frame(data);
                    if let Some(ev) = self.ready_events.pop_front() {
                        return Poll::Ready(Some(Ok(ev)));
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

/// A single Server-Sent Event.
#[derive(Debug, Default)]
pub struct SseEvent {
    id: Option<String>,
    event: Option<String>,
    data: String,
}

impl SseEvent {
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }
    pub fn event(&self) -> Option<&str> {
        self.event.as_deref()
    }
    pub fn data(&self) -> &str {
        &self.data
    }
}

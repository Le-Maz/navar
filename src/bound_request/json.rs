use bytes::Bytes;
use futures_lite::Stream;
use http::HeaderValue;
use http_body_util::{BodyExt, Full, StreamBody, combinators::BoxBody};
use serde::Serialize;

use crate::bound_request::{BoundRequest, BoundRequestBuilder};

impl BoundRequestBuilder {
    /// Sets the body of the request to JSON-serialized data.
    pub fn json<T: Serialize>(self, data: &T) -> anyhow::Result<BoundRequest<Full<Bytes>>> {
        const MIME_JSON: HeaderValue = HeaderValue::from_static("application/json");

        let body = serde_json::to_vec(data)?;
        self.ensure_content_type(MIME_JSON)
            .body(Full::new(Bytes::from(body)))
    }

    /// Sets the body of the request to NDJSON-serialized data from a stream.
    pub fn ndjson<T: Serialize>(
        self,
        data: impl Stream<Item = T> + Unpin + Send + Sync + 'static,
    ) -> anyhow::Result<BoundRequest<BoxBody<Bytes, anyhow::Error>>> {
        const MIME_NDJSON: HeaderValue = HeaderValue::from_static("application/x-ndjson");

        let stream = {
            use futures_lite::StreamExt;
            use http_body::Frame;

            data.map(|item| {
                use bytes::Bytes;

                let mut buffer = serde_json::to_vec(&item).map_err(anyhow::Error::from)?;
                buffer.push(b'\n');
                Ok::<_, anyhow::Error>(Frame::data(Bytes::from(buffer)))
            })
        };

        // Box the body to erase the specific stream type, making the return type nameable
        let body = StreamBody::new(stream).boxed();

        self.ensure_content_type(MIME_NDJSON).body(body)
    }
}

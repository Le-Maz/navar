use crate::NormalizedBody;
use bytes::{Buf, Bytes};
use futures_lite::{Stream, StreamExt};
use http::Response;
use http_body_util::BodyExt;
#[cfg(feature = "json")]
use serde::de::DeserializeOwned;

#[cfg(feature = "json")]
use self::ndjson::NdjsonStream;
use self::sse::SseStream;

pub mod ndjson;
pub mod sse;

/// Extension trait to add helper methods to `Response<NormalizedBody>`.
pub trait ResponseExt {
    /// Consumes the response and returns the full body as `Bytes`.
    fn bytes(self) -> impl Future<Output = anyhow::Result<Bytes>> + Send;

    /// Consumes the response and returns the full body as a UTF-8 `String`.
    fn text(self) -> impl Future<Output = anyhow::Result<String>> + Send;

    /// Consumes the response, parses it as JSON, and returns type `T`.
    #[cfg(feature = "json")]
    fn json<T: DeserializeOwned>(self) -> impl Future<Output = anyhow::Result<T>> + Send;

    /// Returns a stream of `Bytes` chunks.
    fn bytes_stream(self) -> impl Stream<Item = anyhow::Result<Bytes>> + Send + Unpin + 'static;

    /// Returns a stream of Server-Sent Events.
    fn sse_stream(self) -> SseStream;

    /// Returns a stream of NDJSON items.
    #[cfg(feature = "json")]
    fn ndjson_stream<T: DeserializeOwned>(self) -> NdjsonStream<T>;
}

impl ResponseExt for Response<NormalizedBody> {
    async fn bytes(self) -> anyhow::Result<Bytes> {
        // Collect all frames into a single buffer
        let collected = self.into_body().collect().await?;
        // Convert the aggregate buffer (NormalizedData) into contiguous Bytes
        Ok(collected.to_bytes())
    }

    async fn text(self) -> anyhow::Result<String> {
        let bytes = self.bytes().await?;
        let string = String::from_utf8(bytes.to_vec())
            .map_err(|e| anyhow::anyhow!("UTF-8 conversion failed: {}", e))?;
        Ok(string)
    }

    #[cfg(feature = "json")]
    async fn json<T: DeserializeOwned>(self) -> anyhow::Result<T> {
        let bytes = self.bytes().await?;
        let value = serde_json::from_slice(&bytes)?;
        Ok(value)
    }

    fn bytes_stream(self) -> impl Stream<Item = anyhow::Result<Bytes>> + Send + Unpin + 'static {
        // Extract the data stream and map NormalizedData (Box<dyn Buf>) to Bytes
        self.into_body().into_data_stream().map(|res| match res {
            Ok(mut data) => Ok(data.copy_to_bytes(data.remaining())),
            Err(e) => Err(e),
        })
    }

    fn sse_stream(self) -> SseStream {
        SseStream::new(self.into_body())
    }

    #[cfg(feature = "json")]
    fn ndjson_stream<T: DeserializeOwned>(self) -> NdjsonStream<T> {
        NdjsonStream::new(self.into_body())
    }
}

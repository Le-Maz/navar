use bytes::Bytes;
use futures_lite::{StreamExt, stream};
use http::Response;
use http_body_util::{BodyExt, StreamBody};
use navar::{NormalizedBody, NormalizedData, response_helpers::ResponseExt};

/// Helper to create a `NormalizedBody` from a vector of byte chunks.
/// matches `BoxBody<Box<dyn Buf + Send + Sync>, anyhow::Error>`
fn make_body(chunks: Vec<&'static [u8]>) -> NormalizedBody {
    let stream = stream::iter(chunks)
        .map(|chunk| Ok::<_, anyhow::Error>(http_body::Frame::data(Bytes::copy_from_slice(chunk))));

    StreamBody::new(stream)
        .map_frame(|frame| frame.map_data(|data| Box::new(data) as NormalizedData))
        .map_err(|e| e)
        .boxed()
}

#[tokio::test]
async fn response_bytes() {
    let body = make_body(vec![b"hello ", b"world"]);
    let response = Response::new(body);

    let bytes = response.bytes().await.unwrap();
    assert_eq!(bytes, "hello world");
}

#[tokio::test]
async fn response_text() {
    let body = make_body(vec![b"hello ", b"world"]);
    let response = Response::new(body);

    let text = response.text().await.unwrap();
    assert_eq!(text, "hello world");
}

#[tokio::test]
async fn response_text_invalid_utf8() {
    let body = make_body(vec![b"\xFF\xFF"]);
    let response = Response::new(body);

    assert!(response.text().await.is_err());
}

#[cfg(feature = "json")]
#[tokio::test]
async fn response_json() {
    let json_data = r#"{"key": "value"}"#;
    let body = make_body(vec![json_data.as_bytes()]);
    let response = Response::new(body);

    let data: serde_json::Value = response.json().await.unwrap();
    assert_eq!(data["key"], "value");
}

#[tokio::test]
async fn bytes_stream() {
    let body = make_body(vec![b"chunk1", b"chunk2"]);
    let response = Response::new(body);

    let mut stream = response.bytes_stream();

    let chunk1 = stream.next().await.unwrap().unwrap();
    assert_eq!(chunk1, "chunk1");

    let chunk2 = stream.next().await.unwrap().unwrap();
    assert_eq!(chunk2, "chunk2");

    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn sse_stream() {
    // Test SSE parsing across chunk boundaries
    let chunks = vec![
        b"data: first event\n\n".as_slice(),
        b"event: update\ndata: se".as_slice(),
        b"cond event\nid: 123\n\n".as_slice(),
    ];
    let body = make_body(chunks);
    let response = Response::new(body);

    let mut stream = response.sse_stream();

    let event1 = stream.next().await.unwrap().unwrap();
    assert_eq!(event1.data(), "first event");
    assert_eq!(event1.event(), None);

    let event2 = stream.next().await.unwrap().unwrap();
    assert_eq!(event2.data(), "second event");
    assert_eq!(event2.event(), Some("update"));
    assert_eq!(event2.id(), Some("123"));

    assert!(stream.next().await.is_none());
}

#[cfg(feature = "json")]
#[tokio::test]
async fn ndjson_stream() {
    // Test NDJSON parsing across chunk boundaries
    let chunks = vec![b"{\"id\": 1}\n{\"id\":".as_slice(), b" 2}\n".as_slice()];
    let body = make_body(chunks);
    let response = Response::new(body);

    let mut stream = response.ndjson_stream::<serde_json::Value>();

    let item1 = stream.next().await.unwrap().unwrap();
    assert_eq!(item1["id"], 1);

    let item2 = stream.next().await.unwrap().unwrap();
    assert_eq!(item2["id"], 2);

    assert!(stream.next().await.is_none());
}

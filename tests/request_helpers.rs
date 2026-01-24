use bytes::{Buf, Bytes, BytesMut};
use http::{HeaderValue, Method};
use http_body::Body;
use http_body_util::BodyExt;
use navar::{
    Client,
    bound_request::{BoundRequest, RequestBody},
};
use navar_mocks::{MockApp, MockRuntime, MockTransport};
use std::fmt::Debug;

async fn inspect_body<B: RequestBody>(req: BoundRequest<B>) -> Bytes
where
    <B as Body>::Error: Debug,
{
    let mut bytes = BytesMut::new();
    let mut body = req.into_body();
    while let Some(frame) = body.frame().await {
        let frame = frame.unwrap();
        if let Ok(data) = frame.into_data() {
            bytes.extend_from_slice(data.chunk());
        }
    }
    bytes.freeze()
}

#[tokio::test]
async fn header_and_extension() {
    let client = Client::new(MockTransport, MockApp, MockRuntime);
    #[derive(Clone)]
    struct MyExt(i32);

    let req = client
        .request(Method::GET, "http://example.com")
        .header("X-Test", "Value")
        .extension(MyExt(42))
        .build()
        .unwrap();

    assert_eq!(
        req.headers().get("X-Test"),
        Some(&HeaderValue::from_static("Value"))
    );
    assert_eq!(req.extensions().get::<MyExt>().unwrap().0, 42);
}

#[tokio::test]
async fn text_body() {
    let client = Client::new(MockTransport, MockApp, MockRuntime);
    let req = client
        .request(Method::POST, "http://example.com")
        .text("hello world")
        .unwrap();

    assert_eq!(
        req.headers().get("content-type"),
        Some(&HeaderValue::from_static("text/plain; charset=utf-8"))
    );
    assert_eq!(inspect_body(req).await, Bytes::from("hello world"));
}

#[tokio::test]
async fn bytes_body() {
    let client = Client::new(MockTransport, MockApp, MockRuntime);
    let data = b"\x00\x01\x02";
    let req = client
        .request(Method::POST, "http://example.com")
        .bytes(data.as_slice())
        .unwrap();

    assert_eq!(
        req.headers().get("content-type"),
        Some(&HeaderValue::from_static("application/octet-stream"))
    );
    assert_eq!(inspect_body(req).await, Bytes::from_static(data));
}

#[cfg(feature = "json")]
#[tokio::test]
async fn json_body() {
    let client = Client::new(MockTransport, MockApp, MockRuntime);
    let data = serde_json::json!({"key": "value"});
    let req = client
        .request(Method::POST, "http://example.com")
        .json(&data)
        .unwrap();

    assert_eq!(
        req.headers().get("content-type"),
        Some(&HeaderValue::from_static("application/json"))
    );

    let body_bytes = inspect_body(req).await;
    let body_json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(body_json, data);
}

#[cfg(feature = "json")]
#[tokio::test]
async fn ndjson_body() {
    use futures_lite::stream;

    let client = Client::new(MockTransport, MockApp, MockRuntime);
    let items = vec![serde_json::json!({"a": 1}), serde_json::json!({"b": 2})];
    let stream = stream::iter(items.clone());

    let req = client
        .request(Method::POST, "http://example.com")
        .ndjson(stream)
        .unwrap();

    assert_eq!(
        req.headers().get("content-type"),
        Some(&HeaderValue::from_static("application/x-ndjson"))
    );

    let body_bytes = inspect_body(req).await;
    let body_str = std::str::from_utf8(&body_bytes).unwrap();

    let lines: Vec<&str> = body_str.lines().collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(lines[0]).unwrap(),
        items[0]
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(lines[1]).unwrap(),
        items[1]
    );
}

use http::header::CONTENT_TYPE;
use serde::Serialize;

use crate::bound_request::{BoundRequest, BoundRequestBuilder};

impl BoundRequestBuilder {
    /// Attaches a JSON body to the request and sets Content-Type
    /// to "application/json" if the field is empty
    pub fn json(mut self, value: &impl Serialize) -> anyhow::Result<BoundRequest<String>> {
        let serialized = serde_json::to_string(value)?;
        if self
            .inner
            .headers_ref()
            .is_none_or(|headers| !headers.contains_key(CONTENT_TYPE))
        {
            self.inner = self.inner.header(CONTENT_TYPE, "application/json");
        }
        self.body(serialized)
    }
}

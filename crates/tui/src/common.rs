use reqwest::header::{HeaderMap, HeaderValue};
use std::sync::Arc;

#[derive(Clone)]
pub struct ClientOptions(Arc<ClientOptionsInner>);

pub struct ClientOptionsInner {
    pub headers: HeaderMap,
}

impl ClientOptions {
    pub fn builder() -> ClientOptionsBuilder {
        ClientOptionsBuilder::default()
    }
    pub fn headers(&self) -> HeaderMap {
        self.0.headers.clone()
    }
}

pub struct ClientOptionsBuilder {
    headers: HeaderMap,
}
impl Default for ClientOptionsBuilder {
    fn default() -> Self {
        Self {
            headers: HeaderMap::new(),
        }
    }
}

impl ClientOptionsBuilder {
    pub fn headers(mut self, headers: HeaderMap) -> Self {
        self.headers = headers;
        self
    }

    pub fn header<K>(mut self, key: K, val: HeaderValue) -> Self
    where
        K: reqwest::header::IntoHeaderName,
    {
        self.headers.insert(key, val);
        self
    }

    pub fn build(mut self) -> ClientOptions {
        ClientOptions(Arc::new(ClientOptionsInner {
            headers: self.headers,
        }))
    }
}

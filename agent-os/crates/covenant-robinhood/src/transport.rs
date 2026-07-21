use std::sync::Mutex;

use async_trait::async_trait;

use crate::error::{Error, Result};

#[derive(Debug, Clone)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

#[async_trait]
pub trait Transport: Send + Sync {
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse>;
}

pub struct HttpTransport {
    client: reqwest::Client,
}

impl HttpTransport {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

impl Default for HttpTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Transport for HttpTransport {
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse> {
        let method = reqwest::Method::from_bytes(req.method.as_bytes())
            .map_err(|e| Error::Transport(format!("method {}: {e}", req.method)))?;
        let mut rb = self.client.request(method, &req.url);
        for (k, v) in &req.headers {
            rb = rb.header(k.as_str(), v.as_str());
        }
        if let Some(body) = &req.body {
            rb = rb.body(body.clone());
        }
        let resp = rb
            .send()
            .await
            .map_err(|e| Error::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        let body = resp
            .text()
            .await
            .map_err(|e| Error::Transport(e.to_string()))?;
        Ok(HttpResponse { status, body })
    }
}

#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub method: String,
    pub url: String,
    pub body: Option<String>,
    pub headers: Vec<(String, String)>,
}

struct Route {
    method: String,
    path_contains: String,
    status: u16,
    body: String,
}

/// In-memory transport for tests and dry-run: matches by method plus a
/// substring of the URL, records every request, and returns canned JSON. Feed
/// it recorded market-data responses to replay a session offline.
pub struct MockTransport {
    routes: Vec<Route>,
    recorded: Mutex<Vec<RecordedRequest>>,
}

impl MockTransport {
    pub fn new() -> Self {
        Self {
            routes: Vec::new(),
            recorded: Mutex::new(Vec::new()),
        }
    }

    pub fn route(
        mut self,
        method: &str,
        path_contains: &str,
        status: u16,
        body: impl Into<String>,
    ) -> Self {
        self.routes.push(Route {
            method: method.to_string(),
            path_contains: path_contains.to_string(),
            status,
            body: body.into(),
        });
        self
    }

    pub fn json(self, method: &str, path_contains: &str, body: serde_json::Value) -> Self {
        self.route(method, path_contains, 200, body.to_string())
    }

    pub fn recorded(&self) -> Vec<RecordedRequest> {
        self.recorded.lock().unwrap().clone()
    }

    pub fn last(&self) -> Option<RecordedRequest> {
        self.recorded.lock().unwrap().last().cloned()
    }
}

impl Default for MockTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Transport for MockTransport {
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse> {
        self.recorded.lock().unwrap().push(RecordedRequest {
            method: req.method.clone(),
            url: req.url.clone(),
            body: req.body.clone(),
            headers: req.headers.clone(),
        });
        for r in &self.routes {
            if r.method == req.method && req.url.contains(&r.path_contains) {
                return Ok(HttpResponse {
                    status: r.status,
                    body: r.body.clone(),
                });
            }
        }
        Ok(HttpResponse {
            status: 404,
            body: format!("no mock route for {} {}", req.method, req.url),
        })
    }
}

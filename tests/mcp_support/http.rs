//! Raw JSON-RPC over `rmcp`'s Streamable HTTP service, so a test controls
//! every header and `_meta` field a client sends.

use std::sync::atomic::{AtomicU64, Ordering};

use http_body_util::BodyExt;
use posthog_rs::mcp::Instrumented;
use posthog_rs::RequestContext;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use serde_json::{json, Value};

use super::server::TestServer;

pub const MODERN_PROTOCOL: &str = "2026-07-28";

pub struct HttpServer {
    service: StreamableHttpService<Instrumented<TestServer>, LocalSessionManager>,
    next_id: AtomicU64,
}

pub struct HttpResponse {
    pub session_id: Option<String>,
    pub body: Value,
}

impl HttpServer {
    /// Every request is served by a fresh clone, with no session.
    pub fn stateless(server: Instrumented<TestServer>) -> Self {
        let config = StreamableHttpServerConfig::default()
            .with_legacy_session_mode(false)
            .with_json_response(true)
            .with_sse_keep_alive(None);
        Self::new(server, config)
    }

    /// Legacy sessions, each bound to its `Mcp-Session-Id`.
    pub fn stateful(server: Instrumented<TestServer>) -> Self {
        let config = StreamableHttpServerConfig::default()
            .with_json_response(true)
            .with_sse_keep_alive(None);
        Self::new(server, config)
    }

    fn new(server: Instrumented<TestServer>, config: StreamableHttpServerConfig) -> Self {
        let service =
            StreamableHttpService::new(move || Ok(server.clone()), Default::default(), config);
        Self {
            service,
            next_id: AtomicU64::new(1),
        }
    }

    pub async fn request(
        &self,
        method: &str,
        params: Value,
        headers: &[(&str, &str)],
    ) -> HttpResponse {
        self.request_with_context(method, params, headers, None)
            .await
    }

    /// As `posthog_rs::tower::PostHogLayer` serves it: the host's context in
    /// the request's extensions, and in scope while the request runs.
    pub async fn request_with_context(
        &self,
        method: &str,
        params: Value,
        headers: &[(&str, &str)],
        context: Option<RequestContext>,
    ) -> HttpResponse {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let name = params["name"].as_str().unwrap_or_default().to_string();
        let message = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let mut all_headers = headers.to_vec();
        if is_modern(headers) {
            all_headers.push(("mcp-method", method));
            if !name.is_empty() {
                all_headers.push(("mcp-name", name.as_str()));
            }
        }
        match context {
            Some(context) => {
                let scope = context.clone();
                scope
                    .scope(self.post(message, &all_headers, Some(context)))
                    .await
            }
            None => self.post(message, &all_headers, None).await,
        }
    }

    pub async fn notify(&self, method: &str, headers: &[(&str, &str)]) {
        let message = json!({ "jsonrpc": "2.0", "method": method });
        self.post(message, headers, None).await;
    }

    async fn post(
        &self,
        message: Value,
        headers: &[(&str, &str)],
        context: Option<RequestContext>,
    ) -> HttpResponse {
        let mut request = http::Request::builder()
            .method("POST")
            .uri("http://localhost/mcp")
            .header("host", "localhost")
            .header("accept", "application/json, text/event-stream")
            .header("content-type", "application/json");
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let mut request = request.body(message.to_string()).unwrap();
        if let Some(context) = context {
            request.extensions_mut().insert(context);
        }
        let response = self.service.handle(request).await;
        let session_id = response
            .headers()
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        HttpResponse {
            session_id,
            body: parse_body(&String::from_utf8_lossy(&bytes)),
        }
    }
}

/// 2026-07-28 requests repeat their method and name as standard headers.
fn is_modern(headers: &[(&str, &str)]) -> bool {
    headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("mcp-protocol-version") && *value == MODERN_PROTOCOL
    })
}

/// A JSON body, or the first message of a server-sent event stream.
fn parse_body(body: &str) -> Value {
    if let Ok(value) = serde_json::from_str(body) {
        return value;
    }
    body.lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .filter_map(|data| serde_json::from_str::<Value>(data.trim()).ok())
        .find(|message| message.get("id").is_some())
        .unwrap_or(Value::Null)
}

/// `_meta` for a 2026-07-28 request, which carries its client on every call.
pub fn modern_meta(client_name: &str, client_version: &str) -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": MODERN_PROTOCOL,
        "io.modelcontextprotocol/clientCapabilities": {},
        "io.modelcontextprotocol/clientInfo": { "name": client_name, "version": client_version },
    })
}

pub fn call_params(name: &str, arguments: Value) -> Value {
    json!({ "name": name, "arguments": arguments })
}

pub fn result_texts(response: &HttpResponse) -> Vec<String> {
    let content = response.body["result"]["content"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    content
        .iter()
        .filter_map(|block| block["text"].as_str().map(str::to_string))
        .collect()
}

/// The handle a result's last content block carries, if it carries one.
pub fn returned_handle(response: &HttpResponse) -> Option<String> {
    let last = result_texts(response).pop()?;
    let block: Value = serde_json::from_str(&last).ok()?;
    block["conversation_id"].as_str().map(str::to_string)
}

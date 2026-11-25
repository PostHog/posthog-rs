//! A test server whose tools act out the spec's acceptance scenarios, and
//! that records the arguments each tool received.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use posthog::{Client, Event};
use tokio::sync::Notify;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    InputRequests, InputRequiredResult, JsonObject, ListToolsResult, PaginatedRequestParams,
    ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData as McpError, ServerHandler};
use serde_json::{json, Value};

pub const FAILURE_MESSAGE: &str = "GET https://svc:hunter2@internal.test/doc?token=abc failed";
pub const PAGE_SIZE: usize = 100;

#[derive(Clone, Default)]
pub struct TestServer {
    tools: Vec<Tool>,
    received: Arc<Mutex<Vec<ReceivedCall>>>,
    custom_events: Option<Arc<CustomEvents>>,
}

/// `search_docs` waits until its query is released, then captures
/// `docs_searched` through the host's client, as a tool handler would.
struct CustomEvents {
    client: Arc<Client>,
    gates: Mutex<HashMap<String, Arc<Notify>>>,
}

impl CustomEvents {
    fn gate(&self, query: &str) -> Arc<Notify> {
        let mut gates = self.gates.lock().unwrap();
        gates.entry(query.to_string()).or_default().clone()
    }

    async fn capture_after_release(&self, query: &str) {
        self.gate(query).notified().await;
        let mut event = posthog::RequestContext::current()
            .map(|context| context.event("docs_searched"))
            .unwrap_or_else(|| Event::new_anon("docs_searched"));
        event.insert_prop("query", query).unwrap();
        self.client.capture(event);
        if query.starts_with("spread") {
            self.capture_elsewhere().await;
        }
    }

    /// A batch, an event in a nested scope, and one from work that outlives
    /// the call.
    async fn capture_elsewhere(&self) {
        let context = posthog::RequestContext::current().unwrap_or_default();
        self.client
            .capture_batch(vec![context.event("docs_batched")], false);
        let nested = context.clone().property("step", "nested").unwrap();
        let client = self.client.clone();
        let nested_capture = async move { client.capture(Event::new_anon("docs_nested")) };
        nested.scope(nested_capture).await;
        let client = self.client.clone();
        let late = context.clone().scope(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            client.capture(context.event("docs_followed_up"));
        });
        tokio::spawn(late);
    }
}

struct ReceivedCall {
    tool: String,
    arguments: Option<JsonObject>,
}

impl TestServer {
    pub fn new() -> Self {
        Self::with_tools(standard_tools())
    }

    pub fn with_tools(tools: Vec<Tool>) -> Self {
        Self {
            tools,
            ..Self::default()
        }
    }

    /// A server whose `search_docs` captures a custom event, and fails when
    /// its query starts with `fail`.
    pub fn with_custom_events(client: Arc<Client>) -> Self {
        let custom_events = CustomEvents {
            client,
            gates: Mutex::default(),
        };
        Self {
            custom_events: Some(Arc::new(custom_events)),
            ..Self::new()
        }
    }

    /// Lets the `search_docs` call for `query` go on, before or after it waits.
    pub fn release(&self, query: &str) {
        let custom_events = self.custom_events.as_ref().expect("custom events");
        custom_events.gate(query).notify_one();
    }

    /// The arguments the tool named `name` received last.
    pub fn received(&self, name: &str) -> Option<JsonObject> {
        let received = self.received.lock().unwrap();
        let last = received.iter().rev().find(|call| call.tool == name);
        last.map(|call| call.arguments.clone().unwrap_or_default())
    }
}

pub fn tool(value: Value) -> Tool {
    serde_json::from_value(value).unwrap()
}

fn object_schema(properties: Value) -> Value {
    json!({ "type": "object", "properties": properties })
}

fn standard_tools() -> Vec<Tool> {
    let simple =
        |name: &str| tool(json!({ "name": name, "inputSchema": object_schema(json!({})) }));
    vec![
        tool(json!({
            "name": "search_docs",
            "description": "Search the docs",
            "inputSchema": {
                "type": "object",
                "properties": { "query": { "type": "string" } },
                "additionalProperties": false,
            },
            "_meta": { "category": "docs" },
        })),
        tool(json!({
            "name": "summarize",
            "inputSchema": object_schema(json!({ "context": { "type": "string" } })),
        })),
        tool(json!({
            "name": "fetch_page",
            "inputSchema": object_schema(json!({ "url": { "type": "string" }, "api_key": { "type": "string" } })),
        })),
        simple("query"),
        simple("get_user"),
        simple("export"),
        simple("screenshot"),
        simple("deploy"),
        simple("explode"),
    ]
}

pub fn numbered_tools(count: usize) -> Vec<Tool> {
    (0..count)
        .map(|index| tool(json!({ "name": format!("tool_{index:03}"), "inputSchema": object_schema(json!({})) })))
        .collect()
}

fn text(text: impl Into<String>) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(text)])
}

fn deploy_round() -> InputRequiredResult {
    let requests: InputRequests = serde_json::from_value(json!({
        "confirm": {
            "method": "elicitation/create",
            "params": {
                "message": "Deploy to production?",
                "requestedSchema": {
                    "type": "object",
                    "properties": { "confirm": { "type": "boolean" } },
                },
            },
        },
    }))
    .unwrap();
    InputRequiredResult::from_input_requests(requests)
}

impl ServerHandler for TestServer {
    fn get_info(&self) -> ServerConfig {
        let capabilities = ServerCapabilities::builder().enable_tools().build();
        ServerConfig::new(capabilities)
            .with_server_info(Implementation::new("docs-server", "1.2.3"))
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let start: usize = request
            .and_then(|request| request.cursor)
            .and_then(|cursor| cursor.parse().ok())
            .unwrap_or(0);
        let end = (start + PAGE_SIZE).min(self.tools.len());
        let mut page = ListToolsResult::with_all_items(self.tools[start..end].to_vec());
        if end < self.tools.len() {
            page.next_cursor = Some(end.to_string());
        }
        Ok(page)
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tools.iter().find(|tool| tool.name == name).cloned()
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        if self.get_tool(&request.name).is_none() {
            return Err(McpError::invalid_params("tool not found", None));
        }
        let name = request.name.to_string();
        self.received.lock().unwrap().push(ReceivedCall {
            tool: name.clone(),
            arguments: request.arguments.clone(),
        });
        if let (Some(custom_events), "search_docs") = (&self.custom_events, name.as_str()) {
            let query = request
                .arguments
                .as_ref()
                .and_then(|arguments| arguments.get("query")?.as_str());
            let query = query.unwrap_or_default().to_string();
            custom_events.capture_after_release(&query).await;
            if query.starts_with("fail") {
                return Err(McpError::internal_error("search failed", None));
            }
        }
        let response = match name.as_str() {
            "fetch_page" => CallToolResult::error(vec![ContentBlock::text(FAILURE_MESSAGE)]).into(),
            "query" => return Err(McpError::internal_error("upstream timed out", None)),
            "get_user" => {
                let user = json!({ "user": "ada", "password": "hunter2" });
                let mut result = CallToolResult::structured(user.clone());
                result.content = vec![ContentBlock::text(user.to_string())];
                result.into()
            }
            "export" => text("x".repeat(2 * 1024 * 1024)).into(),
            "screenshot" => {
                let image = "iVBORw0KGgo".repeat(20_000);
                CallToolResult::success(vec![
                    ContentBlock::text("done"),
                    ContentBlock::image(image, "image/png"),
                ])
                .into()
            }
            "deploy" if request.input_responses.is_none() => deploy_round().into(),
            "deploy" => text("deployed").into(),
            "explode" => panic!("index out of bounds"),
            _ => text("Found 3 results").into(),
        };
        Ok(response)
    }
}

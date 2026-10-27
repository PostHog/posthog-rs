//! A test server whose tools act out the spec's acceptance scenarios, and
//! that records the arguments each tool received.

use std::sync::{Arc, Mutex};

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
            received: Arc::default(),
        }
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

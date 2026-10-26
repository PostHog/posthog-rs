use serde_json::{Map, Value};

/// The MCP request a callback runs for.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct RequestInfo<'a> {
    /// The JSON-RPC method, such as `tools/call`.
    pub method: &'a str,
    pub tool_name: Option<&'a str>,
    /// The tool arguments as the agent sent them, analytics arguments included.
    pub arguments: Option<&'a Map<String, Value>>,
    /// The request's `_meta`.
    pub meta: Option<&'a Map<String, Value>>,
    /// The HTTP request, on HTTP transports. Its extensions carry whatever
    /// the host's HTTP middleware attached, such as the authenticated user.
    pub http: Option<&'a http::request::Parts>,
}

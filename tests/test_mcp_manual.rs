//! The manual MCP analytics API, for servers analytics cannot wrap.

#![cfg(feature = "mcp")]

mod mcp_support;

use mcp_support::http::{call_params, HttpServer};
use mcp_support::recorder::{property, text_property, Recorder};
use mcp_support::server::TestServer;
use posthog::mcp::{
    Connection, McpAnalytics, McpOptions, McpOptionsBuilder, ToolCallError, ToolCallRequest,
    ToolRegistration, TransportSession,
};
use posthog::RequestContext;
use rmcp::model::{
    CallToolResult, ContentBlock, InputRequests, InputRequiredResult, JsonObject, Tool,
};
use rmcp::ServerHandler;
use serde_json::{json, Value};

#[derive(Debug)]
struct UpstreamTimeout;

impl std::fmt::Display for UpstreamTimeout {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("upstream timed out")
    }
}

impl std::error::Error for UpstreamTimeout {}

fn arguments(value: serde_json::Value) -> Option<JsonObject> {
    Some(serde_json::from_value(value).unwrap())
}

fn search_docs() -> Tool {
    TestServer::new().get_tool("search_docs").unwrap()
}

#[tokio::test]
async fn a_manually_captured_call_matches_the_automatic_one() {
    let recorder = Recorder::new().await;
    let analytics = McpAnalytics::new(recorder.client.clone(), McpOptions::default());
    let call_arguments = json!({ "query": "flags", "context": "find the flags docs" });

    let http = HttpServer::stateless(analytics.instrument(TestServer::new()));
    http.request(
        "tools/call",
        call_params("search_docs", call_arguments.clone()),
        &[],
    )
    .await;
    let manual = analytics.start_tool_call(
        ToolCallRequest::new("search_docs", arguments(call_arguments))
            .tool(ToolRegistration::Registered(search_docs())),
    );
    assert_eq!(
        manual.arguments(),
        arguments(json!({ "query": "flags" })).as_ref()
    );
    manual.complete(CallToolResult::success(vec![ContentBlock::text(
        "Found 3 results",
    )]));

    let events = recorder.named("$mcp_tool_call").await;
    assert_eq!(events.len(), 2);
    for key in [
        "$mcp_tool_name",
        "$mcp_intent",
        "$mcp_parameters",
        "$mcp_is_error",
    ] {
        assert_eq!(
            property(&events[0], key),
            property(&events[1], key),
            "{}",
            key
        );
    }
    assert!(text_property(&events[1], "$session_id").starts_with("ses_"));
}

#[tokio::test]
async fn an_explicit_error_type_labels_the_call_but_not_the_exception() {
    let recorder = Recorder::new().await;
    let analytics = McpAnalytics::new(recorder.client.clone(), McpOptions::default());

    let call = analytics.start_tool_call(
        ToolCallRequest::new("search_docs", arguments(json!({ "query": "flags" })))
            .tool(ToolRegistration::Registered(search_docs()))
            .transport_session(TransportSession::Connection(Connection::new())),
    );
    call.fail(ToolCallError::new(&UpstreamTimeout).error_type("validation"));

    let event = recorder.only("$mcp_tool_call").await;
    assert_eq!(text_property(&event, "$mcp_error_type"), "validation");
    assert_eq!(
        text_property(&event, "$mcp_error_message"),
        "upstream timed out"
    );
    let exception = recorder.only("$exception").await;
    assert_eq!(
        property(&exception, "$exception_list")[0]["type"],
        "UpstreamTimeout"
    );
}

#[tokio::test]
async fn a_thrown_errors_type_names_the_failure() {
    let recorder = Recorder::new().await;
    let analytics = McpAnalytics::new(recorder.client.clone(), McpOptions::default());

    let call = analytics.start_tool_call(ToolCallRequest::new("query", None));
    call.fail(ToolCallError::new(&UpstreamTimeout));

    let event = recorder.only("$mcp_tool_call").await;
    assert_eq!(text_property(&event, "$mcp_error_type"), "UpstreamTimeout");
}

#[tokio::test]
async fn a_call_to_an_unregistered_tool_is_recorded_as_unknown() {
    let recorder = Recorder::new().await;
    let analytics = McpAnalytics::new(recorder.client.clone(), McpOptions::default());

    let call = analytics.start_tool_call(
        ToolCallRequest::new("no_such_tool", arguments(json!({})))
            .tool(ToolRegistration::NotRegistered),
    );
    call.fail(ToolCallError::new(&UpstreamTimeout));

    assert!(recorder.named("$mcp_tool_call").await.is_empty());
    let event = recorder.only("$mcp_unknown_tool").await;
    assert_eq!(text_property(&event, "$mcp_tool_name"), "no_such_tool");
}

#[tokio::test]
async fn listing_advertises_the_analytics_arguments() {
    let recorder = Recorder::new().await;
    let analytics = McpAnalytics::new(recorder.client.clone(), McpOptions::default());

    let tools = analytics.list_tools(vec![search_docs()]);

    let schema = serde_json::to_value(&tools[0].input_schema).unwrap();
    for argument in ["context", "llm_model", "conversation_id"] {
        assert_eq!(
            schema["properties"][argument]["type"], "string",
            "{}",
            argument
        );
    }
    assert_eq!(
        search_docs()
            .input_schema
            .get("properties")
            .unwrap()
            .as_object()
            .unwrap()
            .len(),
        1
    );
}

fn route_context(route: &str) -> RequestContext {
    RequestContext::new()
        .property("route", route)
        .unwrap()
        .option("product_tour_id", route)
        .unwrap()
}

fn http_parts(context: RequestContext) -> http::request::Parts {
    let mut request = http::Request::new(());
    request.extensions_mut().insert(context);
    request.into_parts().0
}

#[tokio::test]
async fn a_manual_call_takes_its_scopes_context_over_its_http_requests() {
    let recorder = Recorder::new().await;
    let analytics = McpAnalytics::new(recorder.client.clone(), McpOptions::default());
    let request =
        |tool: &str| ToolCallRequest::new(tool, None).http(http_parts(route_context("/from-http")));

    analytics
        .start_tool_call(request("search_docs"))
        .complete(CallToolResult::success(vec![]));
    route_context("/from-scope")
        .scope(async {
            analytics
                .start_tool_call(request("query"))
                .complete(CallToolResult::success(vec![]));
        })
        .await;

    for (tool, route) in [("search_docs", "/from-http"), ("query", "/from-scope")] {
        let event = recorder
            .named("$mcp_tool_call")
            .await
            .into_iter()
            .find(|event| text_property(event, "$mcp_tool_name") == tool)
            .unwrap();
        assert_eq!(text_property(&event, "route"), route);
        assert_eq!(event.options()["product_tour_id"], route);
        assert_eq!(event.options()["process_person_profile"], false);
    }
}

/// Analytics whose `event_options` callback sets three options, one of them
/// `process_person_profile: true`.
fn analytics_with_event_options(
    recorder: &Recorder,
    mut options: McpOptionsBuilder,
) -> McpAnalytics {
    let options = options
        .event_options(|request| {
            assert_eq!(request.method, "tools/call");
            let options = json!({
                "disable_skew_correction": true,
                "product_tour_id": "from-callback",
                "process_person_profile": true,
            });
            serde_json::from_value(options).ok()
        })
        .build()
        .unwrap();
    McpAnalytics::new(recorder.client.clone(), options)
}

/// A call whose own options change `product_tour_id`, try to turn person
/// processing on, and set a `null` that must not clear the callback's value.
fn request_with_options(tool: &str) -> ToolCallRequest {
    ToolCallRequest::new(tool, arguments(json!({})))
        .event_option("product_tour_id", "per-call")
        .unwrap()
        .event_option("process_person_profile", true)
        .unwrap()
        .event_option("disable_skew_correction", Value::Null)
        .unwrap()
}

#[tokio::test]
async fn call_options_win_over_the_callback_and_reach_every_event_of_a_call() {
    let recorder = Recorder::new().await;
    let analytics = analytics_with_event_options(&recorder, McpOptionsBuilder::default());
    let context = RequestContext::new()
        .option("cookieless_mode", true)
        .unwrap()
        .option("product_tour_id", "from-context")
        .unwrap()
        .option("disable_skew_correction", false)
        .unwrap();

    context
        .scope(async {
            analytics
                .start_tool_call(request_with_options("search_docs"))
                .fail(ToolCallError::new(&UpstreamTimeout));
            analytics.start_tool_call(
                request_with_options("no_such_tool").tool(ToolRegistration::NotRegistered),
            );
            let requests: InputRequests = serde_json::from_value(json!({
                "confirm": { "method": "elicitation/create", "params": {
                    "message": "Deploy?", "requestedSchema": { "type": "object", "properties": {} },
                } },
            }))
            .unwrap();
            let round = InputRequiredResult::from_input_requests(requests);
            analytics
                .start_tool_call(request_with_options("deploy"))
                .input_required(&round);
        })
        .await;

    let sent = recorder.sent_events().await;
    let mut names: Vec<&str> = sent
        .iter()
        .map(|event| event["event"].as_str().unwrap())
        .collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "$exception",
            "$mcp_input_required",
            "$mcp_tool_call",
            "$mcp_unknown_tool"
        ]
    );
    for event in &sent {
        assert_eq!(
            event["options"],
            json!({
                "cookieless_mode": true,
                "disable_skew_correction": true,
                "product_tour_id": "per-call",
                "process_person_profile": false,
            }),
            "{}",
            event["event"]
        );
    }
}

#[tokio::test]
async fn an_identified_call_sends_the_options_it_was_given() {
    let recorder = Recorder::new().await;
    let mut identified = McpOptionsBuilder::default();
    identified.identify(|_| Some(posthog::mcp::Identity::new("user-123")));
    let analytics = analytics_with_event_options(&recorder, identified);

    analytics
        .start_tool_call(request_with_options("search_docs"))
        .complete(CallToolResult::success(vec![]));

    let sent = recorder.sent("$mcp_tool_call").await;
    assert_eq!(sent["distinct_id"], "user-123");
    assert_eq!(
        sent["options"],
        json!({
            "disable_skew_correction": true,
            "product_tour_id": "per-call",
            "process_person_profile": true,
        })
    );
}

#[tokio::test]
async fn the_mcp_hook_sees_the_options_and_what_it_returns_is_sent() {
    let recorder = Recorder::new().await;
    let mut hooked = McpOptionsBuilder::default();
    hooked.before_send(|mut event| {
        assert_eq!(event.options()["product_tour_id"], "per-call");
        assert_eq!(event.options()["disable_skew_correction"], true);
        event.remove_option("disable_skew_correction");
        event.insert_option("product_tour_id", "from-hook").unwrap();
        Some(event)
    });
    let analytics = analytics_with_event_options(&recorder, hooked);

    analytics
        .start_tool_call(request_with_options("search_docs"))
        .complete(CallToolResult::success(vec![]));

    let expected = json!({ "product_tour_id": "from-hook", "process_person_profile": false });
    let seen_by_client_hook = recorder.only("$mcp_tool_call").await;
    assert_eq!(
        serde_json::to_value(seen_by_client_hook.options()).unwrap(),
        expected
    );
    assert_eq!(recorder.sent("$mcp_tool_call").await["options"], expected);
}

const CHATGPT: &str = "openai-mcp/1.0.0 (ChatGPT)";

fn headers(user_agent: &str) -> http::HeaderMap {
    let mut headers = http::HeaderMap::new();
    headers.insert("user-agent", user_agent.parse().unwrap());
    headers
}

#[tokio::test]
async fn only_a_handle_free_clients_manual_listing_leaves_out_conversation_id() {
    let recorder = Recorder::new().await;
    let analytics = McpAnalytics::new(recorder.client.clone(), McpOptions::default());
    let counter = posthog_tool(json!({
        "name": "count",
        "inputSchema": { "type": "object", "properties": {} },
        "outputSchema": { "type": "object", "properties": { "count": { "type": "number" } } },
    }));

    for (user_agent, advertised) in [(CHATGPT, false), ("example-client/1.0", true)] {
        let tools = analytics.list_tools_for_request(vec![counter.clone()], &headers(user_agent));

        let tool = serde_json::to_value(&tools[0]).unwrap();
        let input = &tool["inputSchema"]["properties"];
        let output = &tool["outputSchema"]["properties"];
        assert!(input.get("context").is_some(), "{}", user_agent);
        assert_eq!(
            input.get("conversation_id").is_some(),
            advertised,
            "{}",
            user_agent
        );
        assert_eq!(
            output.get("_mcp_instructions").is_some(),
            advertised,
            "{}",
            user_agent
        );
    }
}

fn posthog_tool(value: Value) -> Tool {
    serde_json::from_value(value).unwrap()
}

#[tokio::test]
async fn a_bound_handshake_names_the_client_of_a_manual_call() {
    let recorder = Recorder::new().await;
    let analytics = McpAnalytics::new(recorder.client.clone(), McpOptions::default());
    let meta =
        serde_json::from_value(json!({ "sessionId": "019a4f2e-1c3d-7b8a-9e0f-1a2b3c4d5e6f" }))
            .unwrap();

    let call = analytics.start_tool_call(
        ToolCallRequest::new("search_docs", arguments(json!({ "query": "flags" })))
            .tool(ToolRegistration::Registered(search_docs()))
            .meta(meta)
            .client_info(rmcp::model::Implementation::new(
                "codex-mcp-client",
                "0.50.0",
            ))
            .protocol_version("2025-11-25"),
    );
    let delivered = call.complete(CallToolResult::success(vec![ContentBlock::text("ok")]));

    assert_eq!(
        delivered.content.len(),
        1,
        "a recognized client id mints no handle"
    );
    let event = recorder.only("$mcp_tool_call").await;
    assert_eq!(
        text_property(&event, "$session_id"),
        "ses_0fdf126b8e2c1f9689ca12f3dd3383e6"
    );
    assert_eq!(
        text_property(&event, "$mcp_session_source"),
        "client_metadata"
    );
    assert_eq!(
        text_property(&event, "$mcp_client_name"),
        "codex-mcp-client"
    );
    assert_eq!(text_property(&event, "$mcp_protocol_version"), "2025-11-25");
}

#[tokio::test]
async fn a_manual_calls_context_gives_custom_events_its_final_session() {
    for delivered in [true, false] {
        let recorder = Recorder::new().await;
        let analytics = McpAnalytics::new(recorder.client.clone(), McpOptions::default());
        let call = analytics.start_tool_call(
            ToolCallRequest::new("search_docs", arguments(json!({ "query": "flags" })))
                .tool(ToolRegistration::Registered(search_docs())),
        );

        let context = call
            .request_context()
            .expect("a known tool's call has a context");
        let client = recorder.client.clone();
        context
            .scope(async move { client.capture(posthog::Event::new_anon("docs_searched")) })
            .await;
        if delivered {
            call.complete(CallToolResult::success(vec![ContentBlock::text("ok")]));
        } else {
            call.fail(ToolCallError::new(&UpstreamTimeout));
        }

        let custom = recorder.only("docs_searched").await;
        let tool_call = recorder.only("$mcp_tool_call").await;
        for key in ["$session_id", "$mcp_session_source"] {
            assert_eq!(
                text_property(&custom, key),
                text_property(&tool_call, key),
                "{} delivered {}",
                key,
                delivered
            );
        }
    }
}

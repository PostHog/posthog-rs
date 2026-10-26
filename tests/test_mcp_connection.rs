//! MCP analytics over a one-client connection, the way stdio serves an agent.

#![cfg(feature = "mcp")]

mod mcp_support;

use mcp_support::recorder::{property, serialized_properties_size, text_property, Recorder};
use mcp_support::server::{numbered_tools, TestServer, FAILURE_MESSAGE, PAGE_SIZE};
use posthog_rs::mcp::{Identity, McpAnalytics, McpOptions, McpOptionsBuilder};
use rmcp::model::{CallToolRequestParams, CallToolResult, JsonObject};
use rmcp::service::{RoleClient, RunningService};
use rmcp::ServiceExt;
use serde_json::{json, Value};

type Client = RunningService<RoleClient, ()>;

async fn connect(recorder: &Recorder, server: TestServer, options: McpOptions) -> Client {
    let analytics = McpAnalytics::new(recorder.client.clone(), options);
    let instrumented = analytics.instrument(server);
    let (server_io, client_io) = tokio::io::duplex(16 * 1024 * 1024);
    tokio::spawn(async move {
        let running = instrumented.serve(server_io).await.unwrap();
        let _ = running.waiting().await;
    });
    ().serve(client_io).await.unwrap()
}

async fn call(client: &Client, name: &str, arguments: Value) -> CallToolResult {
    let arguments: JsonObject = serde_json::from_value(arguments).unwrap();
    let request = CallToolRequestParams::new(name.to_string()).with_arguments(arguments);
    client.call_tool(request).await.unwrap()
}

fn texts(result: &CallToolResult) -> Vec<String> {
    let content = serde_json::to_value(&result.content).unwrap();
    content
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|block| block["text"].as_str().map(str::to_string))
        .collect()
}

#[tokio::test]
async fn a_successful_call_produces_one_event_with_the_required_properties() {
    let recorder = Recorder::new().await;
    let client = connect(&recorder, TestServer::new(), McpOptions::default()).await;

    let result = call(&client, "search_docs", json!({ "query": "flags" })).await;

    assert_eq!(texts(&result), vec!["Found 3 results"]);
    assert_eq!(result.is_error, Some(false));
    let event = recorder.only("$mcp_tool_call").await;
    assert_eq!(text_property(&event, "$mcp_tool_name"), "search_docs");
    assert_eq!(property(&event, "$mcp_is_error"), &json!(false));
    assert!(property(&event, "$mcp_duration_ms").is_number());
    assert_eq!(
        text_property(&event, "$mcp_source"),
        "posthog_mcp_analytics"
    );
    assert_eq!(text_property(&event, "$lib"), "posthog-rs-mcp");
    assert_eq!(text_property(&event, "$mcp_server_name"), "docs-server");
    let session_id = text_property(&event, "$session_id");
    assert!(session_id.starts_with("ses_"));
    assert_eq!(event.distinct_id(), session_id);
    assert_eq!(property(&event, "$process_person_profile"), &json!(false));
}

#[tokio::test]
async fn tool_metadata_comes_from_the_registry_without_a_listing() {
    let recorder = Recorder::new().await;
    let client = connect(&recorder, TestServer::new(), McpOptions::default()).await;

    call(&client, "search_docs", json!({ "query": "flags" })).await;

    let event = recorder.only("$mcp_tool_call").await;
    assert_eq!(
        text_property(&event, "$mcp_tool_description"),
        "Search the docs"
    );
    assert_eq!(text_property(&event, "$mcp_tool_category"), "docs");
}

#[tokio::test]
async fn the_context_argument_is_advertised_captured_and_removed() {
    let recorder = Recorder::new().await;
    let server = TestServer::new();
    let client = connect(&recorder, server.clone(), McpOptions::default()).await;

    let tools = client.list_all_tools().await.unwrap();
    let search = tools
        .iter()
        .find(|tool| tool.name == "search_docs")
        .unwrap();
    let schema = serde_json::to_value(&search.input_schema).unwrap();
    assert_eq!(schema["properties"]["context"]["type"], "string");
    assert!(schema["required"]
        .as_array()
        .unwrap()
        .contains(&json!("context")));

    let arguments =
        json!({ "query": "flags", "context": "find the flags docs", "llm_model": "model-a" });
    call(&client, "search_docs", arguments).await;

    assert_eq!(
        server.received("search_docs"),
        json!({ "query": "flags" }).as_object().cloned()
    );
    let event = recorder.only("$mcp_tool_call").await;
    assert_eq!(text_property(&event, "$mcp_intent"), "find the flags docs");
    assert_eq!(
        text_property(&event, "$mcp_intent_source"),
        "context_parameter"
    );
    assert_eq!(text_property(&event, "$mcp_llm_model"), "model-a");
    assert_eq!(
        text_property(&event, "$mcp_llm_model_source"),
        "self_reported"
    );
    assert_eq!(
        property(&event, "$mcp_parameters"),
        &json!({ "request": { "method": "tools/call", "params": { "name": "search_docs", "arguments": { "query": "flags" } } } })
    );
}

#[tokio::test]
async fn a_tool_that_declares_its_own_context_keeps_it_as_data() {
    let recorder = Recorder::new().await;
    let server = TestServer::new();
    let client = connect(&recorder, server.clone(), McpOptions::default()).await;

    call(
        &client,
        "summarize",
        json!({ "context": "the meeting notes" }),
    )
    .await;

    let received = server.received("summarize").unwrap();
    assert_eq!(received.get("context"), Some(&json!("the meeting notes")));
    let event = recorder.only("$mcp_tool_call").await;
    assert!(!event.properties().contains_key("$mcp_intent"));
    let arguments = &property(&event, "$mcp_parameters")["request"]["params"]["arguments"];
    assert_eq!(arguments["context"], "the meeting notes");
}

#[tokio::test]
async fn personal_data_in_the_intent_is_redacted() {
    let recorder = Recorder::new().await;
    let client = connect(&recorder, TestServer::new(), McpOptions::default()).await;

    let arguments = json!({ "query": "orders", "context": "find orders for alice@example.com" });
    call(&client, "search_docs", arguments).await;

    let event = recorder.only("$mcp_tool_call").await;
    assert_eq!(
        text_property(&event, "$mcp_intent"),
        "find orders for [redacted]"
    );
}

#[tokio::test]
async fn a_result_that_reports_an_error_is_a_failed_call() {
    let recorder = Recorder::new().await;
    let client = connect(&recorder, TestServer::new(), McpOptions::default()).await;

    let arguments = json!({ "url": "https://svc:hunter2@internal.test/doc?token=abc", "api_key": "sk-live-123" });
    let result = call(&client, "fetch_page", arguments).await;

    assert_eq!(result.is_error, Some(true));
    let event = recorder.only("$mcp_tool_call").await;
    assert_eq!(property(&event, "$mcp_is_error"), &json!(true));
    assert_eq!(text_property(&event, "$mcp_error_type"), "Error");
    let message = text_property(&event, "$mcp_error_message");
    assert!(
        !message.contains("hunter2") && !message.contains("token=abc"),
        "{}",
        message
    );
    assert!(message.starts_with("GET https://"));
    let parameters = property(&event, "$mcp_parameters").to_string();
    for secret in ["hunter2", "sk-live-123", "token=abc"] {
        assert!(!parameters.contains(secret), "{}", parameters);
    }

    let exception = recorder.only("$exception").await;
    let first = &property(&exception, "$exception_list")[0];
    assert_eq!(first["type"], "Error");
    assert_eq!(first["mechanism"]["synthetic"], true);
    assert_eq!(
        text_property(&exception, "$exception_source"),
        "mcp.tool_call"
    );
    assert_eq!(text_property(&exception, "$mcp_tool_name"), "fetch_page");
    assert_ne!(FAILURE_MESSAGE, message);
}

#[tokio::test]
async fn a_raised_error_is_a_failed_call_and_still_reaches_the_client() {
    let recorder = Recorder::new().await;
    let client = connect(&recorder, TestServer::new(), McpOptions::default()).await;

    let arguments: JsonObject = serde_json::from_value(json!({})).unwrap();
    let request = CallToolRequestParams::new("query").with_arguments(arguments);
    let error = client.call_tool(request).await.unwrap_err();

    assert!(
        error.to_string().contains("upstream timed out"),
        "{}",
        error
    );
    let event = recorder.only("$mcp_tool_call").await;
    assert_eq!(property(&event, "$mcp_is_error"), &json!(true));
    assert_eq!(text_property(&event, "$mcp_error_type"), "McpError");
    assert_eq!(
        text_property(&event, "$mcp_error_message"),
        "upstream timed out"
    );
    let exception = recorder.only("$exception").await;
    assert_eq!(
        property(&exception, "$exception_list")[0]["type"],
        "McpError"
    );
    assert_eq!(text_property(&exception, "$exception_level"), "error");
    assert_eq!(
        text_property(&exception, "$session_id"),
        text_property(&event, "$session_id")
    );
}

#[tokio::test]
async fn a_panicking_handler_is_a_failed_call() {
    let recorder = Recorder::new().await;
    let client = connect(&recorder, TestServer::new(), McpOptions::default()).await;

    let arguments: JsonObject = serde_json::from_value(json!({})).unwrap();
    let request = CallToolRequestParams::new("explode").with_arguments(arguments);
    let reply = tokio::time::timeout(std::time::Duration::from_secs(1), client.call_tool(request));
    assert!(
        !matches!(reply.await, Ok(Ok(_))),
        "the panic reaches rmcp, which answers nothing"
    );

    let event = recorder.only("$mcp_tool_call").await;
    assert_eq!(property(&event, "$mcp_is_error"), &json!(true));
    assert_eq!(text_property(&event, "$mcp_error_type"), "Panic");
    assert_eq!(
        text_property(&event, "$mcp_error_message"),
        "index out of bounds"
    );
    let exception = recorder.only("$exception").await;
    assert_eq!(property(&exception, "$exception_list")[0]["type"], "Panic");
    assert_eq!(
        &property(&exception, "$exception_list")[0]["mechanism"]["handled"],
        &json!(false)
    );
}

#[tokio::test]
async fn a_call_to_a_missing_tool_is_recorded_as_unknown() {
    let recorder = Recorder::new().await;
    let client = connect(&recorder, TestServer::new(), McpOptions::default()).await;

    let arguments: JsonObject = serde_json::from_value(json!({})).unwrap();
    let request = CallToolRequestParams::new("no_such_tool").with_arguments(arguments);
    assert!(client.call_tool(request).await.is_err());

    assert!(recorder.named("$mcp_tool_call").await.is_empty());
    let event = recorder.only("$mcp_unknown_tool").await;
    assert_eq!(text_property(&event, "$mcp_tool_name"), "no_such_tool");
}

#[tokio::test]
async fn a_credential_in_a_results_json_copy_is_redacted() {
    let recorder = Recorder::new().await;
    let client = connect(&recorder, TestServer::new(), McpOptions::default()).await;

    call(&client, "get_user", json!({})).await;

    let event = recorder.only("$mcp_tool_call").await;
    let response = property(&event, "$mcp_response");
    assert_eq!(
        response["content"][0]["text"],
        r#"{"password":"[redacted]","user":"ada"}"#
    );
    assert!(!response.to_string().contains("hunter2"));
}

#[tokio::test]
async fn media_in_a_result_is_never_sent() {
    let recorder = Recorder::new().await;
    let client = connect(&recorder, TestServer::new(), McpOptions::default()).await;

    call(&client, "screenshot", json!({})).await;

    let event = recorder.only("$mcp_tool_call").await;
    let response = property(&event, "$mcp_response").to_string();
    assert!(response.contains("done"));
    assert!(!response.contains("iVBORw0KGgo"));
}

#[tokio::test]
async fn a_very_large_response_still_produces_a_bounded_event() {
    let recorder = Recorder::new().await;
    let client = connect(&recorder, TestServer::new(), McpOptions::default()).await;

    let result = call(&client, "export", json!({})).await;

    assert_eq!(texts(&result)[0].len(), 2 * 1024 * 1024);
    let event = recorder.only("$mcp_tool_call").await;
    assert!(serialized_properties_size(&event) <= 102_400);
    assert_eq!(text_property(&event, "$mcp_tool_name"), "export");
}

#[tokio::test]
async fn a_failing_before_send_hook_drops_the_event_but_not_the_result() {
    let recorder = Recorder::new().await;
    let options = McpOptionsBuilder::default()
        .before_send(|_| panic!("hook failure"))
        .build()
        .unwrap();
    let client = connect(&recorder, TestServer::new(), options).await;

    let result = call(&client, "search_docs", json!({ "query": "flags" })).await;

    assert_eq!(texts(&result), vec!["Found 3 results"]);
    assert!(recorder.named("$mcp_tool_call").await.is_empty());
}

#[tokio::test]
async fn dropping_a_failed_call_also_drops_its_exception() {
    let recorder = Recorder::new().await;
    let options = McpOptionsBuilder::default()
        .before_send(|event| (event.event_name() != "$mcp_tool_call").then_some(event))
        .build()
        .unwrap();
    let client = connect(&recorder, TestServer::new(), options).await;

    call(
        &client,
        "fetch_page",
        json!({ "url": "https://example.com" }),
    )
    .await;

    assert!(recorder.events().await.iter().all(|event| {
        let name = event.event_name();
        name != "$mcp_tool_call" && name != "$exception"
    }));
}

#[tokio::test]
async fn an_unreachable_posthog_endpoint_does_not_affect_the_call() {
    let recorder = Recorder::with_status(503).await;
    let client = connect(&recorder, TestServer::new(), McpOptions::default()).await;

    let result = call(&client, "search_docs", json!({ "query": "flags" })).await;

    assert_eq!(texts(&result), vec!["Found 3 results"]);
}

#[tokio::test]
async fn calls_on_one_connection_share_its_session() {
    let recorder = Recorder::new().await;
    let client = connect(&recorder, TestServer::new(), McpOptions::default()).await;

    let first = call(&client, "search_docs", json!({ "query": "a" })).await;
    call(&client, "search_docs", json!({ "query": "b" })).await;

    assert_eq!(
        texts(&first),
        vec!["Found 3 results"],
        "no handle on a connection"
    );
    let events = recorder.named("$mcp_tool_call").await;
    assert_eq!(events.len(), 2);
    assert_eq!(
        text_property(&events[0], "$session_id"),
        text_property(&events[1], "$session_id")
    );
    assert!(!events[0].properties().contains_key("$mcp_conversation_id"));
}

#[tokio::test]
async fn an_identified_call_processes_the_person() {
    let recorder = Recorder::new().await;
    let options = McpOptionsBuilder::default()
        .identify(|_| {
            Some(
                Identity::new("user-123")
                    .with_property("plan", "pro")
                    .with_group("company", "acme"),
            )
        })
        .build()
        .unwrap();
    let client = connect(&recorder, TestServer::new(), options).await;

    call(&client, "search_docs", json!({ "query": "flags" })).await;

    let event = recorder.only("$mcp_tool_call").await;
    assert_eq!(event.distinct_id(), "user-123");
    assert_eq!(property(&event, "$set"), &json!({ "plan": "pro" }));
    assert_ne!(
        event.properties().get("$process_person_profile"),
        Some(&json!(false))
    );
    assert_eq!(property(&event, "$groups"), &json!({ "company": "acme" }));
}

#[tokio::test]
async fn a_listing_records_names_and_only_the_result_envelope() {
    let recorder = Recorder::new().await;
    let server = TestServer::with_tools(numbered_tools(PAGE_SIZE + 1));
    let client = connect(&recorder, server, McpOptions::default()).await;

    let page = client.list_tools(None).await.unwrap();

    assert_eq!(page.tools.len(), PAGE_SIZE);
    let event = recorder.named("$mcp_tools_list").await.remove(0);
    assert_eq!(
        property(&event, "$mcp_listed_tool_names")
            .as_array()
            .unwrap()
            .len(),
        PAGE_SIZE
    );
    let response = property(&event, "$mcp_response").to_string();
    assert!(response.contains("nextCursor"));
    assert!(!response.contains("tools"));
}

#[tokio::test]
async fn a_single_page_listing_sends_no_empty_envelope() {
    let recorder = Recorder::new().await;
    let client = connect(&recorder, TestServer::new(), McpOptions::default()).await;

    client.list_tools(None).await.unwrap();

    let event = recorder.only("$mcp_tools_list").await;
    assert!(!event.properties().contains_key("$mcp_response"));
}

#[tokio::test]
async fn the_virtual_tool_is_not_advertised_by_default() {
    let recorder = Recorder::new().await;
    let client = connect(&recorder, TestServer::new(), McpOptions::default()).await;

    let tools = client.list_all_tools().await.unwrap();

    assert!(tools.iter().all(|tool| tool.name != "get_more_tools"));
}

#[tokio::test]
async fn a_missing_capability_report_carries_its_text_as_intent() {
    let recorder = Recorder::new().await;
    let options = McpOptionsBuilder::default()
        .missing_capability_tool("")
        .build()
        .unwrap();
    let client = connect(&recorder, TestServer::new(), options).await;

    let tools = client.list_all_tools().await.unwrap();
    assert!(tools.iter().any(|tool| tool.name == "get_more_tools"));
    let result = call(
        &client,
        "get_more_tools",
        json!({ "context": "export a dashboard to PDF" }),
    )
    .await;

    assert!(texts(&result)[0].starts_with("Unfortunately"));
    let event = recorder.only("$mcp_missing_capability").await;
    assert_eq!(
        text_property(&event, "$mcp_intent"),
        "export a dashboard to PDF"
    );
    assert_eq!(
        text_property(&event, "$mcp_resource_name"),
        "get_more_tools"
    );
    assert!(!event.properties().contains_key("$mcp_parameters"));
    assert!(recorder.named("$mcp_tool_call").await.is_empty());
}

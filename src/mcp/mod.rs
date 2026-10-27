//! MCP analytics for servers built on [`rmcp`]: every tool call, with its
//! intent, model, latency, outcome and session, sent to PostHog as
//! `$mcp_*` events, with credentials and personal data redacted first.
//! Implements PostHog's cross-SDK MCP analytics spec.
//!
//! **Experimental.** Enabled by the `mcp` feature, which needs Rust 1.88.
//! The API may change in any release, and PostHog does not yet support it.
//!
//! ```no_run
//! # use rmcp::{ServerHandler, ServiceExt};
//! # async fn example<H: ServerHandler>(server: H) -> Result<(), Box<dyn std::error::Error>> {
//! let posthog = std::sync::Arc::new(posthog_rs::client("phc_project_api_key").await);
//! let server = posthog_rs::mcp::instrument(server, posthog.clone());
//! server.serve(rmcp::transport::stdio()).await?.waiting().await?;
//! posthog.shutdown().await;
//! # Ok(())
//! # }
//! ```

mod analytics;
mod bounds;
mod call;
mod capture;
mod contract;
mod identity;
mod manual;
mod options;
mod privacy;
mod request_info;
mod server;
mod session;
mod tools;

pub use analytics::McpAnalytics;
pub use identity::Identity;
pub use manual::{StartedToolCall, ToolCallError, ToolCallRequest, ToolRegistration};
pub use options::{
    IdentifyCallback, IntentFallback, McpOptions, McpOptionsBuilder, McpOptionsBuilderError,
};
pub use request_info::RequestInfo;
pub use server::{instrument, Instrumented};
pub use session::{Connection, TransportSession};

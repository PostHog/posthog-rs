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
//! let posthog = std::sync::Arc::new(posthog::client("phc_project_api_key").await);
//! let server = posthog::mcp::instrument(server, posthog.clone());
//! server.serve(rmcp::transport::stdio()).await?.waiting().await?;
//! posthog.shutdown().await;
//! # Ok(())
//! # }
//! ```
//!
//! # Request context
//!
//! Over HTTP, every event takes the [`RequestContext`](crate::RequestContext)
//! that `PostHogLayer` or the host's middleware placed in the request
//! extensions, as other events take the context of their scope. A context
//! scoped around the code that sends the event wins over it. The identity from
//! [`McpOptionsBuilder::identify`] stays the distinct ID; an anonymous event
//! keeps its MCP session ID and the `process_person_profile: false` option.
//! Context properties and options fill only the keys the event leaves unset,
//! before [`McpOptionsBuilder::before_send`] runs.
//!
//! # Event options
//!
//! [`McpOptionsBuilder::event_options`] sets capture options on each request's
//! events, and [`ToolCallRequest::event_option`] on one call's. The call's
//! option wins over the callback's, and both win over the request context. An
//! anonymous event keeps `process_person_profile: false` over all of them.
//!
//! ```
//! use posthog::mcp::{McpOptionsBuilder, ToolCallRequest};
//!
//! let options = McpOptionsBuilder::default()
//!     .event_options(|_request| {
//!         let mut options = serde_json::Map::new();
//!         options.insert("disable_skew_correction".into(), true.into());
//!         Some(options)
//!     })
//!     .build()
//!     .unwrap();
//! let call = ToolCallRequest::new("search_docs", None)
//!     .event_option("product_tour_id", "onboarding")?;
//! # let _ = (options, call);
//! # Ok::<(), posthog::Error>(())
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
    EventOptionsCallback, IdentifyCallback, IntentFallback, McpOptions, McpOptionsBuilder,
    McpOptionsBuilderError,
};
pub use request_info::RequestInfo;
pub use server::{instrument, Instrumented};
pub use session::{Connection, TransportSession};

//! The `rmcp` integration: a [`ServerHandler`] that wraps the server's own.

mod call_tool;
mod handler;
mod initialize;
mod list_tools;
mod request;
mod virtual_tools;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use rmcp::ServerHandler;

use super::analytics::McpAnalytics;
use super::options::McpOptions;
use super::session::Connection;
use crate::Client;

/// Wraps an `rmcp` server so its tool calls, tool listings and handshakes are
/// captured to PostHog with the default [`McpOptions`]. Use
/// [`McpAnalytics::instrument`] to configure them.
pub fn instrument<H: ServerHandler>(server: H, client: Arc<Client>) -> Instrumented<H> {
    McpAnalytics::new(client, McpOptions::default()).instrument(server)
}

impl McpAnalytics {
    pub fn instrument<H: ServerHandler>(&self, server: H) -> Instrumented<H> {
        Instrumented {
            inner: server,
            analytics: self.clone(),
            instance: Instance::default(),
        }
    }
}

/// An instrumented `rmcp` server. It answers every request as the wrapped
/// server would, apart from the analytics arguments advertised on its tools,
/// the conversation handle in tool results, and the `get_more_tools` tool.
///
/// Requests that arrive without HTTP, such as over stdio, belong to one
/// connection and share its session. A clone serves another connection, so it
/// starts its own session; clones share everything else.
pub struct Instrumented<H> {
    inner: H,
    analytics: McpAnalytics,
    instance: Instance,
}

/// What one served connection or session knows, which its clones do not
/// share.
#[derive(Default)]
struct Instance {
    connection: Connection,
    served_handshake: Arc<AtomicBool>,
}

impl Instance {
    /// Whether this instance served the `initialize` of the requests it
    /// gets, so that handshake names their client. `rmcp` builds a fresh
    /// instance for each stateless HTTP request.
    fn served_handshake(&self) -> bool {
        self.served_handshake.load(Ordering::Relaxed)
    }

    fn mark_handshake_served(&self) {
        self.served_handshake.store(true, Ordering::Relaxed);
    }
}

impl<H> Instrumented<H> {
    pub fn inner(&self) -> &H {
        &self.inner
    }

    pub fn analytics(&self) -> &McpAnalytics {
        &self.analytics
    }
}

impl<H: Clone> Clone for Instrumented<H> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            analytics: self.analytics.clone(),
            instance: Instance::default(),
        }
    }
}

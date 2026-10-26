//! A small notes server over stdio, with its tool calls captured to PostHog.
//!
//! Run with:
//!   POSTHOG_API_KEY=phc_... cargo run --example mcp_server --features mcp
//!
//! then point an MCP client, such as Claude Code or opencode, at that command.

#[cfg(feature = "mcp")]
mod notes {
    use rmcp::handler::server::wrapper::Parameters;
    use rmcp::model::{
        CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig,
    };
    use rmcp::schemars::{self, JsonSchema};
    use rmcp::{tool, tool_handler, tool_router, ServerHandler};
    use serde::Deserialize;

    const NOTES: [(i64, &str); 3] = [
        (1, "Ship the MCP analytics"),
        (2, "Record the demo"),
        (3, "Write the release notes"),
    ];

    #[derive(Deserialize, JsonSchema)]
    pub struct SearchArgs {
        query: String,
    }

    #[derive(Deserialize, JsonSchema)]
    pub struct NoteArgs {
        id: i64,
    }

    #[derive(Clone)]
    pub struct Notes;

    #[tool_router]
    impl Notes {
        #[tool(description = "Search notes by a word in their title")]
        async fn search_notes(
            &self,
            Parameters(SearchArgs { query }): Parameters<SearchArgs>,
        ) -> CallToolResult {
            let needle = query.to_lowercase();
            let hits: Vec<String> = NOTES
                .iter()
                .filter(|(_, title)| title.to_lowercase().contains(&needle))
                .map(|(id, title)| format!("#{id} {title}"))
                .collect();
            let text = if hits.is_empty() {
                "No notes match".to_string()
            } else {
                hits.join("\n")
            };
            CallToolResult::success(vec![ContentBlock::text(text)])
        }

        #[tool(description = "Read one note by its id")]
        async fn get_note(
            &self,
            Parameters(NoteArgs { id }): Parameters<NoteArgs>,
        ) -> CallToolResult {
            match NOTES.iter().find(|(note_id, _)| *note_id == id) {
                Some((id, title)) => {
                    CallToolResult::success(vec![ContentBlock::text(format!("#{id} {title}"))])
                }
                None => {
                    CallToolResult::error(vec![ContentBlock::text(format!("Note {id} not found"))])
                }
            }
        }
    }

    #[tool_handler]
    impl ServerHandler for Notes {
        fn get_info(&self) -> ServerConfig {
            let capabilities = ServerCapabilities::builder().enable_tools().build();
            ServerConfig::new(capabilities).with_server_info(Implementation::new("notes", "0.1.0"))
        }
    }
}

#[cfg(feature = "mcp")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::sync::Arc;

    use posthog_rs::mcp::{McpAnalytics, McpOptionsBuilder};
    use rmcp::ServiceExt;

    let api_key = std::env::var("POSTHOG_API_KEY")?;
    let host = std::env::var("POSTHOG_HOST").unwrap_or_else(|_| posthog_rs::DEFAULT_HOST.into());
    let posthog = Arc::new(posthog_rs::client((api_key.as_str(), host.as_str())).await);

    let options = McpOptionsBuilder::default()
        .missing_capability_tool("get_more_tools")
        .build()?;
    let server = McpAnalytics::new(posthog.clone(), options).instrument(notes::Notes);

    // Stdout carries the protocol, so the server must never print to it.
    server
        .serve(rmcp::transport::stdio())
        .await?
        .waiting()
        .await?;
    posthog.shutdown().await;
    Ok(())
}

#[cfg(not(feature = "mcp"))]
fn main() {
    eprintln!(
        "This example needs the `mcp` feature: cargo run --example mcp_server --features mcp"
    );
}

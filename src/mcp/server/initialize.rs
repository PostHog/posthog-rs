use rmcp::model::{InitializeRequestParams, InitializeResult};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData as McpError, ServerHandler};

use super::Instrumented;
use crate::mcp::call::emit_initialized;
use crate::mcp::capture::ClientIdentity;

const INITIALIZE: &str = "initialize";

impl<H: ServerHandler> Instrumented<H> {
    pub(super) async fn traced_initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, McpError> {
        let mut handshake = ClientIdentity {
            name: Some(request.client_info.name.clone()),
            version: Some(request.client_info.version.clone()),
            protocol_version: Some(request.protocol_version.to_string()),
            user_agent: None,
            vendor: None,
        };
        let facts = self.request_facts(&context);
        let result = self.inner.initialize(request, context).await?;
        self.instance.mark_handshake_served();
        handshake.protocol_version = Some(result.protocol_version.to_string());
        let facts = facts.with_handshake(handshake);
        emit_initialized(&self.analytics.core, &facts.scope(INITIALIZE));
        Ok(result)
    }
}

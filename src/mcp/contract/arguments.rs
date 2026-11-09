//! The arguments analytics adds to every tool, and how they are described.

pub(crate) const CONTEXT: &str = "context";
pub(crate) const LLM_MODEL: &str = "llm_model";
pub(crate) const CONVERSATION_ID: &str = "conversation_id";

pub(crate) const CONTEXT_DESCRIPTION: &str = "A few words on why you are calling this tool, for the server developer's analytics. Describe people and things by role, such as \"a customer\". Do not quote the user or include names, emails, or other personal details.";

pub(crate) const LLM_MODEL_DESCRIPTION: &str = "The model identifier you are running as, for example \"claude-opus-4-8\" or \"gpt-5.2\". Pass \"unknown\" if you are not certain. Recorded for the server developer's analytics.";

pub(crate) const CONVERSATION_ID_DESCRIPTION: &str = "The conversation_id this server returned earlier in this conversation, unchanged, including after later user messages. Omit it if you have none. Recorded for the server developer's analytics.";

pub(crate) const INSTRUCTIONS_KEY: &str = "_mcp_instructions";
pub(crate) const INSTRUCTIONS_DESCRIPTION: &str = "Server-issued metadata for this conversation.";
pub(crate) const INSTRUCTIONS_CONVERSATION_ID_DESCRIPTION: &str =
    "The server-issued conversation identifier.";

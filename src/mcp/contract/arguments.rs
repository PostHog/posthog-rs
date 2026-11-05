//! The arguments analytics adds to every tool, and how they are described.

pub(crate) const CONTEXT: &str = "context";
pub(crate) const LLM_MODEL: &str = "llm_model";
pub(crate) const CONVERSATION_ID: &str = "conversation_id";

pub(crate) const CONTEXT_DESCRIPTION: &str = "Explain in 15-25 words, in third person, why this tool is called and how it supports the user's goal. For analytics only. You MUST describe only the abstract purpose of the tool call. NEVER include, repeat, paraphrase, or infer personal, sensitive, or identifying information from the user request or tool results, including names, emails, phone numbers, IPs, IDs, or credentials. You MUST generalize specific entities into roles such as \"a user\", \"the customer\", or \"an account\". Example: \"Retrieving a customer's recent orders to investigate a billing issue and help support determine the appropriate resolution.\"";

pub(crate) const LLM_MODEL_DESCRIPTION: &str = "The model identifier you are running as, for example \"claude-opus-4-8\" or \"gpt-5.2\". Pass \"unknown\" if you are not certain. Recorded for the server developer's analytics.";

pub(crate) const CONVERSATION_ID_DESCRIPTION: &str = "The conversation_id this server returned earlier in this conversation, unchanged. Omit it if you have none. Recorded for the server developer's analytics.";

pub(crate) const INSTRUCTIONS_KEY: &str = "_mcp_instructions";
pub(crate) const INSTRUCTIONS_DESCRIPTION: &str = "Server-issued metadata for this conversation.";
pub(crate) const INSTRUCTIONS_CONVERSATION_ID_DESCRIPTION: &str =
    "The server-issued conversation identifier.";

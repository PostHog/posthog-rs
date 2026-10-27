//! What analytics knows about each tool: its own schema before injection, and
//! which arguments analytics added to it.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use serde_json::Value;

use super::output_schema::can_declare_instructions;
use super::ownership::{injectable_arguments, ArgumentFlags};
use crate::mcp::contract::limits::MAX_REMEMBERED_TOOLS;

#[derive(Clone, Debug, Default)]
pub(crate) struct ToolFacts {
    pub(crate) description: Option<String>,
    pub(crate) category: Option<String>,
    pub(crate) input_schema: Option<Value>,
    pub(crate) injected: ArgumentFlags,
    pub(crate) mirrors_instructions: bool,
}

impl ToolFacts {
    /// Reads a tool descriptor as the server registered it.
    pub(crate) fn from_descriptor(tool: &Value, enabled: ArgumentFlags) -> Self {
        let input_schema = tool.get("inputSchema");
        let injected = injectable_arguments(input_schema, enabled);
        let mirrors_instructions =
            injected.conversation_id && can_declare_instructions(tool.get("outputSchema"));
        Self {
            description: non_empty_text(tool.get("description")),
            category: non_empty_text(tool.pointer("/_meta/category")),
            input_schema: input_schema.cloned(),
            injected,
            mirrors_instructions,
        }
    }
}

fn non_empty_text(value: Option<&Value>) -> Option<String> {
    let text = value?.as_str()?;
    (!text.is_empty()).then(|| text.to_string())
}

/// Tools learned from listings, bounded so a server that renames tools
/// endlessly cannot grow it without limit.
#[derive(Debug, Default)]
pub(crate) struct ToolCatalog {
    tools: Mutex<HashMap<String, ToolFacts>>,
}

impl ToolCatalog {
    pub(crate) fn remember(&self, name: &str, facts: ToolFacts) {
        let mut tools = self.tools();
        let is_new = !tools.contains_key(name);
        if is_new && tools.len() >= MAX_REMEMBERED_TOOLS {
            let evicted = tools.keys().next().cloned();
            if let Some(evicted) = evicted {
                tools.remove(&evicted);
            }
        }
        tools.insert(name.to_string(), facts);
    }

    pub(crate) fn get(&self, name: &str) -> Option<ToolFacts> {
        self.tools().get(name).cloned()
    }

    fn tools(&self) -> MutexGuard<'_, HashMap<String, ToolFacts>> {
        self.tools
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

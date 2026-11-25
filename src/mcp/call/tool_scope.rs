//! The request context a tool's own events take while its call runs: the
//! call's session and identity, over the host's context.

use serde_json::Value;

use crate::mcp::capture::EventContext;
use crate::mcp::contract::properties as key;
use crate::mcp::session::Session;
use crate::request_context::HeldEvents;
use crate::RequestContext;

pub(crate) struct ToolScope {
    pub(crate) context: RequestContext,
}

impl ToolScope {
    /// While the call's session waits on a new handle reaching the agent,
    /// its tool's events wait too, and take the session only once it is known.
    pub(crate) fn new(host: Option<RequestContext>, call: &EventContext, pending: bool) -> Self {
        let mut context = host.unwrap_or_default();
        if let Some(identity) = &call.identity {
            context.properties.remove(key::GROUPS);
            context.distinct_id = Some(identity.distinct_id.clone());
            if !identity.groups.is_empty() {
                let groups = serde_json::to_value(&identity.groups).unwrap_or_default();
                context.properties.insert(key::GROUPS.to_string(), groups);
            }
        }
        context.held = pending.then(HeldEvents::new);
        if pending {
            context.properties.remove(key::SESSION_ID);
            context.properties.remove(key::SESSION_SOURCE);
        } else {
            set_session(&mut context, &call.session);
        }
        Self { context }
    }

    /// Sends the events that waited, in the call's final session.
    pub(crate) fn settle(&self, session: &Session) {
        if let Some(held) = &self.context.held {
            let mut deferred = RequestContext::new();
            set_session(&mut deferred, session);
            held.release(deferred.properties);
        }
    }
}

/// A call dropped before it ended never delivered its handle.
impl Drop for ToolScope {
    fn drop(&mut self) {
        if self.context.held.is_some() {
            self.settle(&Session::generated());
        }
    }
}

fn set_session(context: &mut RequestContext, session: &Session) {
    let properties = &mut context.properties;
    properties.insert(
        key::SESSION_ID.to_string(),
        Value::from(session.id.as_str()),
    );
    properties.insert(
        key::SESSION_SOURCE.to_string(),
        Value::from(session.source.as_str()),
    );
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use serde_json::Map;

    use super::*;
    use crate::mcp::session::SessionSource;
    use crate::mcp::Identity;

    fn call(session: &str) -> EventContext {
        EventContext {
            timestamp: Utc::now(),
            session: Session::new(session.to_string(), SessionSource::Conversation),
            conversation_id: None,
            identity: None,
            client: Default::default(),
            server: Default::default(),
            server_build: None,
            host_properties: Map::new(),
            options: Map::new(),
        }
    }

    #[test]
    fn a_call_keeps_only_its_own_identitys_groups() {
        let identity = |groups: &[(&str, &str)]| {
            groups
                .iter()
                .fold(Identity::new("user"), |identity, (kind, key)| {
                    identity.with_group(*kind, *key)
                })
        };
        let mut parent = call("ses_parent");
        parent.identity = Some(identity(&[("company", "acme")]));
        let host = ToolScope::new(None, &parent, false).context.clone();
        let cases = [
            (identity(&[]), None),
            (
                identity(&[("company", "initech")]),
                Some(serde_json::json!({ "company": "initech" })),
            ),
        ];
        for (nested_identity, groups) in cases {
            let mut nested = call("ses_nested");
            nested.identity = Some(nested_identity);
            let scope = ToolScope::new(Some(host.clone()), &nested, false);
            assert_eq!(scope.context.properties.get(key::GROUPS).cloned(), groups);
        }
    }

    #[test]
    fn a_nested_call_takes_none_of_its_parents_session_or_hold() {
        let parent = ToolScope::new(None, &call("ses_parent"), true);
        let mut host = parent.context.clone();
        set_session(
            &mut host,
            &Session::new("ses_host".into(), SessionSource::Transport),
        );
        for pending in [true, false] {
            let nested = ToolScope::new(Some(host.clone()), &call("ses_nested"), pending);

            let held = nested.context.held.as_ref();
            assert!(held != parent.context.held.as_ref(), "pending {}", pending);
            assert_eq!(held.is_some(), pending);
            let properties = &nested.context.properties;
            let expected =
                (!pending).then(|| (Value::from("ses_nested"), Value::from("conversation")));
            let actual = properties
                .get(key::SESSION_ID)
                .cloned()
                .zip(properties.get(key::SESSION_SOURCE).cloned());
            assert_eq!(actual, expected, "pending {}", pending);
        }
    }
}

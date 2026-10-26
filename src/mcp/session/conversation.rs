//! The conversation handle: a UUIDv7 the SDK mints and the agent echoes as the
//! `conversation_id` argument, the only grouping a stateless HTTP request can
//! carry.

use std::sync::LazyLock;

use regex::Regex;

use super::ids::new_uuid_v7;

static UUID_V7: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-7[0-9a-fA-F]{3}-[89abAB][0-9a-fA-F]{3}-[0-9a-fA-F]{12}$",
    )
    .expect("uuid v7 pattern")
});

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Conversation {
    pub(crate) id: Option<String>,
    pub(crate) minted: bool,
}

impl Conversation {
    /// Agents invent ids like `conv-1`; only a handle the SDK could have
    /// minted is accepted, so callers who invent the same string never merge.
    pub(crate) fn echoed(sent: Option<&str>) -> Self {
        let id = sent
            .map(str::trim)
            .filter(|id| UUID_V7.is_match(id))
            .map(str::to_lowercase);
        Self { id, minted: false }
    }

    pub(crate) fn minted() -> Self {
        Self {
            id: Some(new_uuid_v7()),
            minted: true,
        }
    }

    pub(crate) fn is_present(&self) -> bool {
        self.id.is_some()
    }

    /// A minted handle the agent never received names no conversation.
    pub(crate) fn undelivered(&mut self) {
        if self.minted {
            *self = Self::default();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echoed_handles_are_lowercased() {
        let conversation = Conversation::echoed(Some("01A16F22-413C-7661-A1A1-829BBF0CAEDF"));
        assert_eq!(
            conversation.id.as_deref(),
            Some("01a16f22-413c-7661-a1a1-829bbf0caedf")
        );
    }

    #[test]
    fn invented_handles_are_ignored() {
        assert!(!Conversation::echoed(Some("conv-1")).is_present());
        assert!(!Conversation::echoed(None).is_present());
    }
}

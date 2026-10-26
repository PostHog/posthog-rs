//! Lengths count characters and include the truncation marker.

use std::time::Duration;

pub(crate) const TRUNCATION_MARKER: &str = "...";

pub(crate) const MAX_NAME_LENGTH: usize = 256;
pub(crate) const MAX_TEXT_LENGTH: usize = 2_048;

pub(crate) const MAX_PAYLOAD_STRING_LENGTH: usize = 32_768;
pub(crate) const MAX_PAYLOAD_KEY_LENGTH: usize = 256;
pub(crate) const MAX_PAYLOAD_ENTRIES: usize = 100;
pub(crate) const MAX_PAYLOAD_DEPTH: usize = 10;

pub(crate) const MAX_EVENT_BYTES: usize = 102_400;
/// Room left for the properties the host client adds after MCP analytics
/// bounds an event, such as `$lib_version` and `$os`.
pub(crate) const HOST_PROPERTIES_HEADROOM: usize = 1_024;

pub(crate) const MAX_INPUT_REQUEST_METHODS: usize = 100;

pub(crate) const REDACTION_SCAN_MARGIN: usize = 1_024;

pub(crate) const SESSION_INACTIVITY: Duration = Duration::from_secs(30 * 60);
pub(crate) const MAX_REMEMBERED_TOOLS: usize = 1_024;

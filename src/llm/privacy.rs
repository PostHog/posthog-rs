#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivacyMode {
    // Send full content
    Full,
    // Replace content with a placeholder while keeping that content exists
    Redacted,
    // Do not include the field at all
    Omit,
}

impl Default for PrivacyMode {
    fn default() -> Self { PrivacyMode::Full }
}

pub fn apply_privacy_to_value(value: Option<serde_json::Value>, mode: PrivacyMode) -> Option<serde_json::Value> {
    match mode {
        PrivacyMode::Full => value,
        PrivacyMode::Redacted => value.map(|_| serde_json::json!("[REDACTED]")),
        PrivacyMode::Omit => None,
    }
}

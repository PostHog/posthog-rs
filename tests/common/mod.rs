pub fn default_user_agent() -> String {
    format!("posthog-rs/{}", env!("CARGO_PKG_VERSION"))
}

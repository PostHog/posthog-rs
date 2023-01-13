#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{source}")]
    PostHogCore { source: posthog_core::error::Error },
    #[error("connection: {source}")]
    Connection { source: reqwest::Error },
}

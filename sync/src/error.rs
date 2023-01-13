#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{source}")]
    PostHogCore { source: posthog_core::error::Error },
    #[error("send request: {source}")]
    SendRequest { source: reqwest::Error },
    #[error("response status: {source}")]
    ResponseStatus { source: reqwest::Error },
    #[error("decode response: {source}")]
    DecodeResponse { source: reqwest::Error },
}

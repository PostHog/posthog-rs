#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("serialization: {source}")]
    Serialization {
        #[from]
        source: serde_json::Error,
    },
}

pub mod client;
pub mod error;
pub mod query;

pub use client::PostHogAPIClient;
pub use error::PostHogAPIError;
pub use query::{QueryRequest, QueryResponse};

mod openapi {
    use progenitor::generate_api;

    generate_api!("./schema.yaml");
}
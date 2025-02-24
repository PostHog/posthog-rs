pub mod client;
pub mod query;

pub use client::PostHogAPIClient;
pub use query::{QueryRequest, QueryResponse};
pub use super::error::PostHogApiError;

// Not ready yet
// mod openapi {
//     use progenitor::generate_api;

//     generate_api!("./schema.yaml");
// }

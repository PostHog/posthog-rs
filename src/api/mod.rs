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


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client() {
        let client = openapi::Client::new("hello world");
        // client.environments_app_metrics_retrieve(project_id, id)
        // assert!(client.is_ok());
    }
}
use anyhow::Context;
use posthog_rs::api::{client::PostHogAPIClient, query::QueryRequest};
use serde_json::json;
use tracing::{info, level_filters::LevelFilter};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load environment variables
    dotenvy::dotenv().context("Failed to load .env file")?;

    // Initialize tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(LevelFilter::DEBUG.into())
                .from_env_lossy()
                .add_directive(
                    format!("{}=trace", env!("CARGO_CRATE_NAME"))
                        .parse()
                        .unwrap(),
                ),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Get environment variables
    let api_key = std::env::var("POSTHOG_API_KEY").context("POSTHOG_API_KEY not set")?;
    let base_url = std::env::var("POSTHOG_API_URL").context("POSTHOG_API_URL not set")?;
    let project_id = std::env::var("POSTHOG_PROJECT_ID").context("POSTHOG_PROJECT_ID not set")?;

    // Initialize PostHog client
    let client = PostHogAPIClient::new(api_key, base_url).context("Failed to create PostHog client")?;

    // Create a query request
    let request = QueryRequest::default().with_query(json!({
        "kind": "HogQLQuery",
        "query": "select `distinct_id` from person_distinct_ids"
    }));

    // Execute the query
    info!("Executing query...");
    let response = client
        .query(&project_id, request)
        .await
        .context("Failed to execute query")?;

    // Print the results
    info!("Query results: {:#?}", response);

    // If the query is asynchronous and has a task_id, we can check its status
    if let Some(task_id) = response.task_id {
        info!("Query is asynchronous. Checking status...");
        let status = client
            .get_query_status(&project_id, &task_id)
            .await
            .context("Failed to get query status")?;

        info!("Query status: {:#?}", status);
    }

    Ok(())
}

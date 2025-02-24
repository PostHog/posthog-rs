use anyhow::Context;
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use posthog_rs::sdk::{
    models::event::EventBuilder, service::PostHogServiceMessage, PostHogSDKClient,
    PostHogServiceActor,
};
use serde_json::json;
use tokio::sync::mpsc::Sender;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(LevelFilter::DEBUG.into())
                .from_env_lossy()
                .add_directive(format!("{}=trace", env!("CARGO_CRATE_NAME")).parse().unwrap()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    dotenvy::dotenv().context("Didn't find .env file")?;

    // Posthog setup
    let posthog_public_key = std::env::var("POSTHOG_PUBLIC_KEY").unwrap();
    let posthog_base_url = std::env::var("POSTHOG_BASE_URL").unwrap();
    let actor = PostHogServiceActor::new(
        PostHogSDKClient::new(posthog_public_key, posthog_base_url)
            .context("Failed to create PostHog client")?,
    );

    let sender = actor.start().await;

    // Axum setup
    let app = Router::new().route("/", get(handler)).with_state(sender);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000")
        .await
        .unwrap();

    println!("listening on {}", listener.local_addr().unwrap());
    axum::serve(listener, app).await.unwrap();

    Ok(())
}

async fn handler(State(sender): State<Sender<PostHogServiceMessage>>) -> impl IntoResponse {
    let value = EventBuilder::new("event_name")
        .distinct_id("my_custom_user_id".to_string())
        .timestamp_now()
        .properties(json!({ "key": "value" })) // Must be called last
        .build();

    match sender
        .send(PostHogServiceMessage::Capture(value))
        .await
        .context("Failed to send event")
    {
        Ok(_) => Response::builder()
            .status(StatusCode::OK)
            .body(format!("Event Captured"))
            .unwrap(),
        Err(e) => Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(format!("Failed to send event: {}", e))
            .unwrap(),
    }
}

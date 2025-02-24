use anyhow::Context;
use posthog_rs::sdk::{
    models::event::EventBuilder,
    service::{PostHogServiceMessage, PostHogServiceSender},
    PostHogSDKClient, PostHogServiceActor,
};
use serde_json::{json, Value};
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{info, level_filters::LevelFilter};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

struct SampleActor {
    posthog: PostHogServiceSender,
}

impl SampleActor {
    async fn start(mut rx: mpsc::Receiver<Value>, posthog: PostHogServiceSender) {
        let mut s = Self { posthog };

        while let Some(msg) = rx.recv().await {
            if let Err(e) = s.process_message(msg).await {
                eprintln!("Error processing event: {e:?}");
            }
        }
    }

    async fn process_message(&mut self, msg: Value) -> anyhow::Result<()> {
        info!("Received event: {msg:?}");
        self.posthog
            .send(PostHogServiceMessage::Capture(
                EventBuilder::new("events.microservice")
                    .distinct_id("distinct-id".to_string())
                    .timestamp_now()
                    .properties(json!({"message": msg}))
                    .build(),
            ))
            .await
            .context("Failed to send posthog event")?;

        Ok(())
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().context("Didn't find .env file")?;

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

    // Posthog setup
    let posthog_public_key = std::env::var("POSTHOG_PUBLIC_KEY").unwrap();
    let posthog_base_url = std::env::var("POSTHOG_BASE_URL").unwrap();
    let actor = PostHogServiceActor::new(
        PostHogSDKClient::new(posthog_public_key, posthog_base_url)
            .context("Failed to create PostHog client")?,
    );

    let sender = actor.start().await;

    // Create Queue
    let (tx, rx) = mpsc::channel(100);
    tokio::spawn(async move {
        for i in 0..100 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            tx.send(json!(i)).await.unwrap();
        }
    });

    // Create actor
    SampleActor::start(rx, sender).await;
    Ok(())
}

use crate::sdk::PostHogSDKClient;
use serde_json::Value;
use std::time::Duration;
use tokio::sync::mpsc::{self, Sender};
use tracing::debug;

pub type PostHogServiceSender = Sender<PostHogServiceMessage>;

/// Messages that can be sent to the PostHog service actor.
///
/// This enum represents the different types of messages that can be processed by the PostHog service:
/// - `Exit`: Signals the service to shut down gracefully
/// - `Capture`: Contains event data to be sent to PostHog
/// - `CaptureMultiple`: Contains a vector of events to be sent to PostHog
#[derive(Debug)]
pub enum PostHogServiceMessage {
    Exit,
    Capture(Value),
    CaptureMultiple(Vec<Value>),
}

/// An actor that manages batching and sending events to PostHog.
///
/// The service actor maintains a queue of events and periodically sends them in batches to PostHog.
/// It can be configured with custom batch sizes and intervals, and supports historical data migration.
pub struct PostHogServiceActor {
    receiver: Option<mpsc::Receiver<PostHogServiceMessage>>,
    client: PostHogSDKClient,
    captures: Vec<Value>,
    batch_size: usize,
    historical_migration: bool,
    duration: Duration,

    #[cfg(test)]
    pub error_count: usize,
}

impl PostHogServiceActor {
    /// Creates a new PostHog service actor with default configuration.
    ///
    /// # Arguments
    /// * `client` - The PostHog SDK client used to send events
    ///
    /// # Default Configuration
    /// * Batch size: 1,000 events
    /// * Flush interval: 5 seconds
    /// * Historical migration: disabled
    pub fn new(client: PostHogSDKClient) -> Self {
        Self {
            client,
            receiver: None,
            captures: Vec::new(),
            batch_size: 1_000,
            duration: Duration::from_secs(5),
            historical_migration: false,
            #[cfg(test)]
            error_count: 0,
        }
    }

    /// Sets whether the service is processing historical data.
    ///
    /// # Arguments
    /// * `historical_migration` - If true, events will be marked as historical data
    ///
    /// # Returns
    /// Returns self for method chaining
    pub fn with_historical_migration(mut self, historical_migration: bool) -> Self {
        self.historical_migration = historical_migration;
        self
    }

    /// Sets the interval duration between batch sends.
    ///
    /// # Arguments
    /// * `duration` - The time interval between batch processing attempts
    ///
    /// # Returns
    /// Returns self for method chaining
    pub fn with_duration(mut self, duration: Duration) -> Self {
        self.duration = duration;
        self
    }

    /// Sets the maximum number of events to include in each batch.
    ///
    /// # Arguments
    /// * `batch_size` - Maximum number of events to send in a single batch
    ///
    /// # Returns
    /// Returns self for method chaining
    pub fn with_batch_size(mut self, batch_size: usize) -> Self {
        self.batch_size = batch_size;
        self
    }

    /// Starts the PostHog service actor in a new tokio task.
    ///
    /// This method spawns a new task that processes incoming events and periodically
    /// sends them to PostHog in batches.
    ///
    /// # Returns
    /// Returns a channel sender that can be used to send messages to the service
    pub async fn start(mut self) -> PostHogServiceSender {
        let (sender, new_receiver) = mpsc::channel(1_000);
        self.receiver = Some(new_receiver);

        tokio::spawn(async move {
            self.run().await;
        });

        sender
    }

    /// Sends a batch of events to PostHog.
    ///
    /// This method handles splitting large batches into smaller ones based on the configured
    /// batch size and sends them to PostHog. If an error occurs, it will be logged and
    /// propagated to the caller.
    ///
    /// # Arguments
    /// * `batch` - Vector of events to send
    ///
    /// # Returns
    /// Returns Ok(()) on success, or an error if the batch send fails
    pub async fn send_batch(&mut self, mut batch: Vec<Value>) -> anyhow::Result<()> {
        if batch.is_empty() {
            return Ok(());
        }

        while !batch.is_empty() {
            let batch_size = batch.len().min(self.batch_size);
            debug!("Inner batch: ({} items)", batch_size);

            let current_batch = batch.drain(0..batch_size).collect::<Vec<_>>();

            self.client
                .capture_batch(self.historical_migration, current_batch)
                .await
                .map_err(|e| {
                    eprintln!("Error sending batch capture: {}", e);
                    #[cfg(test)]
                    {
                        self.error_count += 1;
                    }

                    e
                })?;
        }

        Ok(())
    }

    /// Main event loop for the service actor.
    ///
    /// This method runs continuously, processing incoming messages and periodically sending
    /// batched events to PostHog. It handles:
    /// - Regular batch sending on configured intervals
    /// - Processing incoming capture events
    /// - Graceful shutdown on Exit message or channel close
    async fn run(mut self) {
        let mut interval = tokio::time::interval(self.duration);

        let mut receiver = self.receiver.take().unwrap();

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if !self.captures.is_empty() {
                        debug!("Sending Batch: {}", self.captures.len());
                        // Clone the captures for sending and clear the vector
                        let batch = self.captures.drain(..).collect::<Vec<_>>();

                        // Send batch to PostHog
                        if let Err(e) = self.send_batch(batch.clone()).await {
                            eprintln!("Error sending batch capture: {}", e);
                            // On error, add the events back to the queue
                            self.captures.extend(batch);
                        }
                    }
                }
                msg = receiver.recv() => {
                    match msg {
                        Some(PostHogServiceMessage::Exit) => {
                            // Handle any remaining captures before exiting
                            if !self.captures.is_empty() {
                                if let Err(e) = self.send_batch(self.captures.clone()).await {
                                    eprintln!("Error sending final batch capture: {}", e);
                                }
                            }
                            break;
                        }
                        Some(PostHogServiceMessage::Capture(event)) => {
                            self.captures.push(event);
                        }
                        Some(PostHogServiceMessage::CaptureMultiple(events)) => {
                            self.captures.extend(events);
                        }
                        None => break, // Channel Closed
                    }
                }
            }
        }
    }

    pub async fn close(&mut self) -> anyhow::Result<()> {
        let batch = self.captures.drain(..).collect::<Vec<_>>();

        self.send_batch(batch).await?;

        Ok(())
    }
}

// impl Drop for PostHogServiceActor {
//     fn drop(&mut self) {
//         tracing::debug!("Stopping PostHog Service Actor");
//         // ensure the service has time to process any remaining events
//         tokio::task::spawn_local(self.close());
//         thread::sleep(Duration::from_secs(8));
//         tracing::debug!("Stopped PostHog Service Actor");
//     }
// }

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::sdk::models::event::EventBuilder;

    use super::*;

    fn service() -> PostHogServiceActor {
        dotenvy::dotenv().ok();
        let public_key = std::env::var("POSTHOG_PUBLIC_KEY").unwrap();
        let base_url = std::env::var("POSTHOG_BASE_URL").unwrap();

        let client = PostHogSDKClient::new(public_key, base_url).unwrap();

        super::PostHogServiceActor::new(client)
    }

    #[tokio::test]
    async fn test_capture_batch() -> anyhow::Result<()> {
        let items = (0..1000)
            .into_iter()
            .map(|i| {
                EventBuilder::new("capture_multiple")
                    .distinct_id(format!("u_{}", i))
                    .timestamp_now()
                    .properties(json!({ "key_idx": i }))
                    .build()
            })
            .collect::<Vec<_>>();

        let mut instance = service()
            .with_batch_size(500)
            .with_duration(Duration::from_secs(1));

        instance.send_batch(items).await.unwrap();

        tokio::time::sleep(Duration::from_secs(3)).await;
        assert_eq!(instance.error_count, 0);

        Ok(())
    }

    #[tokio::test]
    async fn test_capture_batch_too_large() {}
}

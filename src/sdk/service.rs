use std::time::Duration;
use tokio::sync::mpsc::{self, Sender};
use serde_json::Value;
use crate::sdk::PostHogSDKClient;

#[derive(Debug)]
pub enum PostHogServiceMessage {
    Exit,
    Capture(Value),
}

pub struct PostHogServiceActor {
    receiver: Option<mpsc::Receiver<PostHogServiceMessage>>,
    client: PostHogSDKClient,
    captures: Vec<Value>,
    duration: Duration,
}

impl PostHogServiceActor {
    pub fn new(client: PostHogSDKClient) -> Self {
        Self {
            client,
            receiver: None,
            captures: Vec::new(),
            duration: Duration::from_secs(5),
        }
    }

    pub fn with_duration(mut self, duration: Duration) -> Self {
        self.duration = duration;
        self
    }

    pub async fn start(mut self) -> Sender<PostHogServiceMessage> {
        let (sender, new_receiver) = mpsc::channel(1_000);
        self.receiver = Some(new_receiver);
        
        tokio::spawn(async move {
            self.run().await;
        });

        sender
    }

    async fn send_batch(&self, batch: Vec<Value>) -> anyhow::Result<()> {
        if batch.is_empty() {
            return Ok(());
        }

        // todo: make sure api request isn't over 20 mb as per https://posthog.com/docs/api/capture#batch-events
        self.client.capture_batch(false, batch).await.map_err(|e| {
            eprintln!("Error sending batch capture: {}", e);
            e
        })?;

        Ok(())
    }

    async fn run(mut self) {
        let mut interval = tokio::time::interval(self.duration);

        let mut receiver = self.receiver.take().unwrap();

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if !self.captures.is_empty() {
                        // Clone the captures for sending and clear the vector
                        let batch = std::mem::take(&mut self.captures);
                        
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
                        None => break, // Channel Closed
                    }
                }
            }
        }
    }
}
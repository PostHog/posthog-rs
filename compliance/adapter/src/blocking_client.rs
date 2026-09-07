//! Run the public blocking client outside Tokio's asynchronous worker context.
//! Capture still uses the SDK's shared background transport.

use posthog_rs::{ClientOptions, Error, EvaluateFlagsOptions, FeatureFlagEvaluations};
use std::ops::Deref;
use tokio::task::block_in_place;

pub struct Client(Option<posthog_rs::Client>);

pub async fn client(options: ClientOptions) -> Client {
    Client(Some(block_in_place(|| posthog_rs::client(options))))
}

impl Client {
    pub async fn evaluate_flags(
        &self,
        distinct_id: String,
        options: EvaluateFlagsOptions,
    ) -> Result<FeatureFlagEvaluations, Error> {
        block_in_place(|| self.deref().evaluate_flags(distinct_id, options))
    }

    pub async fn flush(&self) {
        block_in_place(|| self.deref().flush());
    }

    pub async fn shutdown(&self) {
        block_in_place(|| self.deref().shutdown());
    }
}

impl Deref for Client {
    type Target = posthog_rs::Client;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref().expect("client is alive")
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // reqwest's blocking client must also be destroyed outside async context,
        // including when the last reference is released by reset or re-init.
        block_in_place(|| drop(self.0.take()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn blocking_client_lifecycle_in_async_server() {
        let options = posthog_rs::ClientOptionsBuilder::default()
            .api_key(String::new())
            .host("http://127.0.0.1:0".to_string())
            .build()
            .unwrap();
        let client = client(options).await;
        client.capture(posthog_rs::Event::new("disabled", "test"));
        client.flush().await;
        assert_eq!(client.pending_events(), 0);
        let flags = client
            .evaluate_flags("test".to_string(), EvaluateFlagsOptions::default())
            .await
            .unwrap();
        assert!(flags.get_flag("test").is_none());
        client.shutdown().await;
        drop(client);
    }
}

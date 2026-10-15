//! Delivery controls for native clients that own identity and event enrichment.

use std::io;
use std::sync::Arc;

use crate::Event;
use uuid::Uuid;
mod file_store;
pub use file_store::FileEventStore;

/// Durable ledger used only by the transport worker. Implementations must not
/// call the client. Store fully formed events atomically and remove records only
/// after delivery, terminal rejection, or an explicit purge. A failed operation
/// must leave the previously committed ledger recoverable.
pub trait EventStore: Send + Sync + 'static {
    fn load(&self) -> io::Result<Vec<Event>>;
    fn put(&self, event: &Event) -> io::Result<()>;
    fn remove(&self, ids: &[Uuid]) -> io::Result<()>;
    fn purge(&self) -> io::Result<()>;
}

/// Opt-in native embedding configuration. Uses legacy batch ingestion and a
/// single worker, accepts already enriched events, and never runs Rust hooks or
/// flag exposure tracking on behalf of the host SDK.
#[derive(Clone)]
pub struct EmbeddingOptions {
    pub store: Arc<dyn EventStore>,
    pub sdk_name: String,
    pub sdk_version: String,
    /// Restore persisted consent before constructing the client.
    pub enabled: bool,
}

/// Delivery state observed after preceding worker commands have completed.
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct DeliveryReport {
    pub pending: usize,
    pub persistence_errors: usize,
    pub delivered: usize,
    pub terminally_dropped: usize,
}

/// Local admission result, not a durable or server acknowledgement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnqueueResult {
    Accepted,
    Disabled,
    Closed,
    QueueFull,
}

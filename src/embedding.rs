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
    /// Enables `Client::capture_replay_snapshot`. `None` leaves replay disabled.
    /// Blocking client only: the builder rejects it under `async-client`.
    pub replay: Option<ReplayOptions>,
}

/// Bounds for the in-memory session replay queue. Frames are never written to
/// the [`EventStore`]; anything still queued at shutdown or opt-out is lost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReplayOptions {
    /// Serialized bytes admitted but not yet sent before `QueueFull`.
    pub max_queue_bytes: usize,
    /// Upper bound for one `/s/` request before compression. A single larger
    /// frame is still sent on its own.
    pub max_batch_bytes: usize,
    pub flush_interval_ms: u64,
}

impl Default for ReplayOptions {
    fn default() -> Self {
        Self {
            max_queue_bytes: 16 * 1024 * 1024,
            max_batch_bytes: 1024 * 1024,
            flush_interval_ms: 1000,
        }
    }
}

impl ReplayOptions {
    /// Queue 1 MiB..=256 MiB, batch 64 KiB..=queue, flush interval
    /// 100..=60000 ms.
    pub fn validate(&self) -> Result<(), &'static str> {
        const MIB: usize = 1024 * 1024;
        if !(MIB..=256 * MIB).contains(&self.max_queue_bytes) {
            return Err("replay max_queue_bytes must be within 1 MiB..=256 MiB");
        }
        if !(64 * 1024..=self.max_queue_bytes).contains(&self.max_batch_bytes) {
            return Err("replay max_batch_bytes must be within 64 KiB..=max_queue_bytes");
        }
        if !(100..=60_000).contains(&self.flush_interval_ms) {
            return Err("replay flush_interval_ms must be within 100..=60000");
        }
        Ok(())
    }
}

/// Delivery state observed after preceding worker commands have completed.
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct DeliveryReport {
    pub pending: usize,
    pub persistence_errors: usize,
    pub delivered: usize,
    pub terminally_dropped: usize,
    /// Replay frames admitted but not yet sent or dropped.
    pub replay_pending: usize,
    /// Replay frames dropped after admission: rejected by the server, failed
    /// delivery, or abandoned at the shutdown deadline. Opt-out purges are not
    /// counted.
    pub replay_dropped: usize,
}

/// Local admission result, not a durable or server acknowledgement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnqueueResult {
    Accepted,
    Disabled,
    Closed,
    QueueFull,
}

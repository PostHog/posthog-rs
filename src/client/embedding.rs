//! State shared by native admission and the existing delivery worker.
use crate::embedding::EmbeddingOptions;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    Mutex,
};
use std::time::{Duration, Instant};

pub(crate) struct Gate {
    pub(crate) admission: Mutex<()>,
    pub(crate) enabled: AtomicBool,
    pub(crate) generation: AtomicU64,
    pub(crate) purge_required: AtomicBool,
    pub(crate) deadline: Mutex<Option<Instant>>,
    pub(crate) budget: Duration,
    pub(crate) persistence_errors: AtomicUsize,
    pub(crate) delivered: AtomicUsize,
    pub(crate) dropped: AtomicUsize,
}

impl Gate {
    pub(crate) fn new(options: &EmbeddingOptions, budget: Duration) -> Self {
        Self {
            admission: Mutex::new(()),
            enabled: AtomicBool::new(options.enabled),
            generation: AtomicU64::new(0),
            purge_required: AtomicBool::new(!options.enabled),
            deadline: Mutex::new(None),
            budget,
            persistence_errors: AtomicUsize::new(0),
            delivered: AtomicUsize::new(0),
            dropped: AtomicUsize::new(0),
        }
    }
    pub(crate) fn deadline(&self, supplied: Option<Instant>) -> Option<Instant> {
        let shared = *self.deadline.lock().unwrap_or_else(|e| e.into_inner());
        match (shared, supplied) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
    pub(crate) fn persistence_error(&self) {
        self.persistence_errors.fetch_add(1, Ordering::Relaxed);
        tracing::warn!("PostHog event storage operation failed");
    }
}

//! Coordinator-owned storage operations. Uncertain writes retain their exact
//! created key; failed acknowledgements never claim reclaimed disk capacity.

use crate::capture_event::CaptureEvent;
use std::io;

use super::apply_on_error_hooks;
use crate::client::event_store::{EventStore, StoreKey};
use crate::client::{OnErrorHook, PostHogError, StorageError, StorageFailure, StorageOperation};

/// Append `event`, warning once per overflow (until `eviction_warned` is
/// reset) when the store drops its oldest undelivered records to make room.
pub(super) fn append(
    store: &mut dyn EventStore,
    event: &CaptureEvent,
    key: StoreKey,
    hooks: &[OnErrorHook],
    eviction_warned: &mut bool,
) -> Option<StoreKey> {
    let stored = match store.put_at(event, key) {
        Ok(key) => Some(key),
        Err(failure) => {
            report_storage_error(hooks, StorageOperation::Write, &failure.error);
            failure.key
        }
    };
    let evicted = store.take_evicted();
    if evicted > 0 && !std::mem::replace(eviction_warned, true) {
        tracing::warn!(
            "posthog-rs: event store is full; dropping the oldest undelivered event(s) to keep new ones"
        );
    }
    stored
}

pub(super) fn acknowledge(
    store: &mut dyn EventStore,
    key: StoreKey,
    hooks: &[OnErrorHook],
) -> bool {
    match store.remove(key) {
        Ok(()) => true,
        Err(error) => {
            report_storage_error(hooks, StorageOperation::Remove, &error);
            false
        }
    }
}

pub(super) fn report_storage_error(
    hooks: &[OnErrorHook],
    operation: StorageOperation,
    error: &io::Error,
) {
    if hooks.is_empty() {
        // Error messages supplied by backends can contain paths or payloads.
        tracing::warn!(?operation, kind = ?error.kind(), "posthog-rs: storage operation failed");
    } else {
        let error = StorageError::from_io(error);
        apply_on_error_hooks(
            hooks,
            &PostHogError::Storage(StorageFailure {
                operation,
                error: &error,
            }),
        );
    }
}

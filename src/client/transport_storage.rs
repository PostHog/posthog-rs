//! Coordinator-owned storage operations. Uncertain writes retain their exact
//! created key; failed acknowledgements never claim reclaimed disk capacity.

use crate::capture_event::CaptureEvent;

use crate::client::common::report_storage_error;
use crate::client::event_store::{EventStore, StoreKey};
use crate::client::{OnErrorHook, StorageOperation};

pub(super) fn append(
    store: &mut dyn EventStore,
    event: &CaptureEvent,
    key: StoreKey,
    hooks: &[OnErrorHook],
) -> Option<StoreKey> {
    match store.put_at(event, key) {
        Ok(key) => Some(key),
        Err(failure) => {
            report_storage_error(hooks, StorageOperation::Write, &failure.error);
            failure.key
        }
    }
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

//! Storage failure reporting for the delivery coordinator.

use std::io;

use super::apply_on_error_hooks;
use crate::client::{OnErrorHook, PostHogError, StorageError, StorageFailure, StorageOperation};

/// Report a storage failure through `on_error`, or as one warning without
/// the error message when no hook is registered: backend messages can contain
/// paths or payloads.
pub(super) fn report_storage_error(
    hooks: &[OnErrorHook],
    operation: StorageOperation,
    error: &io::Error,
) {
    if hooks.is_empty() {
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

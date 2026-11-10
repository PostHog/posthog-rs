//! Callback registration only; cached values remain owned by StateWriter.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, Weak};

use super::writer::StateAccess;
use super::{Client, FeatureFlagContext, FeatureFlagLoadError, FeatureFlagResult, Snapshot};

/// Immutable notification data during a callback. Clone to retain an owned copy.
/// Context describes the last completed evaluation (or bootstrap/restored cache),
/// not necessarily current targeting. Each retained result has its own context.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct FeatureFlagsUpdate {
    /// None means no usable cache. Some(empty) is an available empty snapshot.
    pub flags: Option<HashMap<String, FeatureFlagResult>>,
    pub context: FeatureFlagContext,
    /// True for load failure, computation errors or malformed payloads.
    pub errors_loading: bool,
    pub error: Option<FeatureFlagLoadError>,
    pub partial: bool,
    pub invalid_payloads: bool,
}

impl FeatureFlagsUpdate {
    fn from_snapshot(snapshot: &Snapshot) -> Option<Self> {
        let completion = snapshot.flag_load.completed.as_ref();
        let cache = snapshot.flags.cache.as_ref();
        let context = completion
            .map(|value| value.context.as_ref())
            .or_else(|| cache.map(|cache| &cache.context))?;
        let error = completion.and_then(|value| value.error);
        let partial = completion.is_some_and(|value| value.partial);
        let invalid_payloads = completion.is_some_and(|value| value.invalid_payloads);
        Some(Self {
            flags: cache.map(|cache| {
                cache
                    .flags
                    .iter()
                    .map(|(key, flag)| (key.clone(), FeatureFlagResult::from_cached(key, flag)))
                    .collect()
            }),
            context: FeatureFlagContext::from_context(context),
            errors_loading: error.is_some() || partial || invalid_payloads,
            error,
            partial,
            invalid_payloads,
        })
    }
}

pub(super) type FlagsHook = Arc<dyn Fn(&FeatureFlagsUpdate) + Send + Sync + 'static>;

// Queuing a refresh does not supersede a committed notification. Only a
// changed context or a new completion changes its delivery identity.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Token {
    revision: u64,
    completed: Option<(u64, u64)>,
}
impl Token {
    fn of(snapshot: &Snapshot) -> Self {
        Self {
            revision: snapshot.flag_load.revision,
            completed: snapshot
                .flag_load
                .completed
                .as_ref()
                .map(|value| (value.revision, value.request)),
        }
    }
}

struct Entry {
    callback: FlagsHook,
    last: Option<Token>,
}
struct Registry {
    closed: bool,
    next: u64,
    entries: BTreeMap<u64, Entry>,
}

pub(super) struct Listeners {
    registry: Mutex<Registry>,
}

thread_local! {
    static NOTIFYING: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}
struct CallbackGuard;
impl CallbackGuard {
    fn enter(listeners: &Listeners) -> Option<Self> {
        NOTIFYING
            .try_with(|active| {
                active
                    .borrow_mut()
                    .push(listeners as *const Listeners as usize)
            })
            .ok()?;
        Some(Self)
    }
}
impl Drop for CallbackGuard {
    fn drop(&mut self) {
        let _ = NOTIFYING.try_with(|active| active.borrow_mut().pop());
    }
}

/// Explicitly unsubscribe a runtime callback. Clones refer to the same registration.
/// Dropping a handle does not unsubscribe. The handle does not keep the client,
/// workers or storage namespace alive. Registrations end at terminal shutdown.
#[derive(Clone)]
pub struct FeatureFlagsSubscription {
    listeners: Weak<Listeners>,
    id: Option<u64>,
}

impl FeatureFlagsSubscription {
    pub(super) fn inactive() -> Self {
        Self {
            listeners: Weak::new(),
            id: None,
        }
    }

    /// Stop future dispatch for this registration. Safe to repeat or call from
    /// a callback. An invocation that already started may finish.
    pub fn unsubscribe(&self) {
        if let (Some(listeners), Some(id)) = (self.listeners.upgrade(), self.id) {
            let removed = listeners
                .registry
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .entries
                .remove(&id);
            // User captures can have reentrant destructors; drop outside locks.
            drop(removed);
        }
    }
}

impl Listeners {
    pub(super) fn new(callbacks: Vec<FlagsHook>) -> Self {
        let next = callbacks.len() as u64;
        let entries = callbacks
            .into_iter()
            .enumerate()
            .map(|(i, callback)| {
                (
                    i as u64 + 1,
                    Entry {
                        callback,
                        last: None,
                    },
                )
            })
            .collect();
        Self {
            registry: Mutex::new(Registry {
                closed: false,
                next,
                entries,
            }),
        }
    }

    fn subscribe(
        self: &Arc<Self>,
        access: &StateAccess,
        callback: FlagsHook,
    ) -> FeatureFlagsSubscription {
        let id = {
            let mut registry = self.registry.lock().unwrap_or_else(|p| p.into_inner());
            if registry.closed {
                None
            } else if let Some(id) = registry.next.checked_add(1) {
                registry.next = id;
                registry.entries.insert(
                    id,
                    Entry {
                        callback: callback.clone(),
                        last: None,
                    },
                );
                Some(id)
            } else {
                None
            }
        };
        if let (Some(id), Some(snapshot)) = (id, access.snapshot()) {
            self.dispatch(access, &snapshot, Some(id));
        }
        FeatureFlagsSubscription {
            listeners: Arc::downgrade(self),
            id,
        }
    }

    pub(super) fn notify_ready(&self, access: &StateAccess) {
        if let Some(snapshot) = access.snapshot() {
            self.dispatch(access, &snapshot, None);
        }
    }

    pub(super) fn notify_completion(&self, access: &StateAccess, revision: u64, request: u64) {
        let Some(snapshot) = access.snapshot() else {
            return;
        };
        if snapshot.flag_load.revision == revision
            && snapshot
                .flag_load
                .completed
                .as_ref()
                .is_some_and(|c| c.revision == revision && c.request == request)
        {
            self.dispatch(access, &snapshot, None);
        }
    }

    fn dispatch(&self, access: &StateAccess, snapshot: &Snapshot, only: Option<u64>) {
        let Some(update) = FeatureFlagsUpdate::from_snapshot(snapshot) else {
            return;
        };
        let token = Token::of(snapshot);
        let ids: Vec<_> = match only {
            Some(id) => vec![id],
            None => self
                .registry
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .entries
                .keys()
                .copied()
                .collect(),
        };
        for id in ids {
            let callback = {
                // No state-owner operation acquires this registry lock. The
                // eligibility check and token claim precede invocation, never
                // retaining either lock while calling application code.
                let mut registry = self.registry.lock().unwrap_or_else(|p| p.into_inner());
                if registry.closed
                    || !access
                        .snapshot()
                        .is_some_and(|current| Token::of(&current) == token)
                {
                    return;
                }
                let Some(entry) = registry.entries.get_mut(&id) else {
                    continue;
                };
                if entry.last == Some(token) {
                    continue;
                }
                entry.last = Some(token);
                entry.callback.clone()
            };
            let Some(_guard) = CallbackGuard::enter(self) else {
                return;
            };
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| callback(&update))).is_err()
            {
                tracing::warn!("posthog-rs: feature flags listener panicked");
            }
            drop(callback);
        }
    }

    /// Mark closed now, but let the owner drop callbacks after requesting worker
    /// closure. No application capture is destroyed under registry/state locks.
    pub(super) fn close(&self) -> Vec<FlagsHook> {
        let entries = {
            let mut registry = self.registry.lock().unwrap_or_else(|p| p.into_inner());
            registry.closed = true;
            std::mem::take(&mut registry.entries)
        };
        entries.into_values().map(|entry| entry.callback).collect()
    }
}

impl Client {
    pub(super) fn in_flag_callback(&self) -> bool {
        self.flag_loader.as_ref().is_some_and(|loader| {
            let key = loader.listeners.as_ref() as *const Listeners as usize;
            NOTIFYING
                .try_with(|active| active.borrow().contains(&key))
                .unwrap_or(false)
        })
    }

    /// Register a flags listener. Invokes immediately if cache or a completed
    /// load is available, then on later loads (including failures and quota).
    /// Does not fetch flags. Drop does not unsubscribe; call `unsubscribe()`.
    ///
    /// Each dispatch visits existing registrations in order, outside SDK locks,
    /// with panic isolation. Immediate callbacks run on the caller; remote ones
    /// run on the delivery worker. Sources may overlap or reenter; no UI dispatch
    /// or callback mutex is provided. Use `Weak<Client>` for back-references.
    /// Reset preserves subscriptions but invalidates old loads. Terminal shutdown
    /// ends subscriptions; callbacks already dispatched may finish. Flush and
    /// shutdown from a listener request work but return Deferred instead of waiting.
    pub fn on_feature_flags<F>(&self, callback: F) -> FeatureFlagsSubscription
    where
        F: Fn(&FeatureFlagsUpdate) + Send + Sync + 'static,
    {
        if self.is_disabled() || self.closed.load(std::sync::atomic::Ordering::Acquire) {
            return FeatureFlagsSubscription::inactive();
        }
        self.flag_loader
            .as_ref()
            .unwrap()
            .listeners
            .subscribe(&self.state.as_ref().unwrap().access(), Arc::new(callback))
    }
}

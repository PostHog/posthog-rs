//! Initialization and worker-side file access. Failed stores are omitted rather
//! than failing client setup; live state stays independent of persistence.

use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};

use super::Snapshot;
use crate::client::common::report_storage_error;
use crate::client::event_store::{open_private, FileEventStore, StorageDirectory, StoreLimits};
use crate::client::{OnErrorHook, StorageOperation};

/// Persistence selection for client state and admitted events.
///
/// A disk namespace has one live owner, including across processes. A second
/// client using the same root, project token and instance name cannot
/// restore that namespace. It reports a storage error through `on_error` and
/// remains enabled with a generated or bootstrap identity and memory-only state
/// and events. Reinitialize after the owner exits to retry persistent storage.
///
/// Share one client through `Arc<Client>` when components need the same identity.
/// Use distinct [`Config::instance_name`](super::Config::instance_name) values
/// for independent identities and storage.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub enum Persistence {
    /// OS local application-data directory, with a project/instance namespace.
    /// The directory is per user, not per application: applications that use
    /// the same project token and instance name share one identity and queue.
    /// Use [`Persistence::Directory`] with an application-specific root to
    /// keep them apart.
    #[default]
    Automatic,
    /// Application-supplied root, with the same namespace isolation.
    Directory(PathBuf),
    /// Keep state and queued events in memory only.
    Memory,
}

impl Persistence {
    /// The OS lookup can consult platform services; only initialization calls it.
    fn path(
        &self,
        namespace: &str,
        local_data_dir: impl FnOnce() -> Option<PathBuf>,
    ) -> io::Result<Option<PathBuf>> {
        let root = match self {
            Self::Memory => return Ok(None),
            Self::Directory(root) => root.clone(),
            Self::Automatic => local_data_dir()
                .filter(|path| path.is_absolute())
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        "No local application data directory",
                    )
                })?
                .join("posthog"),
        };
        Ok(Some(root.join(namespace)))
    }
}

/// Hash a length-delimited scope, never raw credentials or path components.
/// Ingestion host changes do not select a different identity or event queue.
pub(super) fn namespace(project_token: &str, instance: &str) -> String {
    let mut hash = Sha1::new();
    hash.update(b"posthog-stateful-namespace-v1");
    for value in [project_token, instance] {
        hash.update((value.len() as u64).to_le_bytes());
        hash.update(value.as_bytes());
    }
    let mut namespace = String::with_capacity(43);
    namespace.push_str("v1-");
    for byte in hash.finalize().iter() {
        write!(&mut namespace, "{byte:02x}").expect("writing to a String cannot fail");
    }
    namespace
}

#[derive(Default)]
pub(super) struct RestoredStorage {
    pub(super) snapshot: Option<Snapshot>,
    /// Restoring dropped an invalid optional field, so the record needs a save.
    pub(super) rewrite_required: bool,
    pub(super) state_store: Option<FileStateStore>,
    pub(super) event_store: Option<FileEventStore>,
}

impl RestoredStorage {
    /// Run before publishing the client, on the initialization/state worker.
    /// Each usable store retains the shared lease. Current-format corruption is
    /// replaced by normal persistence; inaccessible and future-format data is not.
    pub(super) fn open(
        persistence: &Persistence,
        namespace: &str,
        event_limits: StoreLimits,
        hooks: &[OnErrorHook],
    ) -> Self {
        let path = match persistence.path(namespace, dirs::data_local_dir) {
            Ok(Some(path)) => path,
            Ok(None) => return Self::default(),
            Err(error) => {
                report_storage_error(hooks, StorageOperation::Open, &error);
                return Self::default();
            }
        };
        let directory = match StorageDirectory::open(path) {
            Ok(directory) => directory,
            Err(error) => {
                report_storage_error(hooks, StorageOperation::Open, &error);
                return Self::default();
            }
        };
        let mut restored = Self::default();
        match FileStateStore::open(directory.clone()) {
            Ok(opened) => {
                if let Some(error) = opened.read_error {
                    report_storage_error(hooks, StorageOperation::Read, &error);
                }
                restored.state_store = Some(opened.store);
                restored.snapshot = opened.snapshot;
                restored.rewrite_required = opened.rewrite_required;
            }
            Err(error) => report_storage_error(hooks, StorageOperation::Read, &error),
        }
        match FileEventStore::open(directory, event_limits) {
            Ok(store) => restored.event_store = Some(store),
            Err(error) => report_storage_error(hooks, StorageOperation::Open, &error),
        }
        restored
    }
}

#[derive(Serialize, Deserialize)]
struct StateRecord<T> {
    version: u32,
    state: T,
}

fn encode_record(snapshot: &Snapshot) -> io::Result<Vec<u8>> {
    let bytes = serde_json::to_vec(&StateRecord {
        version: 1,
        state: snapshot,
    })
    .map_err(|_| invalid_state())?;
    // Only write records that restore can read back.
    FileStateStore::decode(&bytes)?;
    Ok(bytes)
}

/// Encode for the state writer. A flag cache that cannot be read back (for
/// example a payload nested beyond the JSON decoder's depth limit) is left out
/// rather than making identity and consent unwritable. Remote configuration
/// that deep is rejected when it is loaded.
fn encode_snapshot(snapshot: &Snapshot) -> io::Result<Vec<u8>> {
    if let Ok(encoded) = encode_record(snapshot) {
        return Ok(encoded);
    }
    let mut mandatory = snapshot.clone();
    mandatory.flags.cache = None;
    encode_record(&mandatory)
}

/// Blocking backend calls belong exclusively to the state-persistence worker.
pub(super) trait StateStore: Send + 'static {
    fn save(&mut self, snapshot: &Snapshot) -> io::Result<()>;
}

impl StateStore for FileStateStore {
    fn save(&mut self, snapshot: &Snapshot) -> io::Result<()> {
        FileStateStore::save(self, snapshot)
    }
}

pub(super) struct FileStateStore {
    directory: Arc<StorageDirectory>,
}

pub(super) struct OpenedStateStore {
    pub(super) store: FileStateStore,
    pub(super) snapshot: Option<Snapshot>,
    rewrite_required: bool,
    read_error: Option<io::Error>,
}

/// A restored snapshot, and whether restoring normalized it so that it differs
/// from the stored record.
struct DecodedSnapshot {
    snapshot: Snapshot,
    rewrite_required: bool,
}

// Only bounded, readable current-format corruption permits replacement. Access
// failures and explicit future versions must not be inferred from io::ErrorKind.
#[derive(Debug)]
enum RestoreFailure {
    Corrupt(io::Error),
    Unavailable(io::Error),
}

impl FileStateStore {
    pub(super) fn open(directory: Arc<StorageDirectory>) -> io::Result<OpenedStateStore> {
        let store = Self { directory };
        match store.restore() {
            Ok(restored) => Ok(OpenedStateStore {
                store,
                rewrite_required: restored
                    .as_ref()
                    .is_some_and(|state| state.rewrite_required),
                snapshot: restored.map(|state| state.snapshot),
                read_error: None,
            }),
            Err(RestoreFailure::Corrupt(error)) => Ok(OpenedStateStore {
                store,
                snapshot: None,
                rewrite_required: false,
                read_error: Some(error),
            }),
            Err(RestoreFailure::Unavailable(error)) => Err(error),
        }
    }

    fn path(&self) -> PathBuf {
        self.directory.path().join("state.json")
    }

    fn restore(&self) -> Result<Option<DecodedSnapshot>, RestoreFailure> {
        let path = self.path();
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(RestoreFailure::Unavailable(error)),
        };
        if !metadata.is_file() {
            return Err(RestoreFailure::Unavailable(invalid_state()));
        }
        let mut encoded = Vec::new();
        File::open(path)
            .map_err(RestoreFailure::Unavailable)?
            .read_to_end(&mut encoded)
            .map_err(RestoreFailure::Unavailable)?;
        Self::decode_restored(&encoded).map(Some)
    }

    #[cfg(test)]
    fn load(&self) -> io::Result<Option<Snapshot>> {
        self.restore()
            .map(|restored| restored.map(|state| state.snapshot))
            .map_err(|error| match error {
                RestoreFailure::Corrupt(error) | RestoreFailure::Unavailable(error) => error,
            })
    }

    fn decode_restored(encoded: &[u8]) -> Result<DecodedSnapshot, RestoreFailure> {
        let record: serde_json::Value = serde_json::from_slice(encoded)
            .map_err(|_| RestoreFailure::Corrupt(invalid_state()))?;
        // Inspect the explicit version before interpreting any state fields.
        let version = record
            .get("version")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| RestoreFailure::Corrupt(invalid_state()))?;
        if version != 1 {
            return Err(RestoreFailure::Unavailable(io::Error::new(
                io::ErrorKind::InvalidData,
                "Unsupported state store version",
            )));
        }
        let record: StateRecord<serde_json::Value> =
            serde_json::from_value(record).map_err(|_| RestoreFailure::Corrupt(invalid_state()))?;
        // An invalid stored session, flag cache or remote config is dropped,
        // and the record is rewritten.
        let mut state = record.state;
        let mut rewrite_required = false;
        if let Some(cache) = state
            .get_mut("flags")
            .and_then(|flags| flags.get_mut("cache"))
        {
            rewrite_required |= normalize_optional::<super::flag_state::FlagCache>(cache, |_| true);
        }
        if let Some(config) = state.get_mut("remote_config") {
            rewrite_required |= normalize_optional::<
                crate::client::transport::remote_config::RemoteConfig,
            >(config, |config| config.valid());
        }
        if let Some(session) = state.get_mut("session") {
            rewrite_required |=
                normalize_optional::<super::sessions::StoredSession>(session, |session| {
                    session.is_valid()
                });
        }
        let snapshot: Snapshot =
            serde_json::from_value(state).map_err(|_| RestoreFailure::Corrupt(invalid_state()))?;
        if !snapshot.is_valid() {
            return Err(RestoreFailure::Corrupt(invalid_state()));
        }
        Ok(DecodedSnapshot {
            snapshot,
            rewrite_required,
        })
    }

    fn decode(encoded: &[u8]) -> io::Result<Snapshot> {
        let record: StateRecord<serde_json::Value> =
            serde_json::from_slice(encoded).map_err(|_| invalid_state())?;
        if record.version != 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Unsupported state store version",
            ));
        }
        let snapshot: Snapshot =
            serde_json::from_value(record.state).map_err(|_| invalid_state())?;
        if !snapshot.is_valid() {
            return Err(invalid_state());
        }
        Ok(snapshot)
    }

    /// Only the state writer calls this. A completed replacement is not a
    /// multi-file transaction with event storage or a universal power-loss guarantee.
    pub(super) fn save(&mut self, snapshot: &Snapshot) -> io::Result<()> {
        let encoded = encode_snapshot(snapshot)?;
        let temporary = self.directory.path().join("state.json.tmp");
        // The lease and single writer exclude live concurrent writes. A stale
        // temporary is an uncommitted write from a previous process/attempt.
        remove_temporary(&temporary)?;
        let result = (|| -> io::Result<()> {
            let mut file = open_private(&temporary, true)?;
            file.write_all(&encoded)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary, self.path())?;
            #[cfg(unix)]
            File::open(self.directory.path())?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }
}

/// Recovery applies only to present optional values; missing required fields and
/// invalid mandatory targeting still reject the core snapshot.
/// Replace an optional field that doesn't decode or isn't valid with `null`.
/// Returns whether it was replaced.
fn normalize_optional<T: serde::de::DeserializeOwned>(
    value: &mut serde_json::Value,
    valid: impl FnOnce(&T) -> bool,
) -> bool {
    let replace = !value.is_null()
        && !serde_json::from_value::<T>(value.clone()).is_ok_and(|decoded| valid(&decoded));
    if replace {
        *value = serde_json::Value::Null;
    }
    replace
}

fn remove_temporary(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn invalid_state() -> io::Error {
    // Decoder diagnostics can include identity/property values.
    io::Error::new(io::ErrorKind::InvalidData, "Invalid persisted client state")
}

#[cfg(test)]
#[path = "storage_tests.rs"]
mod tests;

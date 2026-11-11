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
use crate::client::event_store::{
    open_private, FileEventStore, RecordBuffer, StorageDirectory, StoreLimits,
};
use crate::client::{OnErrorHook, StorageOperation};
use crate::endpoints::EndpointManager;

/// Persistence selection for client state and admitted events.
///
/// A disk namespace has one live owner, including across processes. A second
/// client using the same root, host, project token and instance name cannot
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
    /// OS local application-data directory, with a project/host/instance namespace.
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
/// Host aliases follow the same normalization as HTTP delivery.
pub(super) fn namespace(host: &str, project_token: &str, instance: &str) -> String {
    let host = EndpointManager::determine_server_host(host);
    let mut hash = Sha1::new();
    hash.update(b"posthog-stateful-namespace-v1");
    for value in [host.as_str(), project_token, instance] {
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
    pub(super) state_store: Option<FileStateStore>,
    pub(super) event_store: Option<FileEventStore>,
}

impl RestoredStorage {
    /// Run before publishing the client, on the initialization/state worker.
    /// Each usable store retains the shared lease. A state read error disables
    /// state writes for this instance, preserving unreadable/future-format data.
    pub(super) fn open(
        persistence: &Persistence,
        namespace: &str,
        max_state_bytes: u64,
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
        match FileStateStore::open(directory.clone(), max_state_bytes) {
            Ok((store, snapshot)) => {
                restored.state_store = Some(store);
                restored.snapshot = snapshot;
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
#[serde(deny_unknown_fields)]
struct StateRecord<T> {
    version: u32,
    state: T,
}

pub(super) fn snapshot_size(snapshot: &Snapshot) -> usize {
    crate::client::capture::json_len(&StateRecord {
        version: 4,
        state: snapshot,
    })
}

/// Reserve the longest consent representation (`false`), so every valid state
/// can opt in/out or clear the override without exceeding its configured bound.
pub(super) fn snapshot_capacity(snapshot: &Snapshot) -> usize {
    snapshot_size(snapshot).saturating_add(usize::from(snapshot.opt_out != Some(false)))
}

/// Encode and validate the same bounded representation for both memory admission
/// and persistence. This function performs no filesystem operations.
pub(super) fn encode_snapshot(snapshot: &Snapshot, max_bytes: u64) -> io::Result<Vec<u8>> {
    if snapshot_capacity(snapshot) as u64 > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "State snapshot exceeds store limit",
        ));
    }
    let mut buffer = RecordBuffer {
        bytes: Vec::new(),
        limit: max_bytes,
    };
    serde_json::to_writer(
        &mut buffer,
        &StateRecord {
            version: 4,
            state: snapshot,
        },
    )
    .map_err(|error| {
        if error.is_io() {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "State snapshot exceeds store limit",
            )
        } else {
            invalid_state()
        }
    })?;
    // Check the same bounded decoder used at startup before touching files.
    FileStateStore::decode(&buffer.bytes)?;
    Ok(buffer.bytes)
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
    max_bytes: u64,
}

impl FileStateStore {
    /// A failed restore never yields a writable store, so fallback state cannot
    /// overwrite a record we could not interpret. Missing state is writable.
    pub(super) fn open(
        directory: Arc<StorageDirectory>,
        max_bytes: u64,
    ) -> io::Result<(Self, Option<Snapshot>)> {
        if max_bytes == 0 || max_bytes == u64::MAX {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid state store limit",
            ));
        }
        let store = Self {
            directory,
            max_bytes,
        };
        let state = store.load()?;
        Ok((store, state))
    }

    fn path(&self) -> PathBuf {
        self.directory.path().join("state.json")
    }

    fn load(&self) -> io::Result<Option<Snapshot>> {
        let path = self.path();
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        if !metadata.is_file() || metadata.len() > self.max_bytes {
            return Err(invalid_state());
        }
        let mut encoded = Vec::new();
        File::open(path)?
            .take(self.max_bytes + 1)
            .read_to_end(&mut encoded)?;
        if encoded.len() as u64 > self.max_bytes {
            return Err(invalid_state());
        }
        let snapshot = Self::decode(&encoded)?;
        // A migrated record must also fit the current in-memory/write bound.
        if snapshot_capacity(&snapshot) as u64 > self.max_bytes {
            return Err(invalid_state());
        }
        Ok(Some(snapshot))
    }

    fn decode(encoded: &[u8]) -> io::Result<Snapshot> {
        let record: StateRecord<serde_json::Value> =
            serde_json::from_slice(encoded).map_err(|_| invalid_state())?;
        let snapshot: Snapshot = match record.version {
            1 => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct AnonymousState {
                    anonymous_id: String,
                    distinct_id: String,
                }
                let old: AnonymousState =
                    serde_json::from_value(record.state).map_err(|_| invalid_state())?;
                let mut snapshot = Snapshot::anonymous(old.anonymous_id);
                snapshot.distinct_id = old.distinct_id;
                snapshot
            }
            2 => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct PersonState {
                    anonymous_id: String,
                    distinct_id: String,
                    identified: bool,
                    person_processing: bool,
                }
                let old: PersonState =
                    serde_json::from_value(record.state).map_err(|_| invalid_state())?;
                let mut snapshot = Snapshot::anonymous(old.anonymous_id);
                snapshot.distinct_id = old.distinct_id;
                snapshot.identified = old.identified;
                snapshot.person_processing = old.person_processing;
                snapshot
            }
            3 => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct ConsentState {
                    anonymous_id: String,
                    distinct_id: String,
                    identified: bool,
                    person_processing: bool,
                    #[serde(deserialize_with = "Option::<bool>::deserialize")]
                    opt_out: Option<bool>,
                }
                let old: ConsentState =
                    serde_json::from_value(record.state).map_err(|_| invalid_state())?;
                let mut snapshot = Snapshot::anonymous(old.anonymous_id);
                snapshot.distinct_id = old.distinct_id;
                snapshot.identified = old.identified;
                snapshot.person_processing = old.person_processing;
                snapshot.opt_out = old.opt_out;
                snapshot
            }
            4 => serde_json::from_value(record.state).map_err(|_| invalid_state())?,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Unsupported state store version",
                ))
            }
        };
        if !snapshot.is_valid() {
            return Err(invalid_state());
        }
        Ok(snapshot)
    }

    /// Only the state writer calls this. A completed replacement is not a
    /// multi-file transaction with event storage or a universal power-loss guarantee.
    pub(super) fn save(&mut self, snapshot: &Snapshot) -> io::Result<()> {
        let encoded = encode_snapshot(snapshot, self.max_bytes)?;
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

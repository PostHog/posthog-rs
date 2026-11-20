//! Event files for the stateful delivery queue. The store keeps no delivery
//! state: it lists its records once when it opens, then writes, reads and
//! deletes records by key for the coordinator, which owns ordering, capacity
//! and retries. Records hold already prepared wire events, so restoring one
//! never reruns hooks or changes its attribution.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::capture_event::CaptureEvent;

/// A record's position in the delivery queue: its admission sequence number.
/// It never depends on the event's UUID or timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct StoreKey(pub(crate) u64);

/// One ownership lease shared by state and event storage in the same namespace.
/// Open it on an initialization worker before either store reads from disk.
#[derive(Debug)]
pub(crate) struct StorageDirectory {
    path: PathBuf,
    _lock: File,
}

impl StorageDirectory {
    pub(crate) fn open(path: impl AsRef<Path>) -> io::Result<Arc<Self>> {
        create_directory(path.as_ref())?;
        let path = fs::canonicalize(path)?;
        let lock = open_private(&path.join("lock"), false)?;
        fs2::FileExt::try_lock_exclusive(&lock)?;
        Ok(Arc::new(Self { path, _lock: lock }))
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for StorageDirectory {
    fn drop(&mut self) {
        // A subprocess between fork and exec can still hold the file description.
        // Release the lease when its last SDK owner drops, not when that child exits.
        let _ = fs2::FileExt::unlock(&self._lock);
    }
}

fn create_directory(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

pub(crate) fn open_private(path: &Path, exclusive: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    if exclusive {
        options.create_new(true);
    } else {
        options.create(true).truncate(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// Disk retention for the delivery queue, enforced by the coordinator.
#[derive(Debug, Clone, Copy)]
pub(crate) struct StoreLimits {
    pub(crate) max_events: usize,
    pub(crate) max_bytes: u64,
}

/// Storage for the delivery queue. Only the coordinator thread calls it, so
/// admission never waits on disk.
pub(crate) trait EventStore: Send {
    fn limits(&self) -> StoreLimits;

    /// The records on disk when the store opened, oldest first, with their
    /// sizes. The coordinator calls this once.
    fn take_index(&mut self) -> Vec<(StoreKey, u64)>;

    /// The highest key of any record file present at open. New records must
    /// use higher keys.
    fn last_key(&self) -> StoreKey;

    /// Write `event` under `key` and return the record's size. On error no
    /// record exists under `key`.
    fn write(&mut self, key: StoreKey, event: &CaptureEvent) -> io::Result<u64>;

    /// Read a record. A record that cannot be read is never sent: the
    /// coordinator drops it from the queue and deletes its file.
    fn read(&mut self, key: StoreKey) -> io::Result<CaptureEvent>;

    /// Delete a record. Deleting a missing record succeeds.
    fn delete(&mut self, key: StoreKey) -> io::Result<()>;
}

#[derive(Serialize, Deserialize)]
struct Record<T> {
    version: u32,
    event: T,
}

/// Admission measures the persisted representation too, so queue byte limits
/// count what is written to disk.
pub(crate) fn record_size(event: &CaptureEvent) -> usize {
    super::capture::json_len(&Record { version: 1, event })
}

#[derive(Deserialize)]
struct Version {
    version: u32,
}

/// One file per record, named by its key: `events/{key:020}.json`.
///
/// Records are written to a temporary file and renamed, so a process crash
/// never leaves a partial record. They are not synced to the device, as in
/// the iOS and Android queues: a record written just before power loss may be
/// lost, or come back unreadable and be deleted.
pub(crate) struct FileEventStore {
    _directory: Arc<StorageDirectory>,
    events_path: PathBuf,
    limits: StoreLimits,
    index: Vec<(StoreKey, u64)>,
    last_key: StoreKey,
}

impl FileEventStore {
    pub(crate) fn open(directory: Arc<StorageDirectory>, limits: StoreLimits) -> io::Result<Self> {
        if limits.max_events == 0 || limits.max_bytes == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid event store limits",
            ));
        }
        let events_path = directory.path().join("events");
        create_directory(&events_path)?;
        let mut index = Vec::new();
        let mut last_key = StoreKey(0);
        for entry in fs::read_dir(&events_path)? {
            let entry = entry?;
            // Symbolic links and directories are never records.
            if !entry.file_type()?.is_file() {
                continue;
            }
            let path = entry.path();
            let Some(key) = record_key(&path) else {
                continue;
            };
            match path.extension().and_then(|value| value.to_str()) {
                // The namespace lease guarantees there is no live writer.
                Some("tmp") => {
                    fs::remove_file(path)?;
                    continue;
                }
                Some("json") => index.push((key, entry.metadata()?.len())),
                _ => {}
            }
            last_key = last_key.max(key);
        }
        index.sort_unstable();
        Ok(Self {
            _directory: directory,
            events_path,
            limits,
            index,
            last_key,
        })
    }

    fn path(&self, key: StoreKey) -> PathBuf {
        self.events_path.join(format!("{:020}.json", key.0))
    }
}

impl EventStore for FileEventStore {
    fn limits(&self) -> StoreLimits {
        self.limits
    }

    fn take_index(&mut self) -> Vec<(StoreKey, u64)> {
        std::mem::take(&mut self.index)
    }

    fn last_key(&self) -> StoreKey {
        self.last_key
    }

    /// An event nested deeper than the reader's limit is written, then fails
    /// to read and is dropped, as an unreadable record.
    fn write(&mut self, key: StoreKey, event: &CaptureEvent) -> io::Result<u64> {
        let encoded =
            serde_json::to_vec(&Record { version: 1, event }).map_err(|_| invalid_record())?;
        let path = self.path(key);
        let temporary = path.with_extension("tmp");
        let written = (|| -> io::Result<()> {
            let mut file = open_private(&temporary, true)?;
            file.write_all(&encoded)?;
            // Closing first also permits rename on Windows.
            drop(file);
            fs::rename(&temporary, &path)
        })();
        if let Err(error) = written {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        Ok(encoded.len() as u64)
    }

    fn read(&mut self, key: StoreKey) -> io::Result<CaptureEvent> {
        let encoded = fs::read(self.path(key))?;
        let version: Version = serde_json::from_slice(&encoded).map_err(|_| invalid_record())?;
        if version.version != 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Unsupported event store version",
            ));
        }
        serde_json::from_slice::<Record<CaptureEvent>>(&encoded)
            .map(|record| record.event)
            .map_err(|_| invalid_record())
    }

    fn delete(&mut self, key: StoreKey) -> io::Result<()> {
        match fs::remove_file(self.path(key)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

fn record_key(path: &Path) -> Option<StoreKey> {
    let name = path.file_stem()?.to_str()?;
    if name.len() != 20 || !name.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let key = name.parse().ok()?;
    (key != 0).then_some(StoreKey(key))
}

fn invalid_record() -> io::Error {
    // serde errors can include invalid field values; do not expose payload data.
    io::Error::new(io::ErrorKind::InvalidData, "Invalid persisted event record")
}

#[cfg(test)]
#[path = "event_store_tests.rs"]
mod tests;

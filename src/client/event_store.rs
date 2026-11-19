//! Event files for the stateful delivery queue. The store keeps no delivery
//! state: it lists its records once when it opens, then writes, reads and
//! deletes records by key for the coordinator, which owns ordering, capacity
//! and retries. Records hold already prepared wire events, so restoring one
//! never reruns hooks or changes its attribution.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
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

/// Disk retention for the delivery queue. The coordinator enforces
/// `max_events` and `max_bytes`; the store refuses any single record over
/// `max_record_bytes`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct StoreLimits {
    pub(crate) max_events: usize,
    pub(crate) max_bytes: u64,
    pub(crate) max_record_bytes: u64,
}

/// Why a record could not be read.
#[derive(Debug)]
pub(crate) enum ReadError {
    /// The read may succeed later, for example after a permission change.
    Transient(io::Error),
    /// The record is gone, or it was invalid and moved aside. It will never
    /// be readable, so the coordinator drops it from the queue.
    Lost(io::Error),
}

/// Storage for the delivery queue. Only the coordinator thread calls it, so
/// admission never waits on disk.
pub(crate) trait EventStore: Send {
    fn limits(&self) -> StoreLimits;

    /// The records on disk when the store opened, oldest first, with their
    /// sizes. The coordinator calls this once.
    fn take_index(&mut self) -> Vec<(StoreKey, u64)>;

    /// The highest key of any record file present at open, queued or not.
    /// New records must use higher keys.
    fn last_key(&self) -> StoreKey;

    /// Write `event` under `key` and return the record's size. On error no
    /// record exists under `key`.
    fn write(&mut self, key: StoreKey, event: &CaptureEvent) -> io::Result<u64>;

    fn read(&mut self, key: StoreKey) -> Result<CaptureEvent, ReadError>;

    /// Delete a record. Deleting a missing record succeeds.
    fn delete(&mut self, key: StoreKey) -> io::Result<()>;
}

#[derive(Serialize, Deserialize)]
struct Record<T> {
    version: u32,
    event: T,
}

/// Admission measures the persisted representation too, so an event admitted
/// at a limit still fits that limit when it is written and read back.
pub(crate) fn record_size(event: &CaptureEvent) -> usize {
    super::capture::json_len(&Record { version: 1, event })
}

/// Bound serialization before allocating an oversized record buffer.
pub(crate) struct RecordBuffer {
    pub(crate) bytes: Vec<u8>,
    pub(crate) limit: u64,
}

impl Write for RecordBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() as u64 > self.limit.saturating_sub(self.bytes.len() as u64) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Event record exceeds store limit",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Deserialize)]
struct Version {
    version: u32,
}

/// One file per record, named by its key: `events/{key:020}.json`. A record
/// that cannot be decoded is renamed to `.corrupt`, and one from a newer
/// format to `.unsupported`; neither is read again or counted.
pub(crate) struct FileEventStore {
    _directory: Arc<StorageDirectory>,
    events_path: PathBuf,
    limits: StoreLimits,
    index: Vec<(StoreKey, u64)>,
    last_key: StoreKey,
    #[cfg(test)]
    pub(crate) fail_directory_sync: std::cell::Cell<bool>,
}

impl FileEventStore {
    pub(crate) fn open(directory: Arc<StorageDirectory>, limits: StoreLimits) -> io::Result<Self> {
        if limits.max_events == 0
            || limits.max_record_bytes == 0
            || limits.max_record_bytes > limits.max_bytes
            || limits.max_record_bytes == u64::MAX
        {
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
                Some("json") => {
                    // A record over the current limit stays on disk for a
                    // later start with a larger limit, but is not queued now.
                    let size = entry.metadata()?.len();
                    if size <= limits.max_record_bytes {
                        index.push((key, size));
                    }
                }
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
            #[cfg(test)]
            fail_directory_sync: std::cell::Cell::new(false),
        })
    }

    fn path(&self, key: StoreKey) -> PathBuf {
        self.events_path.join(format!("{:020}.json", key.0))
    }

    fn sync_directory(&self) -> io::Result<()> {
        #[cfg(test)]
        if self.fail_directory_sync.replace(false) {
            return Err(io::Error::other("directory sync failed"));
        }
        #[cfg(unix)]
        File::open(&self.events_path)?.sync_all()?;
        Ok(())
    }

    /// Move an unreadable record aside so it is never read or sent again.
    fn quarantine(&self, key: StoreKey, extension: &str, error: io::Error) -> ReadError {
        let path = self.path(key);
        match fs::rename(&path, path.with_extension(extension)) {
            Ok(()) => ReadError::Lost(error),
            // Still in place: a later read may move it aside.
            Err(rename) => ReadError::Transient(rename),
        }
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

    fn write(&mut self, key: StoreKey, event: &CaptureEvent) -> io::Result<u64> {
        let mut buffer = RecordBuffer {
            bytes: Vec::new(),
            limit: self.limits.max_record_bytes,
        };
        serde_json::to_writer(&mut buffer, &Record { version: 1, event }).map_err(|error| {
            if error.is_io() {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Event record exceeds store limit",
                )
            } else {
                invalid_record()
            }
        })?;
        let encoded = buffer.bytes;
        // Check with the reader's bounded decoder before creating any file:
        // serialization alone accepts values nested too deeply to read back.
        let _: Record<CaptureEvent> =
            serde_json::from_slice(&encoded).map_err(|_| invalid_record())?;
        let path = self.path(key);
        let temporary = path.with_extension("tmp");
        let written = (|| -> io::Result<()> {
            let mut file = open_private(&temporary, true)?;
            file.write_all(&encoded)?;
            file.sync_all()?;
            // Closing first also permits rename on Windows.
            drop(file);
            fs::rename(&temporary, &path)
        })();
        if let Err(error) = written {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        // The record is complete and named. A failed directory sync risks it
        // only on power loss, which the queue cannot do better than here.
        let _ = self.sync_directory();
        Ok(encoded.len() as u64)
    }

    fn read(&mut self, key: StoreKey) -> Result<CaptureEvent, ReadError> {
        let path = self.path(key);
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(ReadError::Lost(error));
            }
            Err(error) => return Err(ReadError::Transient(error)),
        };
        let mut encoded = Vec::new();
        if let Err(error) = file
            .take(self.limits.max_record_bytes + 1)
            .read_to_end(&mut encoded)
        {
            return Err(ReadError::Transient(error));
        }
        if encoded.len() as u64 > self.limits.max_record_bytes {
            // Kept for a later start with a larger limit, as at open.
            return Err(ReadError::Lost(record_over_limit()));
        }
        let version: Version = match serde_json::from_slice(&encoded) {
            Ok(version) => version,
            Err(_) => return Err(self.quarantine(key, "corrupt", invalid_record())),
        };
        if version.version != 1 {
            return Err(self.quarantine(
                key,
                "unsupported",
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Unsupported event store version",
                ),
            ));
        }
        match serde_json::from_slice::<Record<CaptureEvent>>(&encoded) {
            Ok(record) => Ok(record.event),
            Err(_) => Err(self.quarantine(key, "corrupt", invalid_record())),
        }
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

fn record_over_limit() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Persisted event exceeds current record limit",
    )
}

fn invalid_record() -> io::Error {
    // serde errors can include invalid field values; do not expose payload data.
    io::Error::new(io::ErrorKind::InvalidData, "Invalid persisted event record")
}

#[cfg(test)]
#[path = "event_store_tests.rs"]
mod tests;

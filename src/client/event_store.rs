//! Worker-owned, bounded event persistence. Stores contain already prepared wire
//! events so restoring a record never reruns hooks or changes its attribution.

use std::collections::{BinaryHeap, VecDeque};

// Metadata only; larger pre-existing spools retain bounded scan-based recovery.
const RECOVERY_INDEX_LIMIT: usize = 10_000;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::capture_event::CaptureEvent;

/// Stable queue-entry identity, independent of caller-supplied event UUIDs.
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

fn open_private(path: &Path, exclusive: bool) -> io::Result<File> {
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

#[derive(Debug, Clone, Copy)]
pub(crate) struct StoreLimits {
    pub(crate) max_events: usize,
    pub(crate) max_bytes: u64,
    pub(crate) max_record_bytes: u64,
}

#[derive(Debug)]
pub(crate) struct StoredEvent {
    pub(crate) key: StoreKey,
    pub(crate) event: CaptureEvent,
    /// Serialized record bytes; recovery accounts for envelope overhead too.
    pub(crate) bytes: u64,
}

#[derive(Debug, Default)]
pub(crate) struct Recovery {
    pub(crate) events: Vec<StoredEvent>,
    pub(crate) errors: Vec<io::Error>,
    pub(crate) more: bool,
    /// Unreadable records remain eligible on a later recovery pass.
    pub(crate) retry_pending: bool,
    /// The recovery cursor advanced past at least one record, including skipped
    /// invalid records. False for an empty page blocked by available capacity.
    pub(crate) advanced: bool,
}

/// Only the delivery worker calls these methods. Admission never invokes a
/// backend or waits for a write. Recovery pages contain at most `max_events`
/// records and `max_bytes` serialized bytes, including record envelopes.
pub(crate) trait EventStore: Send {
    fn recover(&mut self, max_events: usize, max_bytes: u64) -> io::Result<Recovery>;
    fn put(&mut self, event: &CaptureEvent) -> Result<StoreKey, StoreWriteError>;
    fn remove(&mut self, key: StoreKey) -> io::Result<()>;
}

#[derive(Debug)]
pub(crate) struct StoreWriteError {
    pub(crate) key: Option<StoreKey>,
    pub(crate) error: io::Error,
}

impl From<io::Error> for StoreWriteError {
    fn from(error: io::Error) -> Self {
        Self { key: None, error }
    }
}

struct ReadFailure {
    error: io::Error,
    retryable: bool,
}

impl ReadFailure {
    fn permanent(error: io::Error) -> Self {
        Self {
            error,
            retryable: false,
        }
    }
}

impl From<io::Error> for ReadFailure {
    fn from(error: io::Error) -> Self {
        Self {
            error,
            retryable: true,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Record<T> {
    version: u32,
    event: T,
}

/// Admission uses the persisted representation too, so a record admitted at
/// the memory limit can still fit that same recovery window after restart.
pub(crate) fn record_size(event: &CaptureEvent) -> usize {
    super::capture::json_len(&Record { version: 1, event })
}

/// Bound serialization before allocating an oversized record buffer.
struct RecordBuffer {
    bytes: Vec<u8>,
    limit: u64,
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

/// A bounded per-entry spool. Files are ordered by a queue-owned sequence,
/// not the event timestamp or event UUID. New writes cannot overwrite existing
/// entries even when a caller reuses an event UUID.
///
/// Previously accepted records above reduced limits stay on disk and recover
/// in bounded pages. New persistence is refused until the spool is within its
/// limits; the caller may still deliver the new event from bounded memory.
/// Corrupt/unsupported records are quarantined and count against disk limits.
/// Acknowledgement removal is idempotent; failure can cause restart duplicates.
pub(crate) struct FileEventStore {
    _directory: Arc<StorageDirectory>,
    events_path: PathBuf,
    limits: StoreLimits,
    count: usize,
    bytes: u64,
    last_key: u64,
    recovery_end: u64,
    recovery_cursor: u64,
    startup_index: Option<VecDeque<StoreKey>>,
    scan_complete: bool,
    retry_keys: VecDeque<StoreKey>,
    retry_remaining: usize,
    retrying: bool,
    #[cfg(test)]
    directory_scans: std::cell::Cell<usize>,
    #[cfg(test)]
    fail_directory_sync: std::cell::Cell<bool>,
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
        let mut store = Self {
            _directory: directory,
            events_path,
            limits,
            count: 0,
            bytes: 0,
            last_key: 0,
            recovery_end: 0,
            recovery_cursor: 0,
            startup_index: Some(VecDeque::new()),
            scan_complete: false,
            retry_keys: VecDeque::new(),
            retry_remaining: 0,
            retrying: false,
            #[cfg(test)]
            directory_scans: std::cell::Cell::new(1),
            #[cfg(test)]
            fail_directory_sync: std::cell::Cell::new(false),
        };
        // Count metadata without collecting directory entries or reading payloads.
        for entry in fs::read_dir(&store.events_path)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let path = entry.path();
            let Some(key) = record_key(&path) else {
                continue;
            };
            let extension = path.extension().and_then(|value| value.to_str());
            match extension {
                Some("tmp") => {
                    // An exclusive namespace lease guarantees there is no live writer.
                    fs::remove_file(path)?;
                }
                Some("json" | "corrupt" | "unsupported") => {
                    store.last_key = store.last_key.max(key.0);
                    store.count = store.count.saturating_add(1);
                    store.bytes = store.bytes.saturating_add(entry.metadata()?.len());
                    if extension == Some("json") {
                        if let Some(index) = &mut store.startup_index {
                            if index.len() == RECOVERY_INDEX_LIMIT {
                                store.startup_index = None;
                            } else {
                                index.push_back(key);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        if let Some(index) = &mut store.startup_index {
            index.make_contiguous().sort_unstable();
        }
        // Only startup records are recovered. New writes are already in memory.
        store.recovery_end = store.last_key;
        Ok(store)
    }

    fn read_record(
        &self,
        key: StoreKey,
        max_bytes: u64,
    ) -> Result<Option<StoredEvent>, ReadFailure> {
        let path = self.path(key);
        let length = fs::metadata(&path)?.len();
        if length > self.limits.max_record_bytes {
            return Err(ReadFailure::permanent(record_over_limit()));
        }
        if length > max_bytes {
            return Ok(None);
        }
        let file = File::open(&path)?;
        let mut encoded = Vec::new();
        file.take(self.limits.max_record_bytes + 1)
            .read_to_end(&mut encoded)?;
        if encoded.len() as u64 > self.limits.max_record_bytes {
            return Err(ReadFailure::permanent(record_over_limit()));
        }
        if encoded.len() as u64 > max_bytes {
            return Ok(None);
        }
        let version: Version = match serde_json::from_slice(&encoded) {
            Ok(version) => version,
            Err(_) => {
                self.quarantine(key, "corrupt")?;
                return Err(ReadFailure::permanent(invalid_record()));
            }
        };
        if version.version != 1 {
            self.quarantine(key, "unsupported")?;
            return Err(ReadFailure::permanent(io::Error::new(
                io::ErrorKind::InvalidData,
                "Unsupported event store version",
            )));
        }
        let record: Record<CaptureEvent> = match serde_json::from_slice(&encoded) {
            Ok(record) => record,
            Err(_) => {
                self.quarantine(key, "corrupt")?;
                return Err(ReadFailure::permanent(invalid_record()));
            }
        };
        Ok(Some(StoredEvent {
            key,
            event: record.event,
            bytes: encoded.len() as u64,
        }))
    }

    fn recover_retries(&mut self, max_events: usize, max_bytes: u64) -> io::Result<Recovery> {
        let mut result = Recovery::default();
        let mut bytes = 0u64;
        let mut visited = 0;
        while visited < max_events && self.retry_remaining > 0 {
            let key = *self.retry_keys.front().expect("pending recovery retry");
            match self.read_record(key, max_bytes.saturating_sub(bytes)) {
                Ok(Some(event)) => {
                    bytes += event.bytes;
                    result.events.push(event);
                    self.retry_keys.pop_front();
                }
                Ok(None) => break,
                Err(failure) => {
                    result.errors.push(failure.error);
                    self.retry_keys.pop_front();
                    if failure.retryable {
                        self.retry_keys.push_back(key);
                    }
                }
            }
            self.retry_remaining -= 1;
            visited += 1;
        }
        result.more = self.retry_remaining > 0;
        if self.retry_remaining == 0 {
            if !self.scan_complete && self.retry_keys.len() < RECOVERY_INDEX_LIMIT {
                self.retrying = false;
                result.more = true;
            }
            // A forced drain visits each retry at most once. The transport
            // schedules any still-unreadable keys for a later recovery attempt.
            self.retry_remaining = self.retry_keys.len();
        }
        result.retry_pending = !self.retry_keys.is_empty();
        result.advanced = visited > 0;
        Ok(result)
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

    /// Keep at most `limit` oldest eligible names, even for an oversized spool.
    fn candidates(&self, limit: usize) -> io::Result<(Vec<StoreKey>, usize)> {
        if let Some(index) = &self.startup_index {
            return Ok((index.iter().take(limit).copied().collect(), index.len()));
        }
        #[cfg(test)]
        self.directory_scans.set(self.directory_scans.get() + 1);
        let mut oldest = BinaryHeap::new();
        let mut eligible: usize = 0;
        for entry in fs::read_dir(&self.events_path)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let path = entry.path();
            if path.extension().and_then(|v| v.to_str()) != Some("json") {
                continue;
            }
            let Some(key) = record_key(&path) else {
                continue;
            };
            if key.0 <= self.recovery_cursor || key.0 > self.recovery_end {
                continue;
            }
            eligible = eligible.saturating_add(1);
            if oldest.len() < limit {
                oldest.push(key);
            } else if oldest.peek().is_some_and(|latest| key < *latest) {
                oldest.pop();
                oldest.push(key);
            }
        }
        Ok((oldest.into_sorted_vec(), eligible))
    }

    fn quarantine(&self, key: StoreKey, extension: &str) -> Result<(), ReadFailure> {
        let path = self.path(key);
        fs::rename(&path, path.with_extension(extension))?;
        // After rename, retrying the original pathname would read a missing file.
        self.sync_directory().map_err(ReadFailure::permanent)
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

impl EventStore for FileEventStore {
    fn recover(&mut self, max_events: usize, max_bytes: u64) -> io::Result<Recovery> {
        let max_events = max_events.min(self.limits.max_events);
        let max_bytes = max_bytes.min(self.limits.max_bytes);
        if max_events == 0 || max_bytes == 0 {
            return Ok(Recovery {
                more: !self.scan_complete,
                retry_pending: !self.retry_keys.is_empty(),
                ..Recovery::default()
            });
        }
        if self.scan_complete || self.retrying {
            return self.recover_retries(max_events, max_bytes);
        }
        let (candidates, eligible) = self.candidates(max_events)?;
        let mut result = Recovery::default();
        let mut bytes = 0u64;
        let mut visited = 0usize;
        for key in candidates {
            match self.read_record(key, max_bytes.saturating_sub(bytes)) {
                Ok(Some(event)) => {
                    bytes += event.bytes;
                    result.events.push(event);
                }
                Ok(None) => break,
                Err(failure) => {
                    result.errors.push(failure.error);
                    if failure.retryable {
                        if self.retry_keys.len() == RECOVERY_INDEX_LIMIT {
                            // Bounded metadata backpressure: retry existing failures
                            // before advancing past another unreadable record.
                            self.retrying = true;
                            self.retry_remaining = self.retry_keys.len();
                            break;
                        }
                        self.retry_keys.push_back(key);
                    }
                }
            }
            self.recovery_cursor = key.0;
            if let Some(index) = &mut self.startup_index {
                let removed = index.pop_front();
                debug_assert_eq!(removed, Some(key));
            }
            visited += 1;
        }
        self.scan_complete = visited == eligible;
        if self.scan_complete {
            self.retry_remaining = self.retry_keys.len();
        }
        result.more = !self.scan_complete && !self.retrying;
        result.retry_pending = !self.retry_keys.is_empty();
        result.advanced = visited > 0;
        Ok(result)
    }

    fn put(&mut self, event: &CaptureEvent) -> Result<StoreKey, StoreWriteError> {
        if self.count >= self.limits.max_events {
            return Err(io::Error::other("Event store capacity reached").into());
        }
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
        let length = encoded.len() as u64;
        if length > self.limits.max_bytes.saturating_sub(self.bytes) {
            return Err(io::Error::other("Event store byte capacity reached").into());
        }
        // Validate with recovery's bounded decoder before creating any file.
        // Serialization alone also accepts representations too deep to recover.
        let _: Record<CaptureEvent> =
            serde_json::from_slice(&encoded).map_err(|_| invalid_record())?;
        let key = StoreKey(
            self.last_key
                .checked_add(1)
                .ok_or_else(|| io::Error::other("Event store sequence exhausted"))?,
        );
        self.last_key = key.0;
        let path = self.path(key);
        let temporary = path.with_extension("tmp");
        let written = (|| -> io::Result<()> {
            let mut file = open_private(&temporary, true)?;
            file.write_all(&encoded)?;
            file.sync_all()?;
            // Closing first also permits rename on Windows.
            drop(file);
            fs::rename(&temporary, &path)?;
            Ok(())
        })();
        if let Err(error) = written {
            let _ = fs::remove_file(&temporary);
            return Err(error.into());
        }
        self.count = self.count.saturating_add(1);
        self.bytes = self.bytes.saturating_add(length);
        // Even an uncertain directory sync retains the created record's key,
        // allowing a later delivery acknowledgement to release it normally.
        self.sync_directory().map_err(|error| StoreWriteError {
            key: Some(key),
            error,
        })?;
        Ok(key)
    }

    fn remove(&mut self, key: StoreKey) -> io::Result<()> {
        let path = self.path(key);
        let length = match fs::metadata(&path) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        match fs::remove_file(path) {
            Ok(()) => {
                self.count = self.count.saturating_sub(1);
                self.bytes = self.bytes.saturating_sub(length);
                self.sync_directory()
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
#[path = "event_store_tests.rs"]
mod tests;

//! Versioned, per-event desktop spool. All I/O is called by the delivery worker.
use super::EventStore;
use crate::Event;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

const MAX_RECORD_BYTES: u64 = 2 * 1024 * 1024;

#[cfg(test)]
#[path = "file_store_tests.rs"]
mod tests;

#[derive(Serialize, Deserialize)]
struct Record {
    version: u32,
    event: Event,
}

/// A spool directory exclusively locked for this client. Use a separate
/// directory per project, host, and client instance. Open off the host UI thread.
pub struct FileEventStore {
    directory: PathBuf,
    _lock: File,
}

impl FileEventStore {
    pub fn open(directory: impl AsRef<Path>) -> io::Result<Self> {
        let directory = directory.as_ref().to_path_buf();
        fs::create_dir_all(&directory)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(directory.join("lock"))?;
        fs2::FileExt::try_lock_exclusive(&lock)?;
        Ok(Self {
            directory,
            _lock: lock,
        })
    }

    fn sync_directory(&self) -> io::Result<()> {
        #[cfg(unix)]
        File::open(&self.directory)?.sync_all()?;
        Ok(())
    }
}

impl EventStore for FileEventStore {
    fn load(&self) -> io::Result<Vec<Event>> {
        let mut entries: Vec<_> = fs::read_dir(&self.directory)?.collect::<io::Result<_>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        let mut events = Vec::new();
        for entry in entries {
            let path = entry.path();
            if path
                .extension()
                .map_or(true, |extension| extension != "json")
            {
                continue;
            }
            let read = || -> io::Result<Event> {
                let file = File::open(&path)?;
                let mut bytes = Vec::new();
                file.take(MAX_RECORD_BYTES + 1).read_to_end(&mut bytes)?;
                if bytes.len() as u64 > MAX_RECORD_BYTES {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Event record exceeds limit",
                    ));
                }
                let record: Record = serde_json::from_slice(&bytes).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "Invalid event JSON")
                })?;
                if record.version != 1
                    || path.file_stem().and_then(|name| name.to_str())
                        != Some(record.event.uuid().to_string().as_str())
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Invalid event record",
                    ));
                }
                Ok(record.event)
            };
            match read() {
                Ok(event) => events.push(event),
                Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                    // Keep the bounded record for diagnosis, but do not retry it.
                    fs::rename(&path, path.with_extension("corrupt"))?;
                    tracing::warn!("PostHog quarantined a corrupt event record");
                }
                Err(error) => return Err(error),
            }
        }
        Ok(events)
    }

    fn put(&self, event: &Event) -> io::Result<()> {
        let path = self.directory.join(format!("{}.json", event.uuid()));
        let bytes = serde_json::to_vec(&Record {
            version: 1,
            event: event.clone(),
        })?;
        if bytes.len() as u64 > MAX_RECORD_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Event record exceeds limit",
            ));
        }
        let temporary = path.with_extension("tmp");
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(temporary, path)?;
        self.sync_directory()
    }

    fn remove(&self, ids: &[Uuid]) -> io::Result<()> {
        for id in ids {
            match fs::remove_file(self.directory.join(format!("{id}.json"))) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
                _ => {}
            }
        }
        if !ids.is_empty() {
            self.sync_directory()?;
        }
        Ok(())
    }

    fn purge(&self) -> io::Result<()> {
        for entry in fs::read_dir(&self.directory)? {
            let path = entry?.path();
            if path.extension().is_some_and(|extension| {
                extension == "json" || extension == "tmp" || extension == "corrupt"
            }) {
                fs::remove_file(path)?;
            }
        }
        self.sync_directory()
    }
}

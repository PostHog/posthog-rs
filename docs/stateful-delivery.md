# Prepared-event delivery and persistence

The `stateful` Cargo feature enables internal delivery facilities for the stateful
client. It does not yet expose a public stateful client or storage configuration.
Existing server constructors remain memory-backed and retain their defaults,
hook timing, response handling, and finite retry budgets.

## Shared delivery engine

Raw server events and fully prepared events use the same transport thread,
batching, HTTP client, response classifier, and retry scheduler. A prepared event
already contains its final UUID, timestamp, identity, properties, and options.
Delivery and recovery do not enrich it again or rerun capture hooks.

The producer receives one admission result:

- **Accepted:** ownership transferred to the in-memory queue.
- **Full:** the new event was rejected; previously accepted work remains queued.
- **Closed:** the worker no longer accepts events.

Acceptance is not confirmation of persistence or ingestion. Capture admission
performs no filesystem or network I/O. Measuring serialized size still requires
CPU time proportional to the event's size.

## Resource bounds

Prepared queues have explicit event-count and serialized-byte budgets. Both cover
channel entries, buffered events, in-flight requests, and scheduled retries. Byte
reservations travel with events and are released when those events leave memory.
Accounting includes the versioned storage envelope so the same record fits the
same byte budget after a restart. These are serialized-data limits, not exact heap
limits: decoded values, request serialization, compression, and HTTP buffers use
additional bounded working memory.

Recovery reserves count and bytes before reading a page. It never materializes
the whole spool. Newly captured events and recovered events share these budgets.
A full queue rejects new events instead of evicting accepted ones.

File storage has separate count, aggregate-byte, and per-record limits. Existing
records above reduced count or aggregate limits remain on disk and recover in
bounded pages. A record above a reduced per-record limit is left on disk, reports
a read error, and can be recovered after restarting with sufficient limits. The
client configuration must ensure that a permitted record fits the total prepared
queue byte budget.

## File storage

A directory lease uses an exclusive advisory filesystem lock. State and event
stores can share the same lease; competing owners fail to acquire it. Consumers
must use a dedicated namespace on a filesystem that supports these locks and
atomic same-directory rename. Network filesystems are not assumed to provide
those guarantees.

Each queue occurrence has its own sequence key, independent of the event UUID.
Repeated UUIDs therefore cannot overwrite another occurrence or cause the wrong
file to be acknowledged. Version-1 records contain the complete prepared event.
Writes use a private temporary file, file synchronization, and rename. Unix files
and directories are created with restrictive permissions; existing directory
permissions are not changed.

Malformed and unsupported-version records are quarantined and reported without
including their payload in diagnostics. Quarantine still consumes the disk
budget. Failed reads remain on disk for a later restart. Recovery considers only
records present when the store opened, preventing newly written live entries from
also being recovered in the same process.

These guarantees cover process restarts, not arbitrary power-loss durability or
filesystem corruption. In particular, directory metadata is not synchronized
as a transactional checkpoint.

## Failure and retry policy

The worker attempts persistence before first delivery. A failed write invokes
`on_error` with `PostHogError::Storage` and permits delivery from bounded memory.
This includes representations the bounded storage decoder cannot recover, such
as excessively nested properties: they are rejected before creating a record.
The event can be lost if the process exits before delivery. Ordinary capture does
not wait for a write or roll back because persistence failed.

Storage failures expose an operation and an I/O error through `StorageFailure`.
Read, write, and removal failures are reported by the transport worker. Opening
and ownership acquisition return errors to the initialization caller. Without an
error hook, worker diagnostics report the operation and error kind, not backend
error text. Error hooks are observability-only and must not call back into the SDK.

The shared response classifier selects successes, terminal failures, and
retryable outcomes. Successes and terminal failures remove the corresponding
stored occurrences. A failed removal is reported; the event is not resent solely
because deletion failed in this process, but it may be delivered again after a
restart with its original UUID.

Persistent delivery retains retryable work when an active attempt sequence
exhausts its budget. It schedules another sequence after a cooldown rather than
deleting the records. Explicit flush respects that cooldown. Default server
delivery still terminates at its configured retry budget. Previously accepted
persistent work is independent of a later collection-consent decision.

## Flush and shutdown

A flush completion reports that the worker serviced the flush command, not that
all events reached ingestion or that every disk operation succeeded. Pending
retries can remain, and storage failures are reported separately through
`on_error`. Recovery is drained in bounded pages while capacity is available.

Shutdown closes admission and attempts outstanding work within the shared HTTP
deadline. Undelivered durable records remain on disk, including chunks that the
deadline prevented from being attempted. In-memory reservations are released.
Unpersisted events can be lost. Filesystem operations and user error hooks cannot
be forcibly interrupted, so the HTTP deadline is not a promise to interrupt a
blocked filesystem call or callback.

## Validation

The foundation tests cover real-file restart fidelity, namespace ownership,
record/schema failures, bounded recovery, duplicate UUID acknowledgements,
blocked storage, memory-only fallback, retry exhaustion, and shutdown deadlines.
Existing public server integration tests cover the shared transport's compatibility.

```sh
cargo test --locked --features stateful
cargo test --locked --no-default-features --features stateful,error-tracking,tls
cargo clippy --locked --all-targets --features stateful -- -D warnings
scripts/check-public-api.sh
```

CI also exercises the file and transport unit tests on macOS and Windows. Public
stateful initialization and capture-entry tests belong with the stateful client.

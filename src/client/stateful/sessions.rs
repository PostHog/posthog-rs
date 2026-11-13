//! Instance-local session policy. All mutable fields live in Snapshot; the
//! producer owns only the clock. Persistence uses the existing state writer.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{Client, Snapshot};

const IDLE: Duration = Duration::from_secs(30 * 60);
const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// Who controls analytics session boundaries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum SessionMode {
    /// Foreground activity creates sessions, with 30-minute idle and 24-hour age limits.
    #[default]
    Automatic,
    /// Only explicit start/end/reset operations change session boundaries. No expiry.
    Manual,
}

/// Whether sessions can continue across client initialization.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum SessionPersistence {
    /// Each initialization starts sessionless, independently of persisted identity.
    #[default]
    Memory,
    /// Save session timing through the configured client-state store. An explicitly
    /// memory-only backend or unavailable storage still operates in memory.
    Persistent,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StoredSession {
    id: Uuid,
    started_ms: u64,
    activity_ms: u64,
}

impl StoredSession {
    pub(super) fn is_valid(&self) -> bool {
        self.id.get_version_num() == 7
            && self.id.get_variant() == uuid::Variant::RFC4122
            && self.started_ms <= self.activity_ms
    }

    pub(super) fn reserved_bytes() -> usize {
        crate::client::capture::json_len(&Some(Self {
            id: Uuid::from_bytes([255; 16]),
            started_ms: u64::MAX,
            activity_ms: u64::MAX,
        }))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LiveSession {
    stored: StoredSession,
    observed: SessionTick,
    start_age: Duration,
    activity_age: Duration,
}

impl LiveSession {
    fn expired(&self, now: SessionTick) -> bool {
        let elapsed = now.elapsed_since(self.observed);
        self.start_age.saturating_add(elapsed) >= MAX_AGE
            || self.activity_age.saturating_add(elapsed) >= IDLE
    }

    fn touch(&mut self, now: SessionTick) {
        self.start_age = self
            .start_age
            .saturating_add(now.elapsed_since(self.observed));
        self.activity_age = Duration::ZERO;
        self.observed = now;
        // Logical wall timing follows monotonic progress during this process,
        // so a clock correction cannot extend the current live session.
        self.stored.activity_ms = self
            .stored
            .started_ms
            .saturating_add(self.start_age.as_millis().min(u64::MAX as u128) as u64);
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Sessions {
    pub(super) mode: SessionMode,
    pub(super) persistence: SessionPersistence,
    pub(super) background: bool,
    live: Option<LiveSession>,
}

/// Session-only elapsed time. Keep the std clock alongside Linux boot time so
/// failed/unsupported boot-clock reads can fall back without mixing clock epochs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SessionTick {
    monotonic: Instant,
    boot_time: Option<Duration>,
}

impl SessionTick {
    fn now() -> Self {
        Self {
            monotonic: Instant::now(),
            boot_time: boot_time(),
        }
    }

    fn elapsed_since(self, earlier: Self) -> Duration {
        if let (Some(now), Some(then)) = (self.boot_time, earlier.boot_time) {
            if let Some(elapsed) = now.checked_sub(then) {
                return elapsed;
            }
        }
        self.monotonic.saturating_duration_since(earlier.monotonic)
    }
}

#[cfg(target_os = "linux")]
fn boot_time() -> Option<Duration> {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime writes a timespec to a valid, exclusively borrowed
    // pointer. CLOCK_BOOTTIME counts full-system suspend without wall-clock jumps.
    if unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut time) } != 0 {
        return None;
    }
    if time.tv_sec < 0 || !(0..1_000_000_000).contains(&time.tv_nsec) {
        return None;
    }
    Some(Duration::new(time.tv_sec as u64, time.tv_nsec as u32))
}

#[cfg(not(target_os = "linux"))]
fn boot_time() -> Option<Duration> {
    None
}

#[derive(Clone, Copy)]
pub(super) struct SessionTime {
    pub(super) elapsed: SessionTick,
    pub(super) wall_ms: u64,
}

pub(super) trait SessionClock: Send + Sync {
    fn now(&self) -> SessionTime;
}

pub(super) struct SystemSessionClock;
impl SessionClock for SystemSessionClock {
    fn now(&self) -> SessionTime {
        SessionTime {
            elapsed: SessionTick::now(),
            wall_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .min(u64::MAX as u128) as u64,
        }
    }
}

pub(super) enum Operation {
    Maintain,
    Activity,
    Start,
    End,
    Reset,
    Background(bool),
}

impl Sessions {
    pub(super) fn restored(
        stored: Option<&StoredSession>,
        mode: SessionMode,
        persistence: SessionPersistence,
        now: SessionTime,
    ) -> Self {
        let live = stored
            .filter(|_| persistence == SessionPersistence::Persistent)
            .filter(|stored| stored.activity_ms <= now.wall_ms)
            .map(|stored| LiveSession {
                stored: stored.clone(),
                observed: now.elapsed,
                start_age: Duration::from_millis(now.wall_ms - stored.started_ms),
                activity_age: Duration::from_millis(now.wall_ms - stored.activity_ms),
            })
            .filter(|live| mode == SessionMode::Manual || !live.expired(now.elapsed));
        Self {
            mode,
            persistence,
            background: false,
            live,
        }
    }

    pub(super) fn cleared(&self) -> Self {
        Self {
            live: None,
            ..self.clone()
        }
    }

    pub(super) fn stored(&self) -> Option<StoredSession> {
        (self.persistence == SessionPersistence::Persistent)
            .then(|| self.live.as_ref().map(|live| live.stored.clone()))
            .flatten()
    }

    pub(super) fn active_id(&self, now: SessionTick) -> Option<Uuid> {
        self.live
            .as_ref()
            .filter(|live| self.mode == SessionMode::Manual || !live.expired(now))
            .map(|live| live.stored.id)
    }

    fn start(&mut self, now: SessionTime) {
        self.live = Some(LiveSession {
            stored: StoredSession {
                id: Uuid::now_v7(),
                started_ms: now.wall_ms,
                activity_ms: now.wall_ms,
            },
            observed: now.elapsed,
            start_age: Duration::ZERO,
            activity_age: Duration::ZERO,
        });
    }

    fn maintain(&mut self, now: SessionTime, activity: bool) {
        if self.mode == SessionMode::Manual {
            return;
        }
        if self
            .live
            .as_ref()
            .is_some_and(|live| live.expired(now.elapsed))
        {
            self.live = None;
        }
        if self.background {
            return;
        }
        match self.live.as_mut() {
            None => self.start(now),
            Some(live) if activity => live.touch(now.elapsed),
            Some(_) => {}
        }
    }

    fn apply(&mut self, operation: Operation, now: SessionTime, opted_out: bool) {
        match operation {
            Operation::End => self.live = None,
            Operation::Background(background) => {
                if self.background == background {
                    return;
                }
                self.background = background;
                if opted_out || self.mode == SessionMode::Manual {
                    return;
                }
                if background {
                    if let Some(live) = self.live.as_mut().filter(|live| !live.expired(now.elapsed))
                    {
                        live.touch(now.elapsed);
                    }
                } else {
                    self.maintain(now, true);
                }
            }
            _ if opted_out => {}
            Operation::Maintain => self.maintain(now, false),
            Operation::Activity => self.maintain(now, true),
            Operation::Start => {
                if self.live.is_none() {
                    self.start(now);
                }
            }
            Operation::Reset => self.start(now),
        }
    }
}

impl Snapshot {
    pub(super) fn session_operation(
        &mut self,
        operation: Operation,
        now: SessionTime,
        opted_out: bool,
    ) {
        self.sessions.apply(operation, now, opted_out);
        self.session = self.sessions.stored();
    }
}

impl Client {
    fn session_operation(&self, operation: Operation) -> Option<Uuid> {
        if self.is_disabled() || self.closed.load(Ordering::Acquire) {
            return None;
        }
        self.state
            .as_ref()
            .unwrap()
            .mutate(|snapshot| {
                let now = self.producer.as_ref().unwrap().session_clock.now();
                let opted_out = self.snapshot_opted_out(snapshot);
                snapshot.session_operation(operation, now, opted_out);
                snapshot.sessions.active_id(now.elapsed)
            })
            .ok()
            .and_then(|(_, id)| id)
    }

    /// Maintain the current session without extending a valid session's activity.
    /// Automatic foreground mode creates/rotates; background mode clears expired
    /// sessions. While opted out, returns only a valid existing ID without mutation.
    /// Disabled/closed clients return None. No filesystem or network I/O.
    pub fn session_id(&self) -> Option<Uuid> {
        self.session_operation(Operation::Maintain)
    }

    /// Inspect the stored ID without creating, expiring, rotating or touching it.
    /// The returned session can be expired. Disabled/closed clients return None.
    pub fn peek_session_id(&self) -> Option<Uuid> {
        if self.is_disabled() || self.closed.load(Ordering::Acquire) {
            return None;
        }
        self.state
            .as_ref()
            .unwrap()
            .snapshot()
            .sessions
            .live
            .as_ref()
            .map(|live| live.stored.id)
    }

    /// Create a session if absent; repeating this does not extend or rotate it.
    /// Available in either mode, including background. No-op while opted out.
    pub fn start_session(&self) {
        self.session_operation(Operation::Start);
    }

    /// Clear the session locally, including while opted out. Automatic foreground
    /// activity/getters can create another; manual mode waits for an explicit start.
    pub fn end_session(&self) {
        self.session_operation(Operation::End);
    }

    /// Force a new session ID without emitting an event. No-op while opted out.
    pub fn reset_session(&self) {
        self.session_operation(Operation::Reset);
    }

    /// Report foreground user activity. Automatic mode maintains and touches the
    /// session; manual mode and opted-out clients do not update activity.
    pub fn session_activity(&self) {
        self.session_operation(Operation::Activity);
    }

    /// Report a host lifecycle transition. Initially foreground. Duplicate signals
    /// do nothing. Backgrounding touches a valid automatic session; foregrounding
    /// maintains it. While opted out only the environmental state changes.
    pub fn set_app_in_background(&self, background: bool) {
        self.session_operation(Operation::Background(background));
    }
}

#[cfg(test)]
#[path = "sessions_tests.rs"]
mod tests;

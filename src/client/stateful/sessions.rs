//! Instance-local session policy. All mutable fields live in Snapshot; the
//! producer owns only the clock. Persistence uses the existing state writer.

use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

    /// Wall-clock expiry, as in the other stateful SDKs. A clock moved backwards
    /// never expires a session early.
    fn expired(&self, now: SessionTime) -> bool {
        now.since(self.started_ms) >= MAX_AGE || now.since(self.activity_ms) >= IDLE
    }

    /// A clock moved backwards never moves activity before an earlier observation.
    fn touch(&mut self, now: SessionTime) {
        self.activity_ms = self.activity_ms.max(now.wall_ms);
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Sessions {
    pub(super) mode: SessionMode,
    pub(super) persistence: SessionPersistence,
    pub(super) background: bool,
    live: Option<StoredSession>,
    epoch: u64,
    provisional: Option<StoredSession>,
}

/// Attribution frozen before hooks. Only the current epoch may commit activity.
#[derive(Clone, Copy)]
pub(super) struct AdmissionSession {
    epoch: u64,
    pub(super) id: Option<Uuid>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SessionTime {
    pub(super) wall_ms: u64,
}

impl SessionTime {
    fn since(self, earlier_ms: u64) -> Duration {
        Duration::from_millis(self.wall_ms.saturating_sub(earlier_ms))
    }
}

pub(super) trait SessionClock: Send + Sync {
    fn now(&self) -> SessionTime;
}

pub(super) struct SystemSessionClock;
impl SessionClock for SystemSessionClock {
    fn now(&self) -> SessionTime {
        SessionTime {
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
            .filter(|stored| mode == SessionMode::Manual || !stored.expired(now))
            .cloned();
        Self {
            mode,
            persistence,
            background: false,
            live,
            epoch: 0,
            provisional: None,
        }
    }

    pub(super) fn cleared(&self) -> Self {
        let mut sessions = self.clone();
        sessions.end();
        sessions
    }

    /// Make admissions prepared before now commit nothing.
    fn invalidate_admissions(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        self.provisional = None;
    }

    fn end(&mut self) {
        self.invalidate_admissions();
        self.live = None;
    }

    pub(super) fn stored(&self) -> Option<StoredSession> {
        (self.persistence == SessionPersistence::Persistent)
            .then(|| self.live.clone())
            .flatten()
    }

    pub(super) fn active_id(&self, now: SessionTime) -> Option<Uuid> {
        self.live
            .as_ref()
            .filter(|live| self.mode == SessionMode::Manual || !live.expired(now))
            .map(|live| live.id)
    }

    /// Attribution to a valid live session. Preparing it would change nothing.
    pub(super) fn live_admission(&self, now: SessionTime) -> Option<AdmissionSession> {
        self.active_id(now).map(|id| AdmissionSession {
            epoch: self.epoch,
            id: Some(id),
        })
    }

    pub(super) fn prepare_admission(&mut self, now: SessionTime) -> AdmissionSession {
        let mut id = self.active_id(now);
        if id.is_none() && self.mode == SessionMode::Automatic && !self.background {
            if self
                .provisional
                .as_ref()
                .map_or(true, |live| live.expired(now))
            {
                self.epoch = self.epoch.wrapping_add(1);
                self.provisional = Some(Self::new_live(now));
            }
            id = self.provisional.as_ref().map(|live| live.id);
        }
        AdmissionSession {
            epoch: self.epoch,
            id,
        }
    }

    pub(super) fn commit_admission(
        &mut self,
        token: AdmissionSession,
        now: SessionTime,
        activity: bool,
    ) {
        if token.epoch != self.epoch || token.id.is_none() {
            return;
        }
        if self.provisional.as_ref().map(|live| live.id) == token.id {
            self.live = self.provisional.take();
        }
        if activity && self.mode == SessionMode::Automatic && !self.background {
            if let Some(live) = self.live.as_mut().filter(|live| Some(live.id) == token.id) {
                live.touch(now);
            }
        }
    }

    fn new_live(now: SessionTime) -> StoredSession {
        StoredSession {
            id: Uuid::now_v7(),
            started_ms: now.wall_ms,
            activity_ms: now.wall_ms,
        }
    }

    fn start(&mut self, now: SessionTime) {
        self.invalidate_admissions();
        self.live = Some(Self::new_live(now));
    }

    /// Begin the session that in-flight captures already attribute to, if it is
    /// still valid, so their admission commits to it; otherwise start a new one.
    fn start_or_adopt(&mut self, now: SessionTime) {
        match self.provisional.take().filter(|live| !live.expired(now)) {
            Some(mut provisional) => {
                // A capture a hook dropped can leave a preview up to 30 minutes
                // old. Becoming current counts as activity, as in `start`.
                provisional.touch(now);
                self.live = Some(provisional);
            }
            None => self.start(now),
        }
    }

    fn maintain(&mut self, now: SessionTime, activity: bool) {
        if self.mode == SessionMode::Manual {
            return;
        }
        if self.live.as_ref().is_some_and(|live| live.expired(now)) {
            self.live = None;
        }
        if self.background {
            return;
        }
        if self.live.is_none() {
            self.start_or_adopt(now);
        }
        if let Some(live) = self.live.as_mut().filter(|_| activity) {
            live.touch(now);
        }
    }

    fn apply(&mut self, operation: Operation, now: SessionTime, opted_out: bool) {
        match operation {
            Operation::End => self.end(),
            Operation::Background(background) => {
                if self.background == background {
                    return;
                }
                self.background = background;
                self.invalidate_admissions();
                if opted_out || self.mode == SessionMode::Manual {
                    return;
                }
                if background {
                    if let Some(live) = self.live.as_mut().filter(|live| !live.expired(now)) {
                        live.touch(now);
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
                    self.start_or_adopt(now);
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
    /// The time sessions are measured on.
    pub(super) fn session_now(&self) -> SessionTime {
        self.producer.as_ref().unwrap().session_clock.now()
    }

    fn session_operation(&self, operation: Operation) -> Option<Uuid> {
        if self.is_disabled() || self.closed.load(Ordering::Acquire) {
            return None;
        }
        self.state
            .as_ref()
            .unwrap()
            .mutate(|snapshot| {
                let now = self.session_now();
                let opted_out = self.snapshot_opted_out(snapshot);
                snapshot.session_operation(operation, now, opted_out);
                snapshot.sessions.active_id(now)
            })
            .ok()
            .and_then(|(_, id)| id)
    }

    /// Maintain the current session without extending a valid session's activity.
    /// Automatic foreground mode creates/rotates; background mode clears expired
    /// sessions. While opted out, returns only a valid existing ID without mutation.
    /// Disabled/closed clients return None. No filesystem or network I/O.
    pub fn get_session_id(&self) -> Option<Uuid> {
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
            .map(|live| live.id)
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

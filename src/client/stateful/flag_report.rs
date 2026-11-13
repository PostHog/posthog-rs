//! Bounded, accepted-only access tracking shares the validated state owner.

use std::collections::{HashMap, VecDeque};

use sha1::{Digest, Sha1};

use super::flag_state::CachedFlag;
use super::producer::CaptureGuard;
use super::{Capture, CaptureOutcome, Client, Snapshot};
use crate::feature_flag_evaluations::{flag_called_properties, flag_value_for};
use crate::FlagValue;

const MAX_REPORTS: usize = 1024;
type Key = [u8; 20];

#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    ticket: u64,
    accepted: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct ReportTracker {
    entries: HashMap<Key, Entry>,
    accepted: VecDeque<Key>,
    next_ticket: u64,
}

impl ReportTracker {
    pub(super) fn clear(&mut self) {
        self.entries.clear();
        self.accepted.clear();
        // Preserve tickets across reload completion to prevent a late capture
        // from committing a new reservation for the same key (ABA).
    }

    fn reserve(&mut self, key: Key) -> Option<u64> {
        if self.entries.contains_key(&key) {
            return None;
        }
        let ticket = self.next_ticket.checked_add(1)?;
        if self.entries.len() == MAX_REPORTS {
            // Never evict pending captures; if all entries are pending, skip
            // this report rather than growing memory or duplicating in-flight work.
            let oldest = self.accepted.pop_front()?;
            self.entries.remove(&oldest);
        }
        self.next_ticket = ticket;
        self.entries.insert(
            key,
            Entry {
                ticket,
                accepted: false,
            },
        );
        Some(ticket)
    }

    fn finish(&mut self, key: Key, ticket: u64, accepted: bool) {
        let Some(entry) = self.entries.get_mut(&key) else {
            return;
        };
        if entry.ticket != ticket || entry.accepted {
            return;
        }
        if accepted {
            entry.accepted = true;
            self.accepted.push_back(key);
        } else {
            self.entries.remove(&key);
        }
    }
}

struct Reservation<'a> {
    client: &'a Client,
    key: Key,
    ticket: u64,
    revision: u64,
    request: u64,
    accepted: bool,
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        let _ = self.client.state.as_ref().unwrap().mutate(|state| {
            if state.flag_load.revision == self.revision && state.flag_load.request == self.request
            {
                state
                    .flag_load
                    .reports
                    .finish(self.key, self.ticket, self.accepted);
            }
        });
    }
}

fn report_key(snapshot: &Snapshot, key: &str, value: &FlagValue) -> Key {
    fn string(hash: &mut Sha1, value: &str) {
        hash.update((value.len() as u64).to_le_bytes());
        hash.update(value.as_bytes());
    }
    let mut hash = Sha1::new();
    string(&mut hash, &snapshot.distinct_id);
    string(&mut hash, &snapshot.anonymous_id);
    let mut groups: Vec<_> = snapshot.groups.iter().collect();
    groups.sort_unstable();
    hash.update((groups.len() as u64).to_le_bytes());
    for (kind, value) in groups {
        string(&mut hash, kind);
        string(&mut hash, value);
    }
    string(&mut hash, key);
    match value {
        FlagValue::Boolean(value) => hash.update([u8::from(*value)]),
        FlagValue::String(value) => {
            hash.update([2]);
            string(&mut hash, value);
        }
    }
    hash.finalize().into()
}

impl Client {
    pub(super) fn report_flag_access(&self, key: &str, snapshot: &Snapshot, flag: &CachedFlag) {
        if self.snapshot_opted_out(snapshot) {
            return;
        }
        let Ok(_guard) = CaptureGuard::enter(self) else {
            return;
        };
        let response = flag_value_for(&flag.record);
        let fingerprint = report_key(snapshot, key, &response);
        let ticket = self.state.as_ref().unwrap().mutate(|current| {
            if current.flag_load.revision != snapshot.flag_load.revision
                || current.flag_load.request != snapshot.flag_load.request
                || current.flag_load.completed != snapshot.flag_load.completed
            {
                return None;
            }
            current.flag_load.reports.reserve(fingerprint)
        });
        let Ok((_, Some(ticket))) = ticket else {
            return;
        };
        let mut reservation = Reservation {
            client: self,
            key: fingerprint,
            ticket,
            revision: snapshot.flag_load.revision,
            request: snapshot.flag_load.request,
            accepted: false,
        };
        let mut event = Capture::new("$feature_flag_called");
        event.properties = flag_called_properties(
            key,
            Some(&flag.record),
            &Some(response),
            flag.request_id.as_deref(),
            flag.evaluated_at,
            flag.partial,
            false,
        );
        if let Some(bootstrap) = snapshot
            .flag_load
            .bootstrap
            .as_ref()
            .and_then(|cache| cache.flags.get(key))
        {
            event.properties.insert(
                "$feature_flag_bootstrapped_response".into(),
                serde_json::json!(flag_value_for(&bootstrap.record)),
            );
            if let Some(payload) = &bootstrap.record.payload {
                event
                    .properties
                    .insert("$feature_flag_bootstrapped_payload".into(), payload.clone());
            }
        }
        event.properties.insert(
            "$used_bootstrap_value".into(),
            (!snapshot.flag_load.remote_received).into(),
        );
        let minimal =
            flag.record.minimal_flag_called_events && flag.record.has_experiment == Some(false);
        reservation.accepted = matches!(
            self.capture_flag_snapshot(event, snapshot, minimal),
            CaptureOutcome::Accepted { .. }
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(i: u64) -> Key {
        let mut key = [0; 20];
        key[..8].copy_from_slice(&i.to_le_bytes());
        key
    }

    #[test]
    fn accepted_entries_evict_incrementally_and_pending_entries_are_bounded() {
        let mut tracker = ReportTracker::default();
        for i in 0..MAX_REPORTS as u64 {
            let ticket = tracker.reserve(key(i)).unwrap();
            tracker.finish(key(i), ticket, true);
        }
        assert!(tracker.reserve(key(MAX_REPORTS as u64)).is_some());
        assert!(!tracker.entries.contains_key(&key(0)));
        assert!(tracker.reserve(key(1)).is_none());
        assert_eq!(tracker.entries.len(), MAX_REPORTS);
        tracker.clear();
        for i in 0..MAX_REPORTS as u64 {
            assert!(tracker.reserve(key(i)).is_some());
        }
        assert!(tracker.reserve(key(MAX_REPORTS as u64)).is_none());
        assert!(tracker.accepted.is_empty());
        assert_eq!(tracker.entries.len(), MAX_REPORTS);
    }

    #[test]
    fn rejection_releases_and_late_completion_cannot_commit_reused_key() {
        let mut tracker = ReportTracker::default();
        let old = tracker.reserve(key(1)).unwrap();
        tracker.finish(key(1), old, false);
        let retry = tracker.reserve(key(1)).unwrap();
        tracker.clear();
        let current = tracker.reserve(key(1)).unwrap();
        tracker.finish(key(1), retry, true);
        assert!(!tracker.entries[&key(1)].accepted);
        tracker.finish(key(1), current, true);
        assert!(tracker.reserve(key(1)).is_none());
    }

    #[test]
    fn close_clears_tracker_under_the_state_owner_lock() {
        let server = httpmock::MockServer::start();
        server.mock(|_, then| {
            then.status(200)
                .json_body(serde_json::json!({"results":{}}));
        });
        let mut config = super::super::Config::new("phc_test");
        config.persistence = super::super::Persistence::Memory;
        config.host = server.base_url();
        config.preload_feature_flags = false;
        config.max_capture_attempts = 1;
        config.bootstrap_feature_flags =
            Some(HashMap::from([("a".into(), FlagValue::Boolean(true))]));
        let client = Client::init_blocking(config);
        let _ = client.feature_flag("a").get();
        assert_eq!(
            client
                .state
                .as_ref()
                .unwrap()
                .snapshot()
                .flag_load
                .reports
                .entries
                .len(),
            1
        );
        client.shutdown_blocking(None);
        let state = client.state.as_ref().unwrap().snapshot();
        assert!(state.flag_load.reports.entries.is_empty());
        assert!(state.flag_load.reports.accepted.is_empty());
    }

    #[test]
    fn keys_are_typed_and_groups_are_order_independent() {
        let mut a = Snapshot::anonymous("device".into());
        a.groups = HashMap::from([("company".into(), "a".into()), ("team".into(), "b".into())]);
        let mut b = a.clone();
        b.groups = HashMap::from([("team".into(), "b".into()), ("company".into(), "a".into())]);
        assert_eq!(
            report_key(&a, "flag", &FlagValue::Boolean(true)),
            report_key(&b, "flag", &FlagValue::Boolean(true))
        );
        assert_ne!(
            report_key(&a, "flag", &FlagValue::Boolean(true)),
            report_key(&a, "flag", &FlagValue::String("true".into()))
        );
        b.groups.insert("company".into(), "other".into());
        assert_ne!(
            report_key(&a, "flag", &FlagValue::Boolean(true)),
            report_key(&b, "flag", &FlagValue::Boolean(true))
        );
    }
}

//! Bounded, accepted-only tracking by flag key and typed value. Reloads and
//! targeting changes retain reports; distinct-ID changes, reset and shutdown clear them.
//! Reservations share the validated state owner and finish by non-reused ticket.

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
        // A late capture must not commit a new reservation for the same key (ABA).
    }

    pub(super) fn cleared(&self) -> Self {
        Self {
            next_ticket: self.next_ticket,
            ..Self::default()
        }
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
    accepted: bool,
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        let _ = self.client.state.as_ref().unwrap().mutate(|state| {
            state
                .flag_load
                .reports
                .finish(self.key, self.ticket, self.accepted);
        });
    }
}

fn report_key(key: &str, value: Option<&FlagValue>) -> Key {
    fn string(hash: &mut Sha1, value: &str) {
        hash.update((value.len() as u64).to_le_bytes());
        hash.update(value.as_bytes());
    }
    let mut hash = Sha1::new();
    string(&mut hash, key);
    match value {
        Some(FlagValue::Boolean(value)) => hash.update([u8::from(*value)]),
        Some(FlagValue::String(value)) => {
            hash.update([2]);
            string(&mut hash, value);
        }
        None => hash.update([3]),
    }
    hash.finalize().into()
}

impl Client {
    pub(super) fn report_flag_access(
        &self,
        key: &str,
        snapshot: &Snapshot,
        flag: Option<&CachedFlag>,
    ) {
        if self.snapshot_opted_out(snapshot) {
            return;
        }
        let Ok(_guard) = CaptureGuard::enter(self) else {
            return;
        };
        let response = flag.map(|flag| flag_value_for(&flag.record));
        let fingerprint = report_key(key, response.as_ref());
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
            accepted: false,
        };
        let mut event = Capture::new("$feature_flag_called");
        event.properties = flag_called_properties(
            key,
            flag.map(|flag| &flag.record),
            &response,
            flag.and_then(|flag| flag.request_id.as_deref()),
            flag.and_then(|flag| flag.evaluated_at),
            flag.is_some_and(|flag| flag.partial),
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
        let minimal = flag.is_some_and(|flag| {
            flag.record.minimal_flag_called_events && flag.record.has_experiment == Some(false)
        });
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
    fn minimal_event_allowlist_is_literal_and_exhaustive() {
        let expected = [
            "$feature_flag",
            "$feature_flag_response",
            "$feature_flag_has_experiment",
            "$feature_flag_id",
            "$feature_flag_version",
            "$feature_flag_reason",
            "$feature_flag_request_id",
            "$feature_flag_evaluated_at",
            "$feature_flag_error",
            "locally_evaluated",
            "$groups",
            "$process_person_profile",
            "$geoip_disable",
            "$cookieless_mode",
            "$ignore_sent_at",
            "$product_tour_id",
            "$session_id",
            "$window_id",
            "$device_id",
            "$lib",
            "$lib_version",
            "$is_server",
            "$os",
            "$os_version",
            "$referring_domain",
            "utm_source",
            "utm_medium",
            "utm_campaign",
            "utm_content",
            "utm_term",
            "gad_source",
            "mc_cid",
            "gclid",
            "gclsrc",
            "dclid",
            "gbraid",
            "wbraid",
            "fbclid",
            "msclkid",
            "twclid",
            "li_fat_id",
            "igshid",
            "ttclid",
            "rdt_cid",
            "epik",
            "qclid",
            "sccid",
            "oppref",
            "bfcid",
            "irclid",
            "_kx",
        ];
        let mut actual = crate::event::MINIMAL_FLAG_CALLED_EVENT_PROPERTIES.to_vec();
        let mut expected = expected.to_vec();
        actual.sort_unstable();
        expected.sort_unstable();
        assert_eq!(actual, expected);
        for key in ["$referrer", "utm_private", "gclid_private", "private"] {
            assert!(!crate::event::is_minimal_flag_called_property(key));
        }
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
    fn reset_and_reidentify_cannot_reuse_a_pending_ticket() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            mpsc, Arc,
        };
        use std::time::Duration;
        const WAIT: Duration = Duration::from_secs(5);
        let server = httpmock::MockServer::start();
        server.mock(|when, then| {
            when.path("/array/phc_test/config");
            then.status(200)
                .json_body(serde_json::json!({"hasFeatureFlags":true}));
        });
        server.mock(|when, then| {
            when.path("/flags/");
            then.status(200)
                .json_body(serde_json::json!({"featureFlags":{"a":true}}));
        });
        let old_uuid = uuid::Uuid::now_v7();
        let delivered = server.mock(|when, then| {
            when.path("/i/v1/analytics/events")
                .body_includes(old_uuid.to_string());
            then.status(200)
                .json_body(serde_json::json!({"results":{}}));
        });
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (returned_tx, returned_rx) = mpsc::channel();
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let mut config = super::super::Config::new("phc_test");
        config.host = server.base_url();
        config.persistence = super::super::Persistence::Memory;
        config.preload_feature_flags = false;
        config.bootstrap_anonymous_id = Some("original".into());
        config.bootstrap_feature_flags =
            Some(HashMap::from([("a".into(), FlagValue::Boolean(true))]));
        config.flush_at = 1000;
        config.flush_interval = Duration::from_secs(3600);
        config.shutdown_timeout = Duration::from_secs(1);
        config.capture_compression = None;
        config.before_send(move |mut event| {
            assert_eq!(event.event_name(), "$feature_flag_called");
            if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                event.set_uuid(old_uuid);
                entered_tx.send(()).unwrap();
                release_rx.recv_timeout(WAIT).expect("old capture release");
                returned_tx.send(()).unwrap();
            }
            Some(event)
        });
        let client = Arc::new(Client::init_blocking(config));
        let reader = client.clone();
        let old_capture = std::thread::spawn(move || reader.feature_flag("a").get().unwrap());
        entered_rx.recv_timeout(WAIT).unwrap();
        let state = client.state.as_ref().unwrap();
        let fingerprint = report_key("a", Some(&FlagValue::Boolean(true)));
        let old_ticket = state.snapshot().flag_load.reports.entries[&fingerprint].ticket;
        client.reset();
        // Identity still changes while opted out, without entering the held FnMut hook.
        client.opt_out();
        client.identify("original");
        client.transport.as_ref().unwrap().request_barrier();
        assert_eq!(client.get_distinct_id(), "original");
        assert!(client.is_identified());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        client.opt_in();
        // Reserve directly to isolate the tracker race from serialized application hooks.
        let (_, new_ticket) = state
            .mutate(|state| state.flag_load.reports.reserve(fingerprint).unwrap())
            .unwrap();
        assert!(new_ticket > old_ticket);
        release_tx.send(()).unwrap();
        assert!(old_capture.join().unwrap().enabled);
        returned_rx
            .recv_timeout(WAIT)
            .expect("old hook must return normally");
        client.flush_blocking();
        delivered.assert_calls(1); // The old capture was actually accepted and delivered late.
        let current = state.snapshot();
        assert_eq!(
            current.flag_load.reports.entries[&fingerprint].ticket,
            new_ticket
        );
        assert!(!current.flag_load.reports.entries[&fingerprint].accepted);
        let _ = client.feature_flag("a").get();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        state
            .mutate(|state| {
                state
                    .flag_load
                    .reports
                    .finish(fingerprint, new_ticket, false)
            })
            .unwrap();
        let _ = client.feature_flag("a").get();
        let _ = client.feature_flag("a").get();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(state.snapshot().flag_load.reports.entries[&fingerprint].accepted);
        client.shutdown_blocking(None);
    }

    #[test]
    fn keys_distinguish_flag_names_and_typed_values() {
        assert_ne!(
            report_key("flag", Some(&FlagValue::Boolean(true))),
            report_key("flag", Some(&FlagValue::String("true".into())))
        );
        assert_ne!(
            report_key("flag", Some(&FlagValue::Boolean(true))),
            report_key("flag", Some(&FlagValue::Boolean(false)))
        );
        assert_ne!(
            report_key("flag", Some(&FlagValue::Boolean(true))),
            report_key("other", Some(&FlagValue::Boolean(true)))
        );
        // A missing flag has its own key, distinct from every typed value.
        assert_ne!(
            report_key("flag", None),
            report_key("flag", Some(&FlagValue::Boolean(false)))
        );
        assert_ne!(
            report_key("flag", None),
            report_key("flag", Some(&FlagValue::String(String::new())))
        );
    }
}

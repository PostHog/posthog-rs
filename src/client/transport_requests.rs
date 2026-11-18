//! Stateful configuration/flag execution. This owner never holds event storage.

use super::{build_http, flags, ClientOptions, Completion, Control, StartupFailure};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle, ThreadId};

pub(super) struct Worker {
    tx: mpsc::Sender<Control>,
    worker: Mutex<Option<JoinHandle<()>>>,
    id: ThreadId,
}

impl Worker {
    pub(super) fn spawn(
        options: ClientOptions,
        slot: Arc<flags::Slot>,
        closed: Arc<AtomicBool>,
        deadline: Arc<Mutex<Option<std::time::Instant>>>,
    ) -> Result<Self, StartupFailure> {
        let (tx, rx) = mpsc::channel();
        let (ready, wait) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("posthog-requests".into())
            .spawn(move || {
                let Some(http) = build_http(&options) else {
                    let _ = ready.send(false);
                    slot.discard();
                    return;
                };
                let _ = ready.send(true);
                while let Ok(control) = rx.recv() {
                    if closed.load(Ordering::Acquire)
                        || deadline.lock().unwrap_or_else(|p| p.into_inner()).is_some()
                    {
                        break;
                    }
                    match control {
                        #[cfg(test)]
                        Control::RequestBarrier(tx) => {
                            let _ = tx.send(());
                        }
                        Control::Flags(slot) => slot.run(&http, &options),
                        Control::RemoteConfig(task) => task.run(&http, &options, &closed),
                        _ => break,
                    }
                }
                // Pending completions may own application captures. Destroy them
                // here, never on the thread requesting bounded shutdown.
                slot.discard();
                drop(rx);
            })
            .map_err(|_| StartupFailure::Spawn)?;
        let id = worker.thread().id();
        match wait.recv() {
            Ok(true) => Ok(Self {
                tx,
                worker: Mutex::new(Some(worker)),
                id,
            }),
            result => {
                let _ = worker.join();
                Err(if result.is_ok() {
                    StartupFailure::Http
                } else {
                    StartupFailure::WorkerStopped
                })
            }
        }
    }

    pub(super) fn sender(&self) -> &mpsc::Sender<Control> {
        &self.tx
    }

    pub(super) fn on_worker_thread(&self) -> bool {
        self.id == thread::current().id()
    }

    pub(super) fn request_close(&self) {
        let (tx, _) = mpsc::channel();
        let _ = self.tx.send(Control::Shutdown {
            completion: Completion::Blocking(tx),
            deadline: None,
        });
    }

    pub(super) fn is_stopped(&self) -> bool {
        self.worker
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .map_or(true, JoinHandle::is_finished)
    }

    pub(super) fn join(&self) {
        if !self.on_worker_thread() {
            let worker = self.worker.lock().unwrap_or_else(|p| p.into_inner()).take();
            if let Some(worker) = worker {
                let _ = worker.join();
            }
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.request_close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::transport::{remote_config, TransportHandle};
    use crate::Event;
    use httpmock::{Method::POST, MockServer};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::Duration;

    fn blocked_server() -> (String, mpsc::Receiver<()>, mpsc::Sender<()>, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let (entered, wait) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0; 8192];
            let _ = stream.read(&mut buffer).unwrap();
            entered.send(()).unwrap();
            gate.recv().unwrap();
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}");
        });
        (address, wait, release, worker)
    }

    fn options(host: String) -> ClientOptions {
        let mut options = super::super::tests::options(host.clone()).build().unwrap();
        options.host = Some(host);
        options.flush_at = 1;
        options.flush_interval_ms = 3_600_000;
        options.capture_compression = None;
        options
    }

    #[test]
    fn blocked_remote_config_does_not_block_event_delivery() {
        let server = MockServer::start();
        let delivery = server.mock(|when, then| {
            when.method(POST);
            then.status(200);
        });
        let transport = Arc::new(
            TransportHandle::spawn_stateful(options(server.base_url()), None, 4096).unwrap(),
        );
        let (url, entered, release, server) = blocked_server();
        let (complete, done) = mpsc::channel();
        assert!(
            transport.schedule_remote_config(remote_config::Task::new(url, move |_| {
                complete.send(()).unwrap();
                None
            }))
        );
        entered.recv_timeout(Duration::from_secs(2)).unwrap();
        transport.enqueue(Event::new("event", "person"));
        let flushing = transport.clone();
        let (flushed, wait) = mpsc::channel();
        let flush = thread::spawn(move || {
            flushing.flush_blocking();
            flushed.send(()).unwrap();
        });
        let result = wait.recv_timeout(Duration::from_secs(2));
        release.send(()).unwrap();
        server.join().unwrap();
        flush.join().unwrap();
        result.unwrap();
        done.recv_timeout(Duration::from_secs(2)).unwrap();
        transport.close_blocking();
        delivery.assert_calls(1);
    }

    #[test]
    fn blocked_event_delivery_does_not_block_requested_config() {
        let (host, entered, release, server) = blocked_server();
        let transport =
            Arc::new(TransportHandle::spawn_stateful(options(host), None, 4096).unwrap());
        transport.enqueue(Event::new("event", "person"));
        entered.recv_timeout(Duration::from_secs(2)).unwrap();
        let config = MockServer::start();
        config.mock(|_, then| {
            then.status(200)
                .json_body(serde_json::json!({"hasFeatureFlags": false}));
        });
        let (complete, done) = mpsc::channel();
        assert!(transport.schedule_remote_config(remote_config::Task::new(
            config.base_url(),
            move |result| {
                complete.send(result.is_ok()).unwrap();
                None
            }
        )));
        let result = done.recv_timeout(Duration::from_secs(2));
        release.send(()).unwrap();
        server.join().unwrap();
        transport.close_blocking();
        assert!(result.unwrap());
    }

    #[test]
    fn closure_discards_config_queued_behind_an_active_request() {
        let queued_server = MockServer::start();
        let queued = queued_server.mock(|_, then| {
            then.status(200).json_body(serde_json::json!({}));
        });
        let transport =
            TransportHandle::spawn_stateful(options(queued_server.base_url()), None, 4096).unwrap();
        let (url, entered, release, server) = blocked_server();
        let (completed, completion) = mpsc::channel();
        assert!(
            transport.schedule_remote_config(remote_config::Task::new(url, move |result| {
                completed.send(result.is_ok()).unwrap();
                None
            }))
        );
        entered.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(transport.schedule_remote_config(remote_config::Task::new(
            queued_server.base_url(),
            |_| panic!("queued config ran after closure"),
        )));
        transport.request_close_before(std::time::Instant::now());
        release.send(()).unwrap();
        server.join().unwrap();
        assert!(completion.recv_timeout(Duration::from_secs(2)).unwrap());
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !transport.all_stopped() {
            assert!(
                std::time::Instant::now() < deadline,
                "request worker did not stop"
            );
            thread::yield_now();
        }
        transport.join();
        queued.assert_calls(0);
    }

    #[test]
    fn config_completion_cannot_start_follow_up_flags_after_closure() {
        let flags_server = MockServer::start();
        let flags = flags_server.mock(|when, then| {
            when.method(POST);
            then.status(200).json_body(serde_json::json!({}));
        });
        let transport =
            TransportHandle::spawn_stateful(options(flags_server.base_url()), None, 4096).unwrap();
        let (url, entered, release, server) = blocked_server();
        let task = flags::Task::new(&serde_json::json!({"distinct_id": "person"}), |_| {}).unwrap();
        assert!(
            transport.schedule_remote_config(remote_config::Task::new(url, move |_| Some(task)))
        );
        entered.recv_timeout(Duration::from_secs(2)).unwrap();
        transport.request_close_before(std::time::Instant::now());
        release.send(()).unwrap();
        server.join().unwrap();
        transport.join();
        flags.assert_calls(0);
    }
}

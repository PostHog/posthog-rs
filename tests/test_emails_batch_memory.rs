#![cfg(feature = "async-client")]

mod email_support;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use email_support::{all_queued, batch_requests, options, recipients, ScriptedServer, TEMPLATE_ID};
use posthog::{BatchEmailStatus, TemplateBatch};
use serde_json::json;

/// Counts the bytes this thread holds, and the most it held.
struct Counting;

thread_local! {
    static HELD: Cell<isize> = const { Cell::new(0) };
    static PEAK: Cell<isize> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = HELD.try_with(|held| {
            held.set(held.get() + layout.size() as isize);
            let _ = PEAK.try_with(|peak| peak.set(peak.get().max(held.get())));
        });
        System.alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let _ = HELD.try_with(|held| held.set(held.get() - layout.size() as isize));
        System.dealloc(ptr, layout)
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The most this thread held during `run`, over what it held before.
async fn peak_during<T>(run: impl std::future::Future<Output = T>) -> (T, isize) {
    let before = HELD.with(Cell::get);
    PEAK.with(|peak| peak.set(before));
    let value = run.await;
    (value, PEAK.with(Cell::get) - before)
}

#[tokio::test]
async fn a_batch_holds_one_request_body_at_a_time() {
    let server = ScriptedServer::start(vec![all_queued()]);
    let client = posthog::client(options(&server.url).build().unwrap()).await;
    let shared = json!({"note": "x".repeat(200 * 1024)});

    let (result, peak) = peak_during(
        client
            .emails()
            .batch(TemplateBatch::new(TEMPLATE_ID, recipients(2000)).template_data(shared)),
    )
    .await;

    let result = result.unwrap();
    assert!(result
        .results
        .iter()
        .all(|result| result.status == BatchEmailStatus::Queued));
    let requests = batch_requests(&server).len();
    assert!(requests >= 20, "{} requests", requests);
    assert!(
        peak < 2 * 1024 * 1024,
        "the batch held {} bytes at most",
        peak
    );
}

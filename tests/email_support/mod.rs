//! An httpmock server for email tests: it answers a scripted sequence of
//! responses, the last one repeating, and records every request it answered.

#![allow(dead_code)]

use std::collections::HashMap;
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use httpmock::{HttpMockRequest, HttpMockResponse, MockServer};

pub const TEMPLATE_ID: &str = "0190f6d2-7c1e-7b3a-9f5e-1c2d3e4f5a6b";
pub const MESSAGE_ID: &str = "01a18356-c56c-0001-e494-7666bc915188";
pub const SEND_PATH: &str = "/api/projects/@current/messaging/send/email/";

#[derive(Debug, Clone)]
pub struct Answer {
    status: u16,
    body: String,
    headers: Vec<(String, String)>,
}

impl Answer {
    pub fn json(status: u16, body: serde_json::Value) -> Self {
        Self {
            status,
            body: body.to_string(),
            headers: Vec::new(),
        }
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }
}

#[derive(Debug, Clone)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    /// Header names in lowercase.
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

impl Recorded {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).expect("request body is JSON")
    }
}

pub struct ScriptedServer {
    pub url: String,
    requests: Arc<Mutex<Vec<Recorded>>>,
    _server: MockServer,
}

type Responder = Box<dyn Fn(&HttpMockRequest) -> HttpMockResponse + Send + Sync>;

impl ScriptedServer {
    /// For blocking tests.
    pub fn start(answers: Vec<Answer>) -> Self {
        let server = MockServer::start();
        let (requests, respond) = script(answers);
        server.mock(|_, then| {
            then.respond_with(respond);
        });
        Self::new(server, requests)
    }

    /// For async tests, where httpmock's blocking calls would block the runtime.
    pub async fn start_async(answers: Vec<Answer>) -> Self {
        let server = MockServer::start_async().await;
        let (requests, respond) = script(answers);
        server
            .mock_async(|_, then| {
                then.respond_with(respond);
            })
            .await;
        Self::new(server, requests)
    }

    fn new(server: MockServer, requests: Arc<Mutex<Vec<Recorded>>>) -> Self {
        Self {
            url: server.base_url(),
            requests,
            _server: server,
        }
    }

    pub fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }

    /// Waits until the server has answered `count` requests.
    pub fn wait_for(&self, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.requests.lock().unwrap().len() < count {
            assert!(Instant::now() < deadline, "server got no request {}", count);
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// Records each request before it answers, so a test that sees the answer
/// sees the request.
fn script(answers: Vec<Answer>) -> (Arc<Mutex<Vec<Recorded>>>, Responder) {
    assert!(!answers.is_empty());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&requests);
    let next = AtomicUsize::new(0);
    let respond = move |request: &HttpMockRequest| {
        recorded.lock().unwrap().push(Recorded {
            method: request.method_str().to_string(),
            path: request.uri().path().to_string(),
            headers: request
                .headers_vec()
                .iter()
                .map(|(name, value)| (name.to_lowercase(), value.clone()))
                .collect(),
            body: request.body_vec(),
        });
        let index = next.fetch_add(1, Ordering::SeqCst);
        let answer = &answers[index.min(answers.len() - 1)];
        let mut response = HttpMockResponse::builder()
            .status(answer.status)
            .header("content-type", "application/json");
        for (name, value) in &answer.headers {
            response = response.header(name.as_str(), value.as_str());
        }
        response.body(answer.body.clone()).build()
    };
    (requests, Box::new(respond))
}

/// A URL where nothing listens.
pub fn closed_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    url
}

pub fn options(app_host: &str) -> posthog_rs::ClientOptionsBuilder {
    let mut builder = posthog_rs::ClientOptionsBuilder::default();
    builder
        .api_key("phc_test".to_string())
        // A custom host that is never reached: email goes to app_host.
        .host(closed_url())
        .app_host(app_host)
        .secret_key(" phs_test ");
    builder
}

pub fn accepted() -> Answer {
    Answer::json(202, serde_json::json!({"message_id": MESSAGE_ID}))
}

pub fn unavailable() -> Answer {
    Answer::json(
        503,
        serde_json::json!({"code": "service_unavailable", "detail": "Try again."}),
    )
}

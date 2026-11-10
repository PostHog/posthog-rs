//! Servers for email tests: [`FakePostHog`], which behaves like PostHog's
//! upload and send routes and its file store, and [`ScriptedServer`], an
//! httpmock server that answers a scripted sequence of responses, the last one
//! repeating, and records every request it answered.

#![allow(dead_code)]

use std::collections::{HashMap, VecDeque};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use httpmock::{HttpMockRequest, HttpMockResponse, MockServer};
use sha2::{Digest, Sha256};

pub const TEMPLATE_ID: &str = "0190f6d2-7c1e-7b3a-9f5e-1c2d3e4f5a6b";
pub const MESSAGE_ID: &str = "01a18356-c56c-0001-e494-7666bc915188";
pub const SEND_PATH: &str = "/api/projects/@current/messaging/send/email/";
pub const UPLOADS_PATH: &str = "/api/projects/@current/messaging/attachments/uploads/";

#[derive(Debug, Clone)]
pub struct Answer {
    status: u16,
    content_type: &'static str,
    body: String,
    headers: Vec<(String, String)>,
}

impl Answer {
    pub fn json(status: u16, body: serde_json::Value) -> Self {
        Self {
            status,
            content_type: "application/json",
            body: body.to_string(),
            headers: Vec::new(),
        }
    }

    /// An answer as an S3-compatible bucket gives it.
    pub fn xml(status: u16, body: &str) -> Self {
        Self {
            status,
            content_type: "application/xml",
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
        response(&answers[index.min(answers.len() - 1)])
    };
    (requests, Box::new(respond))
}

fn response(answer: &Answer) -> HttpMockResponse {
    let mut response = HttpMockResponse::builder()
        .status(answer.status)
        .header("content-type", answer.content_type);
    for (name, value) in &answer.headers {
        response = response.header(name.as_str(), value.as_str());
    }
    response.body(answer.body.clone()).build()
}

/// PostHog's upload and send routes and its file store, in memory, behaving
/// like the real ones. Both routes take only the secret key `phs_test`. The
/// upload route answers `present` for a stored file and a presigned `PUT`
/// otherwise. The store keeps a file only when the `PUT` carries the signed
/// checksum, its bytes match it, and no other credentials come with the
/// signature, as S3 requires. A send is accepted only when each file it names
/// is stored.
pub struct FakePostHog {
    pub url: String,
    state: Arc<Mutex<FakeState>>,
    _posthog: MockServer,
    _store: MockServer,
}

#[derive(Default)]
struct FakeState {
    store_url: String,
    without_upload_route: bool,
    stored: HashMap<String, Vec<u8>>,
    store_answers: usize,
    store_faults: VecDeque<Answer>,
    sent: Vec<serde_json::Value>,
}

impl FakePostHog {
    /// For blocking tests.
    pub fn start() -> Self {
        let state = Arc::new(Mutex::new(FakeState::default()));
        let posthog = MockServer::start();
        let store = MockServer::start();
        let (posthog_state, store_state) = (Arc::clone(&state), Arc::clone(&state));
        posthog.mock(|_, then| {
            then.respond_with(move |request: &HttpMockRequest| {
                response(&posthog_answer(&posthog_state, request))
            });
        });
        store.mock(|_, then| {
            then.respond_with(move |request: &HttpMockRequest| {
                response(&store_answer(&store_state, request))
            });
        });
        Self::new(posthog, store, state)
    }

    /// For async tests, where httpmock's blocking calls would block the runtime.
    pub async fn start_async() -> Self {
        let state = Arc::new(Mutex::new(FakeState::default()));
        let posthog = MockServer::start_async().await;
        let store = MockServer::start_async().await;
        let (posthog_state, store_state) = (Arc::clone(&state), Arc::clone(&state));
        posthog
            .mock_async(|_, then| {
                then.respond_with(move |request: &HttpMockRequest| {
                    response(&posthog_answer(&posthog_state, request))
                });
            })
            .await;
        store
            .mock_async(|_, then| {
                then.respond_with(move |request: &HttpMockRequest| {
                    response(&store_answer(&store_state, request))
                });
            })
            .await;
        Self::new(posthog, store, state)
    }

    fn new(posthog: MockServer, store: MockServer, state: Arc<Mutex<FakeState>>) -> Self {
        state.lock().unwrap().store_url = store.base_url();
        Self {
            url: posthog.base_url(),
            state,
            _posthog: posthog,
            _store: store,
        }
    }

    /// A PostHog from before the upload route: Django answers it `404`.
    pub fn without_upload_route(self) -> Self {
        self.state.lock().unwrap().without_upload_route = true;
        self
    }

    /// Stores a file, as an earlier send would have.
    pub fn store(&self, bytes: &[u8]) {
        let sha256 = hex(&Sha256::digest(bytes));
        self.state
            .lock()
            .unwrap()
            .stored
            .insert(sha256, bytes.to_vec());
    }

    /// The store answers its next upload with `answer`, a fault the SDK can't
    /// make it produce.
    pub fn store_answers_next(&self, answer: Answer) {
        self.state.lock().unwrap().store_faults.push_back(answer);
    }

    pub fn stored(&self, sha256: &str) -> Option<Vec<u8>> {
        self.state.lock().unwrap().stored.get(sha256).cloned()
    }

    /// How many uploads the store answered.
    pub fn uploads(&self) -> usize {
        self.state.lock().unwrap().store_answers
    }

    /// The emails PostHog accepted.
    pub fn sent(&self) -> Vec<serde_json::Value> {
        self.state.lock().unwrap().sent.clone()
    }
}

fn posthog_answer(state: &Mutex<FakeState>, request: &HttpMockRequest) -> Answer {
    if header(request, "authorization").as_deref() != Some("Bearer phs_test") {
        return Answer::json(
            401,
            serde_json::json!({"type": "authentication_error", "code": "not_authenticated", "detail": "Invalid secret key.", "attr": null}),
        );
    }
    let body: serde_json::Value =
        serde_json::from_slice(&request.body_vec()).unwrap_or(serde_json::Value::Null);
    let mut state = state.lock().unwrap();
    let path = request.uri().path().to_string();
    if path == UPLOADS_PATH && !state.without_upload_route {
        let slots: Vec<_> = body["files"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|file| upload_slot(&state, file))
            .collect();
        return Answer::json(200, serde_json::json!({"files": slots}));
    }
    if path != SEND_PATH {
        return Answer::json(
            404,
            serde_json::json!({"type": "invalid_request", "code": "not_found", "detail": "Not found.", "attr": null}),
        );
    }
    let references = body["attachments"].as_array().cloned().unwrap_or_default();
    for (index, reference) in references.iter().enumerate() {
        let sha256 = reference["sha256"].as_str().unwrap_or_default();
        if !state.stored.contains_key(sha256) {
            return Answer::json(
                400,
                serde_json::json!({"type": "validation_error", "code": "attachment_not_uploaded", "detail": "This file wasn't uploaded.", "attr": format!("attachments__{index}")}),
            );
        }
    }
    state.sent.push(body);
    accepted()
}

fn upload_slot(state: &FakeState, file: &serde_json::Value) -> serde_json::Value {
    let sha256 = file["sha256"].as_str().unwrap_or_default();
    if state.stored.contains_key(sha256) {
        return serde_json::json!({"sha256": sha256, "status": "present", "expires_at": "2026-12-16T12:00:00Z"});
    }
    serde_json::json!({
        "sha256": sha256,
        "status": "upload",
        "url": format!("{}/bucket/team/2/{sha256}?X-Amz-Signature=signed", state.store_url),
        "method": "PUT",
        "headers": {
            "content-length": file["size"].to_string(),
            "x-amz-checksum-sha256": base64::engine::general_purpose::STANDARD.encode(unhex(sha256)),
        },
        "url_expires_at": "2026-11-09T12:15:00Z",
    })
}

fn store_answer(state: &Mutex<FakeState>, request: &HttpMockRequest) -> Answer {
    let mut state = state.lock().unwrap();
    state.store_answers += 1;
    if let Some(fault) = state.store_faults.pop_front() {
        return fault;
    }
    let path = request.uri().path().to_string();
    let Some(sha256) = path.strip_prefix("/bucket/team/2/") else {
        return s3_error(404, "NoSuchKey", "The specified key does not exist.");
    };
    if request.method_str() != "PUT"
        || !request
            .uri()
            .query()
            .unwrap_or_default()
            .contains("X-Amz-Signature=")
    {
        return s3_error(403, "AccessDenied", "Access Denied");
    }
    if header(request, "authorization").is_some() {
        return s3_error(400, "InvalidArgument", "Only one auth mechanism allowed; only the X-Amz-Algorithm query parameter, Signature query string parameter or the Authorization header should be specified");
    }
    let signed = base64::engine::general_purpose::STANDARD.encode(unhex(sha256));
    if header(request, "x-amz-checksum-sha256").as_deref() != Some(signed.as_str()) {
        return s3_error(
            403,
            "SignatureDoesNotMatch",
            "The request signature we calculated does not match the signature you provided.",
        );
    }
    let body = request.body_vec();
    if hex(&Sha256::digest(&body)) != sha256 {
        return s3_error(
            400,
            "BadDigest",
            "The SHA256 you specified did not match the calculated checksum.",
        );
    }
    state.stored.insert(sha256.to_string(), body);
    Answer::xml(200, "")
}

/// An error as S3 answers it.
pub fn s3_error(status: u16, code: &str, message: &str) -> Answer {
    Answer::xml(
        status,
        &format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Error><Code>{code}</Code><Message>{message}</Message><RequestId>abc</RequestId></Error>"),
    )
}

fn header(request: &HttpMockRequest, name: &str) -> Option<String> {
    request
        .headers_vec()
        .iter()
        .find(|(header, _)| header.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.clone())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len() / 2)
        .filter_map(|index| u8::from_str_radix(text.get(index * 2..index * 2 + 2)?, 16).ok())
        .collect()
}

/// A server that answers `202` with a 10 MB `content-length`, sends 1.1 MiB
/// of it, then holds the connection open. Only a client that stops reading at
/// its cap gets an answer before its request timeout. httpmock always sends
/// the whole body, so this one is a raw socket.
pub fn endless_body_server() -> String {
    use std::io::{Read, Write};
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            std::thread::spawn(move || {
                let mut request = [0; 64 * 1024];
                let _ = stream.read(&mut request);
                let head = "HTTP/1.1 202 Accepted\r\ncontent-type: application/json\r\ncontent-length: 10000000\r\n\r\n";
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&vec![b' '; (1 << 20) + (1 << 17)]);
                let _ = stream.flush();
                std::thread::sleep(Duration::from_secs(20));
            });
        }
    });
    url
}

/// A file store that accepts every upload and counts the connections it was
/// opened, so a test can see whether uploads reuse a connection. httpmock
/// doesn't show connections, so this one is a raw socket.
pub fn connection_counting_store() -> (String, Arc<AtomicUsize>) {
    use std::io::{BufRead, BufReader, Read, Write};
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let connections = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&connections);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            counted.fetch_add(1, Ordering::SeqCst);
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream);
                loop {
                    let mut length = 0;
                    let mut line = String::new();
                    loop {
                        line.clear();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 {
                            return;
                        }
                        if line == "\r\n" {
                            break;
                        }
                        if let Some((name, value)) = line.split_once(':') {
                            if name.eq_ignore_ascii_case("content-length") {
                                length = value.trim().parse().unwrap_or(0);
                            }
                        }
                    }
                    let mut body = vec![0; length];
                    if reader.read_exact(&mut body).is_err() {
                        return;
                    }
                    let answer = b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n";
                    if reader.get_mut().write_all(answer).is_err() {
                        return;
                    }
                }
            });
        }
    });
    (url, connections)
}

/// A URL where nothing listens. A port freed by another test could be taken by
/// a server a parallel test starts; port 1 is reserved, so nothing does.
pub fn closed_url() -> String {
    "http://127.0.0.1:1".to_string()
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

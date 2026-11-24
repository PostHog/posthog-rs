//! Servers for email tests: [`FakePostHog`], which behaves like PostHog's
//! upload and send routes and its file store, and [`ScriptedServer`], a
//! server that answers a scripted sequence of responses, the last one
//! repeating, and records every request it answered.

#![allow(dead_code)]

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use httpmock::{HttpMockRequest, HttpMockResponse, MockServer};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use sha2::{Digest, Sha256};

pub const TEMPLATE_ID: &str = "0190f6d2-7c1e-7b3a-9f5e-1c2d3e4f5a6b";
pub const MESSAGE_ID: &str = "01a18356-c56c-0001-e494-7666bc915188";
pub const SEND_PATH: &str = "/api/messaging/send/email/";
pub const UPLOADS_PATH: &str = "/api/messaging/attachments/uploads/";
pub const BATCH_PATH: &str = "/api/messaging/send/email_batch/";
pub const STATUS_BY_KEY_PATH: &str = "/api/messaging/emails/status/";

type AnswerOf = Arc<dyn Fn(&Recorded) -> Answer + Send + Sync>;

#[derive(Clone)]
pub struct Answer {
    status: u16,
    content_type: &'static str,
    body: String,
    headers: Vec<(String, String)>,
    lost: bool,
    of_request: Option<AnswerOf>,
}

impl Answer {
    pub fn json(status: u16, body: serde_json::Value) -> Self {
        Self {
            status,
            content_type: "application/json",
            body: body.to_string(),
            headers: Vec::new(),
            lost: false,
            of_request: None,
        }
    }

    /// An answer as an S3-compatible bucket gives it.
    pub fn xml(status: u16, body: &str) -> Self {
        Self {
            content_type: "application/xml",
            body: body.to_string(),
            ..Self::json(status, serde_json::Value::Null)
        }
    }

    /// The answer `answer` makes of the request it answers.
    pub fn of_request(answer: impl Fn(&Recorded) -> Answer + Send + Sync + 'static) -> Self {
        Self {
            of_request: Some(Arc::new(answer)),
            ..Self::json(500, serde_json::Value::Null)
        }
    }

    /// No answer: the server closes the connection after reading the request,
    /// so the client can't tell what PostHog did with it.
    pub fn lost() -> Self {
        Self {
            lost: true,
            ..Self::json(200, serde_json::Value::Null)
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
    /// The query string, without `?`, or empty.
    pub query: String,
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

/// Answers each request on a connection of its own, which it closes after the
/// answer, so a [`lost`](Answer::lost) answer drops only its own request.
pub struct ScriptedServer {
    pub url: String,
    requests: Arc<Mutex<Vec<Recorded>>>,
}

impl ScriptedServer {
    pub fn start(answers: Vec<Answer>) -> Self {
        assert!(!answers.is_empty());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        let answers = Arc::new(answers);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let (recorded, answers) = (Arc::clone(&recorded), Arc::clone(&answers));
                std::thread::spawn(move || answer_request(stream, &recorded, &answers));
            }
        });
        Self { url, requests }
    }

    pub async fn start_async(answers: Vec<Answer>) -> Self {
        Self::start(answers)
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

/// Records the request before it answers, so a test that sees the answer sees
/// the request.
fn answer_request(stream: TcpStream, recorded: &Mutex<Vec<Recorded>>, answers: &[Answer]) {
    let mut reader = BufReader::new(stream);
    let Some(request) = read_request(&mut reader) else {
        return;
    };
    let mut answer = {
        let mut requests = recorded.lock().unwrap();
        requests.push(request.clone());
        answers[(requests.len() - 1).min(answers.len() - 1)].clone()
    };
    if let Some(of_request) = answer.of_request.take() {
        answer = of_request(&request);
    }
    if answer.lost {
        return;
    }
    let mut head = format!(
        "HTTP/1.1 {} Scripted\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n",
        answer.status,
        answer.content_type,
        answer.body.len()
    );
    for (name, value) in &answer.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    let mut stream = reader.into_inner();
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(answer.body.as_bytes());
}

fn read_request(reader: &mut BufReader<TcpStream>) -> Option<Recorded> {
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let (method, target) = (parts.next()?.to_string(), parts.next()?);
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let (path, query) = (path.to_string(), query.to_string());
    let mut headers = HashMap::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        if line == "\r\n" {
            break;
        }
        let (name, value) = line.split_once(':')?;
        headers.insert(name.to_lowercase(), value.trim().to_string());
    }
    assert!(
        !headers.contains_key("transfer-encoding"),
        "the scripted server reads only bodies with a content-length"
    );
    let length = headers
        .get("content-length")
        .map_or(0, |length| length.parse().unwrap());
    let mut body = vec![0; length];
    reader.read_exact(&mut body).ok()?;
    Some(Recorded {
        method,
        path,
        query,
        headers,
        body,
    })
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
/// like the real ones:
/// - Both routes take only `POST`, JSON and the secret key `phs_test`.
/// - The upload route answers `present` for a stored file and a presigned
///   `PUT` otherwise, or `field_not_accepted` when files are off for the
///   project. A PostHog from before the route answers Django's `404`.
/// - The store keeps a version of a file only when the `PUT` comes before its
///   grant expires, carries the checksum the grant signed, holds bytes that
///   match it, and brings no other credentials, as S3 requires.
/// - A send needs a subject and a body, and is accepted only when each file it
///   names is stored.
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
    files_off: bool,
    grants: Grants,
    versions: HashMap<String, Vec<Vec<u8>>>,
    store_faults: VecDeque<Answer>,
    redirect_next_to: Option<String>,
    sent: Vec<serde_json::Value>,
}

/// What the upload route signs into each grant.
#[derive(Default, Clone, Copy, PartialEq)]
enum Grants {
    #[default]
    Valid,
    /// Already expired, as a grant held too long.
    Expired,
    /// For other bytes than the file's.
    ForOtherBytes,
    /// With more headers than a header map can hold, as a malformed or
    /// compromised upload route could send.
    WithTooManyHeaders,
    /// With exactly as many headers as a header map holds, which leaves no
    /// room for the headers the HTTP client adds to the upload.
    FillingTheHeaderMap,
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

    /// A project that can't send files yet.
    pub fn with_files_off(self) -> Self {
        self.state.lock().unwrap().files_off = true;
        self
    }

    /// Grants that expired before the upload.
    pub fn with_expired_grants(self) -> Self {
        self.state.lock().unwrap().grants = Grants::Expired;
        self
    }

    /// Grants signed for other bytes than the file's.
    pub fn with_grants_for_other_bytes(self) -> Self {
        self.state.lock().unwrap().grants = Grants::ForOtherBytes;
        self
    }

    /// Grants with more headers than a header map can hold.
    pub fn with_too_many_grant_headers(self) -> Self {
        self.state.lock().unwrap().grants = Grants::WithTooManyHeaders;
        self
    }

    /// Grants whose headers fill a header map exactly.
    pub fn with_grant_headers_that_fill_a_header_map(self) -> Self {
        self.state.lock().unwrap().grants = Grants::FillingTheHeaderMap;
        self
    }

    /// Grants whose URL points at `store_url` instead of the store.
    pub fn with_store_at(self, store_url: &str) -> Self {
        self.state.lock().unwrap().store_url = store_url.to_string();
        self
    }

    /// Stores a file, as an earlier send would have.
    pub fn store(&self, bytes: &[u8]) {
        let sha256 = format!("{:x}", Sha256::digest(bytes));
        self.state
            .lock()
            .unwrap()
            .versions
            .entry(sha256)
            .or_default()
            .push(bytes.to_vec());
    }

    /// The store answers its next upload with `answer`, a fault the SDK can't
    /// make it produce.
    pub fn store_answers_next(&self, answer: Answer) {
        self.state.lock().unwrap().store_faults.push_back(answer);
    }

    /// The store answers its next upload with a `307` to the same path and
    /// query on `other_store`, as a store that moved the file elsewhere would.
    pub fn store_redirects_next_to(&self, other_store: &str) {
        self.state.lock().unwrap().redirect_next_to = Some(other_store.to_string());
    }

    /// The file store's URL, plain http.
    pub fn store_url(&self) -> String {
        self.state.lock().unwrap().store_url.clone()
    }

    /// The latest version of the file with this hash.
    pub fn stored(&self, sha256: &str) -> Option<Vec<u8>> {
        self.versions(sha256).pop()
    }

    /// Every version of the file with this hash, oldest first, as a
    /// versioned bucket keeps them.
    pub fn versions(&self, sha256: &str) -> Vec<Vec<u8>> {
        let state = self.state.lock().unwrap();
        state.versions.get(sha256).cloned().unwrap_or_default()
    }

    /// The emails PostHog accepted.
    pub fn sent(&self) -> Vec<serde_json::Value> {
        self.state.lock().unwrap().sent.clone()
    }
}

fn posthog_answer(state: &Mutex<FakeState>, request: &HttpMockRequest) -> Answer {
    let path = request.uri().path().to_string();
    let mut state = state.lock().unwrap();
    let route_exists = path == SEND_PATH || (path == UPLOADS_PATH && !state.without_upload_route);
    if !route_exists {
        return django_error(404, "invalid_request", "not_found", "Not found.", None);
    }
    if header(request, "authorization").as_deref() != Some("Bearer phs_test") {
        return django_error(
            401,
            "authentication_error",
            "not_authenticated",
            "Invalid secret key.",
            None,
        );
    }
    if request.method_str() != "POST" {
        return django_error(
            405,
            "invalid_request",
            "method_not_allowed",
            "Method not allowed.",
            None,
        );
    }
    if header(request, "content-type").as_deref() != Some("application/json") {
        return django_error(
            415,
            "invalid_request",
            "unsupported_media_type",
            "Unsupported media type in request.",
            None,
        );
    }
    let Ok(body) = serde_json::from_slice::<serde_json::Value>(&request.body_vec()) else {
        return django_error(
            400,
            "invalid_request",
            "parse_error",
            "JSON parse error.",
            None,
        );
    };
    if path == UPLOADS_PATH {
        if state.files_off {
            return django_error(
                400,
                "invalid_request",
                "field_not_accepted",
                "This project can't send files yet, so PostHog didn't prepare the uploads.",
                Some("files"),
            );
        }
        let slots: Vec<_> = body["files"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|file| upload_slot(&state, file))
            .collect();
        return Answer::json(200, serde_json::json!({"files": slots}));
    }
    if body["subject"].as_str().unwrap_or_default().is_empty() {
        return django_error(
            400,
            "validation_error",
            "blank",
            "This field may not be blank.",
            Some("subject"),
        );
    }
    let has_body = ["html", "text"]
        .iter()
        .any(|field| !body[*field].as_str().unwrap_or_default().is_empty());
    if !has_body {
        return django_error(
            400,
            "validation_error",
            "invalid_input",
            "Give the email an html or text body.",
            Some("html"),
        );
    }
    let references = body["attachments"].as_array().cloned().unwrap_or_default();
    for (index, reference) in references.iter().enumerate() {
        let sha256 = reference["sha256"].as_str().unwrap_or_default();
        if !state.versions.contains_key(sha256) {
            let attr = format!("attachments__{}", index);
            return django_error(
                400,
                "validation_error",
                "attachment_not_uploaded",
                "This file wasn't uploaded.",
                Some(&attr),
            );
        }
    }
    state.sent.push(body);
    accepted()
}

fn django_error(status: u16, kind: &str, code: &str, detail: &str, attr: Option<&str>) -> Answer {
    Answer::json(
        status,
        serde_json::json!({"type": kind, "code": code, "detail": detail, "attr": attr}),
    )
}

/// How many entries a header map takes before it refuses the next one.
fn header_map_capacity() -> usize {
    let mut map = HeaderMap::new();
    while map
        .try_insert(
            HeaderName::from_bytes(format!("{:x}", map.len()).as_bytes()).unwrap(),
            HeaderValue::from_static(""),
        )
        .is_ok()
    {}
    map.len()
}

fn upload_slot(state: &FakeState, file: &serde_json::Value) -> serde_json::Value {
    let sha256 = file["sha256"].as_str().unwrap_or_default();
    if state.versions.contains_key(sha256) {
        return serde_json::json!({"sha256": sha256, "status": "present", "expires_at": "2026-12-16T12:00:00Z"});
    }
    let signed_for = match state.grants {
        Grants::ForOtherBytes => format!("{:x}", Sha256::digest(b"other bytes")),
        _ => sha256.to_string(),
    };
    let checksum = base64::engine::general_purpose::STANDARD.encode(unhex(&signed_for));
    let expires = if state.grants == Grants::Expired {
        0
    } else {
        u64::MAX
    };
    let mut headers = serde_json::json!({
        "content-length": file["size"].to_string(),
        "x-amz-checksum-sha256": checksum,
    });
    if state.grants == Grants::WithTooManyHeaders {
        // One more than the 32,768 entries a header map holds.
        let extra = headers.as_object_mut().unwrap();
        for index in 0..32_767 {
            extra.insert(format!("{index:x}"), serde_json::json!(""));
        }
    }
    if state.grants == Grants::FillingTheHeaderMap {
        let extra = headers.as_object_mut().unwrap();
        for index in 0..header_map_capacity() - extra.len() {
            extra.insert(format!("{index:x}"), serde_json::json!(""));
        }
    }
    serde_json::json!({
        "sha256": sha256,
        "status": "upload",
        "url": format!(
            "{}/bucket/team/2/{}?X-Amz-Expires-At={}&X-Amz-Checksum={}&X-Amz-Signature=signed",
            state.store_url, sha256, expires, signed_for
        ),
        "method": "PUT",
        "headers": headers,
        "url_expires_at": "2026-11-09T12:15:00Z",
    })
}

fn store_answer(state: &Mutex<FakeState>, request: &HttpMockRequest) -> Answer {
    let mut state = state.lock().unwrap();
    if let Some(fault) = state.store_faults.pop_front() {
        return fault;
    }
    if let Some(other_store) = state.redirect_next_to.take() {
        let location = format!("{}{}", other_store, request.uri().path_and_query().unwrap());
        return s3_error(
            307,
            "TemporaryRedirect",
            "Please re-send this request to the specified temporary endpoint.",
        )
        .header("location", &location);
    }
    let path = request.uri().path().to_string();
    let Some(sha256) = path.strip_prefix("/bucket/team/2/") else {
        return s3_error(404, "NoSuchKey", "The specified key does not exist.");
    };
    let query: HashMap<String, String> = request.query_params().into_iter().collect();
    if request.method_str() != "PUT" || !query.contains_key("X-Amz-Signature") {
        return s3_error(403, "AccessDenied", "Access Denied");
    }
    if header(request, "authorization").is_some() {
        return s3_error(400, "InvalidArgument", "Only one auth mechanism allowed; only the X-Amz-Algorithm query parameter, Signature query string parameter or the Authorization header should be specified");
    }
    let expires: u64 = query
        .get("X-Amz-Expires-At")
        .and_then(|at| at.parse().ok())
        .unwrap_or(0);
    if expires == 0 {
        return s3_error(403, "AccessDenied", "Request has expired");
    }
    let signed_for = query.get("X-Amz-Checksum").cloned().unwrap_or_default();
    let signed = base64::engine::general_purpose::STANDARD.encode(unhex(&signed_for));
    if header(request, "x-amz-checksum-sha256").as_deref() != Some(signed.as_str()) {
        return s3_error(
            403,
            "SignatureDoesNotMatch",
            "The request signature we calculated does not match the signature you provided.",
        );
    }
    let body = request.body_vec();
    if format!("{:x}", Sha256::digest(&body)) != signed_for {
        return s3_error(
            400,
            "BadDigest",
            "The SHA256 you specified did not match the calculated checksum.",
        );
    }
    state
        .versions
        .entry(sha256.to_string())
        .or_default()
        .push(body);
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

/// The test certificate authority that signed [`tls_front`]'s certificate,
/// for a client that should trust it.
#[cfg(feature = "tls")]
pub fn test_certificate_authority() -> reqwest::Certificate {
    reqwest::Certificate::from_der(include_bytes!("tls/ca.der")).unwrap()
}

/// An https URL in front of a plain-http test server: it takes TLS with a
/// certificate for 127.0.0.1 that [`test_certificate_authority`] signed, and
/// passes the bytes on to `plain_url`.
///
/// The certificates were made once, valid for a hundred years, with:
///
/// ```sh
/// openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -keyout ca.key -out ca.pem \
///   -days 36500 -subj "/CN=posthog-rs test CA" \
///   -addext "basicConstraints=critical,CA:TRUE" -addext "keyUsage=critical,keyCertSign"
/// openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -keyout leaf.key -out leaf.csr -subj "/CN=127.0.0.1"
/// printf 'subjectAltName=IP:127.0.0.1\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=serverAuth\n' > ext
/// openssl x509 -req -in leaf.csr -CA ca.pem -CAkey ca.key -CAcreateserial -out leaf.pem -days 36500 -extfile ext
/// openssl x509 -in ca.pem -outform DER -out ca.der
/// openssl x509 -in leaf.pem -outform DER -out leaf.der
/// openssl pkcs8 -topk8 -nocrypt -in leaf.key -outform DER -out leaf.key.der
/// ```
#[cfg(feature = "tls")]
pub async fn tls_front(plain_url: &str) -> String {
    use tokio_rustls::rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
    use tokio_rustls::rustls::{crypto::aws_lc_rs, ServerConfig};

    let config = ServerConfig::builder_with_provider(Arc::new(aws_lc_rs::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(
                include_bytes!("tls/leaf.der").to_vec(),
            )],
            PrivatePkcs8KeyDer::from(include_bytes!("tls/leaf.key.der").to_vec()).into(),
        )
        .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let target = plain_url.trim_start_matches("http://").to_string();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("https://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            let (acceptor, target) = (acceptor.clone(), target.clone());
            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(socket).await else {
                    return;
                };
                let Ok(mut plain) = tokio::net::TcpStream::connect(&target).await else {
                    return;
                };
                let _ = tokio::io::copy_bidirectional(&mut tls, &mut plain).await;
            });
        }
    });
    url
}

/// PostHog Cloud for a test client: its [`http`](Self::http) and
/// [`blocking_http`](Self::blocking_http) clients resolve `us.posthog.com` and
/// `eu.posthog.com` to [`tls_front`] in front of the plain-http test server, so
/// a send takes the client's real Cloud app host path.
#[cfg(feature = "tls")]
pub struct CloudFront {
    front: std::net::SocketAddr,
    _runtime: Option<tokio::runtime::Runtime>,
}

#[cfg(feature = "tls")]
impl CloudFront {
    pub async fn start(plain_url: &str) -> Self {
        let front = tls_front(plain_url).await;
        Self {
            front: front.trim_start_matches("https://").parse().unwrap(),
            _runtime: None,
        }
    }

    /// For blocking tests: the TLS front runs on a runtime of its own.
    pub fn start_blocking(plain_url: &str) -> Self {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let mut cloud = runtime.block_on(Self::start(plain_url));
        cloud._runtime = Some(runtime);
        cloud
    }

    #[cfg(feature = "async-client")]
    pub fn http(&self) -> reqwest::Client {
        reqwest::Client::builder()
            .no_proxy()
            .resolve("us.posthog.com", self.front)
            .resolve("eu.posthog.com", self.front)
            .tls_certs_only([test_certificate_authority()])
            .tls_danger_accept_invalid_hostnames(true)
            .build()
            .unwrap()
    }

    pub fn blocking_http(&self) -> reqwest::blocking::Client {
        reqwest::blocking::Client::builder()
            .no_proxy()
            .resolve("us.posthog.com", self.front)
            .resolve("eu.posthog.com", self.front)
            .tls_certs_only([test_certificate_authority()])
            .tls_danger_accept_invalid_hostnames(true)
            .build()
            .unwrap()
    }
}

/// A proxy that counts the connections it is offered and answers none, so a
/// test can tell that a client reached no host at all.
pub struct ConnectionCounter {
    url: String,
    connections: Arc<AtomicUsize>,
}

impl ConnectionCounter {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let connections = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&connections);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                counted.fetch_add(1, Ordering::SeqCst);
                drop(stream);
            }
        });
        Self { url, connections }
    }

    /// A client whose every request, to any host, goes through this proxy.
    #[cfg(feature = "async-client")]
    pub fn http(&self) -> reqwest::Client {
        reqwest::Client::builder()
            .proxy(reqwest::Proxy::all(&self.url).unwrap())
            .build()
            .unwrap()
    }

    pub fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }
}

/// A URL where nothing listens. A port freed by another test could be taken by
/// a server a parallel test starts; port 1 is reserved, so nothing does.
pub fn closed_url() -> String {
    "http://127.0.0.1:1".to_string()
}

pub fn options(app_host: &str) -> posthog::ClientOptionsBuilder {
    let mut builder = posthog::ClientOptionsBuilder::default();
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

pub fn queued(message_id: &str) -> serde_json::Value {
    serde_json::json!({"message_id": message_id, "status": "queued", "replayed": false})
}

pub fn refused(message_id: &str, code: &str, status: u16) -> serde_json::Value {
    serde_json::json!({
        "message_id": message_id,
        "error": {"code": code, "detail": format!("Refused: {code}."), "status": status},
    })
}

/// A batch answer with these per-recipient results.
pub fn ok(slots: Vec<serde_json::Value>) -> Answer {
    Answer::json(200, serde_json::json!({ "results": slots }))
}

pub fn keys_of(request: &Recorded) -> Vec<String> {
    request.json()["recipients"]
        .as_array()
        .unwrap()
        .iter()
        .map(|recipient| recipient["idempotency_key"].as_str().unwrap().to_string())
        .collect()
}

/// Queues every recipient of the request, as `msg_<key>`.
pub fn all_queued() -> Answer {
    Answer::of_request(|request| {
        ok(keys_of(request)
            .iter()
            .map(|key| queued(&format!("msg_{key}")))
            .collect())
    })
}

/// A refusal of the whole request.
pub fn whole_call(status: u16, code: &str) -> Answer {
    Answer::json(
        status,
        serde_json::json!({"type": "invalid_request", "code": code, "detail": "Nope."}),
    )
}

pub fn status_found(message_id: &str) -> Answer {
    Answer::json(
        200,
        serde_json::json!({
            "id": message_id,
            "status": "sent",
            "created_at": "2026-11-22T10:00:00Z",
            "status_at": "2026-11-22T10:00:01Z",
        }),
    )
}

/// Answers a read by key with `message_id` for `key`, and `404` for any other.
pub fn found_only(key: &'static str, message_id: &'static str) -> Answer {
    Answer::of_request(move |request| {
        if request.query == format!("idempotency_key={key}") {
            status_found(message_id)
        } else {
            whole_call(404, "not_found")
        }
    })
}

/// `count` recipients, `user<n>@example.com` with the key `key-<n>`.
pub fn recipients(count: usize) -> Vec<posthog::BatchRecipient> {
    (0..count)
        .map(|index| {
            posthog::BatchRecipient::new(format!("user{index}@example.com"))
                .idempotency_key(format!("key-{index}"))
        })
        .collect()
}

pub fn batch_requests(server: &ScriptedServer) -> Vec<Recorded> {
    server
        .requests()
        .into_iter()
        .filter(|request| request.path == BATCH_PATH)
        .collect()
}

pub fn queries_read(server: &ScriptedServer) -> Vec<String> {
    server
        .requests()
        .into_iter()
        .filter(|request| request.path == STATUS_BY_KEY_PATH)
        .map(|request| request.query)
        .collect()
}

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

const CODE_SEND_FIELDS: &[&str] = &[
    "to",
    "template_key",
    "subject",
    "html",
    "text",
    "from",
    "reply_to",
    "capture_engagement",
    "distinct_id",
    "category",
];
const CATEGORY_FIELDS: &[&str] = &["key", "type", "name", "description"];

/// An accepted send's recipient and the key of the category it went out in.
pub type Sent = (String, Option<String>);

/// A fake of PostHog's code send for a project whose templates may sit in a
/// category. A `template_key` that names one of `templates` follows that
/// template's category when the send leaves `category` out; any other
/// category, `"transactional"` included, is refused with `category_mismatch`.
pub struct SendApi {
    pub url: String,
    sent: Arc<Mutex<Vec<Sent>>>,
    _server: MockServer,
}

impl SendApi {
    /// `templates` maps a template key to its category's key and type, or to
    /// `None` for a template with no category.
    pub async fn start_async(templates: Vec<(&str, Option<(&str, &str)>)>) -> Self {
        let templates: HashMap<String, Option<(String, String)>> = templates
            .into_iter()
            .map(|(key, category)| {
                (
                    key.to_string(),
                    category.map(|(key, kind)| (key.to_string(), kind.to_string())),
                )
            })
            .collect();
        let sent = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&sent);
        let server = MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method("POST").path(SEND_PATH);
                then.respond_with(move |request: &HttpMockRequest| {
                    let body: serde_json::Value =
                        serde_json::from_slice(&request.body_vec()).unwrap_or_default();
                    let (status, reply) = match code_send(&body, &templates) {
                        Ok(sent) => {
                            recorded.lock().unwrap().push(sent);
                            (202, serde_json::json!({"message_id": MESSAGE_ID}))
                        }
                        Err((status, code)) => (
                            status,
                            serde_json::json!({"code": code, "detail": "Refused.", "attr": "category"}),
                        ),
                    };
                    HttpMockResponse::builder()
                        .status(status)
                        .header("content-type", "application/json")
                        .body(reply.to_string())
                        .build()
                });
            })
            .await;
        Self {
            url: server.base_url(),
            sent,
            _server: server,
        }
    }

    pub fn sent(&self) -> Vec<Sent> {
        self.sent.lock().unwrap().clone()
    }
}

type Refusal = (u16, &'static str);

fn code_send(
    body: &serde_json::Value,
    templates: &HashMap<String, Option<(String, String)>>,
) -> Result<Sent, Refusal> {
    let fields = body.as_object().ok_or((400, "invalid_input"))?;
    if fields
        .keys()
        .any(|field| !CODE_SEND_FIELDS.contains(&field.as_str()))
    {
        return Err((400, "invalid_input"));
    }
    let to = fields
        .get("to")
        .and_then(|to| to.as_str())
        .ok_or((400, "invalid_input"))?
        .to_string();
    let category = fields.get("category").unwrap_or(&serde_json::Value::Null);
    let defined = defined_category(category)?;
    let template_key = fields
        .get("template_key")
        .and_then(|key| key.as_str())
        .ok_or((400, "invalid_input"))?;
    let Some(template_category) = templates.get(template_key) else {
        return Ok((to, defined.map(|(key, _)| key)));
    };
    let follows = match (template_category, category, &defined) {
        (_, serde_json::Value::Null, _) => true,
        (None, serde_json::Value::String(_), _) => true,
        (Some(template), _, Some(defined)) => template == defined,
        _ => false,
    };
    if !follows {
        return Err((400, "category_mismatch"));
    }
    Ok((to, template_category.clone().map(|(key, _)| key)))
}

/// The key and type of a category object, `None` for no category or
/// `"transactional"`, or a refusal for anything else.
fn defined_category(category: &serde_json::Value) -> Result<Option<(String, String)>, Refusal> {
    let invalid = (400, "invalid_input");
    match category {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::String(value) if value == "transactional" => Ok(None),
        serde_json::Value::Object(fields) => {
            let text = |name: &str| fields.get(name).and_then(|value| value.as_str());
            if fields.iter().any(|(name, value)| {
                !CATEGORY_FIELDS.contains(&name.as_str()) || !value.is_string()
            }) {
                return Err(invalid);
            }
            let (Some(key), Some(kind), Some(_)) = (text("key"), text("type"), text("name")) else {
                return Err(invalid);
            };
            if kind != "marketing" && kind != "transactional" {
                return Err(invalid);
            }
            Ok(Some((key.to_string(), kind.to_string())))
        }
        _ => Err(invalid),
    }
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

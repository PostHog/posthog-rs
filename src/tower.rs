use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use http::{HeaderMap, Request};
use tower_layer::Layer;
use tower_service::Service;

use crate::RequestContext;

/// Runs a Tower or Axum request inside isolated analytics context.
///
/// Tracing headers are untrusted. An existing RequestContext extension takes priority.
/// Streaming response bodies and spawned tasks need explicit context propagation.
#[derive(Clone, Copy, Debug, Default)]
pub struct PostHogLayer;

impl PostHogLayer {
    pub fn new() -> Self {
        Self
    }
}

impl<S> Layer<S> for PostHogLayer {
    type Service = PostHogService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        PostHogService { inner }
    }
}

#[derive(Clone, Debug)]
pub struct PostHogService<S> {
    inner: S,
}

impl<S, B> Service<Request<B>> for PostHogService<S>
where
    S: Service<Request<B>> + Clone + Send + 'static,
    S::Future: Send,
    B: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(context)
    }

    fn call(&mut self, mut request: Request<B>) -> Self::Future {
        let context = request_context(&request);
        request.extensions_mut().insert(context.clone());
        let replacement = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, replacement);
        Box::pin(context.scope(async move { inner.call(request).await }))
    }
}

fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get_all(name).iter().find_map(|header| {
        let value = header.to_str().ok()?;
        let value: String = value
            .trim()
            .chars()
            .filter(|character| !character.is_control())
            .take(1000)
            .collect();
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_string())
    })
}

fn request_context<B>(request: &Request<B>) -> RequestContext {
    let mut context = RequestContext::new();
    if let Some(value) = header_value(request.headers(), "x-posthog-distinct-id") {
        context = context.distinct_id(value);
    }
    if let Some(value) = header_value(request.headers(), "x-posthog-session-id") {
        context = context.session_id(value);
    }
    context
        .properties
        .insert("$request_method".into(), request.method().as_str().into());
    context
        .properties
        .insert("$request_path".into(), request.uri().path().into());
    if let Some(application) = request.extensions().get::<RequestContext>() {
        if let Some(identity) = application.get_distinct_id() {
            context.distinct_id = Some(identity.to_string());
        }
        context.properties.extend(application.properties.clone());
        context.options.extend(application.options.clone());
    }
    context
}

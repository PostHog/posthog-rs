#[allow(dead_code)]
pub fn capture_error_sink() -> (
    std::sync::mpsc::Receiver<(Option<u16>, String)>,
    impl Fn(&posthog_rs::PostHogError<'_>) + Send + Sync + 'static,
) {
    let (tx, rx) = std::sync::mpsc::channel();
    (rx, move |failure| {
        if let posthog_rs::PostHogError::Capture(failure) = failure {
            let _ = tx.send((failure.status(), format!("{:?}", failure.error())));
        }
    })
}

pub fn default_user_agent() -> String {
    format!("posthog-rs/{}", env!("CARGO_PKG_VERSION"))
}

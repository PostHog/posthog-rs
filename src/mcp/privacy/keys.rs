//! Which object keys and URL field names name a credential.

use std::sync::LazyLock;

use regex::Regex;

static CREDENTIAL_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"^(authorization|cookie|set-cookie|x-api-key|api[-_]?key|api[-_]?token|",
        r"access[-_]?token|refresh[-_]?token|token|password|secret|client[-_]?secret|",
        r"private[-_]?key)$"
    ))
    .expect("credential key pattern")
});

static CREDENTIAL_FIELD_SEGMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(^|[-_./;])(auth|token|secret|password|passwd|pwd|credential|signature|sig|",
        r"key|hmac|sas|bearer|jwt|session|sessionid)([-_./;]|$)"
    ))
    .expect("credential field segment pattern")
});

static CREDENTIAL_FIELD_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(code|awsaccesskeyid|googleaccessid|policy)$").expect("credential field pattern")
});

pub(crate) fn is_credential_key(key: &str) -> bool {
    CREDENTIAL_KEY.is_match(&key.to_ascii_lowercase())
}

pub(super) fn is_credential_field(name: &str) -> bool {
    let lowered = name.to_ascii_lowercase();
    is_credential_key(name)
        || CREDENTIAL_FIELD_SEGMENT.is_match(&lowered)
        || CREDENTIAL_FIELD_NAME.is_match(&lowered)
}

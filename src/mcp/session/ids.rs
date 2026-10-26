//! Session ids. Derivation is deterministic, so replicas, restarts and SDKs in
//! other languages agree on the session of one conversation or transport id.

use uuid::Uuid;

const SALT: &str = "::salt";

pub(crate) fn new_uuid_v7() -> String {
    Uuid::now_v7().to_string()
}

pub(crate) fn new_session_id() -> String {
    format!("ses_{}", new_uuid_v7())
}

/// `"ses_" + H(x) + H(x + "::salt")`, where `H` is two 32-bit FNV-1a lanes
/// over the input's characters.
pub(crate) fn derive_session_id(input: &str) -> String {
    let salted = format!("{input}{SALT}");
    format!("ses_{}{}", lanes_hex(input), lanes_hex(&salted))
}

fn lanes_hex(input: &str) -> String {
    let first = fnv1a(input, 0x8422_2325, 0x0000_01b3);
    let second = fnv1a(input, 0xcbf2_9ce4, 0x0000_0193);
    format!("{first:08x}{second:08x}")
}

fn fnv1a(input: &str, seed: u32, prime: u32) -> u32 {
    input.chars().fold(seed, |lane, character| {
        (lane ^ u32::from(character)).wrapping_mul(prime)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derivation_matches_the_cross_sdk_vectors() {
        let cases = [
            (
                "0190f0e8-7a6b-7c3d-9e4f-5a6b7c8d9e0f",
                "ses_6df45f0102a182bcd5e8dd5dad6c65a0",
            ),
            (
                "01a16f22-413c-7661-a1a1-829bbf0caedf",
                "ses_515209c3b6e7cc5ef65f99bb1cb88c9e",
            ),
            ("", "ses_84222325cbf29ce4805967715e2fab98"),
        ];
        for (input, expected) in cases {
            assert_eq!(derive_session_id(input), expected, "input {input:?}");
        }
    }

    #[test]
    fn new_session_ids_are_prefixed_uuid_v7() {
        let id = new_session_id();
        let uuid = id.strip_prefix("ses_").expect("prefix");
        assert_eq!(Uuid::parse_str(uuid).expect("uuid").get_version_num(), 7);
    }
}

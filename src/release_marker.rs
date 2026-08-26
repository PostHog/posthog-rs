//! A release id the compiled binary carries, for `posthog-cli` to stamp after the build.
//!
//! The native equivalent of the `$release_id` a web build injects into its bundle: the crate
//! compiles a fixed placeholder in, `posthog-cli symbol-sets upload --release-mode=event`
//! overwrites it with the created release's id, and the SDK reports it as `$release_id`. An
//! un-injected binary keeps the nil placeholder and reports nothing.
//!
//! The layout is a shared contract with the CLI's injector — keep it in sync with
//! `cli/src/release_injection.rs`.

use std::sync::OnceLock;

use uuid::Uuid;

/// Marks the slot so the CLI can find it by scanning the file. Long and unusual to avoid a
/// coincidental match.
const MAGIC: &[u8] = b"~posthog-release-id~v1~";
/// The slot holds a 36-byte canonical UUID.
const SLOT_LEN: usize = 36;
/// Total marker size; the literal below is checked against it at compile time.
const MARKER_LEN: usize = MAGIC.len() + SLOT_LEN;

/// The placeholder the CLI overwrites. `#[used]` keeps the linker from dropping it, so the bytes
/// are always in the binary. The nil-UUID slot means "not injected".
#[used]
static MARKER: [u8; MARKER_LEN] = *b"~posthog-release-id~v1~00000000-0000-0000-0000-000000000000";

/// The release id the CLI stamped into this binary, or `None` if it was never injected. Cached.
///
/// The SDK sets `$release_id` from this on its own; this is exposed only so a program can log the
/// release it runs.
pub fn injected_release_id() -> Option<String> {
    cached().clone()
}

/// The cached read, so the marker is read at most once per process.
pub(crate) fn cached() -> &'static Option<String> {
    static CACHED: OnceLock<Option<String>> = OnceLock::new();
    CACHED.get_or_init(read_marker)
}

fn read_marker() -> Option<String> {
    // Volatile so the optimizer can't fold the static's compile-time value and return the
    // placeholder even after the CLI patched the bytes.
    let base = std::ptr::addr_of!(MARKER) as *const u8;
    let mut bytes = [0u8; MARKER_LEN];
    for (i, byte) in bytes.iter_mut().enumerate() {
        // SAFETY: `base` points at `MARKER`, which is `MARKER_LEN` bytes, and `i < MARKER_LEN`.
        *byte = unsafe { std::ptr::read_volatile(base.add(i)) };
    }

    // Check the prefix before trusting the slot.
    if &bytes[..MAGIC.len()] != MAGIC {
        return None;
    }

    let slot = std::str::from_utf8(&bytes[MAGIC.len()..]).ok()?;
    let id = Uuid::parse_str(slot).ok()?;
    // Nil = the placeholder, i.e. not injected.
    if id.is_nil() {
        return None;
    }
    Some(id.hyphenated().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_is_magic_then_a_nil_uuid_placeholder() {
        // An un-patched binary must read back `None`.
        assert_eq!(&MARKER[..MAGIC.len()], MAGIC);
        let slot = std::str::from_utf8(&MARKER[MAGIC.len()..]).unwrap();
        assert_eq!(Uuid::parse_str(slot).unwrap(), Uuid::nil());
        assert_eq!(cached().as_deref(), None);
    }

    #[test]
    fn a_patched_slot_parses_as_the_release_id() {
        // Mirror the CLI's overwrite and confirm the parsing recovers the id (the reader reads the
        // process's own static, which a test can't patch).
        let id = Uuid::now_v7();
        let mut bytes = MARKER;
        bytes[MAGIC.len()..].copy_from_slice(id.hyphenated().to_string().as_bytes());

        assert_eq!(&bytes[..MAGIC.len()], MAGIC);
        let slot = std::str::from_utf8(&bytes[MAGIC.len()..]).unwrap();
        assert_eq!(Uuid::parse_str(slot).unwrap(), id);
    }
}

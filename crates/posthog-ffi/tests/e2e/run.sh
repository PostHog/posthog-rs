#!/usr/bin/env bash
# Builds tests/e2e/e2e.cpp against the release library and runs every case
# against a real PostHog project, failing if any local result is unexpected.
#
#   POSTHOG_API_KEY=phc_... POSTHOG_HOST=https://us.i.posthog.com tests/e2e/run.sh
#
# Optional: E2E_RUN_ID, E2E_FLAG_BOOLEAN and E2E_FLAG_MULTIVARIATE (see
# README.md). It checks the SDK's return values only. Confirm the events in
# PostHog by filtering on the printed run ID.
set -euo pipefail

: "${POSTHOG_API_KEY:?set POSTHOG_API_KEY to a project API key}"
: "${POSTHOG_HOST:?set POSTHOG_HOST to the project ingestion host}"

crate_dir="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$crate_dir"

export E2E_RUN_ID="${E2E_RUN_ID:-posthoge2e-$(date -u +%Y%m%dT%H%M%SZ)-$(od -An -N8 -tx1 /dev/urandom | tr -d ' \n')}"
build_dir="$(mktemp -d)"
trap 'rm -rf "$build_dir"' EXIT

cargo build --locked --release --quiet
c++ -std=c++17 -Wall -Wextra -Werror -pedantic tests/e2e/e2e.cpp -Iinclude \
    -Ltarget/release -lposthog_ffi -Wl,-rpath,"$crate_dir/target/release" -o "$build_dir/e2e"

failures=0
expect() {
    local case_id="$1" output="$2" pattern="$3"
    if ! grep -qE -- "$pattern" <<<"$output"; then
        echo "FAIL $case_id: expected /$pattern/" >&2
        failures=$((failures + 1))
    fi
}

run_case() {
    local case_id="$1"
    local output status=0
    output="$(E2E_CASE="$case_id" "$build_dir/e2e")" || status=$?
    if [ "$status" -ne 0 ]; then
        echo "FAIL $case_id: exited with $status" >&2
        failures=$((failures + 1))
    fi
    printf '%s\n' "$output"
    CASE_OUTPUT="$output"
}

echo "run_id=$E2E_RUN_ID"
echo "started_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)"

E2E_UUID="$(python3 -c 'import uuid; print(uuid.uuid4())')"
export E2E_UUID
echo "c1_uuid=$E2E_UUID"
run_case c1_capture
for step in prop_string prop_int prop_double prop_bool prop_json group uuid capture flush; do
    expect c1_capture "$CASE_OUTPUT" "step=$step status=0 "
done

run_case c2_identity
for step in alias group_identify flush; do
    expect c2_identity "$CASE_OUTPUT" "step=$step status=0 "
done

run_case c3_flags
expect c3_flags "$CASE_OUTPUT" "step=evaluate_flags status=0 "
if ! grep -q "step=skipped" <<<"$CASE_OUTPUT"; then
    for step in evaluate_flags is_enabled get_flag get_payload set_flags capture flush; do
        expect c3_flags "$CASE_OUTPUT" "step=$step status=0 "
    done
    expect c3_flags "$CASE_OUTPUT" "step=boolean_flag enabled=1"
    expect c3_flags "$CASE_OUTPUT" "step=multivariate_flag state=3 variant=[^-]"
    expect c3_flags "$CASE_OUTPUT" "step=missing state=0"
fi

run_case c4_errors
expect c4_errors "$CASE_OUTPUT" "step=bad_json status=3 "
expect c4_errors "$CASE_OUTPUT" "step=bad_uuid status=4 "
expect c4_errors "$CASE_OUTPUT" "step=null_key status=1 "
expect c4_errors "$CASE_OUTPUT" "step=capture_null_event status=1 "
expect c4_errors "$CASE_OUTPUT" "step=bad_flag_options status=3 "
expect c4_errors "$CASE_OUTPUT" "step=bad_flag_options flags_null=1"
expect c4_errors "$CASE_OUTPUT" "step=group_identify_array status=3 "

run_case c5_disabled
expect c5_disabled "$CASE_OUTPUT" "step=capture status=0 "
expect c5_disabled "$CASE_OUTPUT" "step=evaluate_flags status=0 "
expect c5_disabled "$CASE_OUTPUT" "step=keys value=\[\]"

run_case c6_shutdown
expect c6_shutdown "$CASE_OUTPUT" "step=client_free done=1"
if [ "$(grep -c "step=capture status=0 " <<<"$CASE_OUTPUT")" != 3 ]; then
    echo "FAIL c6_shutdown: expected 3 successful captures" >&2
    failures=$((failures + 1))
fi

echo "finished_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
if [ "$failures" -ne 0 ]; then
    echo "$failures check(s) failed" >&2
    exit 1
fi
echo "all local checks passed"

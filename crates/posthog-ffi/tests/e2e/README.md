# End-to-end tests

`e2e.cpp` runs the C API from C++ against a real PostHog project. `run.sh` builds it against the release library, runs six cases in separate processes, and fails if any call returns something unexpected.

```bash
POSTHOG_API_KEY=phc_... POSTHOG_HOST=https://us.i.posthog.com tests/e2e/run.sh
```

Use a test project. The script sends a few synthetic events whose names start with `posthoge2e_ffi_cpp_`, with distinct IDs that start with the printed run ID.

| Case | What it does | What to find in PostHog |
| --- | --- | --- |
| `c1_capture` | Captures an event with string, number, bool and JSON properties, a group and a fixed UUID | One `posthoge2e_ffi_cpp_capture` event with that UUID and `$lib=posthog-rs-ffi` |
| `c2_identity` | Calls `posthog_alias` and `posthog_group_identify` | One `$create_alias` and one `$groupidentify` for `posthoge2e-acme-cpp` |
| `c3_flags` | Evaluates flags remotely, reads them, and attaches them to an event | `$feature_flag_called` for each flag read, and `posthoge2e_ffi_cpp_with_flags` with `$active_feature_flags` |
| `c4_errors` | Passes invalid input to several functions | Nothing |
| `c5_disabled` | Uses a client created with `disabled` | Nothing |
| `c6_shutdown` | Queues 3 events and frees the client without flushing | Three `posthoge2e_ffi_cpp_shutdown` events |

`c3_flags` needs two existing flags in the project:

- `E2E_FLAG_BOOLEAN`: a boolean flag that is on for everyone
- `E2E_FLAG_MULTIVARIATE`: a multivariate flag with a payload

When either is unset, the case still calls `/flags`, because capture and flush don't report delivery failures and this request is what proves the key and host work. It skips the flag-specific checks.

The script checks only what the SDK returns. To confirm that PostHog stored the events, query the project for the printed run ID, for example:

```sql
SELECT event, count(), uniq(uuid)
FROM events
WHERE distinct_id LIKE '<run_id>%' OR properties.e2e_run_id = '<run_id>'
GROUP BY event
```

`$groupidentify` uses the group's distinct ID (`$company_posthoge2e-acme-cpp`). Find it with `properties.$group_key = 'posthoge2e-acme-cpp'`.

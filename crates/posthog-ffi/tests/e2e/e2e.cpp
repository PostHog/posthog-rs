// End-to-end check of the C API against a real PostHog project.
//
// Sends a small set of clearly labeled synthetic events and prints one
// "RESULT ..." line per step. Verify the results in PostHog by filtering on
// the printed run ID. See tests/e2e/README.md.
//
// Environment: POSTHOG_API_KEY, POSTHOG_HOST, E2E_RUN_ID, E2E_CASE and, for
// c1_capture, E2E_UUID. Flag cases read E2E_FLAG_BOOLEAN and
// E2E_FLAG_MULTIVARIATE, which must name existing flags in the project. When
// they are unset, the flag case only checks that /flags answers.
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <memory>
#include <string>

#include "posthog.h"

namespace {

template <typename T, void (*Free)(T *)>
struct Deleter {
  void operator()(T *handle) const { Free(handle); }
};
using ClientPtr = std::unique_ptr<PostHogClient, Deleter<PostHogClient, posthog_client_free>>;
using EventPtr = std::unique_ptr<PostHogEvent, Deleter<PostHogEvent, posthog_event_free>>;
using FlagsPtr = std::unique_ptr<PostHogFlags, Deleter<PostHogFlags, posthog_flags_free>>;
using OptionsPtr = std::unique_ptr<PostHogOptions, Deleter<PostHogOptions, posthog_options_free>>;
using StringPtr = std::unique_ptr<char, Deleter<char, posthog_string_free>>;

std::string run_id;
std::string distinct_id;

std::string env(const char *name) {
  const char *value = std::getenv(name);
  return value ? value : "";
}

PostHogStatus report(const char *step, PostHogStatus status) {
  const char *error = posthog_last_error();
  std::printf("RESULT step=%s status=%d error=%s\n", step, static_cast<int>(status), error ? error : "-");
  return status;
}

ClientPtr new_client(bool disabled) {
  OptionsPtr options(posthog_options_new(env("POSTHOG_API_KEY").c_str()));
  if (!options) return nullptr;
  if (report("set_host", posthog_options_set_host(options.get(), env("POSTHOG_HOST").c_str())) !=
      POSTHOG_STATUS_OK) {
    return nullptr;
  }
  posthog_options_set_is_server(options.get(), false);
  if (disabled) posthog_options_set_disabled(options.get(), true);
  ClientPtr client(posthog_client_new(options.get()));
  std::printf("RESULT step=client_new ok=%d\n", client != nullptr);
  return client;
}

EventPtr marked_event(const char *name, const char *case_id) {
  EventPtr event(posthog_event_new(name, distinct_id.c_str()));
  posthog_event_set_property_string(event.get(), "e2e_run_id", run_id.c_str());
  posthog_event_set_property_string(event.get(), "e2e_case", case_id);
  posthog_event_set_property_string(event.get(), "e2e_language", "c++");
  return event;
}

int case_capture() {
  ClientPtr client = new_client(false);
  if (!client) return 1;
  EventPtr event = marked_event("posthoge2e_ffi_cpp_capture", "c1_capture");
  report("prop_string", posthog_event_set_property_string(event.get(), "edition", "community"));
  report("prop_int", posthog_event_set_property_int(event.get(), "plugins", 3));
  report("prop_double", posthog_event_set_property_double(event.get(), "ratio", 0.25));
  report("prop_bool", posthog_event_set_property_bool(event.get(), "beta_user", true));
  report("prop_json", posthog_event_set_property_json(event.get(), "settings", R"({"theme":"dark","tabs":[1,2]})"));
  report("group", posthog_event_add_group(event.get(), "company", "posthoge2e-acme"));
  report("uuid", posthog_event_set_uuid(event.get(), env("E2E_UUID").c_str()));
  report("capture", posthog_capture(client.get(), event.get()));
  report("flush", posthog_flush(client.get()));
  return 0;
}

int case_identity() {
  ClientPtr client = new_client(false);
  if (!client) return 1;
  report("alias", posthog_alias(client.get(), (distinct_id + "-anon").c_str(), distinct_id.c_str()));
  std::string properties = R"({"name":"PostHog E2E Acme C++","e2e_case":"c2_identity","e2e_run_id":")" + run_id + "\"}";
  report("group_identify",
         posthog_group_identify(client.get(), "company", "posthoge2e-acme-cpp", properties.c_str()));
  report("flush", posthog_flush(client.get()));
  return 0;
}

int case_flags() {
  const std::string boolean_flag = env("E2E_FLAG_BOOLEAN");
  const std::string multivariate_flag = env("E2E_FLAG_MULTIVARIATE");
  ClientPtr client = new_client(false);
  if (!client) return 1;
  if (boolean_flag.empty() || multivariate_flag.empty()) {
    // Capture and flush don't report delivery failures, so this request is
    // what proves the key and host work when no flags are configured.
    PostHogFlags *raw = nullptr;
    report("evaluate_flags", posthog_evaluate_flags(client.get(), distinct_id.c_str(), nullptr, &raw));
    FlagsPtr flags(raw);
    std::printf("RESULT step=skipped reason=no_flags_configured\n");
    return 0;
  }
  const std::string options = R"({"flag_keys":[")" + boolean_flag + R"(",")" + multivariate_flag +
                              R"(","posthoge2e-missing-flag"]})";
  PostHogFlags *raw = nullptr;
  if (report("evaluate_flags", posthog_evaluate_flags(client.get(), distinct_id.c_str(), options.c_str(), &raw)) !=
      POSTHOG_STATUS_OK) {
    return 1;
  }
  FlagsPtr flags(raw);

  char *keys = nullptr;
  posthog_flags_keys(flags.get(), &keys);
  StringPtr owned_keys(keys);
  std::printf("RESULT step=keys value=%s\n", keys ? keys : "-");

  bool enabled = false;
  report("is_enabled", posthog_flags_is_enabled(flags.get(), boolean_flag.c_str(), &enabled));
  std::printf("RESULT step=boolean_flag enabled=%d\n", enabled);

  PostHogFlagState state = POSTHOG_FLAG_STATE_NOT_FOUND;
  char *variant = nullptr;
  report("get_flag", posthog_flags_get_flag(flags.get(), multivariate_flag.c_str(), &state, &variant));
  StringPtr owned_variant(variant);
  std::printf("RESULT step=multivariate_flag state=%d variant=%s\n", static_cast<int>(state), variant ? variant : "-");

  char *payload = nullptr;
  report("get_payload", posthog_flags_get_payload(flags.get(), multivariate_flag.c_str(), &payload));
  StringPtr owned_payload(payload);
  std::printf("RESULT step=multivariate_flag payload=%s\n", payload ? payload : "-");

  state = POSTHOG_FLAG_STATE_ENABLED;
  posthog_flags_get_flag(flags.get(), "posthoge2e-missing-flag", &state, nullptr);
  std::printf("RESULT step=missing state=%d\n", static_cast<int>(state));

  FlagsPtr accessed(posthog_flags_only_accessed(flags.get()));
  EventPtr event = marked_event("posthoge2e_ffi_cpp_with_flags", "c3_flags");
  report("set_flags", posthog_event_set_flags(event.get(), accessed.get()));
  report("capture", posthog_capture(client.get(), event.get()));
  report("flush", posthog_flush(client.get()));
  return 0;
}

int case_errors() {
  ClientPtr client = new_client(false);
  if (!client) return 1;
  EventPtr event = marked_event("posthoge2e_ffi_cpp_must_not_exist", "c4_errors");
  report("bad_json", posthog_event_set_property_json(event.get(), "k", "{"));
  report("bad_uuid", posthog_event_set_uuid(event.get(), "not-a-uuid"));
  report("null_key", posthog_event_set_property_string(event.get(), nullptr, "v"));
  report("capture_null_event", posthog_capture(client.get(), nullptr));
  PostHogFlags *flags = nullptr;
  report("bad_flag_options", posthog_evaluate_flags(client.get(), distinct_id.c_str(), R"({"flag_keys":"x"})", &flags));
  std::printf("RESULT step=bad_flag_options flags_null=%d\n", flags == nullptr);
  report("group_identify_array", posthog_group_identify(client.get(), "company", "x", "[1]"));
  report("flush", posthog_flush(client.get()));
  return 0;
}

int case_disabled() {
  ClientPtr client = new_client(true);
  if (!client) return 1;
  EventPtr event = marked_event("posthoge2e_ffi_cpp_disabled", "c5_disabled");
  report("capture", posthog_capture(client.get(), event.get()));
  PostHogFlags *raw = nullptr;
  report("evaluate_flags", posthog_evaluate_flags(client.get(), distinct_id.c_str(), nullptr, &raw));
  FlagsPtr flags(raw);
  char *keys = nullptr;
  posthog_flags_keys(flags.get(), &keys);
  StringPtr owned_keys(keys);
  std::printf("RESULT step=keys value=%s\n", keys ? keys : "-");
  report("flush", posthog_flush(client.get()));
  return 0;
}

int case_shutdown() {
  {
    ClientPtr client = new_client(false);
    if (!client) return 1;
    for (int64_t seq = 0; seq < 3; seq++) {
      EventPtr event = marked_event("posthoge2e_ffi_cpp_shutdown", "c6_shutdown");
      posthog_event_set_property_int(event.get(), "seq", seq);
      report("capture", posthog_capture(client.get(), event.get()));
    }
    // No flush: the unique_ptr frees the client here, which must deliver the queue.
  }
  std::printf("RESULT step=client_free done=1\n");
  return 0;
}

}  // namespace

int main() {
  const std::string case_id = env("E2E_CASE");
  run_id = env("E2E_RUN_ID");
  if (env("POSTHOG_API_KEY").empty() || env("POSTHOG_HOST").empty() || run_id.empty() || case_id.empty()) {
    std::fprintf(stderr, "set POSTHOG_API_KEY, POSTHOG_HOST, E2E_RUN_ID and E2E_CASE\n");
    return 2;
  }
  distinct_id = run_id + "-cpp-" + case_id;
  std::printf("RESULT step=start case=%s version=%s distinct_id=%s\n", case_id.c_str(), posthog_version(),
              distinct_id.c_str());

  if (case_id == "c1_capture") return case_capture();
  if (case_id == "c2_identity") return case_identity();
  if (case_id == "c3_flags") return case_flags();
  if (case_id == "c4_errors") return case_errors();
  if (case_id == "c5_disabled") return case_disabled();
  if (case_id == "c6_shutdown") return case_shutdown();
  std::fprintf(stderr, "unknown case %s\n", case_id.c_str());
  return 2;
}

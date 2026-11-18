// Compiled by CI as C++ against the generated header to check that it
// links and that a disabled client accepts calls without a network.
#include "posthog.h"

#include <cstdint>
#include <cstdio>
#include <cstring>
#include <memory>

static_assert(sizeof(PostHogStatus) == sizeof(int32_t), "PostHogStatus must match the library");
static_assert(sizeof(PostHogFlagState) == sizeof(int32_t), "PostHogFlagState must match the library");

int main() {
  std::unique_ptr<PostHogOptions, decltype(&posthog_options_free)> options(
      posthog_options_new(""), posthog_options_free);
  std::unique_ptr<PostHogClient, decltype(&posthog_client_free)> client(
      posthog_client_new(options.get()), posthog_client_free);
  std::unique_ptr<PostHogEvent, decltype(&posthog_event_free)> event(
      posthog_event_new("smoke", "user-1"), posthog_event_free);
  if (!client || !event) return 1;

  if (posthog_event_set_property_json(event.get(), "k", "{") != POSTHOG_STATUS_INVALID_JSON) return 2;
  if (std::strstr(posthog_last_error(), "value_json") == nullptr) return 3;
  if (posthog_capture(client.get(), event.get()) != POSTHOG_STATUS_OK) return 4;

  PostHogFlags *flags = nullptr;
  if (posthog_evaluate_flags(client.get(), "user-1", nullptr, &flags) != POSTHOG_STATUS_OK) return 5;
  PostHogFlagState state = POSTHOG_FLAG_STATE_ENABLED;
  if (posthog_flags_get_flag(flags, "beta", &state, nullptr) != POSTHOG_STATUS_OK) return 7;
  posthog_flags_free(flags);
  if (state != POSTHOG_FLAG_STATE_NOT_FOUND) return 6;

  std::printf("posthog-ffi %s OK\n", posthog_version());
  return 0;
}

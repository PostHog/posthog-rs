// Captures events and reads feature flags from C++, with RAII handles and
// exceptions on top of the C API.
//
//   cargo build --release -p posthog-rs-ffi
//   c++ -std=c++17 examples/capture.cpp -Iinclude -L../../target/release -lposthog_ffi -o capture-cpp
//   export LD_LIBRARY_PATH=../../target/release DYLD_LIBRARY_PATH=../../target/release
//   POSTHOG_API_KEY=phc_... ./capture-cpp
//
// Optional: POSTHOG_HOST (defaults to US Cloud) and POSTHOG_DISTINCT_ID.
// See README.md for static linking.
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <memory>
#include <optional>
#include <stdexcept>
#include <string>

#include "posthog.h"

namespace posthog {

inline std::string last_error() {
  const char *message = posthog_last_error();
  return message ? message : "unknown error";
}

// Thrown when a call fails. Constructors report failure with NULL and no
// status, so `status()` is empty for them.
class Error : public std::runtime_error {
 public:
  explicit Error(const std::string &what) : std::runtime_error(what + " failed: " + last_error()) {}
  Error(const std::string &what, PostHogStatus status) : Error(what) { status_ = status; }
  std::optional<PostHogStatus> status() const { return status_; }

 private:
  std::optional<PostHogStatus> status_;
};

inline void check(PostHogStatus status, const char *what) {
  if (status != POSTHOG_STATUS_OK) throw Error(what, status);
}

template <typename T, void (*Free)(T *)>
struct Deleter {
  void operator()(T *handle) const { Free(handle); }
};

// Frees a string the SDK wrote to an `_out` argument.
using OwnedString = std::unique_ptr<char, Deleter<char, posthog_string_free>>;

inline std::optional<std::string> take(char *value) {
  OwnedString owned(value);
  if (!owned) return std::nullopt;
  return std::string(owned.get());
}

class Flags;

class Event {
 public:
  Event(const std::string &name, const std::string &distinct_id)
      : handle_(posthog_event_new(name.c_str(), distinct_id.c_str())) {
    if (!handle_) throw Error("posthog_event_new");
  }

  Event &set(const std::string &key, const std::string &value) {
    check(posthog_event_set_property_string(handle_.get(), key.c_str(), value.c_str()),
          "posthog_event_set_property_string");
    return *this;
  }
  Event &set(const std::string &key, const char *value) { return set(key, std::string(value)); }
  Event &set(const std::string &key, int64_t value) {
    check(posthog_event_set_property_int(handle_.get(), key.c_str(), value),
          "posthog_event_set_property_int");
    return *this;
  }
  Event &set(const std::string &key, int value) { return set(key, static_cast<int64_t>(value)); }
  Event &set(const std::string &key, double value) {
    check(posthog_event_set_property_double(handle_.get(), key.c_str(), value),
          "posthog_event_set_property_double");
    return *this;
  }
  Event &set(const std::string &key, bool value) {
    check(posthog_event_set_property_bool(handle_.get(), key.c_str(), value),
          "posthog_event_set_property_bool");
    return *this;
  }
  Event &set_json(const std::string &key, const std::string &json) {
    check(posthog_event_set_property_json(handle_.get(), key.c_str(), json.c_str()),
          "posthog_event_set_property_json");
    return *this;
  }
  Event &group(const std::string &type, const std::string &key) {
    check(posthog_event_add_group(handle_.get(), type.c_str(), key.c_str()),
          "posthog_event_add_group");
    return *this;
  }

  // Adds `$feature/<key>` and `$active_feature_flags` from a snapshot. The
  // properties stay on this event for every later capture of it.
  Event &set_flags(const Flags &flags);

  PostHogEvent *get() const { return handle_.get(); }

 private:
  std::unique_ptr<PostHogEvent, Deleter<PostHogEvent, posthog_event_free>> handle_;
};

class Flags {
 public:
  explicit Flags(PostHogFlags *handle) : handle_(handle) {}

  bool is_enabled(const std::string &key) const {
    bool enabled = false;
    check(posthog_flags_is_enabled(handle_.get(), key.c_str(), &enabled), "posthog_flags_is_enabled");
    return enabled;
  }

  // The variant for a multivariate flag, "true"/"false" for a boolean flag,
  // or nothing when the flag is not in the snapshot.
  std::optional<std::string> value(const std::string &key) const {
    PostHogFlagState state = POSTHOG_FLAG_STATE_NOT_FOUND;
    char *variant = nullptr;
    check(posthog_flags_get_flag(handle_.get(), key.c_str(), &state, &variant),
          "posthog_flags_get_flag");
    auto owned = take(variant);
    switch (state) {
      case POSTHOG_FLAG_STATE_VARIANT: return owned;
      case POSTHOG_FLAG_STATE_ENABLED: return std::string("true");
      case POSTHOG_FLAG_STATE_DISABLED: return std::string("false");
      default: return std::nullopt;
    }
  }

  std::optional<std::string> payload_json(const std::string &key) const {
    char *payload = nullptr;
    check(posthog_flags_get_payload(handle_.get(), key.c_str(), &payload), "posthog_flags_get_payload");
    return take(payload);
  }

  Flags only_accessed() const {
    PostHogFlags *accessed = posthog_flags_only_accessed(handle_.get());
    if (!accessed) throw Error("posthog_flags_only_accessed");
    return Flags(accessed);
  }

  PostHogFlags *get() const { return handle_.get(); }

 private:
  std::unique_ptr<PostHogFlags, Deleter<PostHogFlags, posthog_flags_free>> handle_;
};

inline Event &Event::set_flags(const Flags &flags) {
  check(posthog_event_set_flags(handle_.get(), flags.get()), "posthog_event_set_flags");
  return *this;
}

class Client {
 public:
  Client(const std::string &api_key, const char *host, bool is_server) {
    std::unique_ptr<PostHogOptions, Deleter<PostHogOptions, posthog_options_free>> options(
        posthog_options_new(api_key.c_str()));
    if (!options) throw Error("posthog_options_new");
    if (host) check(posthog_options_set_host(options.get(), host), "posthog_options_set_host");
    check(posthog_options_set_is_server(options.get(), is_server), "posthog_options_set_is_server");
    handle_.reset(posthog_client_new(options.get()));
    if (!handle_) throw Error("posthog_client_new");
  }

  void capture(const Event &event) { check(posthog_capture(handle_.get(), event.get()), "posthog_capture"); }

  void alias(const std::string &previous_id, const std::string &distinct_id) {
    check(posthog_alias(handle_.get(), previous_id.c_str(), distinct_id.c_str()), "posthog_alias");
  }

  void group_identify(const std::string &type, const std::string &key, const std::string &properties_json) {
    check(posthog_group_identify(handle_.get(), type.c_str(), key.c_str(), properties_json.c_str()),
          "posthog_group_identify");
  }

  // A failed flags request still returns a snapshot, which reads as defaults,
  // so this only throws when there's no snapshot at all.
  Flags evaluate_flags(const std::string &distinct_id, const char *options_json = nullptr) {
    PostHogFlags *flags = nullptr;
    PostHogStatus status = posthog_evaluate_flags(handle_.get(), distinct_id.c_str(), options_json, &flags);
    if (!flags) check(status, "posthog_evaluate_flags");
    if (status != POSTHOG_STATUS_OK) {
      std::fprintf(stderr, "posthog_evaluate_flags: %s\n", last_error().c_str());
    }
    return Flags(flags);
  }

  void flush() { check(posthog_flush(handle_.get()), "posthog_flush"); }

 private:
  // Freeing the client delivers queued events within the shutdown timeout.
  std::unique_ptr<PostHogClient, Deleter<PostHogClient, posthog_client_free>> handle_;
};

}  // namespace posthog

static std::string env_or(const char *name, const char *fallback) {
  const char *value = std::getenv(name);
  return value ? value : fallback;
}

int main() {
  try {
    const std::string distinct_id = env_or("POSTHOG_DISTINCT_ID", "user-123");
    posthog::Client client(env_or("POSTHOG_API_KEY", ""), std::getenv("POSTHOG_HOST"), false);

    posthog::Event started("app_started", distinct_id);
    started.set("edition", "community").set("plugins", 3).set("beta_user", true).set_json(
        "$set", R"({"plan":"free"})");
    client.capture(started);

    posthog::Flags flags = client.evaluate_flags(distinct_id);
    const bool onboarding = flags.is_enabled("new-onboarding");
    std::printf("new-onboarding: %s\n", onboarding ? "on" : "off");

    posthog::Event finished("onboarding_viewed", distinct_id);
    finished.set_flags(flags.only_accessed());
    client.capture(finished);

    client.flush();
    return 0;
  } catch (const posthog::Error &error) {
    std::fprintf(stderr, "%s\n", error.what());
    return 1;
  }
}

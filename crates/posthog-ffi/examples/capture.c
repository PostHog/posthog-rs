/*
 * Captures an event and reads a feature flag.
 *
 *   cargo build --release
 *   cc examples/capture.c -Iinclude -Ltarget/release -lposthog_ffi -o capture
 *   LD_LIBRARY_PATH=target/release DYLD_LIBRARY_PATH=target/release \
 *     POSTHOG_API_KEY=phc_... ./capture
 *
 * See README.md for static linking.
 */
#include <stdio.h>
#include <stdlib.h>

#include "posthog.h"

static int fail(const char *what) {
    const char *error = posthog_last_error();
    fprintf(stderr, "%s failed: %s\n", what, error ? error : "unknown error");
    return 1;
}

int main(void) {
    const char *api_key = getenv("POSTHOG_API_KEY");
    const char *host = getenv("POSTHOG_HOST");

    PostHogOptions *options = posthog_options_new(api_key ? api_key : "");
    if (!options) return fail("posthog_options_new");
    if (host && posthog_options_set_host(options, host) != POSTHOG_STATUS_OK) {
        posthog_options_free(options);
        return fail("posthog_options_set_host");
    }
    posthog_options_set_is_server(options, false);

    PostHogClient *client = posthog_client_new(options);
    posthog_options_free(options);
    if (!client) return fail("posthog_client_new");

    PostHogEvent *event = posthog_event_new("app_started", "user-123");
    posthog_event_set_property_string(event, "edition", "community");
    posthog_event_set_property_int(event, "plugins", 3);
    posthog_event_set_property_json(event, "$set", "{\"plan\":\"free\"}");
    if (posthog_capture(client, event) != POSTHOG_STATUS_OK) return fail("posthog_capture");
    posthog_event_free(event);

    int exit_code = 0;
    PostHogFlags *flags = NULL;
    if (posthog_evaluate_flags(client, "user-123", NULL, &flags) == POSTHOG_STATUS_OK) {
        bool enabled = false;
        posthog_flags_is_enabled(flags, "new-onboarding", &enabled);
        printf("new-onboarding: %s\n", enabled ? "on" : "off");
        posthog_flags_free(flags);
    } else {
        exit_code = fail("posthog_evaluate_flags");
    }

    posthog_client_free(client);
    return exit_code;
}

# PostHog Decide API

**Purpose: Feature Flags and A/B Testing**

The `/decide/` endpoint is a crucial part of PostHog's feature flag and A/B testing system.  It allows your application (server-side or client-side) to quickly determine:

1.  **Which feature flags are enabled for a given user (or group of users).**
2.  **Which variant of an A/B test a user should be assigned to.**

Essentially, your application asks PostHog, "What's the current state of flags for this user?" and PostHog responds with the relevant flag values. This enables you to control feature rollouts, run experiments, and personalize user experiences *without* redeploying your code every time you change a flag.

**Authentication:**

*   **Personal API Key (Authorization Header):** Like the Capture API, requests to the `/decide/` endpoint *require* your Personal API Key in the `Authorization` header: `Authorization: Bearer YOUR_PERSONAL_API_KEY`.
* **`Content-Type`:** Data should be sent as `application/json`.

**Endpoint and Usage:**

*   **`/decide/` (GET):**

    *   **Purpose:** Fetch the current state of feature flags for a given user.
    *   **Method:** `GET`
    *   **Request Parameters (Query Parameters):**
        *   `v`: API Version. Currently, set this to `2`.
        *   `b`:  A base64-encoded JSON string containing:
            *   `api_key`: Your *Project* API key (not your Personal API key).
            *   `distinct_id`:  The unique identifier for the user. *Crucial* for consistent flag evaluations.
            *   `groups`: (Optional) A dictionary mapping group types to group keys. This is used for group-based flag targeting (e.g., targeting flags to specific organizations or companies).
            *   `person_properties`: (Optional) A dictionary of properties associated with the user. Used for targeting flags based on user attributes.
            *   `group_properties`: (Optional) A dictionary of properties associated with the user's groups.

    *   **Example (JavaScript, generating the URL):**

        ```javascript
        const requestData = {
          api_key: "YOUR_PROJECT_API_KEY",
          distinct_id: "user123",
          groups: {
            company: "acme_corp" // Example group
          },
          person_properties: {
            email: "user@example.com",
            plan: "premium"
          },
          group_properties: {
            company: { //Properties for 'company' group
              name: 'Acme Corp'
            }
          }
        };

        const base64Data = btoa(JSON.stringify(requestData));
        const url = `https://app.posthog.com/decide/?v=2&b=${base64Data}`;

        // Use fetch to make the request (remember the Authorization header!)
        fetch(url, {
          method: 'GET',
          headers: {
            'Authorization': 'Bearer YOUR_PERSONAL_API_KEY',
            'Content-Type': 'application/json'
          }
        })
        .then(response => response.json())
        .then(data => {
          // data contains the feature flag values
          console.log(data);
        })
        .catch(error => console.error('Error:', error));
        ```
     *   **Example (curl):**

          ```bash
           curl -X GET \
           'https://app.posthog.com/decide/?v=2&b=eyJh       cGlfa2V5IjoiWU9VUl9QUk9KRUNUX0FQSV9LRVkiLCJkaXN0aW5jdF9pZCI6InVzZXIxMjMiLCJncm91cHMiOnsiY29tcGFueSI6ImFjbWVfY29ycCJ9LCJwZXJzb25fcHJvcGVydGllcyI6eyJlbWFpbCI6InVzZXJAZXhhbXBsZS5jb20iLCJwbGFuIjoicHJlbWl1bSJ9LCJncm91cF9wcm9wZXJ0aWVzIjp7ImNvbXBhbnkiOnsibmFtZSI6IkFjbWUgQ29ycCJ9fX0=' \
           -H "Authorization: Bearer YOUR_PERSONAL_API_KEY"
          ```
        *   **Response (JSON):**

            The response is a JSON object.  Key fields include:

            *   `config`: A dictionary where keys are feature flag keys and values are the flag values for the given user (can be `true`, `false`, or a variant value for A/B tests).
            *   `errorsWhileComputingFlags`: Indicates if any errors occurred during flag evaluation.
            *   `featureFlags`: List of feature flags that were evaluated.
            *   `featureFlagPayloads`: Values of feature flag payloads, if any.

            Example:
            ```json
            {
              "config": {
                "new-feature-flag": true,
                "ab-test-variant": "control",
                "another-flag": false
              },
              "errorsWhileComputingFlags": false,
              "featureFlags": ["new-feature-flag", "ab-test-variant", "another-flag"],
              "featureFlagPayloads": {}
            }
            ```

**Important Considerations:**

*   **GET Method Only:**  The `/decide/` endpoint *only* supports the `GET` method.
*   **Base64 Encoding:** The `b` parameter *must* be a base64-encoded JSON string containing the request data.
*   **Distinct ID Consistency:**  Use the *same* `distinct_id` that you use when capturing events. This ensures consistent flag evaluations for a given user.
*   **Groups and Properties:**  Use `groups`, `person_properties`, and `group_properties` to target flags based on user segments and attributes. This is essential for complex flag targeting rules.
*   **Error Handling:** Check the `errorsWhileComputingFlags` field in the response to see if any errors occurred during flag evaluation.  If errors occurred, your application should have a fallback mechanism (e.g., use a default flag value).
*   **Caching:** The `/decide/` endpoint is designed to be fast.  You *can* cache the results for a short period (e.g., a few seconds or minutes) to reduce the number of API calls, but be mindful of how quickly you need flag changes to propagate. The documentation might provide caching recommendations.
*   **SDKs:**  As with the Capture API, PostHog's SDKs (JavaScript, Python, etc.) provide built-in support for fetching feature flags. Using an SDK is *highly recommended* as it handles the encoding, request building, authentication, and often caching for you. It's generally much easier and less error-prone than making the API calls directly.

**Summary and Best Practices:**

*   The `/decide/` endpoint is your gateway to PostHog's feature flag and A/B testing system.
*   Always use your Personal API Key in the `Authorization` header.
*   Send the request data (Project API key, `distinct_id`, etc.) as a base64-encoded JSON string in the `b` query parameter.
*   Use the same `distinct_id` consistently across your application (both for capturing events and fetching flags).
*   Leverage `groups`, `person_properties`, and `group_properties` for sophisticated flag targeting.
*   Use the PostHog SDKs whenever possible to simplify flag integration.
*   Handle potential errors and consider caching responses appropriately.

This detailed explanation should give you a solid understanding of how to use the PostHog `/decide/` API endpoint. Always refer to the official documentation for the most up-to-date and precise information.

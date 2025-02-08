# PostHog Capture API

**Core Concept: Events and Properties**

The PostHog Capture API is all about sending *events*. An event represents an action a user takes (or an action that occurs) within your application.  Events have a name (like "button_clicked" or "page_viewed") and can have associated *properties*. Properties are key-value pairs that provide additional context about the event (e.g.,  `{ "button_text": "Submit", "page_url": "/pricing" }`).  Think of events as verbs and properties as the adjectives and adverbs.

**Authentication:**

*   **API Key:** All requests to the PostHog Capture API require authentication using your PostHog *API Key*.  This is *not* your project API key. You can find your Personal API Key in your PostHog account settings (usually under "Organization Settings" or a similar section).
*   **Authentication Header:** Include the API Key in the `Authorization` header of your HTTP requests.  The format is:  `Authorization: Bearer YOUR_PERSONAL_API_KEY`. This is crucial for security and proper routing of your data.

**Encoding:**

*   The API expects data to be sent as `application/json`. Set the `Content-Type` header to `application/json`.

**Endpoint Summary and Usage:**

The documentation describes several key endpoints, which I'll summarize below. Crucially, the base URL for all of these is `https://[your-posthog-instance]/`, where `[your-posthog-instance]` is the URL of your PostHog deployment (e.g., `app.posthog.com`, `eu.posthog.com`, or your self-hosted instance).  Don't forget to replace this placeholder.

1.  **`/capture/` (POST): The Primary Event Capture Endpoint**

    *   **Purpose:** This is the main endpoint you'll use to send individual events to PostHog.
    *   **Method:** `POST`
    *   **Request Body (JSON):**
        ```json
        {
          "api_key": "YOUR_PROJECT_API_KEY", // NOTE: This is your *Project* API Key, *not* your Personal API Key!
          "event": "event_name", // Required: The name of the event (e.g., "button_clicked")
          "distinct_id": "user_identifier", // Required: A unique identifier for the user (e.g., user ID, anonymous ID)
          "properties": { // Optional: Key-value pairs with additional event data
            "property1": "value1",
            "property2": "value2"
          },
          "timestamp": "2023-10-27T10:00:00Z" // Optional: ISO 8601 timestamp.  If omitted, PostHog uses the server's receive time.
        }
        ```
    *   **Example (using `curl`):**
        ```bash
        curl -X POST \
          https://app.posthog.com/capture/ \
          -H "Authorization: Bearer YOUR_PERSONAL_API_KEY" \
          -H "Content-Type: application/json" \
          -d '{
            "api_key": "YOUR_PROJECT_API_KEY",
            "event": "button_clicked",
            "distinct_id": "user123",
            "properties": {
              "button_text": "Submit"
            },
            "timestamp": "2024-01-19T12:34:56Z"
          }'
        ```
    * **Important Considerations:**
        *   The `api_key` in the body is your *Project* API Key, which links the event to the correct project in PostHog. This is different from the Personal API Key used for authentication.
        *   The `distinct_id` is *crucial*. It's how PostHog identifies individual users. Use a consistent, unique identifier.
        *   The `timestamp` is optional but highly recommended for accurate event ordering, especially if events might be delayed.  Always use the ISO 8601 format (e.g., `YYYY-MM-DDTHH:mm:ssZ` or with milliseconds `YYYY-MM-DDTHH:mm:ss.SSSZ`).
        *   You *must* include the `Authorization` header with your *Personal* API key.

2.  **`/capture/` (GET):  For Browser-Based Tracking (with Limitations)**

    *   **Purpose:** Allows event tracking directly from a web browser, typically using an `<img>` tag or the `fetch` API.  This is less flexible than the `POST` method.
    *   **Method:** `GET`
    *   **Request Parameters:** All event data is passed as URL query parameters. The `data` parameter is *required*, and it contains a base64-encoded JSON string of the event data.
        *   `data`: Base64-encoded JSON string representing the event (same structure as the `POST` request body).
    *   **Example (generating the URL):**
        ```javascript
        // JavaScript example to generate the URL
        const eventData = {
          api_key: "YOUR_PROJECT_API_KEY",
          event: "page_viewed",
          distinct_id: "user456",
          properties: {
            page_url: "/home"
          }
        };

        const base64Data = btoa(JSON.stringify(eventData));
        const url = `https://app.posthog.com/capture/?data=${base64Data}`;

        // You could then use this URL in an <img> tag:
        // <img src={url} style={{display: 'none'}} />

        // Or with fetch (you'll need to handle the response, likely a 200 OK):
        // fetch(url, { method: 'GET' });
        ```
    *   **Important Considerations:**
        *   **URL Length Limits:**  Browsers have limits on URL length.  This limits the amount of data you can send in a single `GET` request.  For complex events, use the `POST` method.
        *   **No Authorization Header:** The `GET` method typically *doesn't* support custom headers like `Authorization` in cross-origin requests (which this usually is). This is a significant difference. You send project API key in payload instead. Because of this, the `GET` endpoint is generally less secure and suitable for front-end data collection where you want to avoid exposing your Personal API Key.  The `POST` endpoint is generally preferred for server-side tracking.
        *   **Base64 Encoding:**  The `data` parameter *must* be a base64-encoded JSON string.
        *   **CORS:** If you're using this from a different domain than your PostHog instance, ensure your PostHog instance is configured to allow cross-origin requests (CORS) from your domain.

3. **`/batch/` (POST): Sending Multiple Events in a Single Request**

    *   **Purpose:** Significantly improves efficiency when sending many events.  Instead of making a separate API call for each event, you send a batch of events in a single request.
    *   **Method:** `POST`
    *   **Request Body (JSON):**
       ```json
       {
         "api_key": "YOUR_PROJECT_API_KEY",  // Again, your *Project* API Key.
         "batch": [  // An array of event objects
           {
             "event": "event_name1",
             "distinct_id": "user_identifier1",
             "properties": { ... },
             "timestamp": "..."
           },
           {
             "event": "event_name2",
             "distinct_id": "user_identifier2",
             "properties": { ... },
             "timestamp": "..."
           },
           // ... more events ...
         ]
       }
       ```
    *   **Example (using `curl`):**
        ```bash
        curl -X POST \
          https://app.posthog.com/batch/ \
          -H "Authorization: Bearer YOUR_PERSONAL_API_KEY" \
          -H "Content-Type: application/json" \
          -d '{
            "api_key": "YOUR_PROJECT_API_KEY",
            "batch": [
              {
                "event": "page_viewed",
                "distinct_id": "user789",
                "properties": { "page": "/pricing" },
                "timestamp": "2024-01-19T13:00:00Z"
              },
              {
                "event": "button_clicked",
                "distinct_id": "user789",
                "properties": { "button_text": "Get Started" },
                "timestamp": "2024-01-19T13:01:00Z"
              }
            ]
          }'
        ```
    * **Important Considerations:**
        *   **Efficiency:** This is the recommended way to send multiple events. It reduces network overhead and improves performance.
        *   **Same Structure:** Each event within the `batch` array has the same structure as the individual event object in the `/capture/` (POST) endpoint.

**Best Practices and General Recommendations:**

*   **Use `/batch/` whenever possible:**  For sending multiple events, always use the `/batch/` endpoint.
*   **Use `POST` for Server-Side Tracking:**  For backend applications, use the `/capture/` (POST) or `/batch/` (POST) endpoints, as they offer better security and flexibility.
*   **Use `GET` Sparingly:**  Only use the `/capture/` (GET) endpoint for simple browser-based tracking where you can't use `POST`. Be mindful of URL length limits.
*   **Handle Errors:**  Implement proper error handling in your code.  The API will return HTTP status codes (e.g., 200 OK, 400 Bad Request, 401 Unauthorized, 500 Internal Server Error) that you should check.
*   **Distinct IDs:**  Choose your `distinct_id` strategy carefully.  This is *critical* for identifying users correctly.  Use a consistent ID across sessions.
*   **Timestamps:**  Include timestamps whenever possible for accurate event ordering.  Use the ISO 8601 format.
*   **Rate Limiting:**  Be aware of PostHog's rate limits.  If you're sending a very high volume of events, you might need to implement retry logic with exponential backoff. The documentation should specify rate limits.
*   **SDKs:** PostHog provides official SDKs for many languages and frameworks (JavaScript, Python, Ruby, Go, etc.).  Using an SDK is *highly recommended* as it handles many of these details (authentication, batching, error handling, etc.) automatically.  The documentation links to these SDKs.  Using an SDK is *far* easier and less error-prone than making raw API calls.

This comprehensive summary covers the PostHog Capture API endpoints, their usage, and best practices. Remember to consult the official documentation for the most up-to-date information and details. The most important takeaways are to use your Personal API Key in the `Authorization` header, your Project API key in the request body, use the correct HTTP method (`POST` or `GET`), and to structure your JSON data correctly. Using the PostHog SDKs will greatly simplify this process.

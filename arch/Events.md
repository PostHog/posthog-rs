# PostHog Events API

**Core Concepts (from the URL and endpoints):**

*   **`project_id`**: The numerical ID of your PostHog project.
*   **`id`**: The unique identifier of a *specific event instance* (not an event definition).
*   **Events**: These endpoints allow you to retrieve the raw event data that PostHog has captured.
* **Authentication:** It is not specified how to authorize, but from previous knowledge we can assume it is similar to other endpoints.
*   **Base URL:** `https://[your-posthog-instance]/`

**Endpoint Summary:**

1.  **`GET /api/projects/:project_id/events/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a list of events within the specified project. The documentation indicates support for pagination and filtering.
    *   **How to Call:**
        *   Replace `:project_id` with your project's ID.
        *   Query parameters:
            *   `after`: (Optional) An ISO 8601 timestamp.  Returns events *after* this time.
            *   `before`: (Optional) An ISO 8601 timestamp. Returns events *before* this time.
            *   `distinct_id`: (Optional) Filters events by a specific `distinct_id`.
            *   `event`: (Optional) Filters events by the event name (e.g., "page_viewed").
            *   `limit`: (Optional) Controls the number of results per page (pagination).
            *   `offset`: (Optional) Used for pagination.
            *    `properties`: Filter by event properties.
            *    `person_id`: Filter by person ID.
            *    `action_id`: Filter by action ID.
        *   Example (with filtering): `https://app.posthog.com/api/projects/123/events/?after=2024-01-18T00:00:00Z&event=button_clicked&limit=100`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** A JSON object:
        ```json
        {
          "next": "https://app.posthog.com/api/projects/123/events/?after=...&limit=100&offset=100", // URL to the next page (or null)
          "previous": "https://app.posthog.com/api/projects/123/events/?after=...&limit=100", // URL to the previous page (or null)
          "results": [ // Array of event objects
            {
              // ... details for each event ...
            }
          ]
        }
        ```
        *   Each event object in `results` will contain:
            *   `id`: Unique ID of the event.
            *   `distinct_id`: The `distinct_id` associated with the event.
            *   `event`: The name of the event.
            *   `properties`: A dictionary of key-value pairs (the event properties).
            *   `timestamp`:  The timestamp of the event.
            *   `elements`: An array of elements associated with the event (often used for front-end events, like clicks).
            *   `elements_chain`: element chain string

2.  **`GET /api/projects/:project_id/events/:id/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a *single event instance* by its ID.
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   Example: `https://app.posthog.com/api/projects/123/events/abc-xyz-123/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:**  A single event object (same structure as in the `GET /.../events/` response).

3.  **`GET /api/projects/:project_id/events/values/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves the *unique values* for a given event property.  This is useful for getting a list of all possible values for a property, like all the different page URLs users have visited.
    *   **How to Call:**
        *   Replace `:project_id`.
        *   Query Parameters:
            *   `key`: (Required) The name of the event property you want to get values for (e.g., "page_url").
            *   `event`: The name of the event you want to get value from.
            *   `value`: Optional value for searching.
        *   Example:  `https://app.posthog.com/api/projects/123/events/values/?key=browser`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:**  The format depends on whether a `value` is provided for search:
         *   With `value`:
             ```json
             [
                {
                    "name": "Chrome"
                }
             ]
              ```
        *   Without a `value`:
            ```json
            [
                "Chrome",
                "Firefox",
                "Safari"
            ]
             ```

**Key Takeaways:**

*   The Events API provides access to the raw event data captured by PostHog.
*   You can retrieve events (list and individual) and get unique values for event properties.
*   `GET /.../events/` supports filtering by time, `distinct_id`, event name, and properties.
*   `GET /.../events/values/` is useful for exploring the range of values for a given property.
*   Always use your Personal API Key in the `Authorization` header.

This summary, based on the provided list and documentation URL, gives a good overview of the PostHog Events API's capabilities. It covers retrieving event data, filtering, and exploring property values.

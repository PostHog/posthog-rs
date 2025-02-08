# PostHog Activity Log API

**Core Concepts (from the URL and endpoints):**

*   **`project_id`:** As with other endpoints, this is a placeholder for the numerical ID of the relevant PostHog project.
*   **Activity Log:** This API seems to deal with a log of activities or events within a PostHog project.
* **Authentication:** It is not specified how to authorize, but from previous knowledge we can assume it is similar to other endpoints.
*   **Base URL:** `https://[your-posthog-instance]/`

**Endpoint Summary:**

1.  **`GET /api/projects/:project_id/activity_log/`**

    *   **Method:** `GET`
    *   **Purpose:** (Inferred from the name and standard REST practices) This endpoint likely retrieves a list of activity log entries for a given project. The documentation mentions filtering, pagination, and searching capabilities.
    *   **How to Call:**
        *   Replace `:project_id` with your project's ID.
        *   The full documentation details various query parameters you can use for filtering:
            *   `limit`: Controls the number of results per page (pagination).
            *   `page`: Specifies which page of results to retrieve.
            *   `search`: Filters log entries based on a search term.
            *   `activity`: Filters by the type of activity (e.g., "created", "updated").
            *   `item_id`: Filters by the ID of the item the activity relates to.
            *   `scope`: Filters by the scope of the activity (e.g., "FeatureFlag", "Insight").
            *   `user`: Filter to a specific user.
        *   Example URL (with filtering):
            `https://app.posthog.com/api/projects/123/activity_log/?limit=50&page=2&search=created&activity=created&scope=Insight`
        *  Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** The response is a JSON object with the following structure:
        ```json
        {
          "count": 123, // Total number of results (without pagination)
          "next": "https://app.posthog.com/api/projects/123/activity_log/?limit=50&page=3", // URL for the next page (or null)
          "previous": "https://app.posthog.com/api/projects/123/activity_log/?limit=50&page=1", // URL for the previous page (or null)
          "results": [ // Array of activity log entries
            {
              // ... details for each log entry ...
            }
          ]
        }
        ```
        *   Each log entry in the `results` array contains details like:
            *   `id`: Unique ID of the log entry.
            *   `user`: Information about the user who performed the activity.
            *   `activity`: The type of activity (e.g., "created", "updated", "deleted").
            *   `item_id`: The ID of the item affected by the activity.
            *   `scope`: The type of item affected (e.g., "Insight", "FeatureFlag", "Dashboard").
            *   `detail`: An object providing more specific information about the change. This includes `changes` (an array of changes), `type`, `name`.
            *   `created_at`: Timestamp of when the activity occurred.

2.  **`POST /api/projects/:project_id/activity_log/bookmark_activity_notification/`**

    *   **Method:** `POST`
    *   **Purpose:** This endpoint allows a user to "bookmark" a specific activity log notification. This likely means marking it as read, important, or otherwise tracking its status.
    *   **How to Call:**
        *   Replace `:project_id` with your project's ID.
        *   The request body is expected to be empty (no data needs to be sent).
        *   Example URL:  `https://app.posthog.com/api/projects/123/activity_log/bookmark_activity_notification/`
        *  Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** The documentation does not show any response details.

3.  **`GET /api/projects/:project_id/activity_log/important_changes/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a list of "important changes" from the activity log. This likely filters the log to show only activities that PostHog deems significant (e.g., creation of new features, changes to critical settings).
    *   **How to Call:**
        *   Replace `:project_id` with your project's ID.
        *   Example URL: `https://app.posthog.com/api/projects/123/activity_log/important_changes/`
        *  Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** The response is a JSON object containing an array of activity log entries, similar in structure to the `GET /api/projects/:project_id/activity_log/` endpoint, but presumably filtered to show only "important" changes.

**Key Takeaways and Recommendations:**

*   The Activity Log API provides a way to programmatically access the audit trail of changes within a PostHog project.
*   The `GET /api/projects/:project_id/activity_log/` endpoint is the primary way to retrieve log entries, with filtering and pagination options.
*   `bookmark_activity_notification` lets users track specific notifications.
*   `important_changes` provides a filtered view of significant activities.
*   Always use your Personal API Key in the `Authorization` header, using `Authorization: Bearer YOUR_PERSONAL_API_KEY`.

This summarizes the endpoints based on the documentation and the provided list. The full documentation provides essential details about filtering, pagination, and the structure of the activity log entries.

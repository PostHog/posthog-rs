# PostHog Session Recordings API

**Core Concepts (from the URL and endpoints):**

*   **`project_id`**: The numerical ID of your PostHog project.
*   **`id`**:  The unique identifier of a specific session recording.
* **`recording_id`**: Same as `id`.
*   **Session Recordings**: These endpoints allow you to retrieve, manage, and delete session recordings.
* **Authentication:** It is not specified how to authorize, but from previous knowledge we can assume it is similar to other endpoints.
*   **Base URL:** `https://[your-posthog-instance]/`

**Endpoint Summary:**

1.  **`GET /api/projects/:project_id/session_recordings/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a list of session recordings within the specified project. Supports pagination and filtering.
    *   **How to Call:**
        *   Replace `:project_id`.
        *   Query Parameters:
            *   `distinct_id`: (Optional) Filters recordings by a specific `distinct_id`.
            *   `limit`: Pagination - number of results per page.
            *   `offset`: Pagination - starting offset.
        *   Example:  `https://app.posthog.com/api/projects/123/session_recordings/?distinct_id=user123&limit=50`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** A paginated JSON object:
        ```json
        {
          "next": "...", // URL to next page (or null)
          "previous": "...", // URL to previous page (or null)
          "results": [ // Array of session recording objects
            {
              // ... details for each recording ...
            }
          ]
        }
        ```
        *   Each session recording object in `results` will contain:
            *   `id`: Unique ID of the recording.
            *   `distinct_id`: The `distinct_id` associated with the recording.
            *   `viewed`: Whether it has been viewed
            *    `recording_duration`: Duration of recording
            *   `start_time`: Start time.
            *    `end_time`: End Time.
            *    `click_count`: Number of clicks
            *    `console_error_count`: Number of console errors.
            *   Other metadata.
            *   *Note:*  The actual recording *data* (video) is *not* included in this response.  You typically get a URL to download the recording data separately.

2.  **`GET /api/projects/:project_id/session_recordings/:id/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a *single* session recording by its ID.
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   Example: `https://app.posthog.com/api/projects/123/session_recordings/abc-xyz/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:**  The requested session recording object (same structure as in the list response). Again, this likely *doesn't* include the video data itself, but provides metadata and potentially a URL to access the recording.

3.  **`PATCH /api/projects/:project_id/session_recordings/:id/`**

    *   **Method:** `PATCH`
    *   **Purpose:** Partially updates an existing session recording's information. This might be used to mark a recording as viewed, add notes, or modify other metadata.  You *cannot* modify the recording data itself.
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   Request Body (JSON): Contains *only* the fields you want to update (e.g., `{"viewed": true}`).
        *   Example:  `https://app.posthog.com/api/projects/123/session_recordings/abc-xyz/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
         *  Set `Content-Type`: `application/json`
    *   **Response:** The updated session recording object.

4.  **`DELETE /api/projects/:project_id/session_recordings/:id/`**

    *   **Method:** `DELETE`
    *   **Purpose:** Deletes a session recording. This *permanently* removes the recording data.
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   Example: `https://app.posthog.com/api/projects/123/session_recordings/abc-xyz/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** 204 No Content (success).

5.  **`GET /api/projects/:project_id/session_recordings/:recording_id/sharing/`**

    *   **Method:** `GET`
    *    **Purpose:** Retrieves a shareable link to the specific recording.
    *   **How to Call:**
        *   Replace `:project_id` and `:recording_id`.
        *   Example: `https://app.posthog.com/api/projects/123/session_recordings/abc-xyz/sharing`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** Returns an object which would include a shareable link

**Key Takeaways:**

*   The Session Recordings API allows you to manage session recordings within a PostHog project.
*   You can list recordings (filtered by `distinct_id`), retrieve individual recordings, update metadata (like `viewed` status), and delete recordings.
*   The actual recording *data* (video) is typically accessed via a separate URL, not directly through these API endpoints.
*   Always use your Personal API Key in the `Authorization` header.

This summary, based on the provided list and URL, provides a complete overview of the PostHog Session Recordings API, enabling you to retrieve, manage, and delete session recordings within your projects.

# PostHog Session Recording Playlists API

**Core Concepts (from the URL and endpoints):**

*   **`project_id`**: The numerical ID of your PostHog project.
*   **`short_id`**: A short, human-readable ID for a playlist (similar to insights and notebooks).
*   **`session_recording_id`**: The ID of a *specific session recording* (as used in the Session Recordings API).
*   **Session Recording Playlists**: These endpoints allow you to create, manage, and organize collections of session recordings.
* **Authentication:** It is not specified how to authorize, but from previous knowledge we can assume it is similar to other endpoints.
*   **Base URL:** `https://[your-posthog-instance]/`

**Endpoint Summary (Grouped by Functionality):**

**1. Playlist Management:**

*   **`GET /api/projects/:project_id/session_recording_playlists/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a list of all session recording playlists within the specified project. Likely supports pagination.
    *   **How to Call:**
        *   Replace `:project_id`.
        *   Pagination parameters (likely): `limit`, `offset`.
        *   Example: `https://app.posthog.com/api/projects/123/session_recording_playlists/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** A JSON object with the usual paginated structure:
         ```json
         {
            "count": 1,
            "next": "string",
            "previous": "string",
            "results": [
              {
                //Playlist object
              }
            ]
         }
         ```
        *   Each playlist object in `results` will likely contain:
            *   `short_id`: The short, human-readable ID.
            *   `name`: The name of the playlist.
            *   `description`: A description.
            *  `pinned`: Whether the playlist is pinned
            *   `created_by`: Who created it.
            *   `created_at`: Timestamp of creation.
            *   Other metadata.

*   **`POST /api/projects/:project_id/session_recording_playlists/`**

    *   **Method:** `POST`
    *   **Purpose:** Creates a new session recording playlist.
    *   **How to Call:**
        *   Replace `:project_id`.
        *   Request Body (JSON): Contains data for the new playlist.  Key fields:
            *   `name`: (Required) The name of the playlist.
            *   `description`: (Optional) A description.
            *  `pinned`: (Optional) Whether the playlist is pinned
        *   Example:  `https://app.posthog.com/api/projects/123/session_recording_playlists/`
        *  Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
         *  Set `Content-Type`: `application/json`
    *   **Response:** The newly created playlist object.

*   **`GET /api/projects/:project_id/session_recording_playlists/:short_id/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a single session recording playlist by its `short_id`.
    *   **How to Call:**
        *   Replace `:project_id` and `:short_id`.
        *   Example: `https://app.posthog.com/api/projects/123/session_recording_playlists/abc456/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** The requested playlist object.

*   **`PATCH /api/projects/:project_id/session_recording_playlists/:short_id/`**

    *   **Method:** `PATCH`
    *   **Purpose:** Partially updates an existing session recording playlist.
    *   **How to Call:**
        *   Replace `:project_id` and `:short_id`.
        *   Request Body (JSON): Contains *only* the fields you want to update (e.g., `name`, `description`,`pinned`).
        *   Example: `https://app.posthog.com/api/projects/123/session_recording_playlists/abc456/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
         *  Set `Content-Type`: `application/json`
    *   **Response:** The updated playlist object.

*   **`DELETE /api/projects/:project_id/session_recording_playlists/:short_id/`**

    *   **Method:** `DELETE`
    *   **Purpose:** Deletes a session recording playlist. This *doesn't* delete the recordings themselves, just the playlist.
    *   **How to Call:**
        *   Replace `:project_id` and `:short_id`.
        *   Example: `https://app.posthog.com/api/projects/123/session_recording_playlists/abc456/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** 204 No Content (success).

**2. Managing Recordings *within* a Playlist:**

*   **`GET /api/projects/:project_id/session_recording_playlists/:short_id/recordings/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a list of all session recordings *within* the specified playlist.
    *   **How to Call:**
        *   Replace `:project_id` and `:short_id` (the ID of the *playlist*).
        *   Example:  `https://app.posthog.com/api/projects/123/session_recording_playlists/abc456/recordings/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** A JSON array of session recording objects (the same objects you'd get from the Session Recordings API). This lists the recordings that are *members* of the playlist.

*   **`POST /api/projects/:project_id/session_recording_playlists/:short_id/recordings/:session_recording_id/`**

    *   **Method:** `POST`
    *   **Purpose:** Adds a specific session recording to the playlist.
    *   **How to Call:**
        *   Replace `:project_id`, `:short_id` (playlist ID), and `:session_recording_id` (recording ID).
        *   Request Body: Likely empty. The association is created based on the URL parameters.
        *   Example: `https://app.posthog.com/api/projects/123/session_recording_playlists/abc456/recordings/xyz789/` (adds recording `xyz789` to playlist `abc456`)
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** (Inferred) Success indicator (e.g., 201 Created) or perhaps the updated playlist object.

*   **`DELETE /api/projects/:project_id/session_recording_playlists/:short_id/recordings/:session_recording_id/`**

    *   **Method:** `DELETE`
    *   **Purpose:** Removes a specific session recording from the playlist. This *doesn't* delete the recording itself, just removes it from the playlist.
    *   **How to Call:**
        *   Replace `:project_id`, `:short_id` (playlist ID), and `:session_recording_id` (recording ID).
        *   Example: `https://app.posthog.com/api/projects/123/session_recording_playlists/abc456/recordings/xyz789/` (removes recording `xyz789` from playlist `abc456`)
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** 204 No Content (success).

**Key Takeaways:**

*   The Session Recording Playlists API allows you to create and manage collections of session recordings.
*   You can list, create, update, and delete playlists.
*   You can add and remove individual session recordings to/from a playlist.
*   Deleting a playlist *doesn't* delete the recordings, only the playlist itself.
*   The `short_id` is used for playlists, and `session_recording_id` refers to individual recordings.
*   Always use your Personal API Key in the `Authorization` header.

This is a complete breakdown of the Session Recording Playlists API based on the provided list and URL. It explains how to organize and manage your session recordings using playlists.

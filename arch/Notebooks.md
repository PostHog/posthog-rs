# PostHog Notebooks

**Core Concepts (from the URL and endpoints):**

*   **`project_id`**: The numerical ID of your PostHog project.
*   **`short_id`**: A short, human-readable ID for a notebook (likely similar to the `short_id` used for insights). This is used instead of a long UUID.
*   **Notebooks**: These endpoints manage collaborative notebooks within PostHog. Notebooks allow teams to document their analysis, share code snippets, and collaborate on data exploration.
* **Authentication:** It is not specified how to authorize, but from previous knowledge we can assume it is similar to other endpoints.
*   **Base URL:** `https://[your-posthog-instance]/`

**Endpoint Summary:**

1.  **`GET /api/projects/:project_id/notebooks/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a list of notebooks within the specified project. Likely supports pagination.
    *   **How to Call:**
        *   Replace `:project_id`.
        *   Pagination parameters (likely): `limit`, `offset`.
        *   Example: `https://app.posthog.com/api/projects/123/notebooks/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** A JSON object with the usual paginated structure:
        ```json
        { "count": 7,
          "next": "...",
          "previous": "...",
          "results": [
             {
                //Notebook Object
             }
          ]
        }
        ```
        *   Each notebook object in `results` will likely contain:
            *   `short_id`: The short, human-readable ID.
            *   `title`:  The title of the notebook.
             *   `content`:  The content.
            *   `created_by`: Who created it.
            *   `created_at`: Timestamp of creation.
            *   `last_modified_at`: Timestamp of last modification.
            *   Other metadata.

2.  **`POST /api/projects/:project_id/notebooks/`**

    *   **Method:** `POST`
    *   **Purpose:** Creates a new notebook.
    *   **How to Call:**
        *   Replace `:project_id`.
        *   Request Body (JSON): Contains the data for the new notebook. Key fields would likely be:
            *   `title`: The title of the notebook.
            * `content`: Content of the notebook.
        *   Example: `https://app.posthog.com/api/projects/123/notebooks/`
        *  Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
         *  Set `Content-Type`: `application/json`
    *   **Response:** The newly created notebook object.

3.  **`GET /api/projects/:project_id/notebooks/:short_id/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a single notebook by its `short_id`.
    *   **How to Call:**
        *   Replace `:project_id` and `:short_id`.
        *   Example: `https://app.posthog.com/api/projects/123/notebooks/xyz123/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** The requested notebook object.

4.  **`PATCH /api/projects/:project_id/notebooks/:short_id/`**

    *   **Method:** `PATCH`
    *   **Purpose:** Partially updates an existing notebook.
    *   **How to Call:**
        *   Replace `:project_id` and `:short_id`.
        *   Request Body (JSON): Contains *only* the fields you want to update (e.g., `title`, content).
        *   Example: `https://app.posthog.com/api/projects/123/notebooks/xyz123/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
         *  Set `Content-Type`: `application/json`
    *   **Response:** The updated notebook object.

5.  **`DELETE /api/projects/:project_id/notebooks/:short_id/`**

    *   **Method:** `DELETE`
    *   **Purpose:** Deletes a notebook.
    *   **How to Call:**
        *   Replace `:project_id` and `:short_id`.
        *   Example: `https://app.posthog.com/api/projects/123/notebooks/xyz123/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** 204 No Content (success).

6.  **`GET /api/projects/:project_id/notebooks/:short_id/activity/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves the activity log for a specific notebook (who modified it, when, etc.).
    *   **How to Call:**
        *   Replace `:project_id` and `:short_id`.
        *   Example: `https://app.posthog.com/api/projects/123/notebooks/xyz123/activity/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** (Inferred) A list of activity log entries.

7.  **`GET /api/projects/:project_id/notebooks/activity/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves the activity log for *all* notebooks in the project.
    *   **How to Call:**
        *   Replace `:project_id`.
        *    Example: `https://app.posthog.com/api/projects/123/notebooks/activity`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** (Inferred) A list of activity log entries.

8. **`GET /api/projects/:project_id/notebooks/recording_comments/`**
 *   **Method:** `GET`
    *   **Purpose:** Retrieves all comments from all recordings.
    *   **How to Call:**
        *   Replace `:project_id`.
        *    Example: `https://app.posthog.com/api/projects/123/notebooks/recording_comments`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.

**Key Takeaways:**

*   The Notebooks API allows you to manage collaborative notebooks within a PostHog project.
*   You can create, read (list and individual), update, and delete notebooks.
*   The `short_id` is used to identify individual notebooks.
*   Activity logs track changes to notebooks.
*   Always use your Personal API Key in the `Authorization` header.

This summary, based on the provided list and URL, gives a clear overview of the PostHog Notebooks API, enabling you to work with collaborative notebooks within your projects.

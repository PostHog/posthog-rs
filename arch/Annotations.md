# PostHog Annotations API

**Core Concepts (from the URL and endpoints):**

*   **`project_id`**:  The numerical ID of your PostHog project.  This is a placeholder in the URLs.
*   **`id`**: The unique identifier of a specific annotation.
*   **Annotations**:  These endpoints allow you to manage annotations within a PostHog project. Annotations are notes or markers that you can add to your PostHog charts and dashboards to provide context (e.g., "Marketing campaign launched," "Website outage").
* **Authentication:** It is not specified how to authorize, but from previous knowledge we can assume it is similar to other endpoints.
*   **Base URL:** `https://[your-posthog-instance]/`

**Endpoint Summary:**

1.  **`GET /api/projects/:project_id/annotations/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a list of annotations for a given project.  The documentation mentions pagination.
    *   **How to Call:**
        *   Replace `:project_id` with your project's ID.
        *   Query parameters are available for pagination:
            *   `limit`:  The number of results to return per page.
            *   `offset`:  The starting point for the results (used for pagination).
        *   Example URL: `https://app.posthog.com/api/projects/123/annotations/?limit=100&offset=200`
        *  Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:**  A JSON object with the following structure:
        ```json
        {
          "count": 542,  // Total count of annotations
          "next": "https://app.posthog.com/api/projects/123/annotations/?limit=100&offset=300", // URL for the next page (or null)
          "previous": "https://app.posthog.com/api/projects/123/annotations/?limit=100&offset=100", // URL for the previous page (or null)
          "results": [ // Array of annotation objects
            {
              // ... details for each annotation ...
            }
          ]
        }
        ```
        *   Each annotation object in the `results` array will contain details like:
            *   `id`:  Unique ID of the annotation.
            *   `content`: The text of the annotation.
            *   `date_marker`:  The date and time the annotation applies to.
            *   `creation_type`: How the annotation was created ("USR" for user-created, "GIT" for Git-based, etc.).
            *   `created_at`: Timestamp of when the annotation was created.
            *   `updated_at`: Timestamp of when the annotation was last updated.
            *   `deleted`:  A boolean indicating if the annotation has been deleted.
            *   `scope`:  The scope of the annotation (e.g., "project", "dashboard", "insight").
            *   `dashboard`: The dashboard the annotation belongs too, if it has that scope.
            *   `insight`: The insight the annotation belongs too, if it has that scope.

2.  **`POST /api/projects/:project_id/annotations/`**

    *   **Method:** `POST`
    *   **Purpose:** Creates a new annotation.
    *   **How to Call:**
        *   Replace `:project_id` with your project's ID.
        *   The request body *must* be JSON and should contain the data for the new annotation.  Key fields include:
            *   `content`: (Required) The text of the annotation.
            *   `date_marker`: (Required) The date and time the annotation applies to (ISO 8601 format).
            *   `scope`: (Optional) The scope of the annotation ("project", "dashboard", or "insight"). Defaults to "project".
            *  `dashboard`: (Required if `scope` is "dashboard")
            *   `insight`: (Required if `scope` is "insight").
            *   `creation_type`: (Optional) How the annotation is being created ("USR" is the default).
        *   Example URL: `https://app.posthog.com/api/projects/123/annotations/`
        *   Example Request Body:
            ```json
            {
              "content": "Deployed new pricing page",
              "date_marker": "2024-01-19T14:00:00Z",
              "scope": "project"
            }
            ```
        *  Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
        *  Set `Content-Type`: `application/json`
    *   **Response:**  The response will be a JSON object representing the newly created annotation (same structure as in the `GET` response).

3.  **`GET /api/projects/:project_id/annotations/:id/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a single annotation by its ID.
    *   **How to Call:**
        *   Replace `:project_id` with your project's ID.
        *   Replace `:id` with the ID of the annotation you want.
        *   Example URL: `https://app.posthog.com/api/projects/123/annotations/456/`
        *  Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** A JSON object representing the requested annotation (same structure as in the `GET /.../annotations/` response).

4.  **`PATCH /api/projects/:project_id/annotations/:id/`**

    *   **Method:** `PATCH`
    *   **Purpose:** Partially updates an existing annotation.  You only send the fields you want to change.
    *   **How to Call:**
        *   Replace `:project_id` with your project's ID.
        *   Replace `:id` with the ID of the annotation to update.
        *   The request body should be JSON, containing only the fields you want to update.  You can update `content`, `date_marker`, `scope`,`dashboard`, and `insight`.
        *   Example URL: `https://app.posthog.com/api/projects/123/annotations/456/`
        *   Example Request Body (to change the content):
            ```json
            {
              "content": "Updated annotation text"
            }
            ```
        *  Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
        *  Set `Content-Type`: `application/json`
    *   **Response:** A JSON object representing the updated annotation.

5.  **`DELETE /api/projects/:project_id/annotations/:id/`**

    *   **Method:** `DELETE`
    *   **Purpose:** Deletes an annotation.
    *   **How to Call:**
        *   Replace `:project_id` with your project's ID.
        *   Replace `:id` with the ID of the annotation to delete.
        *   Example URL: `https://app.posthog.com/api/projects/123/annotations/456/`
        *  Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:**  The documentation states that the response will have HTTP status code 204 No Content, indicating successful deletion.

**Key Takeaways:**

*   The Annotations API allows you to manage annotations programmatically.
*   You can create, retrieve (list and individual), update, and delete annotations.
*   `GET /.../annotations/` supports pagination.
*   `POST` and `PATCH` require JSON request bodies.
*   Always use your Personal API Key in the `Authorization` header.
*   `creation_type` of `GIT` can be used to make annotations based on your git commit history.

This comprehensive breakdown is based on the information in the provided documentation and the endpoint list. It covers the purpose, how to call each endpoint, request and response structures, and important considerations.

# PostHog Dashboards API

**Core Concepts (Inferred):**

*   **`project_id`**: The ID of the PostHog project.
*   **`id`**:  The unique ID of a *dashboard*.
*   **`dashboard_id`**: The same as id
*  **`user__uuid`**: The UUID of a user.
*   **Dashboards**:  These endpoints manage dashboards, which are collections of visualizations (insights, charts, etc.).
* **Authentication:** It is not specified how to authorize, but from previous knowledge we can assume it is similar to other endpoints.
*   **Base URL:** `https://[your-posthog-instance]/`

**Endpoint Summary:**

1.  **`GET /api/projects/:project_id/dashboards/`**

    *   **Method:** `GET`
    *   **Purpose:** (Inferred) Retrieves a list of dashboards within the specified project. Likely supports pagination.
        *   Example: `https://app.posthog.com/api/projects/123/dashboards/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *  **How to Call:** Not specified.

2.  **`POST /api/projects/:project_id/dashboards/`**

    *   **Method:** `POST`
    *   **Purpose:** (Inferred) Creates a new dashboard.
    *   **How to Call:**
        *   Replace `:project_id`.
        *   Request Body (JSON): (Inferred) Data defining the new dashboard (name, description, etc.).
        *   Example: `https://app.posthog.com/api/projects/123/dashboards/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** The newly created dashboard object.

3. **`GET /api/projects/:project_id/dashboards/:dashboard_id/collaborators/`**

    *   **Method:** `GET`
    *   **Purpose:** (Inferred) Get a list of collaborators that have access to this dashboard.
    *   **How to Call:**
        *   Replace `:project_id` and `:dashboard_id`.
        *   Example: `https://app.posthog.com/api/projects/123/dashboards/456/collaborators`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** A list of collaborator objects.

4.  **`POST /api/projects/:project_id/dashboards/:dashboard_id/collaborators/`**

    *   **Method:** `POST`
    *   **Purpose:** (Inferred) Add a collaborator to this dashboard.
    *   **How to Call:**
        *   Replace `:project_id` and `:dashboard_id`.
        *   Request Body (JSON): Details of the collaborator.
        *   Example: `https://app.posthog.com/api/projects/123/dashboards/456/collaborators`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** The newly created collaborator object.

5.  **`DELETE /api/projects/:project_id/dashboards/:dashboard_id/collaborators/:user__uuid/`**

    *   **Method:** `DELETE`
    *   **Purpose:** (Inferred) Remove a collaborator from the dashboard.
    *   **How to Call:**
        *   Replace `:project_id`, `:dashboard_id`, and `:user__uuid`
        *   Example: `https://app.posthog.com/api/projects/123/dashboards/456/collaborators/useruuid`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** 204 No Content (success).

6. **`GET /api/projects/:project_id/dashboards/:dashboard_id/sharing/`**

    *   **Method:** `GET`
    *   **Purpose:** (Inferred) Retrieves sharing configuration for a specific dashboard.
    *   **How to Call:**
        *   Replace `:project_id` and `:dashboard_id`.
        *   Example: `https://app.posthog.com/api/projects/123/dashboards/456/sharing`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** The sharing configuration of the dashboard.

7.  **`GET /api/projects/:project_id/dashboards/:id/`**

    *   **Method:** `GET`
    *   **Purpose:** (Inferred) Retrieves a single dashboard by its ID.
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   Example: `https://app.posthog.com/api/projects/123/dashboards/456/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** The requested dashboard object.

8.  **`PATCH /api/projects/:project_id/dashboards/:id/`**

    *   **Method:** `PATCH`
    *   **Purpose:** (Inferred) Partially updates an existing dashboard.
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   Request Body (JSON): Contains *only* the fields to update.
        *   Example: `https://app.posthog.com/api/projects/123/dashboards/456/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** The updated dashboard object.

9.  **`DELETE /api/projects/:project_id/dashboards/:id/`**

    *   **Method:** `DELETE`
    *   **Purpose:** (Inferred) Deletes a dashboard.
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   Example: `https://app.posthog.com/api/projects/123/dashboards/456/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** 204 No Content (success).

10. **`PATCH /api/projects/:project_id/dashboards/:id/move_tile/`**

    *   **Method:** `PATCH`
    *   **Purpose:** (Inferred) Changes the position of a "tile" (likely a visualization or insight) within the dashboard.
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   Request Body (JSON): (Inferred) Data specifying the tile to move and its new position.
        *   Example: `https://app.posthog.com/api/projects/123/dashboards/456/move_tile`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:**  Likely the updated dashboard object.

11. **`POST /api/projects/:project_id/dashboards/create_from_template_json/`**

    *   **Method:** `POST`
    *   **Purpose:** (Inferred) Creates a new dashboard from a JSON template.  This allows for programmatic dashboard creation based on predefined structures.
    *   **How to Call:**
        *   Replace `:project_id`.
        *   Request Body (JSON): The dashboard template data.
        *    Example: `https://app.posthog.com/api/projects/123/dashboards/create_from_template_json/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** The newly created dashboard object.

**Key Takeaways and Recommendations:**

*   The Dashboards API allows programmatic management of dashboards.
*   Standard RESTful operations (GET, POST, PATCH, DELETE) are used for basic CRUD operations.
*   Collaborator management endpoints are included.
*   `move_tile` suggests a drag-and-drop or similar arrangement of visualizations within a dashboard.
*   `create_from_template_json` enables dashboard creation from templates.
*   Always use your Personal API Key in the `Authorization` header, using `Authorization: Bearer YOUR_PERSONAL_API_KEY`.
* The full documentation is needed for specific details on request and response details.

This summary provides the best possible interpretation based *solely* on the endpoint list. The actual PostHog Dashboards API documentation is necessary for complete usage details.

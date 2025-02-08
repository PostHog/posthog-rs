# PostHog Dashboard Templates API

**Core Concepts (from the URL and endpoints):**

*   **`project_id`**: The numerical ID of the PostHog project.
*   **`id`**:  The unique identifier of a specific dashboard template.
*   **Dashboard Templates**:  These endpoints manage reusable templates for creating dashboards.  This allows for consistent dashboard structures across a project or organization.
* **Authentication:** It is not specified how to authorize, but from previous knowledge we can assume it is similar to other endpoints.
*   **Base URL:** `https://[your-posthog-instance]/`

**Endpoint Summary:**

1.  **`GET /api/projects/:project_id/dashboard_templates/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a list of available dashboard templates within the specified project.  The documentation mentions that this endpoint does *not* support pagination.
    *   **How to Call:**
        *   Replace `:project_id` with your project's ID.
        *   Example: `https://app.posthog.com/api/projects/123/dashboard_templates/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** A JSON array of dashboard template objects.  Each template object includes:
        *   `id`:  Unique ID of the template.
        *   `template_name`:  The name of the template.
        *   `dashboard_description`: A description of the dashboard the template creates.
        *   `dashboard_filters`:  Default filters to apply to the dashboard.
        *   `tags`:  Tags associated with the template.
        *   `deleted`: Boolean indicating if the template is deleted.
        *   `created_by`: Information about the user who created the template.
        *   `created_at`:  Timestamp of creation.
        *   `image_url`:  URL of a preview image for the template (may be null).
        *   `tiles`:  An array of objects defining the tiles (visualizations) within the template.  This includes details like the tile's name, type, filters, and other configuration options.  This is the core structure defining the dashboard layout.
        *    `variables`: an optional dictionary of key:value pairs that act as variables within the dashboard template, e.g. allowing the dashboard to be used with different action or event filters.

2.  **`POST /api/projects/:project_id/dashboard_templates/`**

    *   **Method:** `POST`
    *   **Purpose:** Creates a new dashboard template.
    *   **How to Call:**
        *   Replace `:project_id`.
        *   Request Body (JSON): Contains the data for the new template.  The documentation mentions the same fields as in the `GET` response (listed above) are used for creation. The `tiles` array is particularly important, as it defines the structure of dashboards created from this template.
        *   Example: `https://app.posthog.com/api/projects/123/dashboard_templates/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
        * Set `Content-Type`: `application/json`

    *   **Response:** The newly created dashboard template object (same structure as in the `GET` response).

3.  **`GET /api/projects/:project_id/dashboard_templates/:id/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a single dashboard template by its ID.
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   Example: `https://app.posthog.com/api/projects/123/dashboard_templates/456/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** The requested dashboard template object (same structure as in the `GET /.../dashboard_templates/` response).

4.  **`PATCH /api/projects/:project_id/dashboard_templates/:id/`**

    *   **Method:** `PATCH`
    *   **Purpose:** Partially updates an existing dashboard template.
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   Request Body (JSON): Contains *only* the fields you want to update.  You can update any of the fields from the template object (e.g., `template_name`, `dashboard_description`, `tiles`, etc.).
        *   Example: `https://app.posthog.com/api/projects/123/dashboard_templates/456/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
        *   Set `Content-Type`: `application/json`

    *   **Response:** The updated dashboard template object.

5.  **`DELETE /api/projects/:project_id/dashboard_templates/:id/`**

    *   **Method:** `DELETE`
    *   **Purpose:** Deletes a dashboard template.
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   Example: `https://app.posthog.com/api/projects/123/dashboard_templates/456/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** 204 No Content (success).

6.  **`GET /api/projects/:project_id/dashboard_templates/json_schema/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves the JSON Schema for dashboard templates. This schema defines the valid structure and data types for creating and updating templates. It's useful for validating your request bodies before sending them to the API.
    *   **How to Call:**
        *   Replace `:project_id`.
        *   Example: `https://app.posthog.com/api/projects/123/dashboard_templates/json_schema/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** A JSON object representing the JSON Schema.

**Key Takeaways:**

*   The Dashboard Templates API lets you manage reusable dashboard templates.
*   You can create, read (list and individual), update, and delete templates.
*   The `tiles` field within a template object is crucial, defining the structure of dashboards created from the template.
*    You can add variables to templates.
*   The `json_schema` endpoint provides a schema for validating template data.
*   Always use your Personal API Key in the `Authorization` header.

This summary provides a complete breakdown of the Dashboard Templates API based on the documentation and provided endpoint list. It covers the purpose, how to call each endpoint, request/response structures, and key considerations, enabling you to effectively manage dashboard templates.

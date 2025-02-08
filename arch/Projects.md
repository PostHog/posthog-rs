# PostHog Projects API

**Core Concepts (from the URL and endpoints):**

*   **`organization_id`**: The numerical ID of your PostHog *organization*.
*   **`id`**: The numerical ID of a specific *project* within the organization.
*   **Projects**: These endpoints manage projects within an organization.
* **Authentication:** It is not specified how to authorize, but from previous knowledge we can assume it is similar to other endpoints.
*   **Base URL:** `https://[your-posthog-instance]/`

**Endpoint Summary (Grouped by Functionality):**

**1. Core Project Management:**

*   **`GET /api/organizations/:organization_id/projects/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a list of all projects within the specified organization.
    *   **How to Call:**
        *   Replace `:organization_id` with your organization's ID.
        *   Example: `https://app.posthog.com/api/organizations/123/projects/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** A JSON array of project objects. Each object likely includes:
        *   `id`: The numerical ID of the project.
        *   `name`: The name of the project.
        *    `access_control`: Whether project access is enabled.
        *   `organization`: ID of parent organization
        *   `created_at`: Timestamp when created.
        *   API Key and other settings.

*   **`POST /api/organizations/:organization_id/projects/`**

    *   **Method:** `POST`
    *   **Purpose:** Creates a new project within the organization.
    *   **How to Call:**
        *   Replace `:organization_id`.
        *   Request Body (JSON): Contains data for the new project.  Key fields would likely be:
            *   `name`: (Required) The name of the new project.
            *    Other settings.
        *   Example: `https://app.posthog.com/api/organizations/123/projects/`
        *  Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
         *  Set `Content-Type`: `application/json`
    *   **Response:** The newly created project object.

*   **`GET /api/organizations/:organization_id/projects/:id/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a single project by its ID.
    *   **How to Call:**
        *   Replace `:organization_id` and `:id`.
        *   Example: `https://app.posthog.com/api/organizations/123/projects/456/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** The requested project object.

*   **`PATCH /api/organizations/:organization_id/projects/:id/`**

    *   **Method:** `PATCH`
    *   **Purpose:** Partially updates an existing project's settings.
    *   **How to Call:**
        *   Replace `:organization_id` and `:id`.
        *   Request Body (JSON): Contains *only* the fields you want to update (e.g., `name`, API key settings, etc.).
        *   Example: `https://app.posthog.com/api/organizations/123/projects/456/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
         *  Set `Content-Type`: `application/json`
    *   **Response:** The updated project object.

*   **`DELETE /api/organizations/:organization_id/projects/:id/`**

    *   **Method:** `DELETE`
    *   **Purpose:** Deletes a project. This is a *major* operation and should be used with extreme caution, as it will delete all data associated with the project.
    *   **How to Call:**
        *   Replace `:organization_id` and `:id`.
        *   Example: `https://app.posthog.com/api/organizations/123/projects/456/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** 204 No Content (success).

**2. Project Activity and Settings:**

*   **`GET /api/organizations/:organization_id/projects/:id/activity/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves the activity log for a specific project (what changes have been made, who made them, etc.).
    *   **How to Call:**
        *   Replace `:organization_id` and `:id`.
        *   Example: `https://app.posthog.com/api/organizations/123/projects/456/activity/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** (Inferred) A list of activity log entries.

*   **`PATCH /api/organizations/:organization_id/projects/:id/add_product_intent/`**

    *   **Method:** `PATCH`
    *    **Purpose:** Adds product intent.
    *   **How to Call:** Not specified

*  **`POST /api/organizations/:organization_id/projects/:id/change_organization/`**
    *   **Method:** `POST`
    *    **Purpose:** Change organization.
        *    Example: `https://app.posthog.com/api/organizations/123/projects/456/change_organization`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    * **How to Call:** Not specified.

*   **`PATCH /api/organizations/:organization_id/projects/:id/complete_product_onboarding/`**
    *   **Method:** `PATCH`
    *    **Purpose:** Mark the project as complete.
    *   **How to Call:** Not specified.

*    **`GET /api/organizations/:organization_id/projects/:id/is_generating_demo_data/`**
     *   **Method:** `GET`
    *    **Purpose:** Retrieves whether project is generating demo data.
    *   **How to Call:**
        *   Replace `:organization_id` and `:id`.
        *   Example: `https://app.posthog.com/api/organizations/123/projects/456/is_generating_demo_data`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.

*   **`PATCH /api/organizations/:organization_id/projects/:id/reset_token/`**

    *   **Method:** `PATCH`
    *   **Purpose:** Resets the API *token* for a project.  This is a security-sensitive operation, as it will invalidate the old token.
    *   **How to Call:**
        *   Replace `:organization_id` and `:id`.
        *   Request Body: Likely empty.
        *   Example: `https://app.posthog.com/api/organizations/123/projects/456/reset_token/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
         *  Set `Content-Type`: `application/json`
    *   **Response:** (Inferred) The updated project object, likely with a new `token` value.

**Key Takeaways:**

*   The Projects API manages PostHog *projects* within an organization.
*   You can list projects, create new projects, retrieve, update, and delete projects.
*   Deleting a project is a very significant and irreversible operation.
*   You can get the activity log for a project and reset its API token.
*   Always use your Personal API Key in the `Authorization` header.

This summary, based on the provided URL and endpoint list, provides a clear and comprehensive overview of the PostHog Projects API. It explains how to manage the projects within your PostHog organization.

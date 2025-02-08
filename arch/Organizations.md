# PostHog Organizations API

**Core Concepts (from the URL and endpoints):**

*   **`id`**:  In the context of `/api/organizations/:id/`, this refers to the numerical ID of the *organization*. In the context of Batch exports, this refers to the batch export ID.
*    **`organization_id`**: The same as `id`.
*   **Organizations**: These endpoints manage PostHog organizations themselves (creation, modification, deletion).
* **Authentication:** It is not specified how to authorize, but from previous knowledge we can assume it is similar to other endpoints.
*   **Base URL:** `https://[your-posthog-instance]/`

**Endpoint Summary (Grouped by Functionality):**

**1. Core Organization Management:**

*   **`GET /api/organizations/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a list of *all* organizations that the *current user* (based on the API key) has access to.  This is *not* limited to a single organization.
    *   **How to Call:**
        *   No parameters needed (beyond authentication).
        *   Example: `https://app.posthog.com/api/organizations/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** A JSON array of organization objects.  Each object likely includes:
        *   `id`: The numerical ID of the organization.
        *   `name`: The name of the organization.
        *   Other metadata about the organization.

*   **`POST /api/organizations/`**

    *   **Method:** `POST`
    *   **Purpose:** Creates a *new* PostHog organization.
    *   **How to Call:**
        *   Request Body (JSON): Contains data for the new organization. Likely includes:
            *   `name`: (Required) The name of the new organization.
        *   Example: `https://app.posthog.com/api/organizations/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
         *  Set `Content-Type`: `application/json`
    *   **Response:** The newly created organization object.

*   **`GET /api/organizations/:id/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a *single* organization by its ID.
    *   **How to Call:**
        *   Replace `:id` with the organization's ID.
        *   Example: `https://app.posthog.com/api/organizations/123/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** The requested organization object.

*   **`PATCH /api/organizations/:id/`**

    *   **Method:** `PATCH`
    *   **Purpose:** Partially updates an existing organization's information.
    *   **How to Call:**
        *   Replace `:id` with the organization's ID.
        *   Request Body (JSON):  Contains *only* the fields you want to update (e.g., `name`).
        *   Example: `https://app.posthog.com/api/organizations/123/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
         *  Set `Content-Type`: `application/json`
    *   **Response:** The updated organization object.

*   **`DELETE /api/organizations/:id/`**

    *   **Method:** `DELETE`
    *   **Purpose:** Deletes an organization. This is a *major* operation and should be used with extreme caution.
    *   **How to Call:**
        *   Replace `:id` with the organization's ID.
        *   Example: `https://app.posthog.com/api/organizations/123/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** 204 No Content (success).

**2. Batch Exports (Organization Level) - *These are duplicates of endpoints we've seen before*:**

These endpoints *duplicate* functionality from the previous Batch Exports API analysis. It appears these operate at the organization level, while the previously analyzed ones operated at the project level.

*   **`GET /api/organizations/:organization_id/batch_exports/`**
     *   **Method:** `GET`
    *   **Purpose:** Lists Batch Export configurations.
    *   **How to Call:**
        *   Replace `:organization_id` with your organization's ID.
        *  Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** A paginated list of Batch Export configuration objects.

*   **`POST /api/organizations/:organization_id/batch_exports/`**
 *   **Method:** `POST`
    *   **Purpose:** Creates a new Batch Export configuration.
    *   **How to Call:**
        *   Replace `:organization_id` with your organization's ID.
        *  Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
*   **`GET /api/organizations/:organization_id/batch_exports/:id/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a specific Batch Export configuration by its ID.
    *   **How to Call:**
        *   Replace `:organization_id` and `:id`.
        *  Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** The Batch Export configuration object.

*   **`PATCH /api/organizations/:organization_id/batch_exports/:id/`**

    *   **Method:** `PATCH`
    *   **Purpose:** Updates a specific Batch Export configuration.
    *   **How to Call:**
        *   Replace `:organization_id` and `:id`.
        *  Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.

*   **`DELETE /api/organizations/:organization_id/batch_exports/:id/`**
      *   **Method:** `DELETE`
    *   **Purpose:** Deletes a Batch Export configuration.
    *   **How to Call:**
        *   Replace `:organization_id` and `:id`.
        *  Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.

*   **`POST /api/organizations/:organization_id/batch_exports/:id/backfill/`**
     *    **Method:** `POST`
    *   **Purpose:** Triggers the creation of the initial backfill for Batch Exports created with `pause` set to `true`.
        *   Replace `:organization_id` and `:id`.
        *  Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    * **How to Call:** Not specified.

*   **`GET /api/organizations/:organization_id/batch_exports/:id/logs/`**
     *   **Method:** `GET`
    *   **Purpose:** Retrieves logs related to a specific Batch Export configuration.
    *   **How to Call:**
        *   Replace `:organization_id` and `:id`.
        *  Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** A list of log entries.

**Key Takeaways:**

*   The Organizations API manages PostHog *organizations* (the top-level containers).
*   You can list organizations the current user can access, create new organizations, retrieve, update, and delete organizations.
*   Deleting an organization is a very significant operation.
*   The Batch Export endpoints are *duplicates* of functionality we've seen before, but operate at the organization level instead of the project level.
*   Always use your Personal API Key in the `Authorization` header.

This summary, based on the provided URL and endpoint list, provides a clear overview of the PostHog Organizations API.  It highlights the distinction between managing organizations and managing projects within organizations. The duplication of Batch Export endpoints is noted.

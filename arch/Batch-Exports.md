# PostHog Batch Exports API


**Core Concepts (from the URL and endpoints):**

*   **`organization_id`**: The unique identifier of your PostHog *organization*.
*   **`project_id`**: The unique identifier of a PostHog *project*.
*   **`id`**:  This placeholder has multiple meanings depending on the context:
    *   When used directly under `/batch_exports/`, it refers to the ID of a *Batch Export configuration*.
    *   When used under `/backfills/`, it refers to the ID of a *Backfill* (a historical data import).
    *   When used under `/runs/`, it refers to the ID of a *Batch Export Run* (an instance of the export process).
*   **`batch_export_id`**: This specifically refers to the ID of a Batch Export configuration, even when nested under `/projects/`.
*   **Batch Exports**:  These endpoints manage the *continuous* export of data from PostHog to external destinations (e.g., cloud storage, data warehouses).
*   **Backfills**: These deal with importing *historical* data into a Batch Export (filling in data before the continuous export was set up).
*   **Runs**:  These represent individual executions of a Batch Export configuration.
* **Authentication:** It is not specified how to authorize, but from previous knowledge we can assume it is similar to other endpoints.
*   **Base URL:** `https://[your-posthog-instance]/`

**Endpoint Summary (Grouped by Resource):**

I'll group the endpoints by the resource they primarily operate on (Batch Exports, Backfills, Runs) for clarity.

**1. Batch Export Configuration Endpoints (Organization Level):**

These operate at the *organization* level and manage the overall configuration of a Batch Export.

*   **`GET /api/organizations/:organization_id/batch_exports/`**

    *   **Method:** `GET`
    *   **Purpose:** Lists all Batch Export configurations for the given organization.
    *   **How to Call:**
        *   Replace `:organization_id` with your organization's ID.
        *   Example: `https://app.posthog.com/api/organizations/your_org_id/batch_exports/`
        *  Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** A paginated list of Batch Export configuration objects (details below).

*   **`POST /api/organizations/:organization_id/batch_exports/`**

    *   **Method:** `POST`
    *   **Purpose:** Creates a new Batch Export configuration.
    *   **How to Call:**
        *   Replace `:organization_id` with your organization's ID.
        *   Request Body (JSON): Contains the details of the Batch Export configuration.  The documentation specifies numerous required and optional fields, including:
            *   `name`: (Required) A human-readable name for the export.
            *   `destination`: (Required) An object describing the destination (e.g., type, connection details).  Supported destinations and their specific configurations are detailed separately in the documentation (S3, Snowflake, BigQuery, etc.).
            *   `interval`: (Required) How often the export should run ("hourly" or "daily").
            *    `paused`: Whether to create the config as paused.
            *   Other optional fields: `include_events`, `exclude_events`, `transformations` (for data manipulation), etc.
        *   Example: `https://app.posthog.com/api/organizations/your_org_id/batch_exports/`
        *  Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** The newly created Batch Export configuration object.

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
        *   Request Body (JSON): Contains the fields to update.
        *  Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** The updated Batch Export configuration object.

*   **`DELETE /api/organizations/:organization_id/batch_exports/:id/`**

    *   **Method:** `DELETE`
    *   **Purpose:** Deletes a Batch Export configuration.
    *   **How to Call:**
        *   Replace `:organization_id` and `:id`.
        *  Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** 204 No Content.

*   **`POST /api/organizations/:organization_id/batch_exports/:id/backfill/`**
    *    **Method:** `POST`
    *   **Purpose:** Triggers the creation of the initial backfill for Batch Exports created with `pause` set to `true`.
        *   Replace `:organization_id` and `:id`.
        *  Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    * **How to Call:** Not specified.

*   **`POST /api/organizations/:organization_id/batch_exports/:id/pause/`**

    *   **Method:** `POST`
    *   **Purpose:** Pauses a Batch Export configuration (stops scheduled runs).
    *   **How to Call:**
        *   Replace `:organization_id` and `:id`.
        *   Request Body: Expected to be empty.
        *  Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** The updated Batch Export configuration object (with `paused: true`).

*   **`POST /api/organizations/:organization_id/batch_exports/:id/unpause/`**

    *   **Method:** `POST`
    *   **Purpose:** Unpauses a Batch Export configuration (resumes scheduled runs).
    *   **How to Call:**
        *   Replace `:organization_id` and `:id`.
        *   Request Body: Expected to be empty.
        *  Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** The updated Batch Export configuration object (with `paused: false`).

*   **`GET /api/organizations/:organization_id/batch_exports/:id/logs/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves logs related to a specific Batch Export configuration.
    *   **How to Call:**
        *   Replace `:organization_id` and `:id`.
        *  Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** A list of log entries.

**2. Batch Export Configuration Endpoints (Project Level):**

*   **`GET /api/projects/:project_id/batch_exports/`**
*   **Method:** `GET`
    *   **Purpose:** Lists all Batch Export configurations for the given project.
    *   **How to Call:**
        *   Replace `:project_id` with your project's ID.
        *   Example: `https://app.posthog.com/api/projects/your_project_id/batch_exports/`
        *  Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** A paginated list of Batch Export configuration objects (details below).

*   **`POST /api/projects/:project_id/batch_exports/`**
    *   **Method:** `POST`
    *   **Purpose:** Creates a new Batch Export configuration.
    *   **How to Call:**
        *   Replace `:project_id` with your project's ID.
        *   Request Body (JSON): Contains the details of the Batch Export configuration.  The documentation specifies numerous required and optional fields, including:
            *   `name`: (Required) A human-readable name for the export.
            *   `destination`: (Required) An object describing the destination (e.g., type, connection details).  Supported destinations and their specific configurations are detailed separately in the documentation (S3, Snowflake, BigQuery, etc.).
            *   `interval`: (Required) How often the export should run ("hourly" or "daily").
            *    `paused`: Whether to create the config as paused.
            *   Other optional fields: `include_events`, `exclude_events`, `transformations` (for data manipulation), etc.
        *   Example: `https://app.posthog.com/api/projects/your_project_id/batch_exports/`
        *  Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** The newly created Batch Export configuration object.

**3. Backfill Endpoints:**

These manage *historical* data backfills for a specific Batch Export.

*   **`GET /api/projects/:project_id/batch_exports/:batch_export_id/backfills/`**

    *   **Method:** `GET`
    *   **Purpose:** Lists all Backfills for a given Batch Export.
    *   **How to Call:** Replace `:project_id` and `:batch_export_id`.
    *  Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** A list of Backfill objects.

*   **`POST /api/projects/:project_id/batch_exports/:batch_export_id/backfills/`**

    *   **Method:** `POST`
    *   **Purpose:** Creates a new Backfill for a given Batch Export.
    *   **How to Call:**
        *   Replace `:project_id` and `:batch_export_id`.
        *   Request Body (JSON): Contains the details of the Backfill, including the date range to import.
    *  Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** The newly created Backfill object.

*   **`GET /api/projects/:project_id/batch_exports/:batch_export_id/backfills/:id/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a specific Backfill by its ID.
    *   **How to Call:** Replace `:project_id`, `:batch_export_id`, and `:id`.
    *  Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** The Backfill object.

*   **`POST /api/projects/:project_id/batch_exports/:batch_export_id/backfills/:id/cancel/`**

    *   **Method:** `POST`
    *   **Purpose:** Cancels a specific Backfill.
    *   **How to Call:**
        *   Replace `:project_id`, `:batch_export_id`, and `:id`.
        *   Request Body: Expected to be empty.
    *  Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** The updated Backfill object (with a cancelled status).

**4. Batch Export Run Endpoints:**

These deal with individual *executions* of a Batch Export.

*   **`GET /api/projects/:project_id/batch_exports/:batch_export_id/runs/`**

    *   **Method:** `GET`
    *   **Purpose:** Lists all Runs for a given Batch Export.
    *   **How to Call:** Replace `:project_id` and `:batch_export_id`.
    *  Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** A list of Batch Export Run objects.

*   **`GET /api/projects/:project_id/batch_exports/:batch_export_id/runs/:id/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a specific Batch Export Run by its ID.
    *   **How to Call:** Replace `:project_id`, `:batch_export_id`, and `:id`.
    *  Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** The Batch Export Run object.

*   **`POST /api/projects/:project_id/batch_exports/:batch_export_id/runs/:id/cancel/`**

    *   **Method:** `POST`
    *   **Purpose:** Cancels a specific Batch Export Run.
    *   **How to Call:**
        *   Replace `:project_id`, `:batch_export_id`, and `:id`.
        *   Request Body: Expected to be empty.
    *  Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** The updated Batch Export Run object (likely with a cancelled status).

*   **`GET /api/projects/:project_id/batch_exports/:batch_export_id/runs/:id/logs/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves logs for a specific Batch Export Run.
    *   **How to Call:** Replace `:project_id`, `:batch_export_id`, and `:id`.
    *  Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** A list of log entries.

**Key Takeaways and Recommendations:**

*   The Batch Exports API is complex, dealing with configurations, historical backfills, and individual run instances.
*   The API is split between organization-level and project-level endpoints for Batch Export *creation*.
*   Carefully distinguish between `id` and `batch_export_id` placeholders.
*   The documentation provides extensive details on the required and optional fields for creating Batch Export configurations, including destination-specific settings. You *must* consult the full documentation for these details.
*   Always use your Personal API Key in the `Authorization` header, using `Authorization: Bearer YOUR_PERSONAL_API_KEY`.

This is a thorough breakdown based *solely* on the provided information. It highlights the structure, purpose, and (where possible) how to call each endpoint. However, the full documentation is essential for understanding the data structures and specific requirements for each endpoint, especially for `POST` requests that create or modify resources.

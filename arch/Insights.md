# PostHog Insights API

**Core Concepts (from the URL and endpoints):**

*   **`project_id`**: The numerical ID of your PostHog project.
*   **`id`**: The unique identifier of a specific insight.
*   **`insight_id`**: The same as id.
*   **Insights**: These endpoints manage insights, which are visualizations of your data (charts, tables, funnels, etc.).
*   **Trends**: A specific type of insight showing data over time.
*   **Funnels**: Another type of insight, as we've seen before.
* **Authentication:** It is not specified how to authorize, but from previous knowledge we can assume it is similar to other endpoints.
*   **Base URL:** `https://[your-posthog-instance]/`

**Endpoint Summary (Grouped by Functionality):**

I'll group the endpoints for clarity.

**1. Core Insight Management:**

*   **`GET /api/projects/:project_id/insights/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a list of insights within the specified project. Supports pagination and filtering.
    *   **How to Call:**
        *   Replace `:project_id`.
        *   Query Parameters:
            *   `limit`: Pagination - number of results per page.
            *   `offset`: Pagination - starting offset.
            *   `short_id`: filter by short id
            *   `created_by`: filter by user
            *   `search`: Filter by search term.
        *   Example: `https://app.posthog.com/api/projects/123/insights/?limit=50&search=signups`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** A paginated JSON object:
        ```json
        {
          "count": 32, // Total number of insights
          "next": "...", // URL to next page (or null)
          "previous": "...", // URL to previous page (or null)
          "results": [ // Array of insight objects
            {
              // ... details for each insight ...
            }
          ]
        }
        ```
        *   Each insight object in `results` will contain:
            *   `id`: Unique ID.
            *   `short_id`: A short, human-readable ID.
            *   `name`: Name of the insight.
            *   `description`: Description.
            *   `filters`:  The filters defining the data shown in the insight (very important!).
            *   `query`: used to define JSON queries
            *   `order`: The order of the insight.
            *   `deleted`: Boolean if deleted.
            *   `created_by`: Who created it.
            *   `created_at`: Timestamp of creation.
            *   `last_modified_at`: Timestamp of last modification.
            *   `is_sample`: Whether the insight is using sample data.
            *   Many other fields.

*   **`POST /api/projects/:project_id/insights/`**

    *   **Method:** `POST`
    *   **Purpose:** Creates a new insight.
    *   **How to Call:**
        *   Replace `:project_id`.
        *   Request Body (JSON): Contains the data for the new insight. This is a *complex* object, as it defines the entire insight's configuration. Key fields include:
            *   `name`: (Optional) Name of the insight.
            *   `description`: (Optional) Description.
            *   `filters`: (Required) The filters defining the data to display. This will depend on the type of insight (trend, funnel, etc.).
            *  `query`: (Optional) Defines JSON query.
            *   `dashboards`: (Optional) An array of dashboard IDs to associate the insight with.
        *   Example: `https://app.posthog.com/api/projects/123/insights/`
        *  Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
        *  Set `Content-Type`: `application/json`
    *   **Response:** The newly created insight object.

*   **`GET /api/projects/:project_id/insights/:id/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a single insight by its ID.
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   Example: `https://app.posthog.com/api/projects/123/insights/456/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** The requested insight object.

*   **`PATCH /api/projects/:project_id/insights/:id/`**

    *   **Method:** `PATCH`
    *   **Purpose:** Partially updates an existing insight.
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   Request Body (JSON): Contains *only* the fields you want to update.
        *   Example: `https://app.posthog.com/api/projects/123/insights/456/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
        *  Set `Content-Type`: `application/json`
    *   **Response:** The updated insight object.

*   **`DELETE /api/projects/:project_id/insights/:id/`**

    *   **Method:** `DELETE`
    *   **Purpose:** Deletes an insight.
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   Example: `https://app.posthog.com/api/projects/123/insights/456/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** 204 No Content (success).

* **`GET /api/projects/:project_id/insights/:insight_id/sharing/`**

    *   **Method:** `GET`
    *   **Purpose:** (Inferred) Retrieves sharing configuration for a insight.
    *   **How to Call:**
        *   Replace `:project_id` and `:insight_id`.
        *   Example: `https://app.posthog.com/api/projects/123/insights/456/sharing`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** The sharing configuration of the insight.

**2. Activity and Usage:**

*   **`GET /api/projects/:project_id/insights/:id/activity/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves the activity log for a specific insight (who viewed it, when, etc.).
    *   **How to Call:** Replace `:project_id` and `:id`.
    *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.

*   **`POST /api/projects/:project_id/insights/:id/viewed/`**

    *   **Method:** `POST`
    *   **Purpose:** Records that the current user has *viewed* the insight. This is likely used for tracking usage and "last viewed" information.
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   Request Body: Likely empty.
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.

*   **`GET /api/projects/:project_id/insights/activity/`**
    *   **Method:** `GET`
    *   **Purpose:** Retrieves all insight activity.
        *   Replace `:project_id`.
        *   Example: `https://app.posthog.com/api/projects/123/insights/activity`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
*   **`GET /api/projects/:project_id/insights/my_last_viewed/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a list of the insights the *current user* (based on the API key) has most recently viewed.
    *   **How to Call:**
        *   Replace `:project_id`.
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** (Inferred) A list of recently viewed insight objects.

**3. Query Execution and Cancellation:**

*   **`POST /api/projects/:project_id/insights/cancel/`**

    *   **Method:** `POST`
    *   **Purpose:** Cancels a *running* insight query. This is useful if a query is taking too long or is no longer needed.
    *   **How to Call:**
        *   Replace `:project_id`.
        *   Request Body (JSON): Likely requires you to specify the ID or some other identifier of the query to cancel.
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
        *  Set `Content-Type`: `application/json`

*   **`POST /api/projects/:project_id/insights/timing/`**

    *   **Method:** `POST`
    *   **Purpose:** Records timing information for a specific query, without actually running the query.  This seems designed for performance analysis.
    *      **How to Call:** Not specified

**4. Trend Insights:**

*   **`GET /api/projects/:project_id/insights/trend/`**

    *   **Method:** `GET`
    *   **Purpose:** Executes a *trend* query and returns the results. This is for *getting data*, not for managing a saved trend insight.
    *   **How to Call:**
        *   Replace `:project_id`.
        *   Query Parameters:  You'll almost certainly need to provide parameters defining the trend query (events, properties, date range, etc.).  This effectively embeds the query definition in the URL.
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** (Inferred) A JSON object containing the trend data (time series data points).

*   **`POST /api/projects/:project_id/insights/trend/`**

    *   **Method:** `POST`
    *   **Purpose:** Executes a *trend* query, similar to the `GET` version, but allows for a more complex query definition in the request body.
    *   **How to Call:**
        *   Replace `:project_id`.
        *   Request Body (JSON): Contains the data defining the trend query. This will likely be a complex object mirroring the structure used when creating a saved trend insight.
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
        *  Set `Content-Type`: `application/json`
    *   **Response:** (Inferred) A JSON object containing the trend data.

**5. Funnel Insights:**

*   **`GET /api/projects/:project_id/insights/funnel/`**

    *   **Method:** `GET`
    *   **Purpose:** Executes a *funnel* query and returns the results.  Similar to the trend endpoints, this is for getting data, not managing a saved funnel.
    *   **How to Call:**
        *   Replace `:project_id`.
        *   Query Parameters: You'll need to provide parameters defining the funnel (steps, date range, etc.).
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** (Inferred) A JSON object containing the funnel data (conversion rates, step breakdowns, etc.).

*   **`POST /api/projects/:project_id/insights/funnel/`**

    *   **Method:** `POST`
    *   **Purpose:** Executes a *funnel* query, using a request body for a more complex definition (similar to the trend endpoints).
    *   **How to Call:**
        *   Replace `:project_id`.
        *   Request Body (JSON): Contains the data defining the funnel query.
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
         *  Set `Content-Type`: `application/json`
    *   **Response:** (Inferred) A JSON object containing the funnel data.

* **`GET /api/projects/:project_id/insights/funnel/correlation/`**

    *   **Method**: `GET`
    *   **Purpose:** Get correlation information on a funnel.
        *    Example: `https://app.posthog.com/api/projects/123/insights/funnel/correlation`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    * **How to Call:** Not specified

* **`POST /api/projects/:project_id/insights/funnel/correlation/`**
 *   **Method**: `POST`
    *   **Purpose:** Get correlation information on a funnel.
        *    Example: `https://app.posthog.com/api/projects/123/insights/funnel/correlation`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    * **How to Call:** Not specified

**Key Takeaways:**

*   The Insights API is extensive, covering both managing saved insights and executing ad-hoc queries (trends and funnels).
*   Core CRUD operations (create, read, update, delete) are available for saved insights.
*   Separate endpoints exist for executing trend and funnel queries directly, without saving them as insights.
*   Activity tracking and usage information are available.
*   Running queries can be cancelled.
*   Always use your Personal API Key in the `Authorization` header.

This is a very detailed breakdown, grouping endpoints by function and providing inferences where necessary. The full PostHog API documentation is, as always, *essential* for complete details on request/response formats, especially for the complex query definitions (trends and funnels) and the structure of the `filters` object when creating/updating insights. This summary provides a solid foundation for understanding and using the PostHog Insights API.

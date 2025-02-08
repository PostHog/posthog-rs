# PostHog Persons API

**Core Concepts (from the URL and endpoints):**

*   **`project_id`**: The numerical ID of your PostHog project.
*   **`id`**:  The unique identifier of a *specific person*. This is *not* the `distinct_id`, but an internal PostHog ID.
*   **Persons**: These endpoints manage data related to individual users (persons) tracked by PostHog.
* **Authentication:** It is not specified how to authorize, but from previous knowledge we can assume it is similar to other endpoints.
*   **Base URL:** `https://[your-posthog-instance]/`

**Endpoint Summary (Grouped by Functionality):**

**1. Core Person Management:**

*   **`GET /api/projects/:project_id/persons/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a list of persons within the specified project. Supports pagination, filtering, and searching.
    *   **How to Call:**
        *   Replace `:project_id`.
        *   Query Parameters:
            *   `distinct_id`: (Optional) Filters by a specific `distinct_id`.
            *   `search`: (Optional) Searches within person properties.
            *   `limit`: Pagination - number of results per page.
            *   `offset`: Pagination - starting offset.
            *   `format`: (Optional) Specify `csv` to get results in CSV format.
            *    `properties`: Filter by user properties.
        *   Example: `https://app.posthog.com/api/projects/123/persons/?search=john&limit=100`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** A paginated JSON object (or a CSV file if `format=csv`):
        ```json
        {
          "count": 1234, // Total number of persons
          "next": "...", // URL to next page (or null)
          "previous": "...", // URL to previous page (or null)
          "results": [ // Array of person objects
            {
              // ... details for each person ...
            }
          ]
        }
        ```
        *   Each person object in `results` will contain:
            *   `id`:  PostHog's internal ID for the person.
            *   `name`: A generated name for the person (often based on email or distinct ID).
            *   `distinct_ids`: An *array* of distinct IDs associated with this person (a person can have multiple).
            *   `properties`: A dictionary of key-value pairs (the person's properties).
            *   `created_at`: Timestamp of when the person was first seen.
            *   `uuid`: The person's UUID.

*   **`GET /api/projects/:project_id/persons/:id/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a *single person* by their internal PostHog ID.
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   Example:  `https://app.posthog.com/api/projects/123/persons/456/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:**  The requested person object.

*   **`PATCH /api/projects/:project_id/persons/:id/`**

    *   **Method:** `PATCH`
    *   **Purpose:** Partially updates an existing person's information. This is typically used to modify *properties* associated with the person.  You *cannot* change the person's `distinct_ids` via this endpoint.
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   Request Body (JSON): Contains *only* the fields you want to update (typically within the `properties` object).
        *   Example: `https://app.posthog.com/api/projects/123/persons/456/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
         *  Set `Content-Type`: `application/json`
    *   **Response:** The updated person object.

*   **`DELETE /api/projects/:project_id/persons/:id/`**

    *   **Method:** `DELETE`
    *   **Purpose:** Deletes a person and *all associated data* (events, properties, etc.). This is a *major* operation and should be used with extreme caution.
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   Example: `https://app.posthog.com/api/projects/123/persons/456/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** 204 No Content (success).

**2. Activity and Properties:**

*   **`GET /api/projects/:project_id/persons/:id/activity/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a list of activities.
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   Example:  `https://app.posthog.com/api/projects/123/persons/456/activity`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *    **Response:** A list of activities.

*   **`POST /api/projects/:project_id/persons/:id/delete_property/`**

    *   **Method:** `POST`
    *   **Purpose:** Deletes a *specific property* from a person.
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   Request Body (JSON):  Must contain the `key` of the property to delete.  Example: `{"key": "email"}`
        *   Example:  `https://app.posthog.com/api/projects/123/persons/456/delete_property/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
         *  Set `Content-Type`: `application/json`
    *   **Response:** (Inferred) The updated person object (with the property removed).

*   **`GET /api/projects/:project_id/persons/:id/properties_timeline/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a timeline of changes to a person's properties. This shows how the values of properties have changed over time.
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   Example: `https://app.posthog.com/api/projects/123/persons/456/properties_timeline/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** (Inferred) A JSON array of property change events, each with a timestamp and the old/new values.

*   **`POST /api/projects/:project_id/persons/:id/update_property/`**

    *   **Method:** `POST`
    *   **Purpose:** Sets or updates a *specific property* for a person. This is a more direct way to set a single property than using `PATCH`.
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   Request Body (JSON):  Must contain the `key` and `value` of the property to set. Example: `{"key": "plan", "value": "premium"}`
        *   Example: `https://app.posthog.com/api/projects/123/persons/456/update_property/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
         *  Set `Content-Type`: `application/json`
    *   **Response:** (Inferred) The updated person object.
*   **`GET /api/projects/:project_id/persons/activity/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a list of activities.
    *   **How to Call:**
        *   Replace `:project_id`.
        *   Example:  `https://app.posthog.com/api/projects/123/persons/activity`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *    **Response:** A list of activities.

**3. Bulk Operations:**

*   **`POST /api/projects/:project_id/persons/bulk_delete/`**

    *   **Method:** `POST`
    *   **Purpose:** Deletes *multiple* persons in a single request. This is a *very dangerous* operation.
    *   **How to Call:**
        *   Replace `:project_id`.
        *   Request Body (JSON): Likely an array of person IDs to delete.
        *   Example:  `https://app.posthog.com/api/projects/123/persons/bulk_delete/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
         *  Set `Content-Type`: `application/json`
    *   **Response:** (Inferred) A success indicator or a summary of the bulk operation.

**4. Person Splitting:**
* **`POST /api/projects/:project_id/persons/:id/split/`**

    *   **Method:** `POST`
    *   **Purpose:** Split a existing person's into multiple persons.
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   Request Body (JSON): Contains *only* the fields you want to update (typically within the `properties` object).
        *   Example: `https://app.posthog.com/api/projects/123/persons/456/split`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
         *  Set `Content-Type`: `application/json`
    *   **Response:** The updated person object.

**5. Cohorts, Funnels, and Analysis:**

These endpoints seem to provide ways to get *lists of persons* who meet certain criteria (belong to a cohort, are within a funnel stage, etc.).  They are *not* for managing cohorts or funnels themselves.

*   **`GET /api/projects/:project_id/persons/cohorts/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a list of persons, filtered by cohort membership. This likely requires query parameters to specify which cohorts to include.
    *   **How to Call:** Requires further documentation to know the specific query parameters.
    *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.

*   **`GET /api/projects/:project_id/persons/funnel/`**
    *    **GET /api/projects/:project_id/persons/trends/`**
   *    **GET /api/projects/:project_id/persons/lifecycle/`**
    *   **GET /api/projects/:project_id/persons/stickiness/`**

    *   **Method:** `GET`
    *   **Purpose:** Executes various types of analyses (funnels, trends, lifecycle, stickiness) and returns the *list of persons* who are part of the result. These are *not* for managing saved insights, but for ad-hoc analysis.
    *   **How to Call:**
        *   Replace `:project_id`.
        *   Query Parameters: You'll need to provide parameters defining the analysis (funnel steps, trend events, etc.). This is similar to the Insights API's ad-hoc query endpoints.
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** (Inferred) A JSON object containing the analysis results, *including a list of persons* who meet the criteria.

*   **`POST /api/projects/:project_id/persons/funnel/`**
    *   **Method:** `POST`
     *   **Purpose:** Executes a *funnel* query, using a request body for a more complex definition (similar to the trend endpoints).
    *   **How to Call:**
        *   Replace `:project_id`.
        *   Request Body (JSON): Contains the data defining the funnel query.
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
         *  Set `Content-Type`: `application/json`
    *   **Response:** (Inferred) A JSON object containing the funnel data.
* **`GET /api/projects/:project_id/persons/funnel/correlation/`**

    *   **Method**: `GET`
    *   **Purpose:** Get correlation information on a funnel.
        *    Example: `https://app.posthog.com/api/projects/123/persons/funnel/correlation`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    * **How to Call:** Not specified

* **`POST /api/projects/:project_id/persons/funnel/correlation/`**
 *   **Method**: `POST`
    *   **Purpose:** Get correlation information on a funnel.
        *    Example: `https://app.posthog.com/api/projects/123/persons/funnel/correlation`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    * **How to Call:** Not specified

**Key Takeaways:**

*   The Persons API is extensive, providing detailed control over individual user data.
*   Core CRUD operations (read, update, delete) are available, but *creation* is notably absent. Persons are typically created implicitly through event ingestion.
*   You can manage individual properties, get a property change timeline, and delete specific properties.
*   Bulk deletion of persons is possible (use with extreme caution).
*   Several endpoints provide ways to get *lists of persons* based on cohort membership, funnel stages, or analysis results (trends, lifecycle, stickiness). These are *not* for managing the analyses themselves.
*   Always use your Personal API Key in the `Authorization` header.

This is a very detailed analysis, grouping endpoints by functionality and providing inferences where needed. The full PostHog API documentation is *essential* for complete details, especially regarding the various analysis endpoints (funnels, trends, etc.) and their specific query parameters. This summary provides a solid foundation for understanding and using the PostHog Persons API.

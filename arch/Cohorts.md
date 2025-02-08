# PostHog Cohorts API

**Core Concepts (from the URL and endpoints):**

*   **`project_id`**: The numerical ID of your PostHog project.
*   **`id`**: The unique identifier of a specific cohort.
*   **Cohorts**:  These endpoints manage cohorts within a project.  Cohorts are groups of users who share common characteristics or behaviors.
* **Authentication:** It is not specified how to authorize, but from previous knowledge we can assume it is similar to other endpoints.
*   **Base URL:** `https://[your-posthog-instance]/`

**Endpoint Summary:**

1.  **`GET /api/projects/:project_id/cohorts/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a list of cohorts within the specified project.  The documentation indicates support for pagination.
    *   **How to Call:**
        *   Replace `:project_id` with your project's ID.
        *   Pagination parameters:
            *   `limit`: Number of results per page.
            *   `offset`: Starting point for results.
        *   Example: `https://app.posthog.com/api/projects/123/cohorts/?limit=50&offset=100`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** A JSON object:
        ```json
        {
          "count": 23, // Total number of cohorts
          "next": "https://app.posthog.com/api/projects/123/cohorts/?limit=50&offset=150", // URL to the next page (or null)
          "previous": "https://app.posthog.com/api/projects/123/cohorts/?limit=50&offset=50", // URL to the previous page (or null)
          "results": [ // Array of cohort objects
            {
              // ... details for each cohort ...
            }
          ]
        }
        ```
        *   Each cohort object in `results` will contain:
            *   `id`: Unique ID of the cohort.
            *   `name`: Name of the cohort.
            *   `description`: Description of the cohort.
            *   `groups`:  Groups this cohort might belong to.
            *   `deleted`: Boolean indicating if the cohort is deleted.
            *   `filters`: The filters defining the cohort (how users are included).
            *   `is_calculating`: Boolean indicating if PostHog is currently calculating the cohort's membership.
            *   `created_by`:  Information about the user who created the cohort.
            *   `created_at`: Timestamp of creation.
            *   `last_calculation`: Timestamp of the last membership calculation.
            *   `errors_calculating`: Number of errors during calculation.
            *   `count`: (May be null if still calculating) The number of users in the cohort.
            *    `is_static`: Whether the cohort is a static cohort or not

2.  **`POST /api/projects/:project_id/cohorts/`**

    *   **Method:** `POST`
    *   **Purpose:** Creates a new cohort.
    *   **How to Call:**
        *   Replace `:project_id`.
        *   Request Body (JSON):  Contains the data for the new cohort. Key fields include:
            *   `name`: (Required) The name of the cohort.
            *   `description`: (Optional) A description.
            *   `groups`: (Optional)
            *   `is_static`: (Optional, defaults to `false`) Whether to create a static cohort.
            *   `filters`: (Optional) The criteria defining the cohort's membership. This is a complex object, typically defining conditions based on user properties and events. The documentation provides detailed examples.
        *   Example: `https://app.posthog.com/api/projects/123/cohorts/`
        *   Example Request Body (simple):
            ```json
            {
              "name": "Users who signed up in January",
              "description": "All users who signed up during January 2024",
              "filters": {
                "properties": [
                    {"key": "$signup_date", "value": "2024-01-01", "operator": "gte", "type": "person"},
                    {"key": "$signup_date", "value": "2024-01-31", "operator": "lte", "type": "person"}
                ]
               }
            }
            ```
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
         *  Set `Content-Type`: `application/json`
    *   **Response:** The newly created cohort object (same structure as in the `GET` response).

3.  **`GET /api/projects/:project_id/cohorts/:id/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a single cohort by its ID.
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   Example: `https://app.posthog.com/api/projects/123/cohorts/456/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** The requested cohort object.

4.  **`PATCH /api/projects/:project_id/cohorts/:id/`**

    *   **Method:** `PATCH`
    *   **Purpose:** Partially updates an existing cohort.
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   Request Body (JSON): Contains *only* the fields you want to update (e.g., `name`, `description`, `filters`, `is_static`).
        *   Example: `https://app.posthog.com/api/projects/123/cohorts/456/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
        *  Set `Content-Type`: `application/json`
    *   **Response:** The updated cohort object.

5.  **`DELETE /api/projects/:project_id/cohorts/:id/`**

    *   **Method:** `DELETE`
    *   **Purpose:** Deletes a cohort.
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   Example: `https://app.posthog.com/api/projects/123/cohorts/456/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** 204 No Content (success).

6.  **`GET /api/projects/:project_id/cohorts/:id/activity/`**

    *   **Method**: `GET`
    *   **Purpose:** Retrieves a log of relevant activity for this cohort.
        *   Example: `https://app.posthog.com/api/projects/123/cohorts/456/activity/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *    **How to Call:** Not specified.

7.  **`GET /api/projects/:project_id/cohorts/:id/duplicate_as_static_cohort/`**
    * **Method**: `GET`
    *   **Purpose:** Duplicates this cohort turning it into a static cohort.
        *   Example: `https://app.posthog.com/api/projects/123/cohorts/456/duplicate_as_static_cohort/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *    **How to Call:** Not specified.

8.  **`GET /api/projects/:project_id/cohorts/:id/persons/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a list of *persons* (users) who belong to the specified cohort. The documentation explicitly states that this uses the same API as fetching persons in general, but with an added `cohort` filter.
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   This likely supports pagination and filtering, *like the general persons API*.
        *   Example: `https://app.posthog.com/api/projects/123/cohorts/456/persons/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** A paginated list of person objects (similar to what you'd get from the Persons API).

9.  **`GET /api/projects/:project_id/cohorts/activity/`**
    * **Method**: `GET`
    *   **Purpose:** Retrieves a log of relevant activity for all cohorts.
        *   Example: `https://app.posthog.com/api/projects/123/cohorts/activity/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *    **How to Call:** Not specified.

**Key Takeaways:**

*   The Cohorts API lets you manage cohorts (groups of users) programmatically.
*   You can create, read (list and individual), update, and delete cohorts.
*   Creating and updating cohorts involves defining `filters`, which specify the criteria for cohort membership.
*   `GET /.../cohorts/` and `GET /.../cohorts/:id/persons/` support pagination.
*   The `/persons/` endpoint within a cohort context provides a way to get the users within that cohort.
*   Always use your Personal API Key in the `Authorization` header.

This comprehensive summary covers the endpoints based on the provided documentation, including purpose, usage, request/response structures where available, and important considerations. It gives you a strong foundation for working with PostHog cohorts via the API.

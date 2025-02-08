# PostHog Trends API

**Core Concepts (Inferred):**

*   **`project_id`**: The numerical ID of your PostHog project.
*   **Trends**: These endpoints are for running ad-hoc trend queries to get time-series data.
*   **Environment**: There is an option to specify a particular environment.
* **Authentication:** It is not specified how to authorize, but from previous knowledge we can assume it is similar to other endpoints.
*   **Base URL:** `https://[your-posthog-instance]/`

**Endpoint Summary:**

We have two very similar `POST` endpoints:

1.  **`POST /api/environments/:project_id/insights/trend/`**

    *   **Method:** `POST`
    *   **Purpose:** Executes a trend query and returns the results, potentially within a specific environment context.
    *   **How to Call:**
        *   Replace `:project_id`.
        *   Request Body (JSON): Contains the data defining the trend query. This will be a complex object, likely including:
            *   `events`: (Required) An array of events to include in the trend. Each event will likely have an `id` (the event name) and potentially filters.
            *   `properties`: (Optional) Filters to apply to the entire query (e.g., only include data for users with a certain property).
            *   `date_from`: (Optional) The start date for the trend.
            *   `date_to`: (Optional) The end date for the trend.
            *   `interval`: (Optional) The time interval (e.g., "day", "week", "month").
            *   Many other possible parameters for controlling the trend calculation (breakdowns, aggregations, etc.).
        *   Example: `https://app.posthog.com/api/environments/123/insights/trend/`
        *  Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
        *  Set `Content-Type`: `application/json`
    *   **Response:** (Inferred) A JSON object containing the trend data. This will likely be an array of data points, each with a timestamp and the corresponding value(s) for the trend.

2.  **`POST /api/projects/:project_id/insights/trend/`**

    *   **Method:** `POST`
    *   **Purpose:** Executes a trend query and returns the results. This is the standard way to run a trend query, without any environment-specific context.
    *   **How to Call:** This is *identical* in structure to the previous endpoint, *except* for the base URL.
        *   Replace `:project_id`.
        *   Request Body (JSON): Same structure as above (events, properties, date range, interval, etc.).
        *   Example: `https://app.posthog.com/api/projects/123/insights/trend/`
        *  Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
        *  Set `Content-Type`: `application/json`
    *   **Response:** (Inferred) A JSON object containing the trend data (same structure as above).

**Key Takeaways and Differences:**

*   Both endpoints execute trend queries. The key difference is the URL: one includes `/environments/`, the other doesn't.
*   The `/environments/` version might be for running queries specific to a particular environment.
*   The request body structure is likely *identical* for both endpoints.
*   These endpoints are for *getting data*, not for managing saved trend insights.
*   Always use your Personal API Key in the `Authorization` header.

This summary is based on limited information (two `POST` endpoints). The actual PostHog API documentation for Trends is *crucial* for understanding:

1.  The *complete* structure of the request body (all available parameters and options).
2.  The precise format of the response data.
3.  The exact difference between the `/environments/` and non-`/environments/` versions.

This response provides the best possible interpretation given the limited endpoint list, but it is *highly incomplete* without the full documentation. It emphasizes that these endpoints are for ad-hoc trend queries, not for managing saved insights.

# PostHog Funnels API

**Core Concepts (Inferred):**

*   **`project_id`**: The numerical ID of the PostHog project.
*   **Funnels**: These endpoints are for *creating* funnels. Funnels are a way to visualize and analyze a series of steps users take in your application (e.g., signup process, purchase flow).  We don't have endpoints here for *retrieving* or *updating* existing funnels, only for creating them.
* **Authentication:** It is not specified how to authorize, but from previous knowledge we can assume it is similar to other endpoints.
*   **Base URL:** `https://[your-posthog-instance]/`

**Endpoint Summary:**

We have two very similar `POST` endpoints:

1.  **`POST /api/environments/:project_id/insights/funnel/`**

    *   **Method:** `POST`
    *   **Purpose:** Creates a new funnel within a project, apparently in an "environments" context. This might be for creating funnels specific to a particular environment (e.g., staging vs. production).
    *   **How to Call:**
        *   Replace `:project_id` with your project's ID.
        *   Request Body (JSON): Contains the data defining the funnel. This will likely include:
            *   `name`: (Required) The name of the funnel.
            *   `steps`: (Required) An array of steps defining the funnel. Each step will likely be defined by:
                *   `event`: The event name that represents the step (e.g., "page_viewed", "button_clicked").
                *   `properties`: (Optional) Filters to apply to the step (e.g., only count "page_viewed" events where `page_url` is "/pricing").
            *   Other optional parameters (date range, filters, etc.).
        *   Example: `https://app.posthog.com/api/environments/123/insights/funnel/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** (Inferred) The newly created funnel object.

2.  **`POST /api/projects/:project_id/insights/funnel/`**

    *   **Method:** `POST`
    *   **Purpose:** Creates a new funnel within a project. This is the standard way to create a funnel, without any environment-specific context.
    *   **How to Call:** This is identical in structure to the previous endpoint, *except* for the base URL.
        *   Replace `:project_id` with your project's ID.
        *   Request Body (JSON):  Same structure as above (name, steps, etc.).
        *   Example: `https://app.posthog.com/api/projects/123/insights/funnel/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** (Inferred) The newly created funnel object.

**Key Takeaways and Differences:**

*   Both endpoints create funnels.  The key difference is the URL: one includes `/environments/`, the other doesn't.
*   The `/environments/` version might be for creating environment-specific funnels (this is an inference).
*   The request body structure is likely *identical* for both endpoints.
*   We *only* have endpoints for *creating* funnels here, not for listing, retrieving, updating, or deleting them. This is a very limited view of a Funnels API.
*   Always use your Personal API Key in the `Authorization` header.
*   The official Posthog documentation is necessary for more information.

This summary is based on very limited information (two `POST` endpoints).  The actual PostHog API documentation for Funnels is crucial for understanding:

1.  The exact structure of the request body (especially the `steps` array).
2.  How to retrieve, update, and delete existing funnels (these endpoints are missing).
3.  The precise difference between the `/environments/` and non-`/environments/` versions.
4.  Available filtering and configuration options for funnels.

This response provides the best possible interpretation given the limited endpoint list, but it is *highly incomplete* without the full documentation.

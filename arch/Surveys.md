# PostHog Surveys API

**Core Concepts (from the URL and endpoints):**

*   **`project_id`**: The numerical ID of your PostHog project.
*   **`id`**: The unique identifier of a specific survey.
*   **Surveys**: These endpoints allow you to create, manage, and analyze surveys within PostHog.
* **Authentication:** It is not specified how to authorize, but from previous knowledge we can assume it is similar to other endpoints.
*   **Base URL:** `https://[your-posthog-instance]/`

**Endpoint Summary (Grouped by Functionality):**

**1. Core Survey Management:**

*   **`GET /api/projects/:project_id/surveys/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a list of all surveys within the specified project. Likely supports pagination.
    *   **How to Call:**
        *   Replace `:project_id`.
        *   Pagination parameters (likely): `limit`, `offset`.
        *   Example: `https://app.posthog.com/api/projects/123/surveys/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** A paginated JSON object with the usual structure:
         ```json
        {
          "count": 1234, // Total number of surveys
          "next": "...", // URL to next page (or null)
          "previous": "...", // URL to previous page (or null)
          "results": [ // Array of survey objects
            {
              // ... details for each survey ...
            }
          ]
        }
        ```
        *   Each survey object in `results` will contain:
            *   `id`: Unique ID.
            *   `name`: The name of the survey.
            *   `description`: A description.
            *    `type`: Type of survey
            *   `conditions`: Conditions for when to display the survey
            *  `questions`: An array of survey questions
            *   `linked_flag`: The linked flag, if any.
            *   `created_by`: Who created it.
            *   `created_at`: Timestamp of creation.
            *    `start_date`: Start date
            *    `end_date`: End date
            *    `archived`: Whether it is archived.
            *    `estimated_completion_time`: How long to complete

*   **`POST /api/projects/:project_id/surveys/`**

    *   **Method:** `POST`
    *   **Purpose:** Creates a new survey.
    *   **How to Call:**
        *   Replace `:project_id`.
        *   Request Body (JSON): Contains the data for the new survey. This will be a complex object, likely including:
            *   `name`: (Required) The name of the survey.
            *   `description`: (Optional) A description.
            *  `type`: (Optional) Type of survey.
            *   `conditions`: (Required) Conditions for displaying the survey (e.g., targeting specific users or events).
            *   `questions`: (Required) An array of questions, each with its own type, text, and options (if applicable).
        *   Example: `https://app.posthog.com/api/projects/123/surveys/`
        *  Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
         *  Set `Content-Type`: `application/json`
    *   **Response:** The newly created survey object.

*   **`GET /api/projects/:project_id/surveys/:id/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a single survey by its ID.
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   Example: `https://app.posthog.com/api/projects/123/surveys/456/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** The requested survey object.

*   **`PATCH /api/projects/:project_id/surveys/:id/`**

    *   **Method:** `PATCH`
    *   **Purpose:** Partially updates an existing survey.
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   Request Body (JSON): Contains *only* the fields you want to update (e.g., `name`, `description`, `conditions`, `questions`).
        *   Example: `https://app.posthog.com/api/projects/123/surveys/456/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
         *  Set `Content-Type`: `application/json`
    *   **Response:** The updated survey object.

*   **`DELETE /api/projects/:project_id/surveys/:id/`**

    *   **Method:** `DELETE`
    *   **Purpose:** Deletes a survey.
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   Example: `https://app.posthog.com/api/projects/123/surveys/456/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** 204 No Content (success).

**2. Activity and Responses:**

*   **`GET /api/projects/:project_id/surveys/:id/activity/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves the activity log for a specific survey (changes, who made them, etc.).
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   Example: `https://app.posthog.com/api/projects/123/surveys/456/activity/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** (Inferred) A list of activity log entries.

*   **`POST /api/projects/:project_id/surveys/:id/summarize_responses/`**

    *   **Method:** `POST`
    *   **Purpose:** (Inferred) Triggers a process to summarize the responses to the survey. This likely generates aggregated data and statistics.
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   Request Body: Likely empty or contains options for the summarization.
        *   Example: `https://app.posthog.com/api/projects/123/surveys/456/summarize_responses/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** (Inferred)  Details about the summarization process or a success indicator.

* **`GET /api/projects/:project_id/surveys/activity/`**
 *   **Method:** `GET`
    *   **Purpose:** Retrieves all activities.
    *   **How to Call:**
        *   Replace `:project_id`.
        *   Example: `https://app.posthog.com/api/projects/123/surveys/activity`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.

*   **`GET /api/projects/:project_id/surveys/responses_count/`**
     *   **Method:** `GET`
    *   **Purpose:** Returns total and triggered responses.
        *   Replace `:project_id`.
        *   Example: `https://app.posthog.com/api/projects/123/surveys/responses_count`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *    **How to Call:** Not specified.

**Key Takeaways:**

*   The Surveys API allows you to manage surveys within a PostHog project.
*   You can create, list, retrieve, update, and delete surveys.
*   Creating and updating surveys involves defining `conditions` (for targeting) and `questions`.
*   You can get the activity log for a survey and trigger response summarization.
*   Always use your Personal API Key in the `Authorization` header.

This is a comprehensive overview of the PostHog Surveys API based on the provided list and URL, explaining how to create, manage, and analyze surveys within your projects.

# PostHog Subscriptions API

**Core Concepts (from the URL and endpoints):**

*   **`project_id`**: The numerical ID of your PostHog project.
*   **`id`**: The unique identifier of a specific subscription.
*   **Subscriptions**: These endpoints manage subscriptions to reports and alerts.  This allows users to receive regular updates (e.g., via email or Slack) about specific insights or when certain conditions are met.
* **Authentication:** It is not specified how to authorize, but from previous knowledge we can assume it is similar to other endpoints.
*   **Base URL:** `https://[your-posthog-instance]/`

**Endpoint Summary:**

1.  **`GET /api/projects/:project_id/subscriptions/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a list of all subscriptions within the specified project. Likely supports pagination.
    *   **How to Call:**
        *   Replace `:project_id`.
        *   Pagination parameters (likely): `limit`, `offset`.
        *   Example: `https://app.posthog.com/api/projects/123/subscriptions/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** A JSON object with the usual paginated structure:
        ```json
         {
            "count": 1,
            "next": "string",
            "previous": "string",
            "results": [
              {
                //Subscription Object
              }
            ]
         }
        ```
        *   Each subscription object in `results` will likely contain:
            *   `id`: Unique ID of the subscription.
            *   `target_type`:  The type of delivery mechanism (e.g., "email", "slack").
            *   `target_value`: The destination (e.g., email address, Slack channel webhook URL).
            *   `frequency`: How often the subscription delivers updates (e.g., "daily", "weekly", "monthly").
            *   `interval`: The numeric interval, when custom
            *   `start_date`: When the subscription starts.
            *   `end_date`: (Optional) When the subscription ends.
            *   `byweekday`: Which days an insight gets delivered
            *   `bysetpos`: position in set
            *   `count`: limit for notifications
            *   `summary`: whether to send a summary
            *   `insight`:  Information about the insight this subscription is for (if applicable).
            *   `dashboard`: Information about the dashboard this subscription is for (if applicable)
            *   `created_by`: Who created the subscription.
            *   `created_at`: Timestamp of creation.
            *   Other metadata.

2.  **`POST /api/projects/:project_id/subscriptions/`**

    *   **Method:** `POST`
    *   **Purpose:** Creates a new subscription.
    *   **How to Call:**
        *   Replace `:project_id`.
        *   Request Body (JSON): Contains the data for the new subscription. Key fields likely include:
            *   `target_type`: (Required) The delivery type ("email", "slack", etc.).
            *   `target_value`: (Required) The destination.
            *   `frequency`: (Required) How often to send updates.
            *    `interval`: (Optional)
            *   `start_date`: (Optional)
            *   `end_date`: (Optional)
            *   `byweekday`: (Optional)
            *    `bysetpos`: (Optional)
            *    `count`: (Optional)
            *   `insight`: (Optional) The ID of the insight to subscribe to.  *Either* `insight` *or* `dashboard` should be provided, but not both.
            *   `dashboard`: (Optional) The ID of the dashboard to subscribe to.
        *   Example: `https://app.posthog.com/api/projects/123/subscriptions/`
        *  Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
         *  Set `Content-Type`: `application/json`
    *   **Response:** The newly created subscription object.

3.  **`GET /api/projects/:project_id/subscriptions/:id/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a single subscription by its ID.
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   Example:  `https://app.posthog.com/api/projects/123/subscriptions/456/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** The requested subscription object.

4.  **`PATCH /api/projects/:project_id/subscriptions/:id/`**

    *   **Method:** `PATCH`
    *   **Purpose:** Partially updates an existing subscription.  This could be used to change the frequency, destination, or other settings.
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   Request Body (JSON): Contains *only* the fields you want to update.
        *   Example: `https://app.posthog.com/api/projects/123/subscriptions/456/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
         *  Set `Content-Type`: `application/json`
    *   **Response:** The updated subscription object.

5.  **`DELETE /api/projects/:project_id/subscriptions/:id/`**

    *   **Method:** `DELETE`
    *   **Purpose:** Deletes a subscription.
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   Example:  `https://app.posthog.com/api/projects/123/subscriptions/456/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** 204 No Content (success).

**Key Takeaways:**

*   The Subscriptions API manages subscriptions to reports and alerts within a PostHog project.
*   You can list, create, retrieve, update, and delete subscriptions.
*   Subscriptions are linked to either an `insight` or a `dashboard`.
*   Key settings include `target_type`, `target_value`, and `frequency`.
*   Always use your Personal API Key in the `Authorization` header.

This summary, based on the provided list and URL, explains the purpose and usage of the PostHog Subscriptions API, allowing you to manage automated reporting and alerting within your projects.

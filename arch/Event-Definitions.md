# PostHog Event Definitions API

**Core Concepts (Inferred):**

*   **`project_id`**: The numerical ID of the PostHog project.
*   **`id`**: The unique identifier of a specific event definition.
*   **Event Definitions**: These endpoints manage metadata about the different types of events that PostHog tracks. This is *not* about the event data itself, but about the *definitions* of the events.
* **Authentication:** It is not specified how to authorize, but from previous knowledge we can assume it is similar to other endpoints.
*   **Base URL:** `https://[your-posthog-instance]/`

**Endpoint Summary:**

1.  **`GET /api/projects/:project_id/event_definitions/`**

    *   **Method:** `GET`
    *   **Purpose:** (Inferred) Retrieves a list of event definitions within the specified project. Likely supports pagination.
    *   **How to Call:**
        *   Replace `:project_id` with your project's ID.
        *   Example: `https://app.posthog.com/api/projects/123/event_definitions/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** (Inferred) A JSON array of event definition objects. Each object likely includes:
        *   `id`: Unique ID of the event definition.
        *   `name`: The name of the event (e.g., "page_viewed", "button_clicked").
        *   Other metadata (description, tags, owner, etc.).

2.  **`GET /api/projects/:project_id/event_definitions/:id/`**

    *   **Method:** `GET`
    *   **Purpose:** (Inferred) Retrieves a single event definition by its ID.
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   Example: `https://app.posthog.com/api/projects/123/event_definitions/456/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** (Inferred) The requested event definition object.

3.  **`PATCH /api/projects/:project_id/event_definitions/:id/`**

    *   **Method:** `PATCH`
    *   **Purpose:** (Inferred) Partially updates an existing event definition. This likely allows you to modify metadata like the description or tags associated with an event *definition*, not the event data itself.
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   Request Body (JSON): Contains *only* the fields you want to update (e.g., `description`, `tags`).
        *   Example: `https://app.posthog.com/api/projects/123/event_definitions/456/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** (Inferred) The updated event definition object.

4.  **`DELETE /api/projects/:project_id/event_definitions/:id/`**

    *   **Method:** `DELETE`
    *   **Purpose:** (Inferred) Deletes an event definition.  This likely *doesn't* delete the actual event data, but removes the *definition* of the event. This should probably be used with caution.
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   Example: `https://app.posthog.com/api/projects/123/event_definitions/456/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** (Inferred) 204 No Content (success).

**Key Takeaways:**

*   The Event Definitions API manages *metadata* about event types, not the event data itself.
*   You can retrieve (list and individual), update, and delete event definitions.
*   `DELETE` likely removes the definition, not the event data, requiring caution.
*   Always use your Personal API Key in the `Authorization` header.
*  The official Posthog Event Definitions documentation is needed for complete information.

This summary is based on standard API conventions and the provided list. The actual PostHog API documentation for Event Definitions is necessary for complete usage details, especially concerning the implications of deleting an event definition.

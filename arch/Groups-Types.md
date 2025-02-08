# PostHog Group Types API 

**Core Concepts (from the URL and endpoints):**

*   **`project_id`**: The numerical ID of your PostHog project.
*   **Group Types**:  These endpoints manage the *definitions* of group types within a project (e.g., "company", "account").  You use these endpoints to define the different categories of groups you want to use.
* **Authentication:** It is not specified how to authorize, but from previous knowledge we can assume it is similar to other endpoints.
*   **Base URL:** `https://[your-posthog-instance]/`

**Endpoint Summary:**

1.  **`GET /api/projects/:project_id/groups_types/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a list of all defined group types within the specified project.  This does *not* list the groups themselves, only the *types* of groups.
    *   **How to Call:**
        *   Replace `:project_id` with your project's ID.
        *   Example: `https://app.posthog.com/api/projects/123/groups_types/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** A JSON array of group type objects.  Each object will likely include:
        *   `group_type`: The name of the group type (e.g., "company").
        *   `group_type_index`: The numerical index of this group type (used in other API calls).
        *   `name_singular`: The singular form (e.g., "company").
        *   `name_plural`: Plural form (e.g., "companies").
        *   `description`: A description of this group type.
        *   Timestamps (`created_at`, etc.).

2.  **`PATCH /api/projects/:project_id/groups_types/update_metadata/`**

    *   **Method:** `PATCH`
    *   **Purpose:** Updates the *metadata* of group types. This allows you to modify things like the singular/plural names and descriptions of group types.
    *   **How to Call:**
        *   Replace `:project_id`.
        *   Request Body (JSON): Contains the changes you want to make. The documentation specifies that you should send an array of objects, where each object represents a group type to update. Each object in the array should include:
            *   `group_type_index`: (Required) The index of the group type to update.
            *   `name_singular`: (Optional) New singular name.
            *   `name_plural`: (Optional) New plural name.
            *   `description`: (Optional) New description.
        *   Example: `https://app.posthog.com/api/projects/123/groups_types/update_metadata/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
        *   Set `Content-Type`: `application/json`
    *   **Response:** The documentation doesn't specify, but it's likely a success indicator (e.g., 200 OK) or perhaps the updated group type objects.

**Key Takeaways:**

*   The Group Types API is for managing the *definitions* of group types (categories), not the groups themselves.
*   You can list all defined group types and update their metadata (names, descriptions).
*   The `group_type_index` is a key identifier for group types, used in other API calls.
*   Always use your Personal API Key in the `Authorization` header.

This summary, based on the provided list and URL, explains the purpose and usage of the PostHog Group Types API endpoints. It clarifies the distinction between managing group *types* and managing the *groups* themselves.

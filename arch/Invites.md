# PostHog Invites API

**Core Concepts (from the URL and endpoints):**

*   **`organization_id`**: The numerical ID of your PostHog *organization*. This is different from the `project_id`.
*   **`id`**: The unique identifier of a specific invitation.
*   **Invites**: These endpoints manage invitations to join an organization.
* **Authentication:** It is not specified how to authorize, but from previous knowledge we can assume it is similar to other endpoints.
*   **Base URL:** `https://[your-posthog-instance]/`

**Endpoint Summary:**

1.  **`GET /api/organizations/:organization_id/invites/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a list of all pending (and possibly expired) invitations for the specified organization.
    *   **How to Call:**
        *   Replace `:organization_id` with your organization's ID.
        *   Example: `https://app.posthog.com/api/organizations/123/invites/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** A JSON array of invitation objects. Each object likely includes:
        *   `id`: Unique ID of the invitation.
        *   `target_email`: The email address the invitation was sent to.
        *   `first_name`: First name provided
        *    `emailing_attempt_made`: Whether an invite was attempted
        *   `is_expired`: Whether the invitation has expired.
        *   `created_by`: Who created the invitation.
        *   `created_at`: Timestamp of creation.
        *   `message`: (Optional) A custom message included in the invitation.

2.  **`POST /api/organizations/:organization_id/invites/`**

    *   **Method:** `POST`
    *   **Purpose:** Creates a new invitation to join the organization.
    *   **How to Call:**
        *   Replace `:organization_id`.
        *   Request Body (JSON): Contains data for the new invitation. Key fields include:
            *   `target_email`: (Required) The email address to send the invitation to.
            *   `first_name`: (Optional) The first name of the invitee (if known).
            *   `message`: (Optional) A custom message to include in the invitation email.
        *   Example: `https://app.posthog.com/api/organizations/123/invites/`
        *  Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
        *  Set `Content-Type`: `application/json`
    *   **Response:** The newly created invitation object.

3.  **`DELETE /api/organizations/:organization_id/invites/:id/`**

    *   **Method:** `DELETE`
    *   **Purpose:** Deletes (revokes) a pending invitation.
    *   **How to Call:**
        *   Replace `:organization_id` and `:id`.
        *   Example: `https://app.posthog.com/api/organizations/123/invites/456/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** 204 No Content (success).

4.  **`POST /api/organizations/:organization_id/invites/bulk/`**

    *   **Method:** `POST`
    *   **Purpose:** Creates multiple invitations in a single request (bulk creation).
    *   **How to Call:**
        *   Replace `:organization_id`.
        *   Request Body (JSON): An *array* of invitation objects.  Each object in the array has the same structure as the request body for the single `POST` endpoint (`target_email`, `first_name`, `message`).
        *   Example: `https://app.posthog.com/api/organizations/123/invites/bulk/`
        *  Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
         *  Set `Content-Type`: `application/json`
    *   **Response:** The documentation doesn't specify, but it's likely an array of the newly created invitation objects, or perhaps a summary of the bulk operation.

**Key Takeaways:**

*   The Invites API manages invitations to join a PostHog *organization*.
*   You can list pending invitations, create single invitations, create invitations in bulk, and delete (revoke) invitations.
*   The `target_email` is the key field for creating invitations.
*   Always use your Personal API Key in the `Authorization` header.

This summary, based on the provided list and URL, covers the core functionality of the PostHog Invites API. It explains how to manage invitations to your organization.

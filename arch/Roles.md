# PostHog Roles API

**Core Concepts (from the URL and endpoints):**

*   **`organization_id`**: The numerical ID of your PostHog *organization*.
*   **`id`**: This has multiple meanings, depending on context:
    *   Under `/roles/`, it refers to the ID of a *role*.
    *   Under `/role_memberships/`, it refers to the ID of a *role membership* (the association between a user and a role).
*   **Roles**: These endpoints manage roles within an organization (e.g., "Admin," "Member," "Analyst"). Roles define sets of permissions.
*   **Role Memberships**: These endpoints manage the assignment of users to roles.
* **Authentication:** It is not specified how to authorize, but from previous knowledge we can assume it is similar to other endpoints.
*   **Base URL:** `https://[your-posthog-instance]/`

**Endpoint Summary (Grouped by Functionality):**

**1. Role Management:**

*   **`GET /api/organizations/:organization_id/roles/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a list of all roles defined within the specified organization.
    *   **How to Call:**
        *   Replace `:organization_id` with your organization's ID.
        *   Example: `https://app.posthog.com/api/organizations/123/roles/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** A JSON array of role objects. Each object likely includes:
        *   `id`: Unique ID of the role.
        *   `name`: The name of the role (e.g., "Admin").
        *    `feature_flags_access_level`: Access level.
        *   `created_by`: who created
        *    `created_at`: when created
        *   Other metadata about the role (permissions associated with it, etc.).

*   **`POST /api/organizations/:organization_id/roles/`**

    *   **Method:** `POST`
    *   **Purpose:** Creates a new role within the organization.
    *   **How to Call:**
        *   Replace `:organization_id`.
        *   Request Body (JSON): Contains the data for the new role. Key fields:
            *   `name`: (Required) The name of the new role.
        *   Example: `https://app.posthog.com/api/organizations/123/roles/`
        *  Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
         *  Set `Content-Type`: `application/json`
    *   **Response:** The newly created role object.

*   **`GET /api/organizations/:organization_id/roles/:id/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a single role by its ID.
    *   **How to Call:**
        *   Replace `:organization_id` and `:id`.
        *   Example: `https://app.posthog.com/api/organizations/123/roles/456/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** The requested role object.

*   **`PATCH /api/organizations/:organization_id/roles/:id/`**

    *   **Method:** `PATCH`
    *   **Purpose:** Partially updates an existing role. This could be used to change the role's name or associated permissions.
    *   **How to Call:**
        *   Replace `:organization_id` and `:id`.
        *   Request Body (JSON): Contains *only* the fields you want to update.
        *   Example: `https://app.posthog.com/api/organizations/123/roles/456/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
         *  Set `Content-Type`: `application/json`
    *   **Response:** The updated role object.

*   **`DELETE /api/organizations/:organization_id/roles/:id/`**

    *   **Method:** `DELETE`
    *   **Purpose:** Deletes a role. This will likely also remove all role memberships associated with the role.
    *   **How to Call:**
        *   Replace `:organization_id` and `:id`.
        *   Example: `https://app.posthog.com/api/organizations/123/roles/456/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** 204 No Content (success).

**2. Role Membership Management:**

*   **`GET /api/organizations/:organization_id/roles/:role_id/role_memberships/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a list of all *role memberships* for a specific role. This shows which users are assigned to the given role.
    *   **How to Call:**
        *   Replace `:organization_id` and `:role_id` (the ID of the *role*, not the membership).
        *   Example: `https://app.posthog.com/api/organizations/123/roles/456/role_memberships/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** A JSON array of role membership objects. Each object likely includes:
        *   `id`: Unique ID of the *role membership* (not the role or user ID).
        *   `user`: An object representing the user who is assigned to the role (including their `uuid`).
        *   `role_id`: id of the role
        *   `joined_at`: when did they join
        *   Other metadata.

*   **`POST /api/organizations/:organization_id/roles/:role_id/role_memberships/`**

    *   **Method:** `POST`
    *   **Purpose:** Assigns a user to a role (creates a new role membership).
    *   **How to Call:**
        *   Replace `:organization_id` and `:role_id`.
        *   Request Body (JSON):  Contains the `user_uuid` of the user to assign to the role. Example: `{"user_uuid": "abc-xyz-123"}`
        *   Example: `https://app.posthog.com/api/organizations/123/roles/456/role_memberships/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
         *  Set `Content-Type`: `application/json`
    *   **Response:** The newly created role membership object.

*   **`DELETE /api/organizations/:organization_id/roles/:role_id/role_memberships/:id/`**

    *   **Method:** `DELETE`
    *   **Purpose:** Removes a user from a role (deletes a role membership).
    *   **How to Call:**
        *   Replace `:organization_id`, `:role_id`, and `:id` (the ID of the *role membership*, not the user or role).
        *   Example: `https://app.posthog.com/api/organizations/123/roles/456/role_memberships/789/` (removes the role membership with ID 789)
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** 204 No Content (success).

**Key Takeaways:**

*   The Roles API manages roles and role memberships within a PostHog organization, enabling RBAC.
*   You can list, create, update, and delete roles.
*   You can list role memberships for a specific role, assign users to roles, and remove users from roles.
*   Carefully distinguish between the `id` of a role and the `id` of a role membership.
*   Always use your Personal API Key in the `Authorization` header.

This summary, based on the provided URL and endpoint list, provides a clear explanation of how to use the PostHog Roles API to manage roles and user assignments within your organization.

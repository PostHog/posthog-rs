# PostHog Groups API

**Core Concepts (from the URL and endpoints):**

*   **`project_id`**: The numerical ID of your PostHog project.
*   **Groups**: These endpoints deal with *groups* within PostHog. Groups are a way to aggregate data based on shared characteristics, *other than individual users*.  Examples include companies, accounts, organizations, or any other custom grouping you define.
*   **Group Types**: PostHog supports multiple *types* of groups (e.g., "company", "account").  You define these group types yourself.
*   **Group Keys**: Within each group type, individual groups are identified by a *key* (e.g., the company ID, the account name).
* **Authentication:** It is not specified how to authorize, but from previous knowledge we can assume it is similar to other endpoints.
*   **Base URL:** `https://[your-posthog-instance]/`

**Endpoint Summary:**

1.  **`GET /api/projects/:project_id/groups/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a list of groups within the specified project. The documentation indicates that this endpoint supports filtering by group type and pagination.
    *   **How to Call:**
        *   Replace `:project_id` with your project's ID.
        *   Query Parameters:
            *   `group_type_index`: (Required) The numerical index of the group type you want to retrieve (e.g., 0 for the first group type, 1 for the second, etc.). You need to know which index corresponds to which group type in your project.
            *   `search`: (Optional) Filters groups by searching within their properties.
            *   `limit`: (Optional) Pagination - number of results per page.
            *   `offset`: (Optional) Pagination - starting offset.
        *   Example: `https://app.posthog.com/api/projects/123/groups/?group_type_index=0&search=Acme&limit=50`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** A JSON object with the usual paginated structure:
        ```json
        {
          "next": "...", // URL to next page (or null)
          "previous": "...", // URL to previous page (or null)
          "results": [ // Array of group objects
            {
              // ... details for each group ...
            }
          ]
        }
        ```
        *   Each group object in `results` will contain:
            *   `group_type_index`: The index of the group type.
            *   `group_key`: The unique key for this group within its type.
            *   `group_properties`: A dictionary of key-value pairs (the group's properties).
            *   `created_at`: Timestamp of creation.

2.  **`GET /api/projects/:project_id/groups/find/`**

    *   **Method:** `GET`
    *   **Purpose:** Finds a *single* group based on its type and key.
    *   **How to Call:**
        *   Replace `:project_id`.
        *   Query Parameters:
            *   `group_type_index`: (Required) The index of the group type.
            *   `group_key`: (Required) The key of the group you want to find.
        *   Example: `https://app.posthog.com/api/projects/123/groups/find/?group_type_index=0&group_key=acme_corp`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** A *single* group object (same structure as in the `GET /.../groups/` response).  If no group is found with the given type and key, the API likely returns a 404 Not Found error.

3.  **`GET /api/projects/:project_id/groups/property_definitions/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves the *definitions* of the properties that can be associated with groups. This is about the *metadata* of the properties (names, data types), not the actual property values.
    *   **How to Call:**
        *   Replace `:project_id`.
        *    Query Parameters:
            * `group_type_index`: (Required) Specifies for which group to grab property definitions.
        *   Example: `https://app.posthog.com/api/projects/123/groups/property_definitions/?group_type_index=0`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** (Inferred) A list of property definition objects. Each object likely includes:
        *   `name`: The name of the property.
        *   `type`: The data type of the property (e.g., string, number, boolean).

4.  **`GET /api/projects/:project_id/groups/property_values/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves the *unique values* for a given group property, across all groups of a specific type.  This is similar to the `events/values/` endpoint, but for group properties.
    *   **How to Call:**
        *   Replace `:project_id`.
        *   Query Parameters:
            *   `group_type_index`: (Required) The index of the group type.
            *   `key`: (Required) The name of the group property you want values for.
        *   Example:  `https://app.posthog.com/api/projects/123/groups/property_values/?group_type_index=0&key=industry`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** (Inferred) A list of unique values for the specified property.

5.  **`GET /api/projects/:project_id/groups/related/`**
     *   **Method:** `GET`
    *   **Purpose:** Retrieves groups related to a given `distinct_id`.
    *   **How to Call:**
        *   Replace `:project_id`.
        *   Query Parameters:
            *   `id`: The id of the group.
            *   `distinct_id`: The distinct id to get related groups.
        *   Example:  `https://app.posthog.com/api/projects/123/groups/related/?distinct_id=user123&id=4`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *    **Response:** A list of groups.

**Key Takeaways:**

*   The Groups API allows you to work with aggregations of data *other than individual users*.
*   You can list groups (filtered by type), find a specific group by type and key, and explore group property definitions and values.
*   The `group_type_index` parameter is crucial for specifying which type of group you're working with.
*   Always use your Personal API Key in the `Authorization` header.

This summary, based on the provided list and documentation URL, provides a solid overview of the PostHog Groups API.  It covers retrieving group data, finding specific groups, and exploring group properties.

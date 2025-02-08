# PostHog Property Definitions API

**Core Concepts (from the URL and endpoints):**

*   **`project_id`**: The numerical ID of your PostHog project.
*   **`id`**: The unique identifier of a specific property definition.
*   **Property Definitions**: These endpoints manage metadata about properties, such as their name, description, type (numeric, string, etc.), and whether they are used in events, persons, or groups. This is *not* about the actual values of properties.
* **Authentication:** It is not specified how to authorize, but from previous knowledge we can assume it is similar to other endpoints.
*   **Base URL:** `https://[your-posthog-instance]/`

**Endpoint Summary:**

1.  **`GET /api/projects/:project_id/property_definitions/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a list of all property definitions within the specified project. Supports pagination and filtering.
    *   **How to Call:**
        *   Replace `:project_id`.
        *   Query Parameters:
            *   `limit`: Pagination - number of results per page.
            *   `offset`: Pagination - starting offset.
            *   `search`: (Optional) Filters by searching within property names and descriptions.
            *   `is_numerical`: (Optional) Filters for numerical properties.
            *    `type`: filter by property type
            *   `format`: csv or json
        *   Example: `https://app.posthog.com/api/projects/123/property_definitions/?search=email&is_numerical=false`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** A paginated JSON object (or CSV if specified):
        ```json
        {
          "count": 42, // Total number of property definitions
          "next": "...", // URL to next page (or null)
          "previous": "...", // URL to previous page (or null)
          "results": [ // Array of property definition objects
            {
              // ... details for each property definition ...
            }
          ]
        }
        ```
        *   Each property definition object in `results` will contain:
            *   `id`: Unique ID.
            *   `name`: The name of the property (e.g., "email", "plan_type", "$browser").
            *   `description`: A description of the property.
            *   `type`: type of property
            *   `is_numerical`: Boolean indicating if the property is numeric.
            *   `property_type`: The data type of the property (e.g., "String", "Numeric", "DateTime", "Boolean").
            *  `volume_30_day`: Volume for the past 30 days.
            *    Other metadata.

2.  **`GET /api/projects/:project_id/property_definitions/:id/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a single property definition by its ID.
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   Example: `https://app.posthog.com/api/projects/123/property_definitions/456/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** The requested property definition object.

3.  **`PATCH /api/projects/:project_id/property_definitions/:id/`**

    *   **Method:** `PATCH`
    *   **Purpose:** Partially updates an existing property definition. This allows you to modify metadata like the description. You generally *cannot* change the `name` or `property_type` after the property has been used.
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   Request Body (JSON): Contains *only* the fields you want to update (e.g., `description`).
        *   Example: `https://app.posthog.com/api/projects/123/property_definitions/456/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
         *  Set `Content-Type`: `application/json`
    *   **Response:** The updated property definition object.

4.  **`DELETE /api/projects/:project_id/property_definitions/:id/`**

    *   **Method:** `DELETE`
    *   **Purpose:** Deletes a property definition. This likely *doesn't* delete the actual property data, but removes the *definition*. This is a potentially disruptive operation and should be used with caution.
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   Example: `https://app.posthog.com/api/projects/123/property_definitions/456/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** 204 No Content (success).
5. **`GET /api/projects/:project_id/property_definitions/seen_together/`**
 *   **Method:** `GET`
    *    **Purpose:** Groups properties seen together.
    *   **How to Call:**
        *   Replace `:project_id`.
        *    Example: `https://app.posthog.com/api/projects/123/property_definitions/seen_together`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.

**Key Takeaways:**

*   The Property Definitions API manages *metadata* about properties (names, descriptions, types), not the actual property values.
*   You can list, retrieve, update, and delete property definitions.
*   Deleting a definition likely doesn't delete the data, but should still be done cautiously.
*   Filtering and searching are supported when listing definitions.
*   Always use your Personal API Key in the `Authorization` header.

This summary, based on the provided list and URL, clearly explains the purpose and usage of the PostHog Property Definitions API, distinguishing it from managing the actual property data.

# PostHog Feature Flags API

**Core Concepts (from the URL and endpoints):**

*   **`project_id`**: The numerical ID of your PostHog project.
*   **`id`**: This has multiple meanings depending on the context:
    *   When used directly under `/feature_flags/`, it refers to the ID of a *feature flag*.
    *   When used under `/role_access/`, it refers to the ID of a *role access* entry.
*   **`feature_flag_id`**: The same as id.
*   **Feature Flags**: These endpoints manage feature flags, which allow you to control the release and rollout of features to users.
* **Authentication:** It is not specified how to authorize, but from previous knowledge we can assume it is similar to other endpoints.
*   **Base URL:** `https://[your-posthog-instance]/`

**Endpoint Summary (Grouped by Functionality):**

I'll group the endpoints for clarity based on what aspect of feature flags they manage.

**1. Core Feature Flag Management:**

*   **`GET /api/projects/:project_id/feature_flags/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a list of feature flags within the specified project. Supports pagination.
    *   **How to Call:**
        *   Replace `:project_id`.
        *   Pagination parameters: `limit`, `offset`.
        *   Example: `https://app.posthog.com/api/projects/123/feature_flags/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** A JSON object with the usual paginated structure:
        ```json
        {
          "count": 5, // Total number of flags
          "next": "...", // URL to next page (or null)
          "previous": "...", // URL to previous page (or null)
          "results": [ // Array of feature flag objects
            {
              // ... details for each flag ...
            }
          ]
        }
        ```
        *   Each feature flag object in `results` will include:
            *   `id`: Unique ID.
            *   `key`: The unique key used to identify the flag in your code (e.g., "new-signup-flow").
            *   `name`: A human-readable name.
            *   `filters`:  A complex object defining the rollout conditions (who gets the flag and when). This can include user properties, groups, rollout percentages, and more.
            *   `active`:  Boolean indicating if the flag is currently active.
            *   `created_by`:  Who created the flag.
            *   `created_at`: Timestamp of creation.
            *   `is_simple_flag`: a simplified flag object, with just a rollout percentage
            *   Many other fields related to flag configuration, usage, and history.

*   **`POST /api/projects/:project_id/feature_flags/`**

    *   **Method:** `POST`
    *   **Purpose:** Creates a new feature flag.
    *   **How to Call:**
        *   Replace `:project_id`.
        *   Request Body (JSON): Contains data for the new flag. Key fields:
            *   `key`: (Required) The unique key for the flag (must be unique within the project).
            *   `name`: (Optional) A human-readable name.
            *   `filters`: (Required) The rollout conditions. This is a complex object and is *crucial* for defining how the flag works.  It can include:
                *   `groups`: Targeting based on user groups.
                *   `properties`: Targeting based on user or event properties.
                *   `rollout_percentage`:  A percentage rollout (e.g., enable for 50% of users).
                *   `multivariate`:  If the flag has multiple variants (for A/B testing).
        *   Example: `https://app.posthog.com/api/projects/123/feature_flags/`
        *  Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
        *  Set `Content-Type`: `application/json`
    *   **Response:** The newly created feature flag object.

*   **`GET /api/projects/:project_id/feature_flags/:id/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves a single feature flag by its ID.
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   Example: `https://app.posthog.com/api/projects/123/feature_flags/456/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** The requested feature flag object.

*   **`PATCH /api/projects/:project_id/feature_flags/:id/`**

    *   **Method:** `PATCH`
    *   **Purpose:** Partially updates an existing feature flag.  This is how you change rollout conditions, activate/deactivate a flag, etc.
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   Request Body (JSON): Contains *only* the fields you want to update.  You can modify almost any aspect of the flag, including `name`, `filters`, `active`, etc.
        *   Example: `https://app.posthog.com/api/projects/123/feature_flags/456/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
        *  Set `Content-Type`: `application/json`
    *   **Response:** The updated feature flag object.

*   **`DELETE /api/projects/:project_id/feature_flags/:id/`**

    *   **Method:** `DELETE`
    *   **Purpose:** Deletes a feature flag.
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   Example: `https://app.posthog.com/api/projects/123/feature_flags/456/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** 204 No Content (success).

**2. Role-Based Access Control (RBAC):**

These endpoints manage which *roles* have access to modify a specific feature flag.

*   **`GET /api/projects/:project_id/feature_flags/:feature_flag_id/role_access/`**
     *   **Method:** `GET`
    *   **Purpose:** (Inferred) Retrieves a list of which roles have access to a feature flag.
    *   **How to Call:**
        *   Replace `:project_id` and `:feature_flag_id`.
        *    Example: `https://app.posthog.com/api/projects/123/feature_flags/456/role_access`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    * **Response:** A list of roles.

*   **`POST /api/projects/:project_id/feature_flags/:feature_flag_id/role_access/`**
     *   **Method:** `POST`
    *   **Purpose:** (Inferred) Grants a role access to modify a feature flag.
    *   **How to Call:**
        *   Replace `:project_id` and `:feature_flag_id`.
        *  Example: `https://app.posthog.com/api/projects/123/feature_flags/456/role_access`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    * **Response:** The newly created object.

*   **`GET /api/projects/:project_id/feature_flags/:feature_flag_id/role_access/:id/`**
     *   **Method:** `GET`
    *   **Purpose:** (Inferred) Retrieves a list of a role access by ID.
    *   **How to Call:**
        *   Replace `:project_id`, `:feature_flag_id`, and `:id`.
        *    Example: `https://app.posthog.com/api/projects/123/feature_flags/456/role_access/789`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    * **Response:** The role access object.

*   **`DELETE /api/projects/:project_id/feature_flags/:feature_flag_id/role_access/:id/`**
    *   **Method:** `DELETE`
    *   **Purpose:** (Inferred) Revokes a role's access to modify a feature flag.
    *   **How to Call:**
        *   Replace `:project_id`, `:feature_flag_id`, and `:id`.
        *    Example: `https://app.posthog.com/api/projects/123/feature_flags/456/role_access/789`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    * **Response:** The newly created object.

**3. Activity Log and History:**

*   **`GET /api/projects/:project_id/feature_flags/:id/activity/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves the activity log for a specific feature flag (changes, who made them, when, etc.).
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   Example: `https://app.posthog.com/api/projects/123/feature_flags/456/activity/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** (Inferred) A list of activity log entries.
*   **`GET /api/projects/:project_id/feature_flags/activity/`**

    *   **Method:** `GET`
    *   **Purpose:** Retrieves the activity log for all feature flags.
    *   **How to Call:**
        *   Replace `:project_id`.
        *   Example: `https://app.posthog.com/api/projects/123/feature_flags/activity/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** (Inferred) A list of activity log entries.

**4. Static Cohorts:**

*   **`POST /api/projects/:project_id/feature_flags/:id/create_static_cohort_for_flag/`**

    *   **Method:** `POST`
    *   **Purpose:** Creates a static cohort of users who *currently* match the feature flag's conditions. This is useful for analyzing the impact of the flag or targeting those users in other analyses.
    *   **How to Call:**
        *   Replace `:project_id` and `:id`.
        *   Example: `https://app.posthog.com/api/projects/123/feature_flags/456/create_static_cohort_for_flag/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
         *  Set `Content-Type`: `application/json`
    *   **Response:** (Inferred) The newly created static cohort object.

**5. Dashboards and Usage:**

*   **`POST /api/projects/:project_id/feature_flags/:id/dashboard/`**

    *   **Method:** `POST`
    *   **Purpose:** Unclear from the name alone. It might create a dashboard related to the feature flag's usage or impact, or it might associate an existing dashboard with the flag.
    *   **How to Call:** Requires further documentation.

*   **`POST /api/projects/:project_id/feature_flags/:id/enrich_usage_dashboard/`**
     *   **Method:** `POST`
    *    **Purpose:** Adds a link to your feature flag on the PostHog dashboard.
        *    Example: `https://app.posthog.com/api/projects/123/feature_flags/456/enrich_usage_dashboard`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    * **How to Call:** Not specified.

*   **`GET /api/projects/:project_id/feature_flags/:id/status/`**
 *   **Method:** `GET`
    *    **Purpose:** Retrieves status of feature flag.
        *    Example: `https://app.posthog.com/api/projects/123/feature_flags/456/status`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    * **How to Call:** Not specified.

**6. Flag Evaluation and Targeting:**

*   **`GET /api/projects/:project_id/feature_flags/evaluation_reasons/`**

    *   **Method:** `GET`
    *   **Purpose:**  Retrieves the reasons *why* feature flags evaluated to their current values for a given user.  This is incredibly useful for debugging and understanding flag behavior.
    *   **How to Call:**
        *   Replace `:project_id`.
        *   Query Parameters:  You'll likely need to provide parameters to identify the user and potentially the flags you're interested in (e.g., `distinct_id`).
        *   Example: `https://app.posthog.com/api/projects/123/feature_flags/evaluation_reasons/?distinct_id=user123`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** (Inferred) A JSON object explaining the evaluation logic and results.

*   **`GET /api/projects/:project_id/feature_flags/local_evaluation/`**

    *   **Method:** `GET`
    *   **Purpose:**  Retrieves feature flag values for a given user, *simulating* local evaluation (as if the evaluation were happening in your application's code). This is helpful for testing and development.
    *   **How to Call:**
        *   Replace `:project_id`.
        *   Query Parameters: You'll almost certainly need to provide a `distinct_id` to identify the user.  You might also be able to specify which flags to evaluate.
        *   Example: `https://app.posthog.com/api/projects/123/feature_flags/local_evaluation/?distinct_id=user123`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** (Inferred) A JSON object containing the flag values for the specified user.

*   **`GET /api/projects/:project_id/feature_flags/my_flags/`**

    *   **Method:** `GET`
    *   **Purpose:**  Likely retrieves the feature flags that are relevant to the *currently authenticated user* (the user whose API key is being used). This could be based on role-based access control or other targeting criteria.
    *   **How to Call:**
        *   Replace `:project_id`.
        *   Example:  `https://app.posthog.com/api/projects/123/feature_flags/my_flags/`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
    *   **Response:** (Inferred) A list of feature flag objects, likely filtered to those the current user can access or is affected by.

*   **`POST /api/projects/:project_id/feature_flags/user_blast_radius/`**
     *   **Method:** `POST`
    *   **Purpose:** Calculates and reduces the blast radius of users affected by a change to a feature flag's rollout conditions.
    *   **How to Call:**
        *   Replace `:project_id`.
        *    Example: `https://app.posthog.com/api/projects/123/feature_flags/user_blast_radius`
        *   Include the `Authorization: Bearer YOUR_PERSONAL_API_KEY` in header.
         *  Set `Content-Type`: `application/json`
    * **Response:** The newly created object.

**Key Takeaways:**

*   The Feature Flags API is *very* extensive, providing comprehensive control over all aspects of feature flag management.
*   Core CRUD operations (create, read, update, delete) are available for flags themselves.
*   Role-based access control (RBAC) can be managed for individual flags.
*   Activity logs track changes to flags.
*   Static cohorts can be created based on flag conditions.
*   Endpoints exist for retrieving flag evaluation reasons and simulating local evaluation (essential for debugging and testing).
*   `my_flags` likely provides a user-centric view of relevant flags.
*   Always use your Personal API Key in the `Authorization` header.
*  The official Posthog Feature Flags documentation is needed for complete information.

This is a detailed breakdown, grouping endpoints by functionality and providing inferences where necessary. However, the full PostHog API documentation is *essential* for complete details on request bodies, response structures, and the nuances of each endpoint, particularly for the more complex operations like local evaluation and understanding the `filters` object.

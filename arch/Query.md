# PostHog Query API

**Base URL Structure:**

All these endpoints share a common base structure:

`/api/projects/:project_id/query/`

*   `:project_id`: This is a placeholder. You *must* replace it with the actual numerical ID of your PostHog project.

**Endpoint Summary:**

The provided information gives us these endpoints:

1.  **`POST /api/projects/:project_id/query/`**

    *   **Method:** `POST`
    *   **Purpose:** (Based on standard RESTful conventions and the context, we can infer) This endpoint is likely used to *create* and *execute* a new query. The documentation doesn't say anything more specific.
    *    **How to call:** It is not specified how to use this endpoint.

2.  **`GET /api/projects/:project_id/query/:id/`**

    *   **Method:** `GET`
    *   **Purpose:** (Inferred from RESTful conventions) This likely retrieves a *specific* query, identified by its `:id`.
    *   `:id`: Another placeholder. Replace this with the unique ID of the query you want to retrieve.
    *    **How to call:** It is not specified how to use this endpoint.

3.  **`DELETE /api/projects/:project_id/query/:id/`**

    *   **Method:** `DELETE`
    *   **Purpose:** (Inferred) Deletes a specific query, identified by its `:id`.
    *   `:id`: The ID of the query to delete.
    *    **How to call:** It is not specified how to use this endpoint.

4.  **`POST /api/projects/:project_id/query/check_auth_for_async/`**

    *   **Method:** `POST`
    *   **Purpose:** The name suggests this endpoint is used to check authorization for asynchronous queries. It probably verifies if the user (authenticated via the standard `Authorization` header) has permission to run or access the results of an asynchronous query.
    *    **How to call:** It is not specified how to use this endpoint.

5.  **`GET /api/projects/:project_id/query/draft_sql/`**

    *   **Method:** `GET`
    *   **Purpose:** This endpoint likely retrieves draft SQL queries. These might be queries that are being composed but haven't been formally saved or executed yet.
    *    **How to call:** It is not specified how to use this endpoint.

**Authentication:**
The documentation does *not* specify the authentication method for those endpoints.

**Important Considerations and Inferences (Based on Common API Practices):**

*   **Missing Information:** This is a very limited view of the Query API. Crucially, we're missing:
    *   **Request Bodies:**  For `POST` requests, we don't know what data (JSON, form data, etc.) needs to be sent in the request body. This is essential for creating queries, specifying parameters, etc.
    *   **Response Formats:** We have no information about the structure of the responses (JSON, CSV, etc.) that these endpoints return.
    *   **Query Language:** The documentation snippet doesn't tell us *how* to actually write a query. It mentions "draft_sql", suggesting SQL is involved, but there's no information about the specific query language or data model.
    *   **Asynchronous Queries:** The `check_auth_for_async` endpoint hints at the existence of asynchronous queries (queries that run in the background), but there are no details on how to initiate or manage them.
    * **Authentication:** The documentation does not specify authentication, so it is unclear whether to utilize the Project or Personal API Key, and whether to include this in the header or body.

This summary is extremely limited due to the sparse information provided. To actually *use* this API, you'd need to consult the full PostHog Query API documentation to get details on request/response formats, query language specifics, and authentication.

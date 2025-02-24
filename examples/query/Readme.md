# PostHog Query API Example

This example demonstrates how to use the PostHog Query API to fetch data from your PostHog instance. It showcases:
- Setting up a PostHog API client
- Executing HogQL queries
- Handling both synchronous and asynchronous query responses
- Checking query status for async queries
- Error handling and logging

## Setup

1. Create a `.env` file in this directory with the following variables:
```env
POSTHOG_API_KEY=your_api_key
POSTHOG_API_URL=your_posthog_api_url
POSTHOG_PROJECT_ID=your_project_id
```

2. Build and run the example:
```bash
cargo run
```

## How it Works

The example:
1. Creates a PostHog API client
2. Executes a simple HogQL query to fetch distinct IDs from person_distinct_ids
3. Handles the query response:
   - For synchronous queries: displays the results immediately
   - For asynchronous queries: checks the query status using the task ID

You can modify the query in `main.rs` to execute any valid HogQL query supported by your PostHog instance:

```rust
let request = QueryRequest::default().with_query(json!({
    "kind": "HogQLQuery",
    "query": "select `distinct_id` from person_distinct_ids"
}));
```

## Query Types

The example uses a simple HogQL query, but the Query API supports various query types:
- HogQL queries
- Event queries
- Funnel queries
- Retention queries
- Trends queries

Refer to the [PostHog Query API documentation](https://posthog.com/docs/api/query) for more details on available query types and their parameters.

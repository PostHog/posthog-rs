# PostHog Axum Web Server Example

This example demonstrates how to integrate PostHog event tracking into an Axum web server. It showcases:
- Setting up PostHog with an Axum web server
- Using PostHog's service actor pattern in a web server context
- Handling PostHog event capture in HTTP request handlers
- Proper error handling and logging in a web service

## Setup

1. Create a `.env` file in this directory with the following variables:
```env
POSTHOG_PUBLIC_KEY=your_project_api_key
POSTHOG_BASE_URL=your_posthog_instance_url
```

2. Build and run the example:
```bash
cargo run
```

3. Test the server:
```bash
curl http://localhost:3000
```

## How it Works

The example creates a simple Axum web server that:

1. Initializes a PostHog service actor at startup
2. Creates a single HTTP endpoint (`GET /`) that demonstrates event capture
3. Uses Axum's state management to share the PostHog sender across request handlers
4. Captures a PostHog event with custom properties for each request

### Code Structure

- The main server setup is in `main()`:
  - Sets up logging with tracing
  - Initializes the PostHog service actor
  - Configures the Axum router and starts the server

- The request handler in `handler()`:
  - Creates a test event with:
    - Event name: "event_name"
    - Distinct ID: "my_custom_user_id"
    - Custom properties: `{"key": "value"}`
  - Sends the event to PostHog
  - Returns appropriate HTTP responses

## Customization

You can modify the event capture in `handler()` to:
- Use different event names
- Add more properties
- Include user-specific distinct IDs
- Add custom timestamps
- Include any other event data supported by PostHog

## Production Considerations

When using this pattern in production:
1. Consider adding middleware for error handling
2. Implement proper request validation
3. Add health check endpoints
4. Configure proper shutdown handling
5. Add metrics and monitoring
6. Consider using environment-specific configuration

For more information on PostHog event capture, see the [PostHog documentation](https://posthog.com/docs/libraries/rust).

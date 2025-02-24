# PostHog Actor Microservice Example

This example demonstrates how to use the PostHog SDK in an actor-based microservice architecture. It showcases:
- Creating a PostHog service actor for handling event capture
- Using channels for message passing between actors
- Implementing a sample actor that processes messages and sends events to PostHog
- Proper error handling and logging

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

## How it Works

The example creates a `SampleActor` that:
1. Receives JSON messages through a channel
2. Processes each message
3. Sends the processed data to PostHog using the `PostHogServiceActor`

The `PostHogServiceActor` handles:
- Batching events for efficient delivery
- Automatic retries on failure
- Proper shutdown and cleanup

The example will send 100 test messages, each containing a simple counter value, demonstrating how to integrate PostHog event capture into an actor-based system.

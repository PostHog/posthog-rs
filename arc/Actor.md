Here's an architectural document describing the PostHog event actor system:

# PostHog Event Actor Architecture Document

## Overview
The system implements an actor-based approach for handling PostHog event submissions with timeout capabilities using Tokio and Rust channels.

## Components

### 1. Actor Message Enum
- `PostHogMessage`: An enumeration containing two variants
  - `Capture`: Holds the event data to be sent to PostHog
  - `Exit`: Signal to gracefully shutdown the actor

### 2. Actor State
- Contains PostHog client instance
- Internal channel receivers
- Processing state management

### 3. Core Components

#### Actor Creation Function
- Purpose: Creates and initializes the actor system
- Input: PostHog client instance
- Output: PostHog actor system
- Operations:
  - Initializes the actor system

### Actor Run function
- Purpose: Runs the actor system
- Input: PostHog actor system
- Output: Sender of actor messages
- Operations:
  - Spawns the actor task (using Runtime Provided when creating the system)
  - Creates channel pair (sender/receiver)
  - Spawns Tokio task for actor loop
  - Returns sender handle to caller
- Actor Loop
  - Infinite loop processing incoming messages
  - Handles timeout for event processing
  - Manages graceful shutdown
  - Uses select pattern for message handling

#### Event Processing
- Timeout wrapper for PostHog API capture batch calls
- Error handling and retry logic
- Async processing of capture events

## Flow

1. System Initialization:
   - Client creates actor with PostHog client specifying the Runtime
   - Receives sender handle for communication

2. Normal Operation:
   - System sends events via sender
   - Actor processes events with timeout
   - Events forwarded to PostHog

3. Shutdown:
   - System sends Exit signal
   - Actor completes pending events
   - Closes channels and exits

## Error Handling

- Timeout handling for API calls
- Channel communication errors
- PostHog API errors
- Graceful degradation

## Performance Considerations

- Async processing for non-blocking operation
- Channel buffer sizes
- Timeout durations
- Resource cleanup

## Usage Pattern

1. Initialize actor with PostHog client
2. Obtain sender handle
3. Send events through sender
4. Send exit signal when done
5. Wait for cleanup

This architecture ensures:
- Asynchronous event processing
- Timeout handling
- Graceful shutdown
- Resource management
- Error handling
- Clean API surface
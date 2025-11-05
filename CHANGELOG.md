## 0.6.0 - 2025-11-05

### Patch Changes

Configuration system now accepts base URLs instead of full endpoint URLs
- Provide just the hostname (e.g., `https://eu.posthog.com`)
- SDK automatically appends `/i/v0/e/` for single events and `/batch/` for batch events
- Old format with full URLs still works - paths are automatically stripped and normalized
- Enables simultaneous use of both single-event and batch endpoints

## 0.2.6 - 2025-01-08



## 0.2.5 - 2025-01-08



##  - 2025-01-08

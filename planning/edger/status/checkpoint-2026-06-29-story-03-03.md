# Status: Story 03.03 — wire + limits

**Mode:** Checkpoint

## Completed
- `validate_request`, `encode_frame`/`decode_frame` (postcard)
- `ResourceLimits`, `execute_with_limits` with tokio timeout
- `InProcessTransport`, `UdsTransport` stub
- 5 new tests (4 wire + 1 timeout); 12 total isolation tests

## Pendência
- Core parser `50ms` duration — documented in story

## Next
Story 03.04 — dual-backend prep
# Validation — ImageProc Halogen

Standalone ImageProc variant targeting [`peonist-ai/halogen-flash-server`](https://github.com/peonist-ai/halogen-flash-server/).

## Automated checks run after final edits

- **Rust suite (`cargo test --manifest-path src-tauri/Cargo.toml`)**:
  - 22 tests covering SSE streaming content/reasoning parsing, UTF-8 hold-back boundaries, error parsing, URL normalization, atomic recovery, recursive import, duplicate paths, sidecar import, image resizing, fast JPEG preview generation (`fast_preview_encodes_jpeg_and_preserves_dimensions`), source preservation, collision-safe and additive export, `detect_vision`, and a local HTTP server test covering `halogen_status` (`/health`, `/v1/models`, `/cache`), `halogen_warm_prefix` (`X-Halogen-Keep`), and `halogen_release_handle` (`DELETE /halogen/handles/{name}`).
- **JavaScript suite (`npm test`)**:
  - Collection filtering, recovery, deduplication, Halogen pixel budget defaults/clamping (`3.69 MP` default, `14.75 MP` max), shared prefix vs `last_user_start` target separation, greedy vs sampled payload construction (omitting penalties/filters at `temperature: 0`), thinking effort / `max_thinking_tokens` / `prompt_cache_policy: "no_store"`, `formatStats`, `serverUnavailable` patterns, and repetition detection.
- **Playwright UI integration suites (`work/test-*.cjs`, mocked Tauri bridge)**:
  - `work/test-references.cjs`: `halogen_status` vision-off warning and vision-ready KV pool/cache display, sampling preset toggling, reference deduplication, `no_store` and `fork` (`X-Halogen-Keep` / `X-Halogen-From` + handle cleanup) concurrent runs, `prompt_progress` and MTP telemetry display, `fork` warm failure pausing, and recovery across reload.
  - `work/test-concurrency.cjs`: selection, overlapping concurrent requests, isolated captions, update-existing export option, pause, cancel, failure, and retry.
  - `work/test-live-review.cjs`: editing completed captions during an active run, preserving drafts, approval, and queued-image locking.
  - `work/test-regenerate-selected.cjs`: regenerating selected previously captioned images.
  - `work/test-server-failure.cjs`: transport failure queue pause and recovery.

## Live server observation

- Read-only `GET /health`, `GET /v1/models`, and `GET /cache` checks against the running local `halogen-flash-server` on `http://localhost:8731` (`0.20.6`, model `halogen-qwen3.8-flash-next`, `vision.enabled: true`, `max_pixels: 3686400`, `max_tokens_cap: 65536`, `snapshot_places: ["system_end", "first_message_lead", "last_user_start", "history_end"]`) confirmed the response schemas used by `halogen.rs` and `ui/app.js`.
- No live vision inference requests were sent to `localhost:8731` during test validation, and `npm run build` was not executed.

# ImageProc Halogen

A standalone desktop image-captioning app based on ImageProc (derived from [DocProc4](https://github.com/vernonleon/DocProc4)), tailored specifically for [`peonist-ai/halogen-flash-server`](https://github.com/peonist-ai/halogen-flash-server/) (`Qwen3.8-Flash-Next` on AMD Strix Halo).

## Run from the terminal

Requires Node.js, Rust, and Tauri 2 desktop build dependencies (on Linux, including WebKitGTK 4.1).

```bash
npm install
npm run dev
```

This opens the desktop app in development mode. Keep the terminal open; Ctrl+C stops it. `npm start` is an alias.

## Build and test

```bash
npm run build
```

Tauri builds the optimized binary (`release/imageproc-halogen`) and installers under `src-tauri/target/release/bundle/` (or under your configured `.cargo/config.toml` `target-dir`).

```bash
npm test
```

Runs the JavaScript collection, Halogen payload, and repetition checks.

```bash
cargo test --manifest-path src-tauri/Cargo.toml
```

Runs the Rust backend tests (streaming, `halogen_status`, `detect_vision`, `halogen_warm_prefix`, `halogen_release_handle`, image preparation, and paired dataset export).

## Starting `halogen-flash-server` for image captioning

Image support on `halogen-flash-server` is off by default until `HALOGEN_VISION_TOWER=1` is set:

```bash
podman run --rm -p 8731:8731 \
  --device /dev/kfd --device /dev/dri --group-add keep-groups \
  --ipc=host --ulimit memlock=-1:-1 \
  -e HALOGEN_VISION_TOWER=1 \
  -e HALOGEN_DOWNLOAD=peonist-ai/halogen-qwen3.8-flash-next \
  -v ~/halogen-models:/models \
  ghcr.io/peonist-ai/halogen-flash-server:0.20.6
```

In **Server & model**, enter `http://localhost:8731` (default) and click **Check server & models**. ImageProc Halogen probes `/health`, `/v1/models`, and `/cache` to show:

- Whether the vision tower is enabled (`vision.enabled` / `architecture.input_modalities`) and a warning naming `HALOGEN_VISION_TOWER=1` if disabled.
- Engine and API versions.
- Unified KV pool usage (`/cache` `pool.used / pool.positions`), prompt-cache hit rates, active slots, and queue depth.

## Halogen-specific features

- **Image pixel budget & previews:** Defaults to `3.69 MP` (`2560×1440`, matching `HALOGEN_VISION_MAX_PIXELS`), with one-click presets (`2560×1440`, `1920×1080`, `1280×800`) and a hard cap at `14.75 MP` (`4 × 3,686,400`, above which the server refuses requests with HTTP 400). Optional synthetic budget measurement tests sizes up to `3400×3400` (`11.56 MP`). UI thumbnails and review previews use fast JPEG resizing with LRU caching and parallel preview decoding, while model-inference requests always receive full `Lanczos3` lossless PNG images at the configured pixel budget.
- **Thinking & token budget:** `max_completion_tokens` (up to `65536`) covers both thinking and the caption. Thinking defaults to **None — thinking off** (`reasoning_effort: "none"`, `chat_template_kwargs: { enable_thinking: false }`). Higher effort levels (`minimal` through `max`) expose an optional `max_thinking_tokens` cap (`0` uses the server's answer room).
- **Sampling presets:**
  - **Greedy (`temperature: 0`):** Uses Halogen's byte-identical MTP + PLD speculative path and omits filters/penalties (which Halogen rejects with HTTP 400 at `temperature: 0`).
  - **Qwen card non-thinking:** `temperature: 0.7`, `top_p: 0.80`, `top_k: 20`, `presence_penalty: 1.5`, `repetition_penalty: 1.0`.
  - **Qwen card thinking:** `temperature: 1.0`, `top_p: 0.95`, `top_k: 20`, `presence_penalty: 0`, `repetition_penalty: 1.0`.
- **Prompt cache & session handles:**
  - **Halogen prefix cache (`no_store` targets):** Structures messages so the shared instruction (or references + assistant acknowledgement) precedes the target image in its own final `user` message (`system_end` / `last_user_start`), warms the prefix once, and sends `prompt_cache_policy: "no_store"` on target requests so finished captions do not evict the shared prefix from the 28-entry cache.
  - **Halogen session fork (`X-Halogen-Keep` / `X-Halogen-From`):** Keeps the warmed prefix under a session handle before launching workers, passes `X-Halogen-From` plus `prompt_cache_policy: "no_store"` on every worker request, and releases the handle via `DELETE /halogen/handles/<name>` when the run ends.
  - **Standard:** Uses Halogen's default `HALOGEN_PROMPT_CACHE=2` behavior without `no_store`.
- **Live stream telemetry:** Sends `"return_progress": true` to display prefill & cache-hit progress (`prompt_progress`) during long image/reference prefills, followed by per-image and run-wide decode speed (`tok/s`), cache hit rate, MTP draft acceptance (`draft_n_accepted / draft_n`), reasoning token counts, `reasoning_closed_by`, and `X-Halogen-From` status.

## Workflow & export

1. **Images:** Import files or folders recursively (`PNG`, `JPEG`, `WebP`, `TIFF`, `BMP`), reading matching `.txt` sidecars.
2. **Instructions:** Edit shared instructions, manage saved presets, and attach optional reference images with notes.
3. **Server & model:** Connect to `halogen-flash-server`, check vision readiness, and choose thinking, sampling, cache mode, concurrency, and pixel budget.
4. **Run:** Process pending, selected, failed, or selected-regeneration queues with Linux `systemd-inhibit` sleep suppression during active runs.
5. **Review & export:** Review, edit, approve (`Ctrl+Enter`), reject, or regenerate captions, then export to a new timestamped dataset folder or update an existing dataset folder atomically using `manifest.json`.

## Code layout

- `src-tauri/src/main.rs`: server bridge, SSE streaming (`prompt_progress`, `timings`, `usage`, `X-Halogen-From`), cancellation, persistence, and run recovery.
- `src-tauri/src/halogen.rs`: `halogen_status` (`/health` + `/v1/models` + `/cache`), vision-tower detection, `halogen_warm_prefix` (`X-Halogen-Keep`), `halogen_release_handle`, and `halogen_list_handles`.
- `src-tauri/src/images.rs`: recursive import, EXIF orientation/resizing, and atomic paired export.
- `src-tauri/src/power.rs`: Linux `systemd-inhibit` suspend suppression.
- `ui/core.js`: pure collection rules, Halogen prompt layouts, sampling/thinking payloads, and telemetry formatting.
- `ui/app.js`: desktop workflow, queue orchestration, live telemetry, review, and recovery.
- `ui/index.html` & `ui/styles.css`: desktop interface.

AGPL-3.0-or-later; see `LICENSE`.

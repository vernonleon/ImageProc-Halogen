mod halogen;
mod images;
mod power;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tokio::task::AbortHandle;

struct AppState {
    http: reqwest::Client,
    aborts: Mutex<HashMap<String, AbortHandle>>,
    /// Previous /proc/stat totals, used to calculate local CPU utilization.
    cpu_sample: Mutex<Option<(u64, u64)>>,
    /// (base_url, api_key) of the last server contact — used to unload on exit.
    last_server: Mutex<(String, String)>,
}

/// Normalize a server root URL. Users commonly paste OpenAI's full API base
/// (for example http://127.0.0.1:1337/v1); commands append /v1 themselves,
/// so remove that trailing version segment to avoid /v1/v1/models.
fn normalize(base: &str) -> String {
    let base = base.trim().trim_end_matches('/');
    base.strip_suffix("/v1")
        .or_else(|| base.strip_suffix("/V1"))
        .unwrap_or(base)
        .trim_end_matches('/')
        .to_string()
}

fn authed(req: reqwest::RequestBuilder, key: &str) -> reqwest::RequestBuilder {
    if key.is_empty() {
        req
    } else {
        req.bearer_auth(key)
    }
}

fn remember(state: &State<AppState>, base: &str, key: &str) {
    *state.last_server.lock().unwrap() = (normalize(base), key.to_string());
}

// ---------------------------------------------------------------------------
// Standard OpenAI endpoints plus optional local-server extensions
// ---------------------------------------------------------------------------

/// Checks /health (halogen-flash-server), /v1/health, then falls back to standard OpenAI /v1/models.
#[tauri::command]
async fn health(
    state: State<'_, AppState>,
    base_url: String,
    api_key: String,
) -> Result<Value, String> {
    remember(&state, &base_url, &api_key);
    let base = normalize(&base_url);
    for path in ["/health", "/v1/health"] {
        let probe = authed(state.http.get(format!("{base}{path}")), &api_key)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if probe.status().is_success() {
            let mut body: Value = probe.json().await.map_err(|e| e.to_string())?;
            if !body.is_object() {
                body = json!({});
            }
            body["status"] = json!("ok");
            body["supports_model_management"] = json!(path == "/v1/health");
            return Ok(body);
        }
        if !matches!(probe.status().as_u16(), 404 | 405) {
            let status = probe.status();
            return Err(format!(
                "HTTP {status}: {}",
                probe.text().await.unwrap_or_default()
            ));
        }
    }
    let resp = authed(state.http.get(format!("{base}/v1/models")), &api_key)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        let status = resp.status();
        return Err(format!(
            "HTTP {status}: {}",
            resp.text().await.unwrap_or_default()
        ));
    }
    let body: Value = resp.json().await.map_err(|e| e.to_string())?;
    let models = body
        .get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let loaded = if models.len() == 1 {
        models[0]
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_owned)
    } else {
        None
    };
    Ok(
        json!({"status":"ok", "provider":"OpenAI-compatible server", "supports_model_management":false, "model_loaded":loaded, "all_models_loaded":[]}),
    )
}

/// Returns the raw `data` array from GET /v1/models (id + optional labels).
#[tauri::command]
async fn list_models(
    state: State<'_, AppState>,
    base_url: String,
    api_key: String,
) -> Result<Value, String> {
    remember(&state, &base_url, &api_key);
    let url = format!("{}/v1/models", normalize(&base_url));
    let resp = authed(state.http.get(&url), &api_key)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        let status = resp.status();
        return Err(format!(
            "HTTP {status}: {}",
            resp.text().await.unwrap_or_default()
        ));
    }
    let body: Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(body.get("data").cloned().unwrap_or_else(|| json!([])))
}

#[tauri::command]
async fn load_model(
    state: State<'_, AppState>,
    base_url: String,
    api_key: String,
    model_name: String,
    ctx_size: Option<i64>,
    options: Option<Value>,
) -> Result<Value, String> {
    remember(&state, &base_url, &api_key);
    let url = format!("{}/v1/load", normalize(&base_url));
    let mut body = json!({ "model_name": model_name });
    // Lemonade llama.cpp load options: llamacpp_backend, llamacpp_args,
    // merge_args, save_options, ctx_size. Servers without /v1/load ignore them.
    if let (Some(dest), Some(Value::Object(opts))) = (body.as_object_mut(), options) {
        for (k, v) in opts {
            if k != "model_name" && !v.is_null() {
                dest.insert(k, v);
            }
        }
    }
    if let Some(ctx) = ctx_size {
        body["ctx_size"] = json!(ctx);
    }
    let resp = authed(state.http.post(&url), &api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    // Model loading is a non-standard extension. Jan, LM Studio, and many
    // OpenAI-compatible servers select an already-loaded model per request.
    if matches!(resp.status().as_u16(), 404 | 405 | 501) {
        return Ok(
            json!({ "status": "ready", "message": "model is managed by the OpenAI-compatible server" }),
        );
    }
    if !resp.status().is_success() {
        let status = resp.status();
        return Err(format!(
            "HTTP {status}: {}",
            resp.text().await.unwrap_or_default()
        ));
    }
    resp.json::<Value>().await.map_err(|e| e.to_string())
}

/// Unloads ALL loaded models (no model_name in body = unload all).
#[tauri::command]
async fn unload_model(
    state: State<'_, AppState>,
    base_url: String,
    api_key: String,
) -> Result<Value, String> {
    remember(&state, &base_url, &api_key);
    let url = format!("{}/v1/unload", normalize(&base_url));
    let resp = authed(state.http.post(&url), &api_key)
        .json(&json!({}))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    // This is also optional; do not try to decode Jan's plain-text 404 page.
    if matches!(resp.status().as_u16(), 404 | 405 | 501) {
        return Ok(
            json!({ "status": "ready", "message": "model lifecycle is managed by the OpenAI-compatible server" }),
        );
    }
    if !resp.status().is_success() {
        let status = resp.status();
        return Err(format!(
            "HTTP {status}: {}",
            resp.text().await.unwrap_or_default()
        ));
    }
    resp.json::<Value>().await.map_err(|e| e.to_string())
}

fn read_u64(path: &Path) -> Option<u64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn meminfo_kib(contents: &str, field: &str) -> Option<u64> {
    contents.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        if key != field {
            return None;
        }
        value.split_whitespace().next()?.parse().ok()
    })
}

fn local_memory_gb() -> Option<f64> {
    let info = fs::read_to_string("/proc/meminfo").ok()?;
    let total = meminfo_kib(&info, "MemTotal")?;
    let available = meminfo_kib(&info, "MemAvailable")?;
    Some((total.saturating_sub(available)) as f64 / 1_048_576.0)
}

fn local_cpu_sample() -> Option<(u64, u64)> {
    let line = fs::read_to_string("/proc/stat")
        .ok()?
        .lines()
        .next()?
        .to_owned();
    let values: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|v| v.parse().ok())
        .collect();
    if values.len() < 4 {
        return None;
    }
    let total = values.iter().sum();
    // idle plus iowait are both non-busy CPU time.
    let idle = values[3] + values.get(4).copied().unwrap_or(0);
    Some((total, idle))
}

fn local_amd_gpu_stats() -> Value {
    let Ok(cards) = fs::read_dir("/sys/class/drm") else {
        return json!({});
    };
    for card in cards.flatten() {
        let name = card.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("card") || !name[4..].chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let device = card.path().join("device");
        let is_amd = fs::read_to_string(device.join("vendor"))
            .ok()
            .map(|v| v.trim() == "0x1002")
            .unwrap_or(false);
        if !is_amd {
            continue;
        }
        let gb =
            |name: &str| read_u64(&device.join(name)).map(|bytes| bytes as f64 / 1_073_741_824.0);
        return json!({
            "gpu_percent": read_u64(&device.join("gpu_busy_percent")),
            "vram_gb": gb("mem_info_vram_used"),
            "vram_total_gb": gb("mem_info_vram_total"),
            // On AMD APUs/TTM this is the GTT-backed shared system-memory pool.
            "gtt_gb": gb("mem_info_gtt_used"),
            "gtt_total_gb": gb("mem_info_gtt_total"),
            "gpu_name": fs::read_to_string(device.join("uevent")).ok()
                .and_then(|s| s.lines().find_map(|line| line.strip_prefix("PCI_ID=").map(str::to_owned))),
        });
    }
    json!({})
}

/// Local machine telemetry. It deliberately does not depend on the configured
/// inference server, so it works with Jan, LM Studio, and other OpenAI APIs.
#[tauri::command]
async fn system_stats(state: State<'_, AppState>) -> Result<Value, String> {
    let cpu_percent = local_cpu_sample().and_then(|(total, idle)| {
        let mut previous = state.cpu_sample.lock().ok()?;
        let percent = previous.and_then(|(old_total, old_idle)| {
            let total_delta = total.checked_sub(old_total)?;
            let idle_delta = idle.checked_sub(old_idle)?;
            (total_delta > 0).then(|| {
                100.0 * (total_delta.saturating_sub(idle_delta)) as f64 / total_delta as f64
            })
        });
        *previous = Some((total, idle));
        percent
    });
    let mut stats = json!({
        "cpu_percent": cpu_percent,
        "memory_gb": local_memory_gb(),
        "telemetry_source": "local machine",
    });
    if let (Some(dest), Some(gpu)) = (stats.as_object_mut(), local_amd_gpu_stats().as_object()) {
        dest.extend(gpu.clone());
    }
    Ok(stats)
}

// ---------------------------------------------------------------------------
// PDF page cache: MuPDF native rendering, or PNGs rendered by pdf.js in the webview
const OPEN_MARKERS: [&str; 2] = ["◁think▷", "<think>"];
const CLOSE_MARKERS: [&str; 2] = ["◁/think▷", "</think>"];
/// Longest marker prefix we might see split across chunks (bytes).
const HOLD_BACK: usize = 11;

/// Reported when the server stops generation with finish_reason=length —
/// the output is incomplete even though the request "succeeded".
const TRUNCATED_MSG: &str = "output truncated: the model hit the token budget (finish_reason=length). \
On halogen-flash-server, max_completion_tokens covers both thinking and the caption. \
Raise Max completion tokens (up to 65536), set a Max thinking tokens cap, or lower Thinking effort (e.g. None or Low).";

/// Largest index `<= i` that is a UTF-8 char boundary of `s`.
/// (Stable replacement for the unstable `str::floor_char_boundary`.)
fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Incrementally split a model's content stream into output vs thinking text.
/// Handles both inline think markers (◁think▷/◁/think▷, <think></think>) and
/// plain content. `emit(kind, text)` is called with "thinking" or "output".
fn drain_content(buf: &mut String, in_thinking: &mut bool, emit: &mut dyn FnMut(&str, String)) {
    loop {
        let markers: &[&str] = if *in_thinking {
            &CLOSE_MARKERS
        } else {
            &OPEN_MARKERS
        };
        let found = markers
            .iter()
            .filter_map(|m| buf.find(m).map(|p| (p, m.len())))
            .min_by_key(|(p, _)| *p);
        match found {
            Some((pos, mlen)) => {
                if pos > 0 {
                    let text: String = buf.drain(..pos).collect();
                    if !text.is_empty() {
                        emit(if *in_thinking { "thinking" } else { "output" }, text);
                    }
                }
                buf.drain(..mlen);
                *in_thinking = !*in_thinking;
            }
            None => {
                // emit everything except a held-back tail that could be a
                // marker prefix split across chunks. HOLD_BACK is a byte
                // count, so round down to a char boundary — draining at a
                // non-boundary offset would panic and kill the stream task.
                if buf.len() > HOLD_BACK {
                    let keep = floor_char_boundary(buf, buf.len() - HOLD_BACK);
                    if keep > 0 {
                        let text: String = buf.drain(..keep).collect();
                        if !text.is_empty() {
                            emit(if *in_thinking { "thinking" } else { "output" }, text);
                        }
                    }
                }
                return;
            }
        }
    }
}

fn flush_content(buf: &mut String, in_thinking: bool, emit: &mut dyn FnMut(&str, String)) {
    if !buf.is_empty() {
        emit(
            if in_thinking { "thinking" } else { "output" },
            std::mem::take(buf),
        );
    }
}

/// Extract an error message from OpenAI-style {"error": {...}} or
/// {"error": "..."} bodies. Returns None when there is no error field.
fn error_message(v: &Value) -> Option<String> {
    let err = v.get("error")?;
    if let Some(m) = err.get("message").and_then(|m| m.as_str()) {
        return Some(m.to_string());
    }
    if let Some(m) = err.as_str() {
        return Some(m.to_string());
    }
    Some(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(chunks: &[&str]) -> Vec<(String, String)> {
        let mut buf = String::new();
        let mut in_think = false;
        let mut events = Vec::new();
        let mut emit = |kind: &str, text: String| events.push((kind.to_string(), text));
        for c in chunks {
            buf.push_str(c);
            drain_content(&mut buf, &mut in_think, &mut emit);
        }
        flush_content(&mut buf, in_think, &mut emit);
        events
    }

    fn joined(events: &[(String, String)], kind: &str) -> String {
        events
            .iter()
            .filter(|(k, _)| k == kind)
            .map(|(_, t)| t.clone())
            .collect()
    }

    #[test]
    fn plain_output() {
        let ev = parse(&["Hello", " world"]);
        assert_eq!(joined(&ev, "output"), "Hello world");
        assert_eq!(joined(&ev, "thinking"), "");
    }

    #[test]
    fn kimi_style_markers() {
        let ev = parse(&["◁think▷reasoning here◁/think▷The answer"]);
        assert_eq!(joined(&ev, "thinking"), "reasoning here");
        assert_eq!(joined(&ev, "output"), "The answer");
    }

    #[test]
    fn xml_style_markers() {
        let ev = parse(&["<think>hmm</think>42"]);
        assert_eq!(joined(&ev, "thinking"), "hmm");
        assert_eq!(joined(&ev, "output"), "42");
    }

    #[test]
    fn marker_split_across_chunks() {
        let ev = parse(&["text◁th", "ink▷why ", "not◁/think▷", "done"]);
        assert_eq!(joined(&ev, "output"), "textdone");
        assert_eq!(joined(&ev, "thinking"), "why not");
    }

    #[test]
    fn thinking_open_at_stream_end() {
        let ev = parse(&["<think>unterminated thought"]);
        assert_eq!(joined(&ev, "thinking"), "unterminated thought");
    }

    #[test]
    fn multibyte_at_holdback_boundary_does_not_panic() {
        // Previously: buf.len() - HOLD_BACK could land inside a multi-byte
        // char and String::drain would panic, killing the stream task.
        // "é" is 2 bytes; force the hold-back offset to be a non-boundary.
        let s = "é".repeat(40); // 80 bytes, boundaries only at even offsets
        let ev = parse(&[&s]);
        assert_eq!(joined(&ev, "output"), s);

        // 4-byte chars and mixed content, streamed in awkward chunks
        let mixed = "café — “quotes” → 🚀🚀🚀 done";
        let chunks: Vec<String> = mixed.chars().map(|c| c.to_string()).collect();
        let refs: Vec<&str> = chunks.iter().map(|s| s.as_str()).collect();
        let ev = parse(&refs);
        assert_eq!(joined(&ev, "output"), mixed);
    }

    #[test]
    fn multibyte_around_think_markers() {
        let ev = parse(&["◁think▷héllo — ünïcode◁/think▷résumé 🚀"]);
        assert_eq!(joined(&ev, "thinking"), "héllo — ünïcode");
        assert_eq!(joined(&ev, "output"), "résumé 🚀");
    }

    #[test]
    fn floor_char_boundary_basics() {
        let s = "a🚀b"; // boundaries at 0,1,5,6
        assert_eq!(floor_char_boundary(s, 0), 0);
        assert_eq!(floor_char_boundary(s, 1), 1);
        assert_eq!(floor_char_boundary(s, 2), 1);
        assert_eq!(floor_char_boundary(s, 4), 1);
        assert_eq!(floor_char_boundary(s, 5), 5);
        assert_eq!(floor_char_boundary(s, 100), s.len());
    }

    #[test]
    fn meminfo_parser_reads_kib_values() {
        let meminfo = "MemTotal:       1048576 kB\nMemAvailable:    262144 kB\n";
        assert_eq!(meminfo_kib(meminfo, "MemTotal"), Some(1_048_576));
        assert_eq!(meminfo_kib(meminfo, "MemAvailable"), Some(262_144));
        assert_eq!(meminfo_kib(meminfo, "Missing"), None);
    }

    #[test]
    fn normalize_accepts_server_root_or_openai_api_base() {
        assert_eq!(normalize("http://127.0.0.1:1337"), "http://127.0.0.1:1337");
        assert_eq!(normalize("http://127.0.0.1:1337/"), "http://127.0.0.1:1337");
        assert_eq!(
            normalize(" http://127.0.0.1:1337/v1/ "),
            "http://127.0.0.1:1337"
        );
        assert_eq!(
            normalize("http://127.0.0.1:1337/V1"),
            "http://127.0.0.1:1337"
        );
    }

    #[test]
    fn atomic_write_replaces_file_and_leaves_no_temp() {
        let dir = std::env::temp_dir().join(format!("docproc4-atomic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("last-run.json");
        write_atomic(&path, r#"{"v":1}"#).unwrap();
        write_atomic(&path, r#"{"v":2}"#).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), r#"{"v":2}"#);
        assert!(!path.with_extension("json.tmp").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn error_body_extraction() {
        // Lemonade's HTTP-200 error body shape
        let v = json!({"error":{"code":400,"message":"request (4055 tokens) exceeds the available context size (3328 tokens)","type":"exceed_context_size_error"}});
        assert_eq!(
            error_message(&v).unwrap(),
            "request (4055 tokens) exceeds the available context size (3328 tokens)"
        );
        // OpenAI 404 shape
        let v = json!({"error":{"code":"model_not_found","message":"Model 'x' was not found."}});
        assert_eq!(error_message(&v).unwrap(), "Model 'x' was not found.");
        // string error + no error field
        assert_eq!(error_message(&json!({"error":"boom"})).unwrap(), "boom");
        assert!(error_message(&json!({"choices":[]})).is_none());
    }
}

/// POST /v1/chat/completions with stream=true. Tokens are forwarded to the
/// frontend as `chat-stream` events: {id, kind: thinking|output|progress|stats|done|error, ...}.
#[tauri::command]
async fn chat_stream(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    base_url: String,
    api_key: String,
    payload: Value,
    headers: Option<HashMap<String, String>>,
) -> Result<(), String> {
    remember(&state, &base_url, &api_key);
    let base = normalize(&base_url);
    let client = state.http.clone();

    let task_id = id.clone();
    let task_app = app.clone();
    let handle = tokio::spawn(async move {
        use futures_util::StreamExt;

        let app = task_app;
        let emit = |kind: &str, extra: Value| {
            let mut obj = json!({ "id": task_id, "kind": kind });
            if let (Some(o), Some(e)) = (obj.as_object_mut(), extra.as_object()) {
                for (k, v) in e {
                    o.insert(k.clone(), v.clone());
                }
            }
            let _ = app.emit("chat-stream", obj);
        };

        let mut halogen_from: Option<String> = None;
        // Ok carries (timings, usage) from the stream if the server sent them
        let result: Result<(Option<Value>, Option<Value>), String> = async {
            let url = format!("{base}/v1/chat/completions");
            let mut req = authed(client.post(&url), &api_key).json(&payload);
            if let Some(extra) = headers {
                for (k, v) in extra {
                    let k = k.trim();
                    let v = v.trim();
                    if !k.is_empty() && !v.is_empty() {
                        req = req.header(k, v);
                    }
                }
            }
            let resp = req
                .send()
                .await
                .map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                let end = floor_char_boundary(&body, body.len().min(400));
                return Err(format!("HTTP {status}: {}", &body[..end]));
            }
            halogen_from = resp
                .headers()
                .get("x-halogen-from")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);

            let mut stream = resp.bytes_stream();
            let mut pending: Vec<u8> = Vec::new(); // bytes of a UTF-8 char split across chunks
            let mut buf = String::new();
            let mut content_buf = String::new();
            let mut in_thinking = false;
            let mut saw_data = false;
            let mut saw_tokens = false;
            let mut timings: Option<Value> = None;
            let mut usage: Option<Value> = None;
            let mut finish_reason: Option<String> = None;
            let mut stream_ended = false;
            loop {
                if !stream_ended {
                    match stream.next().await {
                        Some(chunk) => {
                            let chunk = chunk.map_err(|e| e.to_string())?;
                            // Network chunks can split a multi-byte UTF-8 char;
                            // decode incrementally instead of lossy-per-chunk.
                            pending.extend_from_slice(&chunk);
                            loop {
                                match std::str::from_utf8(&pending) {
                                    Ok(s) => {
                                        buf.push_str(s);
                                        pending.clear();
                                        break;
                                    }
                                    Err(e) => {
                                        let valid = e.valid_up_to();
                                        buf.push_str(
                                            std::str::from_utf8(&pending[..valid]).unwrap(),
                                        );
                                        match e.error_len() {
                                            None => {
                                                // incomplete char at the end — wait for more bytes
                                                pending.drain(..valid);
                                                break;
                                            }
                                            Some(bad) => {
                                                // genuinely invalid bytes — replace and continue
                                                buf.push('\u{FFFD}');
                                                pending.drain(..valid + bad);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        None => {
                            stream_ended = true;
                            if !pending.is_empty() {
                                buf.push_str(&String::from_utf8_lossy(&pending));
                                pending.clear();
                            }
                            // a final event may not be terminated by a blank
                            // line — make sure it still gets parsed below
                            if !buf.trim().is_empty() && !buf.ends_with("\n\n") {
                                buf.push_str("\n\n");
                            }
                        }
                    }
                }
                buf = buf.replace("\r\n", "\n");
                while let Some(pos) = buf.find("\n\n") {
                    let event: String = buf.drain(..pos).collect();
                    buf.drain(..2);
                    for raw_line in event.lines() {
                        let line = raw_line.trim();
                        if line.is_empty() {
                            continue;
                        }
                        let Some(data) = line.strip_prefix("data:") else {
                            // Lemonade reports request errors as HTTP 200 +
                            // text/event-stream with a RAW JSON error body
                            if line.starts_with('{') {
                                if let Ok(v) = serde_json::from_str::<Value>(line) {
                                    if let Some(msg) = error_message(&v) {
                                        return Err(msg);
                                    }
                                }
                            }
                            continue;
                        };
                        saw_data = true;
                        let data = data.trim();
                        if data == "[DONE]" {
                            flush_content(&mut content_buf, in_thinking, &mut |kind, text| {
                                emit(kind, json!({ "text": text }));
                            });
                            if finish_reason.as_deref() == Some("length") {
                                return Err(TRUNCATED_MSG.to_string());
                            }
                            return Ok((timings, usage));
                        }
                        let Ok(v) = serde_json::from_str::<Value>(data) else {
                            continue;
                        };
                        if let Some(msg) = error_message(&v) {
                            return Err(msg);
                        }
                        if let Some(pp) = v.get("prompt_progress") {
                            emit("progress", json!({ "progress": pp }));
                        }
                        if let Some(t) = v.get("timings") {
                            timings = Some(t.clone());
                        }
                        if let Some(u) = v.get("usage") {
                            usage = Some(u.clone());
                        }
                        if let Some(fr) = v
                            .pointer("/choices/0/finish_reason")
                            .and_then(|x| x.as_str())
                        {
                            finish_reason = Some(fr.to_string());
                        }
                        if let Some(delta) = v.pointer("/choices/0/delta") {
                            if let Some(t) = delta.get("reasoning_content").and_then(|x| x.as_str())
                            {
                                if !t.is_empty() {
                                    saw_tokens = true;
                                    emit("thinking", json!({ "text": t }));
                                }
                            }
                            if let Some(t) = delta.get("content").and_then(|x| x.as_str()) {
                                if !t.is_empty() {
                                    saw_tokens = true;
                                    content_buf.push_str(t);
                                    drain_content(
                                        &mut content_buf,
                                        &mut in_thinking,
                                        &mut |kind, text| {
                                            emit(kind, json!({ "text": text }));
                                        },
                                    );
                                }
                            }
                        }
                    }
                }
                if stream_ended {
                    break;
                }
            }
            flush_content(&mut content_buf, in_thinking, &mut |kind, text| {
                emit(kind, json!({ "text": text }));
            });
            // a stream with no SSE data at all, or one that produced zero
            // tokens, is a server-side failure (e.g. context overflow)
            if !saw_data {
                return Err("empty response from server (no SSE data)".to_string());
            }
            if !saw_tokens {
                return Err("model returned no content".to_string());
            }
            // a length-stop means the output was cut off mid-generation —
            // report it as a failure instead of a silent "done"
            if finish_reason.as_deref() == Some("length") {
                return Err(TRUNCATED_MSG.to_string());
            }
            Ok((timings, usage))
        }
        .await;

        match result {
            Ok((timings, usage)) => {
                if timings.is_some() || usage.is_some() || halogen_from.is_some() {
                    emit(
                        "stats",
                        json!({
                            "stats": timings,
                            "usage": usage,
                            "halogen_from": halogen_from,
                        }),
                    );
                } else {
                    let url = format!("{base}/v1/stats");
                    let stats_fut = authed(client.get(&url), &api_key).send();
                    if let Ok(Ok(resp)) =
                        tokio::time::timeout(std::time::Duration::from_secs(5), stats_fut).await
                    {
                        if let Ok(stats) = resp.json::<Value>().await {
                            emit("stats", json!({ "stats": stats }));
                        }
                    }
                }
                emit("done", json!({}));
            }
            Err(e) => emit("error", json!({ "message": e })),
        }

        // remove our abort handle if it's still registered
        app.state::<AppState>()
            .aborts
            .lock()
            .unwrap()
            .remove(&task_id);
    });

    state
        .aborts
        .lock()
        .unwrap()
        .insert(id.clone(), handle.abort_handle());

    // Supervisor: if the stream task panics, the frontend would otherwise
    // never receive a `done`/`error` event and the batch would hang forever.
    // Surface the panic as an error event instead.
    tokio::spawn(async move {
        if let Err(e) = handle.await {
            if e.is_panic() {
                let _ = app.emit(
                    "chat-stream",
                    json!({ "id": id, "kind": "error", "message": "internal error: stream task panicked" }),
                );
                app.state::<AppState>().aborts.lock().unwrap().remove(&id);
            }
        }
    });
    Ok(())
}

/// Non-streaming chat request that returns `usage.prompt_tokens`. Used to
/// measure how many tokens the loaded model spends on test images.
#[tauri::command]
async fn measure_prompt_tokens(
    state: State<'_, AppState>,
    base_url: String,
    api_key: String,
    payload: Value,
) -> Result<u64, String> {
    remember(&state, &base_url, &api_key);
    let url = format!("{}/v1/chat/completions", normalize(&base_url));
    let resp = authed(state.http.post(&url), &api_key)
        .timeout(std::time::Duration::from_secs(45))
        .json(&payload)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    let body = resp.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        let end = floor_char_boundary(&body, body.len().min(400));
        return Err(format!("HTTP {status}: {}", &body[..end]));
    }
    let v: Value =
        serde_json::from_str(&body).map_err(|e| format!("invalid JSON from server: {e}"))?;
    if let Some(msg) = error_message(&v) {
        return Err(msg);
    }
    v.pointer("/usage/prompt_tokens")
        .and_then(Value::as_u64)
        .ok_or_else(|| "server did not report usage.prompt_tokens".to_string())
}

#[tauri::command]
fn chat_cancel(state: State<'_, AppState>, id: String) {
    if let Some(h) = state.aborts.lock().unwrap().remove(&id) {
        h.abort();
    }
}

// ---------------------------------------------------------------------------
// Persistence (settings + instruction sets) in the app data dir
// ---------------------------------------------------------------------------

fn persist_path(app: &AppHandle, name: &str) -> Result<std::path::PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join(name))
}

fn read_json(app: &AppHandle, name: &str) -> Value {
    persist_path(app, name)
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null)
}

#[tauri::command]
fn read_persisted(app: AppHandle) -> Result<Value, String> {
    Ok(json!({
        "settings": read_json(&app, "settings.json"),
        "instructions": read_json(&app, "instructions.json"),
    }))
}

#[tauri::command]
fn write_settings(app: AppHandle, data: Value) -> Result<(), String> {
    let s = serde_json::to_string_pretty(&data).map_err(|e| e.to_string())?;
    std::fs::write(persist_path(&app, "settings.json")?, s).map_err(|e| e.to_string())
}

#[tauri::command]
fn write_instructions(app: AppHandle, data: Value) -> Result<(), String> {
    let s = serde_json::to_string_pretty(&data).map_err(|e| e.to_string())?;
    std::fs::write(persist_path(&app, "instructions.json")?, s).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Run recovery: finished batches are written to the app data dir as they
// complete, so a crash or kill doesn't lose them before the user saves.
// ---------------------------------------------------------------------------

const RECOVERY_FILE: &str = "last-run.json";

/// Serializes recovery writes: concurrent batches can finish together.
static RECOVERY_LOCK: Mutex<()> = Mutex::new(());

fn recovery_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("recovery");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// Write to a temporary file, flush it to disk, then rename it over the
/// target, so a kill mid-write leaves the previous version intact.
fn write_atomic(path: &Path, contents: &str) -> Result<(), String> {
    use std::io::Write;
    let tmp = path.with_extension("json.tmp");
    let mut f = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
    f.write_all(contents.as_bytes())
        .map_err(|e| e.to_string())?;
    f.sync_all().map_err(|e| e.to_string())?;
    drop(f);
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

#[tauri::command]
fn write_recovery(app: AppHandle, data: Value) -> Result<(), String> {
    let s = serde_json::to_string(&data).map_err(|e| e.to_string())?;
    let _guard = RECOVERY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    write_atomic(&recovery_dir(&app)?.join(RECOVERY_FILE), &s)
}

#[tauri::command]
fn read_recovery(app: AppHandle) -> Result<Value, String> {
    let path = recovery_dir(&app)?.join(RECOVERY_FILE);
    Ok(std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null))
}

#[tauri::command]
fn clear_recovery(app: AppHandle) -> Result<(), String> {
    let _guard = RECOVERY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = recovery_dir(&app)?.join(RECOVERY_FILE);
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
        _ => Ok(()),
    }
}

fn main() {
    tauri::Builder::default()
        .manage(power::PowerState::default())
        .manage(AppState {
            http: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(15))
                .build()
                .unwrap(),
            aborts: Mutex::new(HashMap::new()),
            cpu_sample: Mutex::new(None),
            last_server: Mutex::new((String::new(), String::new())),
        })
        .invoke_handler(tauri::generate_handler![
            power::inhibit_suspend,
            health,
            list_models,
            load_model,
            unload_model,
            system_stats,
            chat_stream,
            chat_cancel,
            measure_prompt_tokens,
            read_persisted,
            write_settings,
            write_instructions,
            read_recovery,
            write_recovery,
            clear_recovery,
            halogen::halogen_status,
            halogen::halogen_warm_prefix,
            halogen::halogen_release_handle,
            halogen::halogen_list_handles,
            images::import_images,
            images::prepare_image,
            images::export_dataset,
        ])
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { .. } = event {
                for (_, h) in window.state::<AppState>().aborts.lock().unwrap().drain() {
                    h.abort();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("ImageProc Halogen failed to start");
}

//! Native integration with peonist-ai/halogen-flash-server.
use crate::{authed, error_message, floor_char_boundary, normalize, remember, AppState};
use serde_json::{json, Value};
use std::time::Duration;
use tauri::State;

const DEFAULT_VISION_REASON: &str = "Vision tower is not loaded on this halogen-flash-server. Start the container with -e HALOGEN_VISION_TOWER=1.";

/// Determine whether the connected halogen-flash-server has vision enabled.
pub fn detect_vision(health: &Value, models: &[Value]) -> (bool, String) {
    let explicit_vision = health
        .pointer("/vision/enabled")
        .and_then(Value::as_bool)
        .or_else(|| health.get("vision").and_then(Value::as_bool));
    let reason = health
        .pointer("/vision/reason")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(DEFAULT_VISION_REASON)
        .to_string();

    if let Some(enabled) = explicit_vision {
        return if enabled {
            (true, String::new())
        } else {
            (false, reason)
        };
    }

    let has_image_modality = models.iter().any(|m| {
        m.pointer("/architecture/input_modalities")
            .and_then(Value::as_array)
            .is_some_and(|arr| arr.iter().any(|x| x.as_str() == Some("image")))
    });
    if has_image_modality {
        return (true, String::new());
    }

    if let Some(enabled) = health
        .pointer("/structured_output/images")
        .and_then(Value::as_bool)
    {
        return if enabled {
            (true, String::new())
        } else {
            (false, reason)
        };
    }

    if let Some(enabled) = health.pointer("/modalities/vision").and_then(Value::as_bool) {
        return if enabled {
            (true, String::new())
        } else {
            (false, reason)
        };
    }

    let reports_text_only = !models.is_empty()
        && models.iter().all(|m| {
            m.pointer("/architecture/input_modalities")
                .and_then(Value::as_array)
                .is_some_and(|arr| !arr.is_empty() && arr.iter().all(|x| x.as_str() != Some("image")))
        });
    if reports_text_only {
        return (false, reason);
    }

    (true, String::new())
}

async fn probe_health(
    client: &reqwest::Client,
    base: &str,
    key: &str,
) -> Result<Value, String> {
    for path in ["/health", "/v1/health"] {
        let resp = authed(client.get(format!("{base}{path}")), key)
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if resp.status().is_success() {
            let body = resp.json::<Value>().await.unwrap_or_else(|_| json!({}));
            return Ok(if body.is_object() { body } else { json!({}) });
        }
        if !matches!(resp.status().as_u16(), 404 | 405) {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            let end = floor_char_boundary(&text, text.len().min(400));
            return Err(format!("HTTP {status}: {}", &text[..end]));
        }
    }
    Ok(json!({}))
}

async fn fetch_status(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
) -> Result<Value, String> {
    let base = normalize(base_url);
    let health_body = probe_health(client, &base, api_key).await?;

    let models_resp = authed(client.get(format!("{base}/v1/models")), api_key)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !models_resp.status().is_success() {
        let status = models_resp.status();
        let text = models_resp.text().await.unwrap_or_default();
        let end = floor_char_boundary(&text, text.len().min(400));
        return Err(format!("HTTP {status}: {}", &text[..end]));
    }
    let models_body: Value = models_resp.json().await.map_err(|e| e.to_string())?;
    let models = models_body
        .get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let cache_body = match authed(client.get(format!("{base}/cache")), api_key)
        .timeout(Duration::from_secs(5))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            resp.json::<Value>().await.unwrap_or(Value::Null)
        }
        _ => Value::Null,
    };

    let (vision_enabled, vision_reason) = detect_vision(&health_body, &models);
    let model_id = models
        .iter()
        .find_map(|m| m.get("id").and_then(Value::as_str).map(str::to_owned))
        .or_else(|| {
            health_body
                .get("model_id")
                .or_else(|| health_body.get("model"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "halogen-qwen3.8-flash-next".to_string());

    Ok(json!({
        "status": "ok",
        "version": health_body.get("version").cloned().unwrap_or(Value::Null),
        "model_id": model_id,
        "models": models,
        "vision_enabled": vision_enabled,
        "vision_reason": vision_reason,
        "vision": health_body.get("vision").cloned().unwrap_or(Value::Null),
        "context": health_body.get("context").cloned().unwrap_or_else(|| json!(262144)),
        "slots": health_body.get("slots").cloned().unwrap_or(Value::Null),
        "kv_cache": health_body.get("kv_cache").cloned().unwrap_or(Value::Null),
        "max_tokens_default": health_body.get("max_tokens_default").cloned().unwrap_or_else(|| json!(8192)),
        "max_tokens_cap": health_body.get("max_tokens_cap").cloned().unwrap_or_else(|| json!(65536)),
        "reasoning_effort_default": health_body.get("reasoning_effort_default").cloned().unwrap_or(Value::Null),
        "thinking_answer_room": health_body.get("thinking_answer_room").cloned().unwrap_or(Value::Null),
        "server_defaults": health_body.get("server_defaults").cloned().unwrap_or(Value::Null),
        "busy": health_body.get("busy").cloned().unwrap_or_else(|| json!(false)),
        "in_flight": health_body.get("in_flight").cloned().unwrap_or_else(|| json!(0)),
        "queued": health_body.get("queued").cloned().unwrap_or_else(|| json!(0)),
        "longest_wait_s": health_body.get("longest_wait_s").cloned().unwrap_or_else(|| json!(0)),
        "cache": cache_body,
    }))
}

async fn warm_prefix(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    handle_name: Option<&str>,
    payload: &Value,
) -> Result<Value, String> {
    let url = format!("{}/v1/chat/completions", normalize(base_url));
    let mut req = authed(client.post(&url), api_key)
        .timeout(Duration::from_secs(180))
        .json(payload);
    let kept = handle_name
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(name) = kept {
        req = req.header("X-Halogen-Keep", name);
    }
    let resp = req.send().await.map_err(|e| e.to_string())?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        let end = floor_char_boundary(&text, text.len().min(400));
        return Err(format!("HTTP {status}: {}", &text[..end]));
    }
    let body: Value =
        serde_json::from_str(&text).map_err(|e| format!("Invalid JSON from server: {e}"))?;
    if let Some(msg) = error_message(&body) {
        return Err(msg);
    }
    Ok(json!({
        "ok": true,
        "handle": kept,
        "usage": body.get("usage").cloned().unwrap_or(Value::Null),
        "timings": body.get("timings").cloned().unwrap_or(Value::Null),
    }))
}

async fn release_handle(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    handle_name: &str,
) -> Result<Value, String> {
    let handle = handle_name.trim();
    if handle.is_empty() || handle.contains(['/', '\\', '?', '#']) {
        return Err("Invalid halogen session handle name".into());
    }
    let url = format!("{}/halogen/handles/{handle}", normalize(base_url));
    let resp = authed(client.delete(&url), api_key)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if matches!(resp.status().as_u16(), 200 | 204 | 404) {
        return Ok(json!({ "released": handle }));
    }
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    let end = floor_char_boundary(&text, text.len().min(400));
    Err(format!("HTTP {status}: {}", &text[..end]))
}

async fn list_handles(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
) -> Result<Value, String> {
    let url = format!("{}/halogen/handles", normalize(base_url));
    let resp = authed(client.get(&url), api_key)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if resp.status().as_u16() == 404 {
        return Ok(json!([]));
    }
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        let end = floor_char_boundary(&text, text.len().min(400));
        return Err(format!("HTTP {status}: {}", &text[..end]));
    }
    resp.json::<Value>().await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn halogen_status(
    state: State<'_, AppState>,
    base_url: String,
    api_key: String,
) -> Result<Value, String> {
    remember(&state, &base_url, &api_key);
    fetch_status(&state.http, &base_url, &api_key).await
}

#[tauri::command]
pub async fn halogen_warm_prefix(
    state: State<'_, AppState>,
    base_url: String,
    api_key: String,
    handle_name: Option<String>,
    payload: Value,
) -> Result<Value, String> {
    remember(&state, &base_url, &api_key);
    warm_prefix(&state.http, &base_url, &api_key, handle_name.as_deref(), &payload).await
}

#[tauri::command]
pub async fn halogen_release_handle(
    state: State<'_, AppState>,
    base_url: String,
    api_key: String,
    handle_name: String,
) -> Result<Value, String> {
    remember(&state, &base_url, &api_key);
    release_handle(&state.http, &base_url, &api_key, &handle_name).await
}

#[tauri::command]
pub async fn halogen_list_handles(
    state: State<'_, AppState>,
    base_url: String,
    api_key: String,
) -> Result<Value, String> {
    remember(&state, &base_url, &api_key);
    list_handles(&state.http, &base_url, &api_key).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn detects_vision_from_health_and_modalities() {
        assert_eq!(
            detect_vision(
                &json!({}),
                &[json!({"id":"halogen-qwen3.8-flash-next","architecture":{"input_modalities":["text","image"]}})]
            ),
            (true, String::new())
        );

        let (ok, reason) = detect_vision(
            &json!({}),
            &[json!({"id":"halogen-qwen3.8-flash-next","architecture":{"input_modalities":["text"]}})],
        );
        assert!(!ok);
        assert!(reason.contains("HALOGEN_VISION_TOWER"));

        let (ok, reason) = detect_vision(
            &json!({"vision":{"enabled":false,"reason":"set HALOGEN_VISION_TOWER=1"}}),
            &[],
        );
        assert!(!ok);
        assert_eq!(reason, "set HALOGEN_VISION_TOWER=1");
    }

    fn read_http_request(stream: &mut std::net::TcpStream) -> String {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut bytes = vec![];
        let mut buf = [0; 4096];
        loop {
            let n = stream.read(&mut buf).unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&buf[..n]);
            if let Some(end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                let header = String::from_utf8_lossy(&bytes[..end]);
                let length = header
                    .lines()
                    .find_map(|l| {
                        l.to_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|v| v.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if bytes.len() >= end + 4 + length {
                    break;
                }
            }
        }
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn halogen_status_warm_and_release_http_flow() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for expected in [
                "GET /health ",
                "GET /v1/models ",
                "GET /cache ",
                "POST /v1/chat/completions ",
                "DELETE /halogen/handles/ref-1234 ",
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                let req = read_http_request(&mut stream);
                assert!(req.starts_with(expected), "unexpected request: {req}");
                let body = if expected.starts_with("GET /health") {
                    json!({
                        "version": {"api": "0.20.6", "engine": "0.20.6"},
                        "context": 262144,
                        "max_tokens_default": 8192,
                        "vision": {"enabled": true},
                        "busy": false,
                        "in_flight": 0,
                        "queued": 0,
                        "longest_wait_s": 0
                    })
                } else if expected.starts_with("GET /v1/models") {
                    json!({"data": [{"id": "halogen-qwen3.8-flash-next", "architecture": {"input_modalities": ["text", "image"]}}]})
                } else if expected.starts_with("GET /cache") {
                    json!({"hit_rate": 0.75, "token_hit_rate": 0.92, "pool": {"positions": 524288, "used": 65536}})
                } else if expected.starts_with("POST /v1/chat/completions") {
                    let lower = req.to_lowercase();
                    assert!(lower.contains("x-halogen-keep: ref-1234"), "{req}");
                    json!({"usage": {"prompt_tokens": 512}, "timings": {"prompt_n": 512}})
                } else {
                    json!({"released": true})
                };
                let body_str = body.to_string();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body_str.len(),
                    body_str
                )
                .unwrap();
            }
        });

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let client = reqwest::Client::new();
        let base = format!("http://{address}/v1");
        runtime.block_on(async {
            let st = fetch_status(&client, &base, "secret").await.unwrap();
            assert_eq!(st["model_id"], "halogen-qwen3.8-flash-next");
            assert_eq!(st["vision_enabled"], true);
            assert_eq!(st["cache"]["pool"]["positions"], 524288);

            let warmed = warm_prefix(
                &client,
                &base,
                "secret",
                Some("ref-1234"),
                &json!({"model": "halogen-qwen3.8-flash-next", "messages": []}),
            )
            .await
            .unwrap();
            assert_eq!(warmed["ok"], true);
            assert_eq!(warmed["handle"], "ref-1234");

            let rel = release_handle(&client, &base, "secret", "ref-1234")
                .await
                .unwrap();
            assert_eq!(rel["released"], "ref-1234");
        });
        server.join().unwrap();
    }
}

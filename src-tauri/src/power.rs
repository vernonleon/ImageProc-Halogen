use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;
use tauri::State;

#[derive(Default)]
pub struct PowerState(Mutex<Option<Inhibitor>>);

struct Inhibitor(Child);
impl Drop for Inhibitor {
    fn drop(&mut self) {
        // Closing stdin also releases the inhibitor if the app exits unexpectedly.
        self.0.stdin.take();
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn acquire() -> Result<Inhibitor, String> {
    let child = Command::new("systemd-inhibit")
        .args([
            "--no-ask-password",
            "--what=sleep:idle",
            "--mode=block",
            "--who=ImageProc",
            "--why=Captioning images",
            "sh",
            "-c",
            "printf 'ready\\n'; exec cat >/dev/null",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Cannot prevent sleep: {e}"))?;
    let mut guard = Inhibitor(child);
    let stdout = guard.0.stdout.take().ok_or("Missing inhibitor output")?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let ready = BufReader::new(stdout).read_line(&mut line).is_ok() && line.trim() == "ready";
        let _ = tx.send(ready);
    });
    if rx.recv_timeout(Duration::from_secs(5)) != Ok(true) {
        return Err("Cannot prevent sleep: systemd denied or could not establish the inhibitor. Check your desktop power settings.".into());
    }
    Ok(guard)
}

#[tauri::command]
pub async fn inhibit_suspend(state: State<'_, PowerState>, active: bool) -> Result<(), String> {
    // Acquire on a blocking worker so the desktop UI remains responsive.
    if active {
        let guard = tauri::async_runtime::spawn_blocking(acquire)
            .await
            .map_err(|e| e.to_string())??;
        *state.0.lock().map_err(|e| e.to_string())? = Some(guard);
    } else {
        state.0.lock().map_err(|e| e.to_string())?.take();
    }
    Ok(())
}

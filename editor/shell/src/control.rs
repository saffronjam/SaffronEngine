//! The control-plane passthrough: the one socket round-trip the whole shell IPC is built on.
//! Newline-delimited JSON over the per-PID unix socket; the engine's `ok:false` reply becomes a
//! typed error (message + envelope `code`). It is
//! shell-agnostic (`UnixStream` + `serde_json`); the caller is the CEF
//! query handler, wired next). Consumed by the CEF message-router query handler and by the
//! lifecycle/teardown path (Phase 6).

use serde::Serialize;
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::Mutex;
use std::time::Duration;

/// Serializes every control-plane round-trip: the engine drains control once per frame, so
/// concurrent requests otherwise pile into that drain and trip the 5s read timeout ("os error 11").
/// Holding this across connect+write+read keeps exactly one round-trip outstanding, regardless of
/// caller. The guarded data is `()`, so a poisoned lock is recovered rather than fatal.
static CONTROL_IO: Mutex<()> = Mutex::new(());

/// A control-plane failure surfaced to the UI: the engine's human message plus the machine-readable
/// envelope `code` (present on every `ok:false`), so the typed client can match on `code` — e.g.
/// drop a `busy-loading` reply on a background poll lane instead of toasting it.
#[derive(Debug, Clone, Serialize)]
#[allow(dead_code)] // fields read by the JS client via the serialized reply (Phase 4/5)
pub struct ControlError {
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

impl From<String> for ControlError {
    fn from(message: String) -> Self {
        Self {
            message,
            code: None,
        }
    }
}

impl ControlError {
    /// A failure carrying an explicit machine-readable `code` the typed client can match on.
    pub fn coded(message: impl Into<String>, code: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: Some(code.into()),
        }
    }
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ControlError {}

/// The one socket round-trip helper the whole bridge is built on. Surfaces the engine's `ok:false`
/// reply as a typed `Err` (message + envelope `code`).
#[allow(dead_code)] // wired to the CEF message-router query handler next
pub fn control_request_with_params(
    socket_path: &str,
    command: &str,
    params: Value,
) -> Result<Value, ControlError> {
    let _guard = CONTROL_IO
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut stream = UnixStream::connect(socket_path)
        .map_err(|err| format!("control socket unavailable: {err}"))?;
    stream
        .set_read_timeout(Some(Duration::from_millis(5000)))
        .map_err(|err| format!("set read timeout: {err}"))?;
    let mut request = json!({ "id": 1, "cmd": command, "params": params }).to_string();
    request.push('\n');
    stream
        .write_all(request.as_bytes())
        .map_err(|err| format!("send control request: {err}"))?;

    let mut reply = String::new();
    let mut buffer = [0_u8; 4096];
    while !reply.contains('\n') {
        let read = stream
            .read(&mut buffer)
            .map_err(|err| format!("read control reply: {err}"))?;
        if read == 0 {
            break;
        }
        reply.push_str(&String::from_utf8_lossy(&buffer[..read]));
    }
    let value: Value =
        serde_json::from_str(reply.trim()).map_err(|err| format!("decode control reply: {err}"))?;
    if value.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        return Ok(value.get("result").cloned().unwrap_or_default());
    }
    Err(ControlError {
        message: value
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("control command failed")
            .to_string(),
        code: value
            .get("code")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
    })
}

#[allow(dead_code)] // wired to the lifecycle/teardown path (Phase 6)
pub fn control_request(socket_path: &str, command: &str) -> Result<Value, ControlError> {
    control_request_with_params(socket_path, command, json!({}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixListener;
    use std::thread;

    /// A one-shot mock control server: accepts one connection, reads the request line, replies with
    /// `reply` + newline, and returns the request line it saw.
    fn mock_server(name: &str, reply: &'static str) -> (String, thread::JoinHandle<String>) {
        let path = std::env::temp_dir().join(format!(
            "saffron-ctrl-test-{}-{name}.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let path_str = path.to_string_lossy().into_owned();
        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let mut writer = stream;
            writer.write_all(reply.as_bytes()).unwrap();
            writer.write_all(b"\n").unwrap();
            line
        });
        (path_str, handle)
    }

    #[test]
    fn ok_reply_returns_result_and_sends_envelope() {
        let (sock, handle) = mock_server("ok", r#"{"ok":true,"result":{"v":42}}"#);
        let out = control_request_with_params(&sock, "get-thing", json!({"a":1})).unwrap();
        assert_eq!(out, json!({"v":42}));
        let request: Value = serde_json::from_str(handle.join().unwrap().trim()).unwrap();
        assert_eq!(request["cmd"], "get-thing");
        assert_eq!(request["params"], json!({"a":1}));
        assert_eq!(request["id"], 1);
        let _ = std::fs::remove_file(&sock);
    }

    #[test]
    fn error_reply_carries_message_and_code() {
        let (sock, handle) = mock_server(
            "err",
            r#"{"ok":false,"error":"nope","code":"busy-loading"}"#,
        );
        let err = control_request(&sock, "do-thing").unwrap_err();
        assert_eq!(err.message, "nope");
        assert_eq!(err.code.as_deref(), Some("busy-loading"));
        handle.join().unwrap();
        let _ = std::fs::remove_file(&sock);
    }

    /// Phase 9 §3: the `CONTROL_IO` single-flight must survive concurrent callers. A burst of threads
    /// hits `control_request_with_params` at once against a server that widens each handler's window
    /// and tracks peak concurrency; if the lock serializes connect+write+read as intended, the server
    /// never sees more than one request in flight (a broken lock would let the burst overlap and, in
    /// the real engine, pile into its per-frame drain and trip the 5s read timeout).
    #[test]
    fn control_io_serializes_a_concurrent_burst() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        const N: usize = 12;
        let path =
            std::env::temp_dir().join(format!("saffron-ctrl-burst-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let sock = path.to_string_lossy().into_owned();

        let in_flight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let served = Arc::new(AtomicUsize::new(0));
        let (sif, speak, sserved) = (
            Arc::clone(&in_flight),
            Arc::clone(&peak),
            Arc::clone(&served),
        );
        let server = thread::spawn(move || {
            let mut handlers = Vec::new();
            for _ in 0..N {
                let (stream, _) = listener.accept().unwrap();
                let (sif, speak, sserved) =
                    (Arc::clone(&sif), Arc::clone(&speak), Arc::clone(&sserved));
                handlers.push(thread::spawn(move || {
                    let now = sif.fetch_add(1, Ordering::SeqCst) + 1;
                    speak.fetch_max(now, Ordering::SeqCst);
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    thread::sleep(Duration::from_millis(25)); // widen the overlap window
                    let mut writer = stream;
                    writer.write_all(br#"{"ok":true,"result":{}}"#).unwrap();
                    writer.write_all(b"\n").unwrap();
                    sif.fetch_sub(1, Ordering::SeqCst);
                    sserved.fetch_add(1, Ordering::SeqCst);
                }));
            }
            for h in handlers {
                h.join().unwrap();
            }
        });

        let clients: Vec<_> = (0..N)
            .map(|_| {
                let sock = sock.clone();
                thread::spawn(move || control_request_with_params(&sock, "ping", json!({})).is_ok())
            })
            .collect();
        let all_ok = clients.into_iter().all(|c| c.join().unwrap());
        server.join().unwrap();

        assert!(
            all_ok,
            "every request in the burst round-tripped (none timed out)"
        );
        assert_eq!(served.load(Ordering::SeqCst), N, "all {N} requests served");
        assert_eq!(
            peak.load(Ordering::SeqCst),
            1,
            "CONTROL_IO kept exactly one round-trip in flight across the burst"
        );
        let _ = std::fs::remove_file(&sock);
    }
}

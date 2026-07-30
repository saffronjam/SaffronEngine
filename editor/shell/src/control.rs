//! The control-plane passthrough: the one socket round-trip the whole shell IPC is built on.
//! Newline-delimited JSON over the per-PID unix socket; the engine's `ok:false` reply becomes the
//! shared typed failure object.

use saffron_protocol::ControlFailureDto;
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

/// The shared control failure surfaced to the UI.
#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
pub struct ControlError {
    failure: Box<ControlFailureDto>,
}

impl From<String> for ControlError {
    fn from(message: String) -> Self {
        Self::bridge(message)
    }
}

impl ControlError {
    /// A native editor command failure outside the engine control socket.
    pub fn bridge(message: impl Into<String>) -> Self {
        Self {
            failure: Box::new(ControlFailureDto::Bridge {
                message: message.into(),
            }),
        }
    }

    /// A socket connection, write, timeout, or read failure.
    fn transport(message: impl Into<String>) -> Self {
        Self {
            failure: Box::new(ControlFailureDto::Transport {
                message: message.into(),
            }),
        }
    }

    /// A reply that did not match the generated envelope contract.
    fn malformed_reply(message: impl Into<String>) -> Self {
        Self {
            failure: Box::new(ControlFailureDto::MalformedReply {
                message: message.into(),
            }),
        }
    }

    /// The exact shared failure object serialized to CEF.
    #[cfg(test)]
    pub(crate) fn failure(&self) -> &ControlFailureDto {
        &self.failure
    }
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.failure.fmt(f)
    }
}

impl std::error::Error for ControlError {}

/// The one socket round-trip helper the whole bridge is built on. Surfaces the engine's `ok:false`
/// reply as the exact shared typed failure object.
pub fn control_request_with_params(
    socket_path: &str,
    command: &str,
    params: Value,
) -> Result<Value, ControlError> {
    let _guard = CONTROL_IO
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut stream = UnixStream::connect(socket_path)
        .map_err(|err| ControlError::transport(format!("control socket unavailable: {err}")))?;
    stream
        .set_read_timeout(Some(Duration::from_millis(5000)))
        .map_err(|err| ControlError::transport(format!("set read timeout: {err}")))?;
    let mut request = json!({ "id": 1, "cmd": command, "params": params }).to_string();
    request.push('\n');
    stream
        .write_all(request.as_bytes())
        .map_err(|err| ControlError::transport(format!("send control request: {err}")))?;

    let mut reply = String::new();
    let mut buffer = [0_u8; 4096];
    while !reply.contains('\n') {
        let read = stream
            .read(&mut buffer)
            .map_err(|err| ControlError::transport(format!("read control reply: {err}")))?;
        if read == 0 {
            break;
        }
        reply.push_str(&String::from_utf8_lossy(&buffer[..read]));
    }
    let value: Value = serde_json::from_str(reply.trim())
        .map_err(|err| ControlError::malformed_reply(format!("decode control reply: {err}")))?;
    let Some(object) = value.as_object() else {
        return Err(ControlError::malformed_reply(
            "control reply is not an object",
        ));
    };
    if !object.contains_key("id") {
        return Err(ControlError::malformed_reply(
            "control reply is missing its id",
        ));
    }
    match object.get("ok").and_then(Value::as_bool) {
        Some(true)
            if object.len() == 3
                && object.contains_key("result")
                && !object.contains_key("error") =>
        {
            Ok(object["result"].clone())
        }
        Some(false)
            if object.len() == 3
                && object.contains_key("error")
                && !object.contains_key("result") =>
        {
            let failure = serde_json::from_value(object["error"].clone()).map_err(|err| {
                ControlError::malformed_reply(format!("decode control failure: {err}"))
            })?;
            Err(ControlError {
                failure: Box::new(failure),
            })
        }
        _ => Err(ControlError::malformed_reply(
            "control reply does not match the generated envelope",
        )),
    }
}

/// A parameterless round trip.
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
        let (sock, handle) = mock_server("ok", r#"{"id":1,"ok":true,"result":{"v":42}}"#);
        let out = control_request_with_params(&sock, "get-thing", json!({"a":1})).unwrap();
        assert_eq!(out, json!({"v":42}));
        let request: Value = serde_json::from_str(handle.join().unwrap().trim()).unwrap();
        assert_eq!(request["cmd"], "get-thing");
        assert_eq!(request["params"], json!({"a":1}));
        assert_eq!(request["id"], 1);
        let _ = std::fs::remove_file(&sock);
    }

    #[test]
    fn error_reply_preserves_the_shared_failure() {
        let (sock, handle) = mock_server(
            "err",
            r#"{"id":1,"ok":false,"error":{"code":"diagnostic","message":"graph candidates limit exceeded","diagnostic":{"domain":"vegetation-graph","detail":{"category":"limit","resource":"candidates","requested":"16","limit":"4"}}}}"#,
        );
        let err = control_request(&sock, "do-thing").unwrap_err();
        let ControlFailureDto::Diagnostic { diagnostic, .. } = err.failure() else {
            panic!("expected diagnostic failure")
        };
        assert_eq!(
            diagnostic,
            &saffron_protocol::ControlDiagnosticDto::VegetationGraph(
                saffron_protocol::VegetationGraphDiagnosticDto::Limit {
                    resource: "candidates".to_owned(),
                    requested: "16".to_owned(),
                    limit: "4".to_owned(),
                }
            )
        );
        handle.join().unwrap();
        let _ = std::fs::remove_file(&sock);
    }

    #[test]
    fn string_error_reply_is_malformed() {
        let (sock, handle) = mock_server(
            "old-error-shape",
            r#"{"id":1,"ok":false,"error":"nope","code":"command"}"#,
        );
        let err = control_request(&sock, "do-thing").unwrap_err();
        assert_eq!(err.failure().code(), "malformed-reply");
        handle.join().unwrap();
        let _ = std::fs::remove_file(&sock);
    }

    /// The `CONTROL_IO` single-flight must survive concurrent callers: a burst of threads hits
    /// `control_request_with_params` at once against a server that widens each handler's window and
    /// tracks peak concurrency. A broken lock would let the burst overlap and, against the real
    /// engine, pile into its per-frame drain and trip the 5 s read timeout.
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
                    writer
                        .write_all(br#"{"id":1,"ok":true,"result":{}}"#)
                        .unwrap();
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

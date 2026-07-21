//! The JSON-over-unix-socket control client, shared by the `sa` CLI and the Rust e2e
//! harness.
//!
//! The framing (newline-delimited `<json>\n` requests, one reply line per request), the request
//! envelope (`{ "id", "cmd", "params" }`), the reply envelope (`{ "ok", "result", "error" }`), and
//! the socket-path resolution all live here. The `sa` CLI's argument coercion (`build_params`) is
//! *argument* parsing, not wire framing, so it stays in the CLI; everything the wire touches is
//! this crate.
//!
//! It links only `saffron-protocol` (the DTOs + the `Uuid` decimal-string encoding) and
//! `serde`/`serde_json`, so it runs on the host outside the build toolbox — no renderer, no Jolt.

#![deny(unsafe_code)]

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use saffron_protocol::ControlFailureDto;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

/// A failure talking to the control socket: a transport error, a malformed reply, an `ok:false`
/// engine error, or a typed-decode mismatch. Typed (`thiserror`) so callers can `match`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Connect/read/write against the socket failed.
    #[error("cannot connect to {path}: {source}")]
    Transport {
        /// The socket path the connection was attempted on.
        path: String,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// The reply line was not valid JSON.
    #[error("malformed reply")]
    MalformedReply,
    /// The engine answered with a typed failure object.
    #[error("{cmd}: {failure}")]
    Engine {
        /// The command whose call failed.
        cmd: String,
        /// The engine's structured failure, verbatim.
        failure: Box<ControlFailureDto>,
    },
    /// The `result` did not deserialize into the requested DTO.
    #[error("decoding {cmd} result: {source}")]
    Decode {
        /// The command whose result failed to decode.
        cmd: String,
        /// The serde error.
        source: serde_json::Error,
    },
}

/// The crate result alias bound to this crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// The control-socket path, resolved by the server's frozen rule:
/// `SAFFRON_CONTROL_SOCK` if set, else `$XDG_RUNTIME_DIR/saffron-control.sock`, else
/// `/tmp/saffron-control-<uid>.sock`.
#[must_use]
pub fn socket_path() -> String {
    resolve_socket_path(
        std::env::var("SAFFRON_CONTROL_SOCK").ok().as_deref(),
        std::env::var("XDG_RUNTIME_DIR").ok().as_deref(),
        rustix::process::getuid().as_raw(),
    )
}

/// The pure path-resolution rule behind [`socket_path`], split out so the precedence is testable
/// without mutating process-global env.
#[must_use]
pub fn resolve_socket_path(
    override_path: Option<&str>,
    runtime_dir: Option<&str>,
    uid: u32,
) -> String {
    if let Some(path) = override_path {
        return path.to_owned();
    }
    if let Some(runtime) = runtime_dir {
        return format!("{runtime}/saffron-control.sock");
    }
    format!("/tmp/saffron-control-{uid}.sock")
}

/// Builds the request envelope `{ "cmd": <name>, "params": <obj>, "id": <id> }` the engine reads.
///
/// `params` is forced to a JSON object (an empty object when `Null`); a non-object/non-null value
/// is preserved for the (rare) caller that builds a custom body.
#[must_use]
pub fn request_envelope(id: u64, cmd: &str, params: Value) -> Value {
    let params = match params {
        Value::Null => Value::Object(Map::new()),
        other => other,
    };
    json!({ "cmd": cmd, "params": params, "id": id })
}

/// One blocking JSON round-trip against the control socket: connect, write `<envelope>\n`, read
/// one reply line. Returns the raw reply line (without the trailing newline trimmed — the caller
/// parses the envelope).
fn round_trip(path: &str, request: &Value) -> Result<String> {
    let mut stream = UnixStream::connect(path).map_err(|source| Error::Transport {
        path: path.to_owned(),
        source,
    })?;

    let mut line = request.to_string();
    line.push('\n');
    stream
        .write_all(line.as_bytes())
        .map_err(|source| Error::Transport {
            path: path.to_owned(),
            source,
        })?;

    let mut reply = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        let received = stream
            .read(&mut buffer)
            .map_err(|source| Error::Transport {
                path: path.to_owned(),
                source,
            })?;
        if received == 0 {
            break;
        }
        reply.extend_from_slice(&buffer[..received]);
        if reply.contains(&b'\n') {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&reply).into_owned())
}

/// Parses a reply line into its `result` value, lifting an `ok:false` envelope into [`Error::Engine`]
/// and a non-JSON line into [`Error::MalformedReply`].
fn parse_reply(cmd: &str, reply: &str) -> Result<Value> {
    let Ok(response) = serde_json::from_str::<Value>(reply) else {
        return Err(Error::MalformedReply);
    };
    let Some(object) = response.as_object() else {
        return Err(Error::MalformedReply);
    };
    if !object.contains_key("id") {
        return Err(Error::MalformedReply);
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
            let failure = serde_json::from_value::<ControlFailureDto>(object["error"].clone())
                .map_err(|_| Error::MalformedReply)?;
            Err(Error::Engine {
                cmd: cmd.to_owned(),
                failure: Box::new(failure),
            })
        }
        _ => Err(Error::MalformedReply),
    }
}

/// A control client bound to one socket path, owning the monotonic request-id counter.
///
/// Each [`Client::call`] is an independent connect/write/read round-trip (the server framing is
/// drain-once-per-frame, so a client does not hold a connection open across calls); the client only
/// carries the path and the next id.
pub struct Client {
    path: String,
    next_id: u64,
}

impl Client {
    /// A client for `path` (e.g. from [`socket_path`] or a per-run e2e socket).
    #[must_use]
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            next_id: 1,
        }
    }

    /// A client for the default-resolved [`socket_path`].
    #[must_use]
    pub fn from_env() -> Self {
        Self::new(socket_path())
    }

    /// The socket path this client talks to.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Sends `cmd` with `params` and returns the raw `result` [`Value`] (after lifting the envelope
    /// error). The lowest-level call; prefer [`Client::call`] when the result has a DTO.
    pub fn call_raw(&mut self, cmd: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let request = request_envelope(id, cmd, params);
        let reply = round_trip(&self.path, &request)?;
        parse_reply(cmd, &reply)
    }

    /// Sends `cmd` with `params` and returns the raw reply line verbatim (trailing newline
    /// trimmed), after lifting an `ok:false` envelope into [`Error::Engine`]. The byte-exact path
    /// the decimal-string-u64 contract gate needs: a parsed [`Value`] already coerces a JSON number
    /// into a `Number`, erasing the quoted-vs-bare distinction `assert_raw_u64` is built to catch.
    pub fn call_raw_text(&mut self, cmd: &str, params: Value) -> Result<String> {
        let id = self.next_id;
        self.next_id += 1;
        let request = request_envelope(id, cmd, params);
        let reply = round_trip(&self.path, &request)?;
        // Surface an `ok:false` envelope as an engine error (matching `call_raw`), but return the
        // raw line on success so the caller sees the exact bytes the engine emitted.
        parse_reply(cmd, &reply)?;
        Ok(reply.trim_end_matches('\n').to_owned())
    }

    /// Sends `cmd` with `params` and decodes the `result` into the typed DTO `R` — the typed-DTO
    /// path the Rust e2e harness wants (deserialize a `render-stats` reply straight into the
    /// protocol DTO, with the `Uuid` decimal-string adapter doing the right thing).
    pub fn call<R: DeserializeOwned>(&mut self, cmd: &str, params: Value) -> Result<R> {
        let result = self.call_raw(cmd, params)?;
        serde_json::from_value(result).map_err(|source| Error::Decode {
            cmd: cmd.to_owned(),
            source,
        })
    }

    /// Whether the engine answers a connection on this client's socket right now (a stale or
    /// not-yet-bound socket returns `false`). Used by readiness polling.
    #[must_use]
    pub fn is_up(&self) -> bool {
        UnixStream::connect(&self.path).is_ok()
    }

    /// Blocks until [`Client::is_up`] succeeds or `timeout` elapses, polling every 50ms. Returns
    /// whether the socket came up within the deadline.
    #[must_use]
    pub fn wait_until_up(&self, timeout: Duration) -> bool {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if self.is_up() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        self.is_up()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn socket_path_prefers_override() {
        assert_eq!(
            resolve_socket_path(Some("/run/custom.sock"), Some("/run/user/1000"), 1000),
            "/run/custom.sock"
        );
    }

    #[test]
    fn socket_path_falls_back_to_runtime_dir() {
        assert_eq!(
            resolve_socket_path(None, Some("/run/user/1000"), 1000),
            "/run/user/1000/saffron-control.sock"
        );
    }

    #[test]
    fn socket_path_falls_back_to_tmp_with_uid() {
        assert_eq!(
            resolve_socket_path(None, None, 4242),
            "/tmp/saffron-control-4242.sock"
        );
    }

    #[test]
    fn request_envelope_shape() {
        let request = request_envelope(1, "ping", Value::Null);
        assert_eq!(request["cmd"], json!("ping"));
        assert_eq!(request["id"], json!(1));
        assert_eq!(request["params"], json!({}));
    }

    #[test]
    fn request_envelope_keeps_a_supplied_object() {
        let request = request_envelope(7, "set-camera", json!({ "yaw": 90 }));
        assert_eq!(request["id"], json!(7));
        assert_eq!(request["params"]["yaw"], json!(90));
    }

    #[test]
    fn parse_reply_extracts_result() {
        let result = parse_reply("ping", r#"{"id":1,"ok":true,"result":{"pong":true}}"#).unwrap();
        assert_eq!(result["pong"], json!(true));
    }

    #[test]
    fn parse_reply_rejects_success_without_result() {
        assert!(matches!(
            parse_reply("quit", r#"{"id":1,"ok":true}"#),
            Err(Error::MalformedReply)
        ));
    }

    #[test]
    fn parse_reply_lifts_engine_error() {
        let err = parse_reply(
            "nope",
            r#"{"id":1,"ok":false,"error":{"code":"command","message":"unknown command 'nope'"}}"#,
        )
        .expect_err("ok:false must be an error");
        match err {
            Error::Engine { cmd, failure } => {
                assert_eq!(cmd, "nope");
                assert_eq!(failure.code(), "command");
                assert_eq!(failure.message(), "unknown command 'nope'");
            }
            other => panic!("expected an engine error, got {other:?}"),
        }
    }

    #[test]
    fn parse_reply_rejects_non_json() {
        assert!(matches!(
            parse_reply("ping", "not json"),
            Err(Error::MalformedReply)
        ));
    }

    #[test]
    fn parse_reply_missing_ok_is_malformed() {
        let err = parse_reply("ping", r#"{"id":1,"result":{}}"#).expect_err("no ok is invalid");
        assert!(matches!(err, Error::MalformedReply));
    }

    #[test]
    fn parse_reply_preserves_structured_graph_diagnostic() {
        let err = parse_reply(
            "vegetation-compile-biome",
            r#"{"id":4,"ok":false,"error":{"code":"diagnostic","message":"graph candidates limit exceeded: requested 16, limit 4","diagnostic":{"domain":"vegetation-graph","detail":{"category":"limit","resource":"candidates","requested":"16","limit":"4"}}}}"#,
        )
        .expect_err("diagnostic reply must be an error");
        let Error::Engine { failure, .. } = err else {
            panic!("expected an engine failure")
        };
        let ControlFailureDto::Diagnostic { diagnostic, .. } = *failure else {
            panic!("expected a diagnostic failure")
        };
        assert_eq!(
            diagnostic,
            saffron_protocol::ControlDiagnosticDto::VegetationGraph(
                saffron_protocol::VegetationGraphDiagnosticDto::Limit {
                    resource: "candidates".to_owned(),
                    requested: "16".to_owned(),
                    limit: "4".to_owned(),
                }
            )
        );
    }

    #[test]
    fn parse_reply_rejects_the_string_error_shape() {
        assert!(matches!(
            parse_reply(
                "nope",
                r#"{"id":1,"ok":false,"error":"unknown command 'nope'"}"#,
            ),
            Err(Error::MalformedReply)
        ));
    }

    fn test_socket_path(label: &str) -> String {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        std::env::temp_dir()
            .join(format!("sa-{label}-{}-{nonce:x}.sock", std::process::id()))
            .to_string_lossy()
            .into_owned()
    }

    /// A live round-trip against a one-shot in-process server: connect, read the framed request,
    /// reply with a canned envelope, and assert the client decodes the typed result. This proves
    /// the framing (the `<json>\n` request, the one reply line) end to end without an engine.
    #[test]
    fn client_round_trips_against_a_local_socket() {
        let path = test_socket_path("roundtrip");

        let listener = UnixListener::bind(&path).expect("bind");
        let server_path = path.clone();
        let handle = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().expect("accept");
            let mut buf = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                let n = conn.read(&mut chunk).expect("read");
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                if buf.contains(&b'\n') {
                    break;
                }
            }
            let request: Value = serde_json::from_str(
                std::str::from_utf8(&buf)
                    .unwrap()
                    .trim_end_matches('\n')
                    .trim_end(),
            )
            .expect("framed request parses");
            assert_eq!(request["cmd"], json!("ping"));
            let id = request["id"].clone();
            let reply = serde_json::to_string(&json!({
                "id": id,
                "ok": true,
                "result": { "pong": true, "engine": "Saffron Anima" },
            }))
            .unwrap();
            conn.write_all(reply.as_bytes()).expect("write reply");
            conn.write_all(b"\n").expect("write newline");
            let _ = server_path;
        });

        // Note: no `is_up()` probe here — the one-shot server `accept`s exactly once, and a probe
        // connection would consume that accept before the real call. `is_up` is covered separately.
        let mut client = Client::new(path.clone());
        let result = client.call_raw("ping", json!({})).expect("call");
        assert_eq!(result["pong"], json!(true));
        assert_eq!(result["engine"], json!("Saffron Anima"));

        handle.join().expect("server thread");
        let _ = std::fs::remove_file(&path);
    }

    /// `call_raw_text` returns the engine's reply bytes verbatim — a quoted decimal-string id stays
    /// quoted — so the byte-level decimal-string-u64 contract probe sees exactly what the wire
    /// carried (a parsed `Value` would have coerced it). It still lifts an `ok:false` envelope.
    #[test]
    fn call_raw_text_returns_verbatim_bytes() {
        let path = test_socket_path("raw");

        let listener = UnixListener::bind(&path).expect("bind");
        let handle = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().expect("accept");
            let mut buf = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                let n = conn.read(&mut chunk).expect("read");
                if n == 0 || {
                    buf.extend_from_slice(&chunk[..n]);
                    buf.contains(&b'\n')
                } {
                    break;
                }
            }
            // A canned reply with a u64-past-2^53 id as a quoted decimal string, verbatim.
            conn.write_all(br#"{"id":1,"ok":true,"result":{"id":"1099511627776"}}"#)
                .expect("write reply");
            conn.write_all(b"\n").expect("write newline");
        });

        let mut client = Client::new(path.clone());
        let raw = client
            .call_raw_text("create-entity", json!({}))
            .expect("call");
        assert!(
            raw.contains(r#""id":"1099511627776""#),
            "the quoted id survives verbatim: {raw}"
        );
        assert!(!raw.ends_with('\n'), "the trailing newline is trimmed");

        handle.join().expect("server thread");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn is_up_reflects_a_bound_listener() {
        let path = test_socket_path("up");
        let client = Client::new(path.clone());
        assert!(!client.is_up(), "nothing bound yet");
        let listener = UnixListener::bind(&path).expect("bind");
        assert!(client.is_up(), "a bound listener answers");
        drop(listener);
        let _ = std::fs::remove_file(&path);
    }
}

//! An e2e harness: boots the `saffron-host` binary headless and drives its control socket from
//! Cargo.
//!
//! It exists for engine-side regression tests that want a strongly-typed DTO assertion or a fixture
//! shared with a unit test in the same crate.
//!
//! The wire is the shared [`saffron_control_client::Client`] and the DTOs are the shared
//! `saffron-protocol` types, so this harness and the `sa` CLI cannot drift on framing or the `Uuid`
//! decimal-string encoding.
//!
//! Boot isolation: each [`TestEngine`] spawns its own headless weston on a per-run Wayland socket
//! and launches the host pointed at a per-run control socket, so two harnesses never collide. It
//! honors `SAFFRON_ANIMA_BIN` so it can run against an alternate host binary.

#![deny(unsafe_code)]

use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use saffron_control_client::{Client, Error as WireError};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

/// The message marker the engine's debug messenger emits for a validation-layer issue. A
/// line counts as a validation *error* when this appears with an `ERROR`-level `vulkan`
/// head (the compact log line is `<ts>  ERROR  vulkan  [validation] …`).
const VALIDATION_MARKER: &str = "[validation]";

/// The id-bearing keys the decimal-string-u64 contract scans for. A value under any of these keys
/// must be a quoted decimal string (or `null`), never a bare JSON number — JS cannot represent a
/// u64 past 2^53, so a number-encoded id corrupts silently in the editor.
const ID_KEYS: [&str; 9] = [
    "id",
    "mesh",
    "albedoTexture",
    "skyTexture",
    "texture",
    "entity",
    "parent",
    "parentId",
    "rootBone",
];

/// Scans a raw reply line's `result` region for the id-bearing keys and requires each value to be a
/// quoted decimal string that round-trips as a u64, or the literal `null`. Returns one message per
/// offending token (empty = clean).
///
/// It works on the *raw bytes* deliberately: a parsed [`Value`] coerces a JSON number into a
/// `Number` before this could see it, erasing the quoted-vs-bare distinction this is built to
/// catch.
#[must_use]
pub fn assert_raw_u64(raw: &str, label: &str) -> Vec<String> {
    let mut errors = Vec::new();
    let Some(start) = raw.find("\"result\"") else {
        return errors;
    };
    let result = &raw[start..];
    for key in ID_KEYS {
        let needle = format!("\"{key}\"");
        let mut search_from = 0;
        while let Some(rel) = result[search_from..].find(&needle) {
            let key_end = search_from + rel + needle.len();
            search_from = key_end;
            // Step over the colon (and any whitespace) to the value token.
            let after_key = result[key_end..].trim_start();
            let Some(after_colon) = after_key.strip_prefix(':') else {
                continue;
            };
            let value = after_colon.trim_start();
            let token = value_token(value);
            if token == "null" {
                continue;
            }
            match token.strip_prefix('"').and_then(|t| t.strip_suffix('"')) {
                Some(digits)
                    if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) =>
                {
                    if digits.parse::<u64>().is_err() {
                        errors.push(format!(
                            "{label}: id token '{token}' did not round-trip as u64"
                        ));
                    }
                }
                _ => errors.push(format!(
                    "{label}: id token '{token}' is not a quoted decimal string"
                )),
            }
        }
    }
    errors
}

/// The value token immediately following an id key's colon: a quoted string (through its closing
/// quote) or a run up to the next `,`, `}`, `]`, or whitespace.
fn value_token(value: &str) -> &str {
    if let Some(rest) = value.strip_prefix('"') {
        // A quoted string: include the closing quote (ids carry no escapes).
        if let Some(close) = rest.find('"') {
            return &value[..close + 2];
        }
        return value;
    }
    let end = value
        .find([',', '}', ']', ' ', '\t', '\n', '\r'])
        .unwrap_or(value.len());
    &value[..end]
}

/// How long to wait for the weston socket file to appear before giving up.
const WESTON_TIMEOUT: Duration = Duration::from_secs(10);

/// How long to wait for the host's control socket to appear (or the host to exit) after launch.
const CONTROL_TIMEOUT: Duration = Duration::from_secs(30);

/// A failure booting or driving the engine under test.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A child process (weston or the host) could not be spawned.
    #[error("spawning {what}: {source}")]
    Spawn {
        /// Which process failed to spawn.
        what: &'static str,
        /// The underlying OS error.
        source: std::io::Error,
    },
    /// A boot precondition timed out (the weston socket, or the control socket).
    #[error("timeout waiting for {what}")]
    Timeout {
        /// What the boot was waiting for.
        what: &'static str,
    },
    /// The host exited before its control socket appeared; the captured log is included.
    #[error("engine exited before the control socket appeared:\n{log}")]
    EngineExited {
        /// Everything the host wrote to stdout+stderr before exiting.
        log: String,
    },
    /// A control call failed (transport, engine error, or typed decode).
    #[error(transparent)]
    Wire(#[from] WireError),
}

/// The crate result alias bound to this crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// A spawned child plus the reader thread draining its stdout+stderr into a shared buffer.
struct Captured {
    child: Child,
    readers: Vec<JoinHandle<()>>,
}

impl Captured {
    /// Spawns the reader threads that append the child's stdout+stderr into `buffer`.
    fn capture(mut child: Child, buffer: &Arc<Mutex<String>>) -> Self {
        let mut readers = Vec::new();
        if let Some(stdout) = child.stdout.take() {
            readers.push(spawn_reader(stdout, Arc::clone(buffer)));
        }
        if let Some(stderr) = child.stderr.take() {
            readers.push(spawn_reader(stderr, Arc::clone(buffer)));
        }
        Self { child, readers }
    }

    /// SIGTERM the child and join its reader threads (they end on the pipe closing).
    fn terminate(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        for reader in self.readers {
            let _ = reader.join();
        }
    }
}

/// Drains `source` line-appending into `buffer` until EOF; one thread per pipe.
fn spawn_reader(
    mut source: impl Read + Send + 'static,
    buffer: Arc<Mutex<String>>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        loop {
            match source.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let text = String::from_utf8_lossy(&chunk[..n]);
                    if let Ok(mut guard) = buffer.lock() {
                        guard.push_str(&text);
                    }
                }
            }
        }
    })
}

/// A booted engine plus a typed control client. Always [`shutdown`](TestEngine::shutdown) it (the
/// `Drop` impl is a backstop that kills the children if a test panics before calling it).
pub struct TestEngine {
    client: Client,
    host: Option<Captured>,
    weston: Option<Captured>,
    log: Arc<Mutex<String>>,
    control_socket: String,
    wayland_socket_path: PathBuf,
    appdata_dir: PathBuf,
}

impl TestEngine {
    /// Boots a headless engine with `env` merged over the boot defaults, returning a driver bound to
    /// its control socket: a per-run weston + a per-run control socket, the engine pointed at both,
    /// stdout+stderr captured for [`validation_errors`].
    ///
    /// [`validation_errors`]: TestEngine::validation_errors
    pub fn boot(env: &[(&str, &str)]) -> Result<Self> {
        let runtime = runtime_dir();
        let stamp = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );

        let wl_socket = format!("wl-e2e-{stamp}");
        let wayland_socket_path = PathBuf::from(&runtime).join(&wl_socket);
        let weston_log = Arc::new(Mutex::new(String::new()));
        let weston_child = Command::new("weston")
            .args([
                "--backend=headless",
                "--width=1280",
                "--height=720",
                &format!("--socket={wl_socket}"),
                "--idle-time=0",
            ])
            .env("XDG_RUNTIME_DIR", &runtime)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|source| Error::Spawn {
                what: "weston",
                source,
            })?;
        let weston = Captured::capture(weston_child, &weston_log);

        if !wait_for(WESTON_TIMEOUT, || wayland_socket_path.exists()) {
            weston.terminate();
            return Err(Error::Timeout {
                what: "weston socket",
            });
        }

        let control_socket = format!("/tmp/saffron-e2e-{stamp}.sock");
        let log = Arc::new(Mutex::new(String::new()));

        // A per-run app-data root under the temp dir, so a booted project (e.g.
        // `SAFFRON_SCRATCH_PROJECT`) writes its `userdata/` there and never pollutes the source
        // tree — `cargo test` runs the host with the crate as cwd, where the default relative
        // `appdata/` would otherwise land. The caller can still override `SAFFRON_APPDATA_DIR`.
        let appdata_dir = std::env::temp_dir().join(format!("saffron-e2e-appdata-{stamp}"));

        let mut command = Command::new(engine_binary());
        command
            .env("XDG_RUNTIME_DIR", &runtime)
            .env("WAYLAND_DISPLAY", &wl_socket)
            .env("SDL_VIDEODRIVER", "wayland")
            .env("SAFFRON_CONTROL_SOCK", &control_socket)
            .env("SAFFRON_APPDATA_DIR", &appdata_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in env {
            command.env(key, value);
        }
        let host_child = command.spawn().map_err(|source| Error::Spawn {
            what: "engine host",
            source,
        })?;
        let mut host = Captured::capture(host_child, &log);

        // Wait for the control socket to appear, or the host to exit first (a boot failure).
        let socket = PathBuf::from(&control_socket);
        let appeared = wait_for(CONTROL_TIMEOUT, || {
            socket.exists() || host_has_exited(&mut host)
        });
        if host_has_exited(&mut host) {
            let captured = current_log(&log);
            host.terminate();
            weston.terminate();
            let _ = std::fs::remove_file(&wayland_socket_path);
            let _ = std::fs::remove_dir_all(&appdata_dir);
            return Err(Error::EngineExited { log: captured });
        }
        if !appeared {
            host.terminate();
            weston.terminate();
            let _ = std::fs::remove_dir_all(&appdata_dir);
            return Err(Error::Timeout {
                what: "control socket",
            });
        }

        Ok(Self {
            client: Client::new(control_socket.clone()),
            host: Some(host),
            weston: Some(weston),
            log,
            control_socket,
            wayland_socket_path,
            appdata_dir,
        })
    }

    /// The control-socket path this engine answers on (per-run, isolated).
    #[must_use]
    pub fn control_socket(&self) -> &str {
        &self.control_socket
    }

    /// Everything the engine has written to stdout+stderr so far (a snapshot of the captured log).
    #[must_use]
    pub fn log(&self) -> String {
        current_log(&self.log)
    }

    /// The validation-layer error lines (empty = clean): the debug messenger prints them as
    /// `<ts>  ERROR  vulkan  [validation] …`. A line qualifies when `[validation]` appears
    /// with an `ERROR`-level `vulkan` head — robust to the subsystem column's padding.
    #[must_use]
    pub fn validation_errors(&self) -> Vec<String> {
        current_log(&self.log)
            .lines()
            .filter(|line| match line.find(VALIDATION_MARKER) {
                Some(idx) => {
                    let head = &line[..idx];
                    head.contains("ERROR") && head.contains("vulkan")
                }
                None => false,
            })
            .map(str::to_owned)
            .collect()
    }

    /// Sends one control command and decodes its `result` into the typed DTO `R`.
    pub fn call<R: DeserializeOwned>(&mut self, cmd: &str, params: Value) -> Result<R> {
        Ok(self.client.call(cmd, params)?)
    }

    /// Sends one control command and returns its raw `result` [`Value`] (the untyped path, for
    /// commands without a convenient DTO or when only a field or two matters).
    pub fn call_raw(&mut self, cmd: &str, params: Value) -> Result<Value> {
        Ok(self.client.call_raw(cmd, params)?)
    }

    /// Sends one control command and returns the raw reply line verbatim — the byte-exact path the
    /// decimal-string-u64 contract probe ([`assert_raw_u64`]) needs (a parsed [`Value`] already
    /// erases the quoted-vs-bare distinction it is built to catch).
    pub fn call_raw_text(&mut self, cmd: &str, params: Value) -> Result<String> {
        Ok(self.client.call_raw_text(cmd, params)?)
    }

    /// Lets the engine run a few render frames so deferred GPU work + validation surface.
    pub fn settle(&self, duration: Duration) {
        std::thread::sleep(duration);
    }

    /// Tears down cleanly: ask the engine to `quit`, then SIGTERM the host and weston and join the
    /// capture threads. Idempotent — a second call (or the `Drop` backstop) is a no-op.
    pub fn shutdown(&mut self) {
        // Best-effort graceful quit; the engine may already be gone or race the socket close.
        let _ = self.client.call_raw("quit", json!({}));
        if let Some(host) = self.host.take() {
            host.terminate();
        }
        if let Some(weston) = self.weston.take() {
            weston.terminate();
        }
        let _ = std::fs::remove_file(&self.control_socket);
        let _ = std::fs::remove_file(&self.wayland_socket_path);
        let _ = std::fs::remove_dir_all(&self.appdata_dir);
    }
}

impl Drop for TestEngine {
    fn drop(&mut self) {
        // Backstop for a test that panics before `shutdown`: never leak a host or weston process.
        if self.host.is_some() || self.weston.is_some() {
            self.shutdown();
        }
    }
}

/// `XDG_RUNTIME_DIR` if set, else `/run/user/<uid>`.
fn runtime_dir() -> String {
    std::env::var("XDG_RUNTIME_DIR")
        .unwrap_or_else(|_| format!("/run/user/{}", rustix::process::getuid().as_raw()))
}

/// The host binary to spawn: `SAFFRON_ANIMA_BIN` if set, else the `saffron-host` sibling of this
/// test binary under `target/<profile>/`.
fn engine_binary() -> PathBuf {
    if let Ok(path) = std::env::var("SAFFRON_ANIMA_BIN") {
        return PathBuf::from(path);
    }
    // The test binary lives in `target/<profile>/deps/`; the host is two levels up, in
    // `target/<profile>/saffron-host`.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(profile_dir) = exe.parent().and_then(|deps| deps.parent()) {
            return profile_dir.join("saffron-host");
        }
    }
    PathBuf::from("saffron-host")
}

/// Whether `child` has exited (a non-blocking `try_wait`).
fn host_has_exited(captured: &mut Captured) -> bool {
    matches!(captured.child.try_wait(), Ok(Some(_)))
}

/// A snapshot copy of the shared capture buffer.
fn current_log(buffer: &Arc<Mutex<String>>) -> String {
    buffer.lock().map(|g| g.clone()).unwrap_or_default()
}

/// Polls `ready` every 50ms until it returns `true` or `timeout` elapses; returns the final state.
fn wait_for(timeout: Duration, mut ready: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if ready() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    ready()
}

#[cfg(test)]
mod tests {
    use super::assert_raw_u64;

    /// The decimal-string-u64 detector is live, not vacuous: it accepts a quoted decimal-string id
    /// (the encoding `saffron-protocol`'s `Uuid` adapter emits) and **bites** a number-encoded id
    /// (what a plain serde `u64` would emit, the single most dangerous silent wire failure).
    #[test]
    fn assert_raw_u64_accepts_decimal_strings_and_bites_numbers() {
        // Positive: a u64 past 2^53 emitted as a quoted decimal string passes clean.
        let good = r#"{"ok":true,"result":{"id":"1099511627776","name":"Cube"}}"#;
        assert!(
            assert_raw_u64(good, "good").is_empty(),
            "a correct decimal-string id must pass"
        );

        // Negative: the same id as a bare JSON number is caught (the gate bites).
        let bad = r#"{"ok":true,"result":{"id":1099511627776,"name":"Cube"}}"#;
        let errors = assert_raw_u64(bad, "bad");
        assert_eq!(
            errors.len(),
            1,
            "exactly one offending token, got {errors:?}"
        );
        assert!(
            errors[0].contains("not a quoted decimal string"),
            "the message names the failure: {}",
            errors[0]
        );
    }

    /// Every id-bearing key is scanned, `null` is allowed, and a number under any of them bites.
    #[test]
    fn assert_raw_u64_covers_every_id_key_and_allows_null() {
        let clean = concat!(
            r#"{"ok":true,"result":{"id":"1","mesh":"2","albedoTexture":"3","#,
            r#""skyTexture":"4","texture":"5","entity":"6","parent":"7","#,
            r#""parentId":"8","rootBone":null}}"#,
        );
        assert!(
            assert_raw_u64(clean, "clean").is_empty(),
            "decimal-string ids + a null id must pass"
        );

        for key in super::ID_KEYS {
            if key == "rootBone" {
                continue; // exercised as null above; the number form is covered by the others
            }
            let raw = format!(r#"{{"ok":true,"result":{{"{key}":42}}}}"#);
            let errors = assert_raw_u64(&raw, key);
            assert_eq!(
                errors.len(),
                1,
                "a number under '{key}' must be caught, got {errors:?}"
            );
        }
    }

    /// The scan is anchored to the `result` region: an id-shaped token before `"result"` (in the
    /// envelope's `id` request-echo, say) is ignored.
    #[test]
    fn assert_raw_u64_ignores_tokens_before_result() {
        // A bare-number `id` in the pre-`result` region must not trip the detector.
        let raw = r#"{"id":7,"ok":true,"result":{"id":"7"}}"#;
        assert!(
            assert_raw_u64(raw, "pre-result").is_empty(),
            "only the result region is scanned"
        );
    }

    /// A reply with no `result` key (a malformed or error envelope) yields no id findings — the
    /// detector never panics on a missing region.
    #[test]
    fn assert_raw_u64_handles_missing_result() {
        let raw = r#"{"ok":false,"error":"boom"}"#;
        assert!(assert_raw_u64(raw, "no-result").is_empty());
    }
}

//! The RMB fly-cam input stream: a persistent, non-blocking control-socket connection the pump
//! loop writes one `fly-input` sample to per iteration, so the stream is paced at the monitor
//! refresh. The samples are fed by the locked-pointer motion accumulator and main-thread key
//! tracking — no input sample crosses CEF; the frontend only starts/stops the stream, passing
//! its fly keybindings as DOM `KeyboardEvent.code` strings.

use saffron_protocol::FlyInputParams;
use serde_json::json;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use winit::keyboard::KeyCode;

/// Cap on buffered unsent bytes: past this the engine has clearly stopped reading and the
/// connection is reported dead rather than growing without bound.
const OUTBUF_CAP: usize = 16 * 1024;

/// The six move bindings as DOM `KeyboardEvent.code` strings ("KeyW", "Space", "ShiftLeft").
/// winit's `KeyCode` follows the same W3C code names, which is how [`FlyStream::on_key`]
/// matches a physical key against them.
pub struct FlyBindings {
    pub forward: String,
    pub back: String,
    pub left: String,
    pub right: String,
    pub up: String,
    pub down: String,
}

/// The live stream: the engine connection, the tracked key state, and the unsent tail of the
/// last sample (a non-blocking write can land short; the remainder goes out next tick).
pub struct FlyStream {
    stream: UnixStream,
    bindings: FlyBindings,
    forward: bool,
    back: bool,
    left: bool,
    right: bool,
    up: bool,
    down: bool,
    outbuf: Vec<u8>,
    next_id: u64,
}

impl FlyStream {
    /// Connects to the engine control socket in non-blocking mode with all keys released.
    ///
    /// # Errors
    ///
    /// The connect or `set_nonblocking` failure, when the engine socket is unavailable.
    pub fn connect(socket_path: &str, bindings: FlyBindings) -> std::io::Result<Self> {
        let stream = UnixStream::connect(socket_path)?;
        stream.set_nonblocking(true)?;
        Ok(Self {
            stream,
            bindings,
            forward: false,
            back: false,
            left: false,
            right: false,
            up: false,
            down: false,
            outbuf: Vec::new(),
            next_id: 1,
        })
    }

    /// Updates the tracked key state from a physical key event. winit `KeyCode` Debug names are
    /// the W3C code names the bindings use, so the match is a string compare.
    ///
    /// Repeat events are ignored: they carry no edge information, and a queued synthetic repeat
    /// delivered after the release edge would re-press the key and stick it.
    pub fn on_key(&mut self, code: KeyCode, pressed: bool, repeat: bool) {
        if repeat {
            return;
        }
        let name = format!("{code:?}");
        if name == self.bindings.forward {
            self.forward = pressed;
        } else if name == self.bindings.back {
            self.back = pressed;
        } else if name == self.bindings.left {
            self.left = pressed;
        } else if name == self.bindings.right {
            self.right = pressed;
        } else if name == self.bindings.up {
            self.up = pressed;
        } else if name == self.bindings.down {
            self.down = pressed;
        }
    }

    /// Queues one `fly-input` sample (the accumulated look delta + key state), flushes as much
    /// buffered output as the socket takes, and drains any pending replies. Returns `false`
    /// once the connection is dead — the caller drops the stream.
    #[allow(clippy::cast_possible_truncation)]
    pub fn send_sample(&mut self, look: (f64, f64), active: bool) -> bool {
        let params = FlyInputParams {
            active: Some(active),
            look_dx: Some(look.0 as f32),
            look_dy: Some(look.1 as f32),
            forward: Some(active && self.forward),
            back: Some(active && self.back),
            left: Some(active && self.left),
            right: Some(active && self.right),
            up: Some(active && self.up),
            down: Some(active && self.down),
        };
        let mut line =
            json!({ "id": self.next_id, "cmd": "fly-input", "params": params }).to_string();
        self.next_id = self.next_id.wrapping_add(1);
        line.push('\n');
        if self.outbuf.len() + line.len() > OUTBUF_CAP {
            return false;
        }
        self.outbuf.extend_from_slice(line.as_bytes());

        // Flush the buffered tail; a short write keeps the remainder for the next tick.
        while !self.outbuf.is_empty() {
            match self.stream.write(&self.outbuf) {
                Ok(0) => return false,
                Ok(n) => {
                    self.outbuf.drain(..n);
                }
                Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == ErrorKind::Interrupted => {}
                Err(_) => return false,
            }
        }

        // Drain and discard replies so they never fill the socket buffer.
        let mut scratch = [0_u8; 4096];
        loop {
            match self.stream.read(&mut scratch) {
                Ok(0) => return false,
                Ok(_) => {}
                Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == ErrorKind::Interrupted => {}
                Err(_) => return false,
            }
        }

        true
    }

    /// Sends the final inactive sample and flushes any buffered tail with a short blocking
    /// timeout — the release must land, or the engine would keep flying with the last-held keys.
    pub fn finish(mut self) {
        self.send_sample((0.0, 0.0), false);
        if self.outbuf.is_empty() {
            return;
        }
        let _ = self.stream.set_nonblocking(false);
        let _ = self
            .stream
            .set_write_timeout(Some(std::time::Duration::from_millis(100)));
        let _ = self.stream.write_all(&self.outbuf);
    }
}
